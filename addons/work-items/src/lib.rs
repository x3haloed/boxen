pub mod ado;
pub mod approvals;
#[path = "../../../shared/browser_assignment.rs"]
mod browser_assignment;
mod managed_browser;
pub mod operator;
pub mod process;
pub mod supervision;
#[cfg(test)]
mod tests;
pub mod transport;
pub mod workflow;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub fn key(s: &str) -> Result<()> {
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "64 lowercase hexadecimal characters required"
    );
    Ok(())
}
pub fn event(name: &str, data: Value) {
    println!("{}", json!({"event":name,"at":now(),"data":data}));
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub boxen_home: PathBuf,
    pub url: String,
    pub owner: String,
    pub client_id: String,
    pub definition_id: String,
    pub definition_revision: u64,
    pub pack_root: PathBuf,
    pub pack_hash: String,
    pub capacity: usize,
    #[serde(default)]
    pub supervision: Option<supervision::Settings>,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.owner,
            &self.client_id,
            &self.definition_id,
            &self.pack_hash,
        ] {
            key(value)?;
        }
        ensure!(
            self.definition_revision > 0 && (1..=16).contains(&self.capacity),
            "Definition revision and capacity (1–16) required"
        );
        ensure!(
            self.boxen_home.is_absolute() && self.pack_root.is_absolute(),
            "Absolute local paths required"
        );
        let addr = self
            .url
            .strip_prefix("http://")
            .context("Loopback HTTP origin required")?
            .parse::<std::net::SocketAddr>()?;
        ensure!(
            addr.ip().is_loopback(),
            "Credentials are used only with a loopback origin"
        );
        if let Some(settings) = &self.supervision {
            settings.validate()?;
            ensure!(
                settings.notify_to
                    != json!({"type":"client","machine":self.owner,"id":self.client_id}),
                "Notification destination differs from the report inbox"
            );
        }
        Ok(())
    }
    pub fn read(home: &Path) -> Result<Self> {
        let c: Self = serde_json::from_slice(&fs::read(home.join("config.json"))?)?;
        c.validate()?;
        Ok(c)
    }
}

#[derive(Clone)]
pub struct Store {
    pub path: PathBuf,
}
impl Store {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        fs::create_dir_all(path.parent().context("Store parent missing")?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700))?;
        }
        let s = Self { path };
        let c = s.connect()?;
        c.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY,v TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS legacy_records(source TEXT,item INTEGER,kind TEXT,raw TEXT,PRIMARY KEY(source,item,kind));
        CREATE TABLE IF NOT EXISTS work(id TEXT PRIMARY KEY,source TEXT,item INTEGER,coder TEXT NOT NULL,state TEXT NOT NULL,version INTEGER NOT NULL,holds_slot INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,error TEXT,UNIQUE(source,item));
        CREATE TABLE IF NOT EXISTS assignments(job TEXT,version INTEGER,digest TEXT,input TEXT,packet TEXT,instantiate TEXT,message TEXT,claims TEXT,created INTEGER,PRIMARY KEY(job,version));
        CREATE TABLE IF NOT EXISTS assignment_requests(k TEXT PRIMARY KEY,input TEXT,job TEXT,version INTEGER);
        CREATE TABLE IF NOT EXISTS reservations(resource TEXT PRIMARY KEY,job TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS effects(job TEXT,version INTEGER,kind TEXT,state TEXT NOT NULL,detail TEXT,PRIMARY KEY(job,version,kind));
        CREATE TABLE IF NOT EXISTS workflow_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,job TEXT,kind TEXT,data TEXT,at INTEGER);")?;
        ado::migrate(&c)?;
        supervision::migrate(&c)?;
        workflow::migrate(&c)?;
        approvals::migrate(&c)?;
        operator::migrate(&c)?;
        Ok(s)
    }
    pub fn connect(&self) -> Result<Connection> {
        let c = Connection::open(&self.path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA synchronous=FULL;")?;
        Ok(c)
    }
    pub fn log(c: &Connection, job: &str, kind: &str, data: Value) -> Result<()> {
        c.execute(
            "INSERT INTO workflow_events(job,kind,data,at) VALUES(?1,?2,?3,?4)",
            params![job, kind, data.to_string(), now()],
        )?;
        Ok(())
    }
    pub fn import_legacy(&self, path: &Path) -> Result<Value> {
        let legacy = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let raw: String =
            legacy.query_row("SELECT json FROM ado_config WHERE id=1", [], |r| r.get(0))?;
        let mut cfg: ado::Config = serde_json::from_str(&raw)?;
        ensure!(
            !cfg.enabled,
            "Legacy mirror must be disabled and idle before import"
        );
        let running: i64 =
            legacy.query_row("SELECT COALESCE(sum(running),0) FROM ado_sync", [], |r| {
                r.get(0)
            })?;
        ensure!(running == 0, "Legacy scan is still running");
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE k='legacy_import')",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            !exists,
            "Legacy import is already recorded; mirror state is not reset by a retry"
        );
        // Exactly these public mirror columns are copied; actor tokens are never read.
        for (table, columns) in [
            ("ado_items", 9),
            ("ado_revisions", 4),
            ("ado_entities", 5),
            ("ado_events", 9),
        ] {
            let mut q = legacy.prepare(&format!("SELECT * FROM {table}"))?;
            let rows = q.query_map([], |r| {
                (0..columns)
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?;
            let markers = (1..=columns)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            for row in rows {
                tx.execute(
                    &format!("INSERT INTO {table} VALUES({markers})"),
                    rusqlite::params_from_iter(row?),
                )?;
            }
        }
        for (table, kind, query) in [
            (
                "ownership",
                "ownership",
                "SELECT source,item,json FROM interpretation_ownership",
            ),
            (
                "decisions",
                "decision",
                "SELECT source,item,decision FROM interpretation_requests WHERE state='decided' AND decision IS NOT NULL ORDER BY seq",
            ),
            (
                "jobs",
                "job",
                "SELECT source,item,json_object('id',id,'state',state,'actor',actor,'version',version,'commit',commit_sha,'worktree',worktree,'branch',branch) FROM jobs",
            ),
        ] {
            let mut q = legacy.prepare(query)?;
            for row in q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (source, item, raw) = row?;
                tx.execute(
                    "INSERT OR REPLACE INTO legacy_records VALUES(?1,?2,?3,?4)",
                    params![source, item, kind, raw],
                )?;
            }
            let _ = table;
        }
        cfg.enabled = true;
        tx.execute(
            "INSERT INTO ado_config VALUES(1,?1)",
            [serde_json::to_string(&cfg)?],
        )?;
        tx.execute(
            "INSERT INTO ado_sync(source,requested) VALUES(?1,1)",
            [cfg.source()],
        )?;
        tx.execute(
            "INSERT INTO meta VALUES('legacy_import',?1)",
            [path.to_string_lossy().as_ref()],
        )?;
        Self::log(&tx, "", "mirror.imported", json!({"source":cfg.source()}))?;
        tx.commit()?;
        Ok(json!({"imported":true,"interval_minutes":cfg.interval_minutes}))
    }
    pub fn packet(&self, item: i64) -> Result<Value> {
        packet_on(&self.connect()?, &ado::config(self)?.source(), item)
    }
    pub fn assign(&self, cfg: &Config, input: &Value) -> Result<Value> {
        let request_key = input["idempotency_key"]
            .as_str()
            .context("Explicit operator idempotency_key required")?;
        ensure!(
            !request_key.is_empty() && request_key.len() <= 200,
            "Request key must contain 1–200 bytes"
        );
        let encoded = input.to_string();
        ensure!(encoded.len() <= 128 * 1024, "Assignment exceeds 128 KiB");
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((old, job, version)) = tx
            .query_row(
                "SELECT input,job,version FROM assignment_requests WHERE k=?1",
                [request_key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
        {
            ensure!(
                old == encoded,
                "Assignment key already has different inputs"
            );
            return Ok(json!({"job_id":job,"version":version,"idempotent":true}));
        }
        let item = input["item"]
            .as_i64()
            .filter(|v| *v > 0)
            .context("Work item ID required")?;
        let source = ado::config(self)?.source();
        let packet = packet_on(&tx, &source, item)?;
        ensure!(
            packet["source_digest"] == input["source_digest"],
            "Source digest changed; assignment needs current evidence"
        );
        ensure!(
            assignment_eligible(&packet, &input["eligibility_override"]),
            "Work item is not currently eligible; an explicit operator exception requires reason and attributed authority"
        );
        if packet["eligible"] != true {
            ensure!(
                packet["evidence"]["item"]["fields"]["System.TeamProject"]
                    == ado::config(self)?.project,
                "Operator exception cannot cross projects"
            );
        }
        ensure!(
            now() - packet["observed"].as_i64().unwrap_or(0) < 30 * 60_000,
            "Mirror is stale; refresh before assignment"
        );
        ensure!(
            matches!(
                input["context"]["decision"].as_str(),
                Some("new_implementation" | "return_to_coder" | "investigate")
            ),
            "An explicit implementation or bounded-investigation decision is required"
        );
        ensure!(
            input["context"]["evidence"]
                .as_array()
                .is_some_and(|v| !v.is_empty()),
            "Context decision needs attributed evidence references"
        );
        let assignment = &input["assignment"];
        managed_browser::validate_assignment(assignment)?;
        let investigation = input["context"]["decision"] == "investigate";
        if investigation {
            ensure!(
                assignment["task"]["mode"] == "investigation"
                    && matches!(
                        assignment["task"]["validation"].as_str(),
                        Some("read_only" | "isolated_synthetic")
                    )
                    && assignment["task"]["production_edits"] == false
                    && assignment["task"]["external_writes"] == false,
                "Investigation names read_only/isolated_synthetic validation with production edits and external writes false"
            );
            ensure!(
                input["context"]["investigation_rationale"]
                    .as_str()
                    .is_some_and(|v| !v.trim().is_empty()),
                "Investigation rationale identifies useful work independent of unanswered questions"
            );
        }
        for field in [
            "job_id",
            "assignment_revision",
            "repository_root",
            "worktree",
            "branch",
            "base_commit",
            "evidence_dir",
        ] {
            let v = assignment[field]
                .as_str()
                .context(format!("Assignment {field} required"))?;
            ensure!(
                !v.trim().is_empty() && !v.contains("REPLACE_WITH_"),
                "Assignment contains placeholders"
            );
        }
        for field in ["repository_root", "worktree", "evidence_dir"] {
            ensure!(
                Path::new(assignment[field].as_str().unwrap()).is_absolute(),
                "Absolute assignment paths required"
            );
        }
        ensure!(
            assignment["project_pack"]["manifest_sha256"] == cfg.pack_hash,
            "Assigned pack differs from pinned definition"
        );
        ensure!(
            assignment["work_item"]["id"] == item
                && assignment["work_item"]["source_digest"] == input["source_digest"]
                && assignment["work_item"]["revision"] == packet["revision"],
            "Assignment work-item evidence mismatch"
        );
        ensure!(
            assignment["task"]["request"]
                .as_str()
                .is_some_and(|v| !v.trim().is_empty())
                && assignment["task"]["acceptance_criteria"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty()),
            "Task and acceptance criteria required"
        );
        for flag in [
            "push",
            "create_or_update_pr",
            "merge",
            "deploy",
            "autocomplete",
        ] {
            ensure!(
                assignment["delivery"][flag] == false,
                "This slice accepts local handoff only; publication gates are not migrated"
            );
        }
        let legacy = &packet["legacy"];
        if legacy.as_object().is_some_and(|v| !v.is_empty()) {
            ensure!(
                input["legacy_reconciliation"]
                    .as_str()
                    .is_some_and(|v| !v.trim().is_empty()),
                "Historical ownership/holds/PRs need explicit reconciliation"
            );
        }
        if legacy["decision"]["kind"] == "wait_for_clarification"
            || legacy["job"]["state"] == "awaiting_clarification"
        {
            if investigation {
                ensure!(
                    input["context"]["open_questions"]
                        .as_array()
                        .is_some_and(|v| !v.is_empty()),
                    "Investigation retains unresolved questions; it does not release implementation holds"
                );
            } else {
                ensure!(
                    input["context"]["authoritative_answers"]
                        .as_array()
                        .is_some_and(|v| !v.is_empty()),
                    "Existing clarification hold needs attributed answers before implementation"
                );
            }
        }
        let old: Option<(String, String, i64, i64)> = tx
            .query_row(
                "SELECT id,coder,version,holds_slot FROM work WHERE source=?1 AND item=?2",
                params![source, item],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let previous_create: Option<String> = if let Some((job, _, version, _)) = &old {
            Some(tx.query_row(
                "SELECT instantiate FROM assignments WHERE job=?1 AND version=?2",
                params![job, version],
                |r| r.get(0),
            )?)
        } else {
            None
        };
        let new_copy = input["new_copy"] == true;
        let replacement = input.get("replacement").filter(|v| !v.is_null());
        let continuation = input.get("continuation").filter(|v| !v.is_null());
        let mut retained_from = None;
        ensure!(
            continuation.is_none() || old.is_some(),
            "Continuation requires existing work"
        );
        ensure!(
            replacement.is_none() || old.is_some(),
            "Replacement requires an existing assignment"
        );
        let (job, coder, version) = if let Some((job, coder, version, held)) = old {
            ensure!(
                held == 0 || continuation.is_some(),
                "Current reservation must be released after the coder becomes idle before refreshing"
            );
            if let Some(continuation) = continuation {
                ensure!(
                    held == 1
                        && input["new_copy"] == false
                        && replacement.is_none()
                        && input["coder_id"] == coder
                        && continuation["expected_coder"] == coder
                        && continuation["expected_version"] == version
                        && continuation["reason"]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 4096),
                    "Retained continuation requires the expected held owner/version, same coder and reason"
                );
                retained_from = Some(version);
            }
            if let Some(replacement) = replacement {
                let state: String =
                    tx.query_row("SELECT state FROM work WHERE id=?1", [&job], |r| r.get(0))?;
                ensure!(
                    new_copy && input.get("coder_id").is_none() && state == "paused",
                    "Explicit replacement creates a new copy only from paused work"
                );
                ensure!(
                    replacement["expected_coder"] == coder
                        && replacement["expected_version"] == version,
                    "Replacement owner/version changed"
                );
                for field in ["reason", "authority", "handoff"] {
                    ensure!(
                        replacement[field]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty()),
                        "Replacement reason, attributed authority and retained handoff are required"
                    );
                }
                let next_coder = hash(&format!("coder:{job}:{request_key}"));
                Self::log(
                    &tx,
                    &job,
                    "ownership.transferred",
                    json!({"from":coder,"to":next_coder,"previous_version":version,"replacement":replacement}),
                )?;
                (job, next_coder, version + 1)
            } else {
                ensure!(
                    !new_copy,
                    "Existing work retains its coder; another copy is not created"
                );
                ensure!(
                    input.get("coder_id").is_none_or(|v| v == &coder),
                    "Existing coder binding cannot change"
                );
                (job, coder, version + 1)
            }
        } else {
            let job = hash(&format!("work:{source}:{item}"));
            ensure!(
                !(new_copy
                    && (legacy["ownership"]["lifecycle"] == "unfinished"
                        || (legacy["job"]["actor"].is_string()
                            && legacy["job"]["state"] != "closed"))),
                "Existing unfinished coder ownership requires adoption, not a fresh copy"
            );
            let coder = if new_copy {
                hash(&format!("coder:{job}:{request_key}"))
            } else {
                input["coder_id"]
                    .as_str()
                    .context("Explicit new_copy=true or an adopted coder_id is required")?
                    .to_string()
            };
            key(&coder)?;
            (job, coder, 1)
        };
        let planned = if let Some(attempt) = input["context"]["workflow_id"].as_str() {
            tx.query_row("SELECT EXISTS(SELECT 1 FROM workflow_attempts WHERE id=?1 AND source=?2 AND item=?3 AND digest=?4 AND stage='ready')",params![attempt,source,item,input["source_digest"].as_str()],|r|r.get::<_,bool>(0))?
        } else {
            false
        };
        let claims = resource_claims(assignment, planned)?;
        if let Some(previous) = retained_from {
            let raw: String = tx.query_row(
                "SELECT claims FROM assignments WHERE job=?1 AND version=?2",
                params![job, previous],
                |r| r.get(0),
            )?;
            let old_claims: Vec<String> = serde_json::from_str(&raw)?;
            let held_claims = tx
                .prepare("SELECT resource FROM reservations WHERE job=?1 ORDER BY resource")?
                .query_map([&job], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                claims == old_claims && claims == held_claims,
                "Retained continuation requires identical existing resource claims"
            );
            tx.execute(
                "UPDATE supervision SET closed=1,phase='superseded' WHERE job=?1 AND version=?2",
                params![job, previous],
            )?;
            Self::log(
                &tx,
                &job,
                "assignment.continued",
                json!({"previous_version":previous,"version":version,"coder":coder,"continuation":continuation,"resources_retained":true}),
            )?;
        }
        let instantiate = if new_copy {
            json!({"op":"resident_definition","args":{"action":"instantiate","spec":{"id":coder,"definition_id":cfg.definition_id,"definition_revision":cfg.definition_revision,"overrides":{"name":format!("coder~{coder}")}}}})
        } else {
            previous_create
                .map(|v| serde_json::from_str(&v))
                .transpose()?
                .unwrap_or(Value::Null)
        };
        let mid = hash(&format!("assignment:{job}:{version}"));
        if packet["eligible"] != true {
            Self::log(
                &tx,
                &job,
                "eligibility.overridden",
                json!({"item":item,"version":version,"override":input["eligibility_override"]}),
            )?;
        }
        let bundle = json!({"assignment":assignment,"context":input["context"],"legacy_reconciliation":input["legacy_reconciliation"],"replacement":input["replacement"],"continuation":continuation,"eligibility_override":input["eligibility_override"],"source_packet":packet}).to_string();
        let packet_path = self
            .path
            .parent()
            .unwrap()
            .join("evidence")
            .join(&job)
            .join(version.to_string())
            .join("packet.json");
        let header = format!(
            "[Work-item service — accepted assignment record; source content retains its original authors]\nJob {job}, version {version}. This service records an explicit operator request or configured human-authorized workflow decision, as attributed in context; it is not a new human-authored message. The JSON below identifies the frozen assignment and full source/evidence file on this executing host. That file's SHA-256 verifies its exact retained bytes. Local investigation/editing/checks are scoped by the assignment. This implementation assignment grants no direct publication. A configured automatic workflow can request separate exact-commit reviews and publish a ready candidate through its retained publication stage. Source observations can become stale after acceptance; workflow freshness is separate from implementation success. Runtime claims coordinate this service's assignments, not arbitrary processes on the host. Explicit mailbox replies go to the authenticated reply_to address. Private narration is not a handoff.\n"
        );
        let contract = cfg
            .supervision
            .as_ref()
            .map_or_else(String::new, |_| supervision::contract(&job, version, &mid));
        let packet_reference = json!({"packet_path":packet_path,"packet_sha256":hash(&bundle),"source_digest":input["source_digest"]});
        let mut envelope = packet_reference.clone();
        envelope["assignment"] = assignment.clone();
        envelope["context"] = input["context"].clone();
        let mut body = format!("{header}{envelope}{contract}");
        if body.len() > 32768 {
            body = format!(
                "{header}The verified packet contains the complete assignment and context, including any review findings. Reading those fields establishes the scope before work begins.\n{packet_reference}{contract}"
            );
        }
        ensure!(
            body.len() <= 32768,
            "Assignment/source packet exceeds mailbox body bound; no partial assignment was accepted"
        );
        let message = json!({"id":mid,"to":{"type":"resident","id":coder},"subject":format!("Work item {item}: assignment v{version}"),"body":body});
        let next_state = if retained_from.is_some() {
            "reserved"
        } else {
            "queued"
        };
        tx.execute("INSERT INTO work(id,source,item,coder,state,version,created) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source,item) DO UPDATE SET coder=excluded.coder,state=excluded.state,version=excluded.version,error=NULL",params![job,source,item,coder,next_state,version,now()])?;
        tx.execute(
            "INSERT INTO assignments VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                job,
                version,
                input["source_digest"].as_str(),
                encoded,
                bundle,
                instantiate.to_string(),
                message.to_string(),
                json!(claims).to_string(),
                now()
            ],
        )?;
        tx.execute(
            "INSERT INTO assignment_requests VALUES(?1,?2,?3,?4)",
            params![request_key, encoded, job, version],
        )?;
        Self::log(
            &tx,
            &job,
            "assignment.accepted",
            json!({"version":version,"coder":coder}),
        )?;
        if let Some(settings) = &cfg.supervision {
            settings.validate()?;
            tx.execute("INSERT INTO supervision(job,version,coder,message,settings,activity) VALUES(?1,?2,?3,?4,?5,?6)",params![job,version,coder,mid,serde_json::to_string(settings)?,now()])?;
        }
        tx.commit()?;
        Ok(
            json!({"job_id":job,"coder_id":coder,"version":version,"state":next_state,"message_id":mid}),
        )
    }
    pub fn reserve(&self, capacity: usize) -> Result<Option<String>> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let held: i64 = tx.query_row("SELECT count(*) FROM work WHERE holds_slot=1", [], |r| {
            r.get(0)
        })?;
        if held >= capacity as i64 {
            return Ok(None);
        }
        let candidates=tx.prepare("SELECT w.id,w.version FROM work w JOIN assignments a ON a.job=w.id AND a.version=w.version WHERE w.state='queued' AND NOT EXISTS(SELECT 1 FROM json_each(a.claims) claim JOIN reservations r ON r.resource=claim.value) ORDER BY w.created,w.id LIMIT 1")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (job, version) in candidates {
            let raw: String = tx.query_row(
                "SELECT claims FROM assignments WHERE job=?1 AND version=?2",
                params![job, version],
                |r| r.get(0),
            )?;
            let claims: Vec<String> = serde_json::from_str(&raw)?;
            let mut conflict = false;
            for claim in &claims {
                if tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM reservations WHERE resource=?1)",
                    [claim],
                    |r| r.get::<_, bool>(0),
                )? {
                    conflict = true;
                    break;
                }
            }
            if conflict {
                continue;
            }
            for claim in &claims {
                tx.execute(
                    "INSERT INTO reservations VALUES(?1,?2)",
                    params![claim, job],
                )?;
            }
            tx.execute(
                "UPDATE work SET state='reserved',holds_slot=1,error=NULL WHERE id=?1",
                [&job],
            )?;
            Self::log(
                &tx,
                &job,
                "capacity.reserved",
                json!({"claims":claims,"version":version}),
            )?;
            tx.commit()?;
            return Ok(Some(job));
        }
        Ok(None)
    }
    pub fn status(&self) -> Result<Value> {
        let c = self.connect()?;
        let jobs=c.prepare("SELECT id,item,coder,state,version,holds_slot,error FROM work ORDER BY created DESC LIMIT 200")?.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"item":r.get::<_,i64>(1)?,"coder":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"version":r.get::<_,i64>(4)?,"holds_slot":r.get::<_,bool>(5)?,"error":r.get::<_,Option<String>>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(
            json!({"mirror":ado::snapshot(self,0)?,"jobs":jobs,"supervision":supervision::snapshot(self)?,"automatic_assignment":workflow::Settings::read(self)?.is_some_and(|v|v.enabled),"publication_enabled":workflow::Settings::read(self)?.is_some_and(|v|v.enabled&&v.publication),"workflow":workflow::snapshot(self)?,"approvals":approvals::snapshot(self)?}),
        )
    }
}

pub fn assignment_eligible(packet: &Value, exception: &Value) -> bool {
    packet["eligible"] == true
        || exception.as_object().is_some_and(|v| {
            v.len() == 2
                && ["reason", "authority"].iter().all(|k| {
                    v.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.trim().is_empty() && s.len() <= 4096)
                })
        })
}
pub fn packet_on(c: &Connection, source: &str, item: i64) -> Result<Value> {
    let (raw, rev, eligible, observed): (String, i64, bool, i64) = c
        .query_row(
            "SELECT raw,revision,eligible,observed FROM ado_items WHERE source=?1 AND id=?2",
            params![source, item],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .context("Item is not mirrored")?;
    let mut entities = Vec::new();
    for raw in c
        .prepare("SELECT raw FROM ado_entities WHERE source=?1 AND item=?2 ORDER BY kind,entity")?
        .query_map(params![source, item], |r| r.get::<_, String>(0))?
    {
        entities.push(serde_json::from_str::<Value>(&raw?)?);
    }
    let mut revisions = Vec::new();
    for raw in c
        .prepare("SELECT raw FROM ado_revisions WHERE source=?1 AND item=?2 ORDER BY revision")?
        .query_map(params![source, item], |r| r.get::<_, String>(0))?
    {
        revisions.push(serde_json::from_str::<Value>(&raw?)?);
    }
    let mut legacy = json!({});
    for row in c
        .prepare("SELECT kind,raw FROM legacy_records WHERE source=?1 AND item=?2 ORDER BY kind")?
        .query_map(params![source, item], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
    {
        let (k, v) = row?;
        legacy[k] = serde_json::from_str::<Value>(&v)?;
    }
    let evidence = json!({"source":source,"item":serde_json::from_str::<Value>(&raw)?,"revisions":revisions,"entities":entities,"legacy":legacy});
    Ok(
        json!({"source_digest":hash(&evidence.to_string()),"revision":rev,"eligible":eligible,"observed":observed,"evidence":evidence,"legacy":legacy,"attachment_scope":"Attachment URLs/metadata retained; referenced bytes are not inspected"}),
    )
}
pub fn source_changed(c: &Connection, source: &str, item: i64) -> Result<()> {
    let p = packet_on(c, source, item)?;
    let changed=c.execute("UPDATE work SET state='needs_refresh',error='Mirrored source changed; current operator assignment required' WHERE source=?1 AND item=?2 AND state NOT IN ('finished','paused') AND EXISTS(SELECT 1 FROM assignments a WHERE a.job=work.id AND a.version=work.version AND a.digest!=?3)",params![source,item,p["source_digest"].as_str()])?;
    if changed > 0 {
        Store::log(
            c,
            "",
            "source.invalidated_assignment",
            json!({"source":source,"item":item}),
        )?;
    }
    Ok(())
}
pub(crate) fn resource_claims(a: &Value, planned: bool) -> Result<Vec<String>> {
    let worktree = Path::new(a["worktree"].as_str().unwrap());
    let canonical = if worktree.exists() {
        ensure!(worktree.is_dir(), "Assigned worktree is not a directory");
        fs::canonicalize(worktree)?
    } else {
        ensure!(planned, "Assigned worktree does not exist");
        ensure!(
            !worktree.components().any(|c| matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )),
            "Planned worktree has ambiguous path components"
        );
        let mut parent = worktree.to_path_buf();
        let mut missing = Vec::new();
        while !parent.exists() {
            missing.push(
                parent
                    .file_name()
                    .context("Planned worktree has no existing ancestor")?
                    .to_os_string(),
            );
            ensure!(parent.pop(), "Planned worktree has no existing ancestor");
        }
        ensure!(
            parent.is_dir(),
            "Planned worktree ancestor is not a directory"
        );
        let mut canonical = fs::canonicalize(parent)?;
        for component in missing.into_iter().rev() {
            canonical.push(component);
        }
        canonical
    };
    let mut path = canonical.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        path = path.to_lowercase();
    }
    let mut claims = vec![format!("worktree:{path}")];
    if a["runtime"]["required"] == true {
        let r = &a["runtime"];
        for field in [
            "reservation_id",
            "allocation_source",
            "database_server",
            "database_name",
            "browser_context",
        ] {
            ensure!(
                r[field]
                    .as_str()
                    .is_some_and(|v| !v.trim().is_empty() && !v.contains("REPLACE_WITH_")),
                "Runtime {field} needs an explicit operator allocation"
            );
        }
        for field in ["frontend_port", "backend_port"] {
            let p = r[field]
                .as_i64()
                .filter(|p| (1024..=65535).contains(p))
                .context("Allocated port required")?;
            claims.push(format!("port:{p}"));
        }
        ensure!(
            r["frontend_port"] != r["backend_port"],
            "Distinct frontend/backend ports required"
        );
        claims.push(format!(
            "database:{}:{}",
            r["database_server"].as_str().unwrap().to_lowercase(),
            r["database_name"].as_str().unwrap().to_lowercase()
        ));
        claims.push(format!(
            "browser:{}",
            r["browser_context"].as_str().unwrap().to_lowercase()
        ));
    }
    claims.sort();
    claims.dedup();
    ensure!(
        claims.iter().all(|v| v.len() < 1024),
        "Resource name exceeds bound"
    );
    Ok(claims)
}
