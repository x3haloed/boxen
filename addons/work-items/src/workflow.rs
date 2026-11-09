//! Durable project workflow. Existing assignments/reservations own worker identity and capacity.
//! This layer owns typed handoffs and publication, never parses private assistant narration.
use crate::{transport::Boxen, *};
use std::path::PathBuf;

pub(crate) fn existing_question_comment<'a>(
    question: &Value,
    entities: &'a Value,
) -> Result<Option<(i64, &'a Value)>> {
    let reference = &question["existing_comment_id"];
    if reference.is_null() {
        return Ok(None);
    }
    let existing = reference
        .as_i64()
        .or_else(|| {
            reference
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 19 && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<i64>().ok())
        })
        .filter(|id| *id > 0)
        .context("Existing question comment ID is invalid")?;
    let original = entities
        .as_array()
        .context("Source comments unavailable")?
        .iter()
        .find(|v| v["id"] == existing && v["isDeleted"] != true)
        .context("Existing question comment is absent from frozen source")?;
    ensure!(
        original["version"].as_i64().is_some_and(|v| v > 0),
        "Existing question comment version is missing"
    );
    Ok(Some((existing, original)))
}

pub(crate) fn retained_prior_delivery(root: &std::path::Path, raw: &str) -> Result<Value> {
    let previous: Value = serde_json::from_str(raw)?;
    let digest = hash(raw);
    let path = root.join(format!("prior-delivery-{digest}.json"));
    if path.exists() {
        ensure!(
            fs::read_to_string(&path)? == raw,
            "Prior delivery evidence differs"
        );
    } else {
        fs::write(&path, raw)?;
    }
    Ok(json!({
        "job_id":previous["job_id"],"coder_id":previous["coder_id"],
        "assignment_version":previous["assignment_version"],"pr_id":previous["pr_id"],
        "candidate":{"commit":previous["candidate"]["commit"],"base":previous["candidate"]["base"]},
        "evidence_path":path,"evidence_sha256":digest,
        "meaning":"Read the retained full prior delivery for its report, reviews, publication receipt and limitations; this envelope contains identity references only."
    }))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub allow_new_copies: bool,
    pub publication: bool,
    #[serde(default)]
    pub post_clarifications: bool,
    pub authority: String,
    pub context_resident: String,
    pub review_definition: String,
    pub review_revision: u64,
    pub notify_to: Value,
    pub driver: PathBuf,
    pub driver_args: Vec<String>,
    pub driver_script: PathBuf,
    pub driver_hash: String,
    pub project: Value,
    #[serde(default)]
    pub items: Vec<i64>,
}
impl Settings {
    pub fn read(s: &Store) -> Result<Option<Self>> {
        let p = s.path.parent().unwrap().join("workflow.json");
        if !p.exists() {
            return Ok(None);
        }
        let v: Self = serde_json::from_slice(&fs::read(p)?)?;
        v.validate()?;
        Ok(Some(v))
    }
    pub fn validate(&self) -> Result<()> {
        key(&self.context_resident)?;
        key(&self.review_definition)?;
        key(&self.driver_hash)?;
        ensure!(
            self.review_revision > 0 && !self.authority.trim().is_empty(),
            "Workflow authority and pinned reviewer revision required"
        );
        ensure!(
            self.driver.is_absolute() && self.driver_script.is_absolute(),
            "Workflow driver uses absolute operator-owned executable/script paths"
        );
        ensure!(
            self.items.iter().all(|n| *n > 0),
            "Workflow item scope uses positive IDs"
        );
        ensure!(
            self.notify_to["type"] == "client" || self.notify_to["type"] == "resident",
            "Workflow observer is a typed mailbox address"
        );
        key(self.notify_to["id"]
            .as_str()
            .context("Observer ID required")?)?;
        if self.notify_to["type"] == "client" {
            key(self.notify_to["machine"]
                .as_str()
                .context("Observer machine required")?)?;
        }
        Ok(())
    }
}

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS workflow_attempts(id TEXT PRIMARY KEY,source TEXT,item INTEGER,digest TEXT,stage TEXT,data TEXT NOT NULL,error TEXT,next_due INTEGER NOT NULL DEFAULT 0,created INTEGER,checked INTEGER NOT NULL DEFAULT 0,UNIQUE(source,item,digest));
    CREATE TABLE IF NOT EXISTS workflow_sources(source TEXT,item INTEGER,signature TEXT,digest TEXT,PRIMARY KEY(source,item));
    CREATE INDEX IF NOT EXISTS workflow_source_events ON ado_events(source,item,seq DESC);
    CREATE TABLE IF NOT EXISTS workflow_mail(id TEXT PRIMARY KEY,attempt TEXT,role TEXT,recipient TEXT,request TEXT,accepted INTEGER NOT NULL DEFAULT 0,response TEXT,next_due INTEGER NOT NULL DEFAULT 0,error TEXT);
    CREATE TABLE IF NOT EXISTS workflow_receipts(id TEXT PRIMARY KEY,accepted INTEGER,reason TEXT);
    CREATE TABLE IF NOT EXISTS workflow_resumes(k TEXT PRIMARY KEY,input TEXT NOT NULL,receipt TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS workflow_questions(id TEXT PRIMARY KEY,item INTEGER,request TEXT,state TEXT,receipt TEXT,attempt TEXT,next_due INTEGER NOT NULL DEFAULT 0,error TEXT);")?;
    Ok(())
}

#[derive(Clone, Debug)]
struct Attempt {
    id: String,
    item: i64,
    digest: String,
    stage: String,
    data: Value,
}
fn load(s: &Store, id: &str) -> Result<Attempt> {
    Ok(s.connect()?
        .query_row(
            "SELECT item,digest,stage,data FROM workflow_attempts WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .map(|(item, digest, stage, data)| Attempt {
            id: id.into(),
            item,
            digest,
            stage,
            data: serde_json::from_str(&data).unwrap_or(Value::Null),
        })?)
}
fn save(s: &Store, a: &Attempt, stage: &str) -> Result<()> {
    let mut c = s.connect()?;
    let tx = c.transaction()?;
    tx.execute(
        "UPDATE workflow_attempts SET stage=?2,data=?3,error=NULL,next_due=0 WHERE id=?1",
        params![a.id, stage, a.data.to_string()],
    )?;
    if matches!(stage, "published" | "held" | "no_action" | "superseded") {
        tx.execute(
            "UPDATE operator_feedback SET state=?2 WHERE id=?1 AND state='return_accepted'",
            params![
                a.data["operator_feedback"]["id"].as_str().unwrap_or(""),
                format!("workflow_{stage}")
            ],
        )?;
    }
    Store::log(
        &tx,
        a.data["job_id"].as_str().unwrap_or(&a.id),
        "workflow.transition",
        json!({"attempt":a.id,"item":a.item,"from":a.stage,"to":stage}),
    )?;
    tx.commit()?;
    event(
        "workflow.transition",
        json!({"attempt":a.id,"item":a.item,"stage":stage}),
    );
    Ok(())
}
fn enqueue(
    s: &Store,
    cfg: &Config,
    a: &Attempt,
    role: &str,
    to: Value,
    body: Value,
) -> Result<String> {
    let mut identity = format!(
        "workflow:{}:{role}:{}:{}",
        a.id,
        body["reviewed_commit"].as_str().unwrap_or(""),
        body["review_round"]
    );
    if let Some(id) = a.data["operator_feedback"]["id"].as_str() {
        identity.push_str(&format!(":operator:{id}"));
    }
    if role.starts_with("notice:") {
        // Different version/outcome notices coexist; exact notice retries reuse their ID.
        identity.push_str(&format!(":{}", hash(&body.to_string())));
    }
    let mid = hash(&identity);
    let text = if body.is_object() {
        let mut request = body;
        request["request_id"] = json!(mid);
        request["attempt_id"] = json!(a.id);
        request["work_item_id"] = json!(a.item);
        request["source_digest"] = json!(a.digest);
        request.to_string()
    } else {
        body.as_str()
            .context("Workflow body is an object or text")?
            .to_string()
    };
    let message = json!({"op":"mailbox_send","client_id":cfg.client_id,"message":{"id":mid,"to":to,"subject":if role.starts_with("notice:"){format!("Work item {}: workflow update",a.item)}else{format!("Work item {}: {role}",a.item)},"body":text}});
    ensure!(
        message["message"]["body"].as_str().unwrap().len() <= 32768,
        "Workflow message exceeds mailbox body bound; full evidence remains in the retained source/assignment files"
    );
    let c = s.connect()?;
    let previous: Option<String> = c
        .query_row(
            "SELECT request FROM workflow_mail WHERE id=?1",
            [&mid],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        previous
            .as_ref()
            .is_none_or(|old| old == &message.to_string()),
        "Workflow send ID has conflicting input"
    );
    c.execute("INSERT OR IGNORE INTO workflow_mail(id,attempt,role,recipient,request)VALUES(?1,?2,?3,?4,?5)",params![mid,a.id,role,to.to_string(),message.to_string()])?;
    Ok(mid)
}
fn notify(
    s: &Store,
    cfg: &Config,
    settings: &Settings,
    a: &Attempt,
    condition: &str,
    detail: Value,
) -> Result<()> {
    let technical = json!({"protocol":"boxen.loop-notice.v1","condition":condition,"detail":detail,"meaning":"Workflow state or attributed evidence, not a human-authored request. No merge/deploy authority is implied."});
    let summary = if condition == "published" {
        format!(
            "PR delivered: {}\nReviewed commit: {}. The PR is non-draft; merge remains a separate decision unless already completed externally.",
            detail["url"].as_str().unwrap_or("PR receipt retained"),
            detail["commit"].as_str().unwrap_or("")
        )
    } else if condition == "blocked" || condition == "paused" {
        detail["reason"]
            .as_str()
            .unwrap_or("This work needs a decision before it can continue.")
            .to_string()
    } else if condition == "mirror_stale" {
        "Waiting for fresh Azure DevOps observations before continuing.".into()
    } else {
        "An operation needs attention; its request and outcome are retained. Safe retries continue automatically.".into()
    };
    enqueue(
        s,
        cfg,
        a,
        &format!("notice:{condition}"),
        settings.notify_to.clone(),
        json!(summary),
    )?;
    let support = cfg.supervision.as_ref().filter(|v| {
        v.notify_to != settings.notify_to
            && (condition.starts_with("error:")
                || condition.starts_with("delivery_error:")
                || condition.starts_with("question_error:"))
    });
    if let Some(support) = support {
        enqueue(
            s,
            cfg,
            a,
            &format!("notice:support:{condition}"),
            support.notify_to.clone(),
            technical,
        )?;
    }

    Ok(())
}
fn flush(s: &Store, cfg: &Config, settings: &Settings, api: &impl Boxen) -> Result<()> {
    let c = s.connect()?;
    let rows=c.prepare("SELECT id,request,attempt,role,error FROM workflow_mail WHERE accepted=0 AND next_due<=?1 ORDER BY rowid LIMIT 8")?.query_map([now()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, raw, attempt, role, old) in rows {
        let result = (|| -> Result<()> {
            let response = api.call(serde_json::from_str(&raw)?)?;
            ensure!(
                response["id"] == id
                    && response["sender"]
                        == json!({"type":"client","machine":cfg.owner,"id":cfg.client_id}),
                "Workflow mail acceptance identity mismatch"
            );
            c.execute(
                "UPDATE workflow_mail SET accepted=1,error=NULL WHERE id=?1",
                [&id],
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            let detail = error.to_string();
            if old.as_deref() != Some(&detail) {
                Store::log(
                    &c,
                    &attempt,
                    "workflow.delivery_error",
                    json!({"message_id":id,"role":role,"detail":detail}),
                )?;
                if !role.starts_with("notice:") {
                    notify(
                        s,
                        cfg,
                        settings,
                        &load(s, &attempt)?,
                        &format!("delivery_error:{}:{}", &id[..12], &hash(&detail)[..12]),
                        json!({"request_id":id,"role":role,"detail":detail}),
                    )?;
                }
            }
            c.execute(
                "UPDATE workflow_mail SET next_due=?2,error=?3 WHERE id=?1",
                params![id, now() + 15_000, detail],
            )?;
        }
    }
    Ok(())
}

fn intake(s: &Store, cfg: &Config, api: &impl Boxen) -> Result<()> {
    for _ in 0..4 {
        let mut c = s.connect()?;
        let after: i64 = c
            .query_row(
                "SELECT COALESCE((SELECT v FROM meta WHERE k='workflow:cursor'),'0')",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()?;
        let page =
            api.call(json!({"op":"client_inbox","client_id":cfg.client_id,"after":after}))?;
        let messages = page["messages"]
            .as_array()
            .context("Workflow inbox unavailable")?;
        if messages.is_empty() {
            break;
        }
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for message in messages {
            let Some(raw) = message["body"].as_str() else {
                continue;
            };
            let Ok(body) = serde_json::from_str::<Value>(raw) else {
                continue;
            };
            if !matches!(
                body["protocol"].as_str(),
                Some("boxen.interpretation.v1" | "boxen.review.v1")
            ) {
                continue;
            }
            let id = message["id"]
                .as_str()
                .context("Workflow response ID missing")?;
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_receipts WHERE id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )? {
                continue;
            }
            let request = body["request_id"].as_str().unwrap_or("");
            let stored: Option<(String, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT attempt,recipient,request,response FROM workflow_mail WHERE id=?1",
                    [request],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let mut accepted = false;
            if let Some((attempt, to, original, previous)) = stored {
                let original: Value = serde_json::from_str(&original)?;
                let expected: Value =
                    serde_json::from_str(original["message"]["body"].as_str().unwrap())?;
                let stage: String = tx.query_row(
                    "SELECT stage FROM workflow_attempts WHERE id=?1",
                    [&attempt],
                    |r| r.get(0),
                )?;
                let state_data: String = tx.query_row(
                    "SELECT data FROM workflow_attempts WHERE id=?1",
                    [&attempt],
                    |r| r.get(0),
                )?;
                let state_data: Value = serde_json::from_str(&state_data)?;
                let current_response = if body["protocol"] == "boxen.review.v1" {
                    stage == "reviewing"
                        && state_data["candidate"]["commit"] == expected["reviewed_commit"]
                        && state_data[format!("{}_request", body["role"].as_str().unwrap_or(""))]
                            == request
                } else {
                    stage == "interpreting" && state_data["interpretation_request"] == request
                };
                accepted = current_response
                    && message["sender"] == serde_json::from_str::<Value>(&to)?
                    && message["to"]
                        == json!({"type":"client","machine":cfg.owner,"id":cfg.client_id})
                    && message["in_reply_to"] == request
                    && body["attempt_id"] == expected["attempt_id"]
                    && body["source_digest"] == expected["source_digest"]
                    && body["work_item_id"] == expected["work_item_id"]
                    && !matches!(stage.as_str(), "superseded" | "published")
                    && (body["protocol"] != "boxen.review.v1"
                        || (body["reviewed_commit"] == expected["reviewed_commit"]
                            && body["base_commit"] == expected["base_commit"]
                            && body["role"] == expected["role"]))
                    && previous.as_ref().is_none_or(|old| old == raw);
                if accepted {
                    tx.execute(
                        "UPDATE workflow_mail SET response=?2,accepted=1,error=NULL WHERE id=?1 AND response IS NULL",
                        params![request, raw],
                    )?;
                }
            }
            tx.execute(
                "INSERT INTO workflow_receipts VALUES(?1,?2,?3)",
                params![
                    id,
                    accepted,
                    if accepted {
                        "Authenticated versioned response"
                    } else {
                        "Unknown, conflicting, stale or unauthenticated response"
                    }
                ],
            )?;
        }
        let next = page["next_after"]
            .as_i64()
            .context("Workflow inbox cursor missing")?;
        ensure!(next > after, "Workflow inbox cursor did not advance");
        tx.execute(
            "INSERT INTO meta VALUES('workflow:cursor',?1)ON CONFLICT(k)DO UPDATE SET v=excluded.v",
            [next.to_string()],
        )?;
        tx.commit()?;
    }
    Ok(())
}
fn response(s: &Store, id: &str) -> Result<Option<Value>> {
    let raw: Option<String> = s
        .connect()?
        .query_row(
            "SELECT response FROM workflow_mail WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    raw.map(|v| serde_json::from_str(&v).map_err(Into::into))
        .transpose()
}

pub trait Driver {
    fn call(&mut self, operation: &str, request: &Value) -> Result<Value>;
}
pub struct LocalDriver<'a> {
    pub settings: &'a Settings,
    pub store: &'a Store,
}
pub(crate) fn driver_reply(
    operation: &str,
    path: &std::path::Path,
    success: bool,
) -> Result<Value> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(|| format!("Project operation {operation} has no retained response at {} (process success: {success})", path.display()))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Project driver response exceeds limit"
    );
    let value: Value = serde_json::from_slice(&bytes)?;
    if !success || value["ok"] != true {
        // Use the driver's structured diagnostic, never arbitrary command stderr.
        let detail = value["error"]
            .as_str()
            .unwrap_or("Driver did not verify success");
        let bounded: String = detail.chars().take(2048).collect();
        anyhow::bail!(
            "Project operation {operation} failed: {bounded}; retained response: {}",
            path.display()
        );
    }
    Ok(value)
}
impl Driver for LocalDriver<'_> {
    fn call(&mut self, operation: &str, request: &Value) -> Result<Value> {
        use std::{
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };
        ensure!(
            hash(&fs::read_to_string(&self.settings.driver_script)?) == self.settings.driver_hash,
            "Project driver script changed; configured hash no longer matches"
        );
        let root = self.store.path.parent().unwrap().join("workflow-driver");
        fs::create_dir_all(&root)?;
        let path = root.join(format!("{}.json", id()));
        let result = path.with_extension("result.json");
        fs::write(
            &path,
            json!({"operation":operation,"project":self.settings.project,"request":request})
                .to_string(),
        )?;
        let mut command = Command::new(&self.settings.driver);
        command
            .args(&self.settings.driver_args)
            .arg(&self.settings.driver_script)
            .arg("-RequestPath")
            .arg(&path)
            .arg("-ReplyPath")
            .arg(&result)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = process::ManagedChild::spawn(&mut command)?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let status = loop {
            if let Some(status) = child.child.try_wait()? {
                break status;
            }
            ensure!(
                Instant::now() < deadline,
                "Project operation {operation} exceeded its deadline; external effects may be uncertain"
            );
            thread::sleep(Duration::from_millis(50));
        };
        drop(child);
        driver_reply(operation, &result, status.success())
    }
}

pub fn dispatch_ready(s: &Store, job: &str, version: i64) -> Result<bool> {
    let input: String = s.connect()?.query_row(
        "SELECT input FROM assignments WHERE job=?1 AND version=?2",
        params![job, version],
        |r| r.get(0),
    )?;
    let value: Value = serde_json::from_str(&input)?;
    let Some(attempt) = value["context"]["workflow_id"].as_str() else {
        return Ok(true);
    };
    Ok(load(s, attempt)?.stage == "coding")
}
fn source_observation(s: &Store, item: i64) -> Result<(String, i64, bool)> {
    let source = ado::config(s)?.source();
    let c = s.connect()?;
    let (revision,observed,eligible,event):(i64,i64,bool,i64)=c.query_row("SELECT revision,observed,eligible,COALESCE((SELECT MAX(seq) FROM ado_events e WHERE e.source=i.source AND e.item=i.id),0) FROM ado_items i WHERE source=?1 AND id=?2",params![source,item],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let signature = format!("{revision}:{eligible}:{event}");
    let cached: Option<(String, String)> = c
        .query_row(
            "SELECT signature,digest FROM workflow_sources WHERE source=?1 AND item=?2",
            params![source, item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((old, digest)) = cached
        && old == signature
    {
        return Ok((digest, observed, eligible));
    }
    let packet = s.packet(item)?;
    let digest = packet["source_digest"]
        .as_str()
        .context("Source digest missing")?
        .to_string();
    c.execute("INSERT INTO workflow_sources VALUES(?1,?2,?3,?4) ON CONFLICT(source,item) DO UPDATE SET signature=excluded.signature,digest=excluded.digest",params![source,item,signature,digest])?;
    Ok((digest, observed, eligible))
}
fn fresh(s: &Store, a: &Attempt) -> Result<bool> {
    let (digest, observed, eligible) = source_observation(s, a.item)?;
    Ok(eligible && digest == a.digest && now() - observed < 30 * 60_000)
}

/// Explicit operator continuation of held workflow work; correspondence cannot invoke it.
pub fn resume(s: &Store, api: &impl Boxen, input: &Value) -> Result<Value> {
    let k = input["idempotency_key"]
        .as_str()
        .context("Resume idempotency key required")?;
    ensure!(!k.trim().is_empty() && k.len() <= 256, "Invalid resume key");
    let mut c = s.connect()?;
    let prior: Option<(String, String)> = c
        .query_row(
            "SELECT input,receipt FROM workflow_resumes WHERE k=?1",
            [k],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((original, receipt)) = prior {
        ensure!(
            serde_json::from_str::<Value>(&original)? == *input,
            "Resume key has different inputs"
        );
        return Ok(serde_json::from_str(&receipt)?);
    }
    let settings = Settings::read(s)?.context("Workflow configuration required")?;
    ensure!(settings.enabled, "Workflow is disabled");
    let id = input["attempt_id"]
        .as_str()
        .context("Attempt ID required")?;
    let mut a = load(s, id)?;
    ensure!(
        a.stage == "held" && fresh(s, &a)?,
        "Resume requires held work with fresh unchanged source"
    );
    ensure!(
        settings.items.is_empty() || settings.items.contains(&a.item),
        "Attempt is outside enabled item scope"
    );
    ensure!(
        input["reason"]
            .as_str()
            .is_some_and(|v| !v.trim().is_empty() && v.len() <= 8192),
        "Resume reason required"
    );
    let job = a.data["job_id"]
        .as_str()
        .context("Held workflow has no assignment")?
        .to_string();
    let (coder, version, state, slot): (String, i64, String, i64) = c.query_row(
        "SELECT coder,version,state,holds_slot FROM work WHERE id=?1",
        [&job],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        state == "paused"
            && slot == 0
            && input["expected_coder"] == coder
            && input["expected_version"] == version
            && a.data["assignment_version"] == version,
        "Resume owner/version or released pause state changed"
    );
    ensure!(
        assignment(s, &a)?["context"]["workflow_id"] == a.id,
        "Only workflow-owned assignments can resume here"
    );
    ensure!(idle(api, &coder)?, "Resume waits for idle coder execution");
    let allocation_key = format!("workflow:{}:resume:{}", a.id, hash(k));
    let report: Option<String> = c.query_row(
        "SELECT report FROM supervision WHERE job=?1 AND version=?2",
        params![job, version],
        |r| r.get(0),
    )?;
    a.data["operator_resume"] = json!({"request":input,"prior_report":report.map(|v|serde_json::from_str::<Value>(&v)).transpose()?});
    a.data["allocation_request_key"] = json!(allocation_key);
    let receipt = json!({"attempt_id":a.id,"item":a.item,"coder_id":coder,"expected_version":version,"state":"ready","new_copy":false});
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute("UPDATE workflow_attempts SET stage='ready',data=?2,error=NULL,next_due=0 WHERE id=?1 AND stage='held' AND EXISTS(SELECT 1 FROM work WHERE id=?3 AND version=?4 AND coder=?5 AND state='paused' AND holds_slot=0)",params![a.id,a.data.to_string(),job,version,coder])?;
    ensure!(changed == 1, "Resume state changed concurrently");
    tx.execute(
        "INSERT INTO workflow_resumes VALUES(?1,?2,?3)",
        params![k, input.to_string(), receipt.to_string()],
    )?;
    Store::log(
        &tx,
        &job,
        "workflow.operator_resumed",
        json!({"attempt":a.id,"item":a.item,"coder":coder,"version":version,"request":input}),
    )?;
    tx.commit()?;
    event("workflow.operator_resumed", receipt.clone());
    Ok(receipt)
}

fn allocation_key(a: &Attempt) -> String {
    a.data["allocation_request_key"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("workflow:{}:assignment", a.id))
}

fn discover(s: &Store, settings: &Settings) -> Result<()> {
    let source = ado::config(s)?.source();
    let c = s.connect()?;
    let items = c
        .prepare(
            "SELECT id FROM ado_items WHERE source=?1 AND eligible=1 AND observed>?2 ORDER BY id",
        )?
        .query_map(params![source, now() - 30 * 60_000], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for item in items {
        if !settings.items.is_empty() && !settings.items.contains(&item) {
            continue;
        }
        let (digest, _, _) = source_observation(s, item)?;
        let id = hash(&format!("workflow:{source}:{item}:{digest}"));
        if c.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_attempts WHERE id=?1)",
            [&id],
            |r| r.get::<_, bool>(0),
        )? {
            continue;
        }
        let busy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM workflow_attempts WHERE source=?1 AND item=?2 AND stage NOT IN('published','held','no_action','superseded'))",params![source,item],|r|r.get(0))?;
        if busy {
            continue;
        }
        let p = s.packet(item)?;
        let data = json!({"packet":p,"authority":settings.authority});
        c.execute("INSERT OR IGNORE INTO workflow_attempts(id,source,item,digest,stage,data,created)VALUES(?1,?2,?3,?4,'detected',?5,?6)",params![id,source,item,digest,data.to_string(),now()])?;
    }
    Ok(())
}

pub fn snapshot(s: &Store) -> Result<Value> {
    let c = s.connect()?;
    let rows=c.prepare("SELECT id,item,digest,stage,error FROM workflow_attempts ORDER BY created DESC LIMIT 200")?.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"item":r.get::<_,i64>(1)?,"digest":r.get::<_,String>(2)?,"stage":r.get::<_,String>(3)?,"error":r.get::<_,Option<String>>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({"attempts":rows}))
}
fn idle(api: &impl Boxen, resident: &str) -> Result<bool> {
    let activity = api.call(json!({"op":"thread_activity","id":resident}))?;
    ensure!(
        activity["thread_id"] == resident,
        "Thread activity identity mismatch"
    );
    Ok(activity["idle"] == true)
}

fn held(s: &Store, cfg: &Config, settings: &Settings, a: &Attempt, reason: &str) -> Result<()> {
    notify(
        s,
        cfg,
        settings,
        a,
        "blocked",
        json!({"reason":reason,"stage":a.stage,"job_id":a.data["job_id"]}),
    )?;
    if a.data["job_id"].is_string() {
        let mut next = a.clone();
        next.data["pause_reason"] = json!(reason);
        save(s, &next, "pausing")
    } else {
        let mut next = a.clone();
        next.data["pause_reason"] = json!(reason);
        save(s, &next, "held")
    }
}
const PR_DESCRIPTION_GUIDANCE: &str = "The PR description tells reviewers what went wrong and how the change fixes it. A typical bug fix reads: ‘There was a bug in `component-name` that [observable failure]. I fixed it by [new behavior].’ Most changes fit in one or two sentences. Exceptionally complex changes can take up to two short paragraphs. Test results, review details, screenshots, work-item links, and deployment status belong in their separate records.";

fn pr_description(report: &Value) -> Result<()> {
    let description = report["pr_description"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .context("Candidate PR description is missing")?;
    let paragraphs = description
        .trim()
        .lines()
        .map(|line| !line.trim().is_empty())
        .scan(false, |previous, nonempty| {
            let start = nonempty && !*previous;
            *previous = nonempty;
            Some(start)
        })
        .filter(|start| *start)
        .count();
    ensure!(
        paragraphs <= 2,
        "Candidate PR description exceeds two paragraphs"
    );
    Ok(())
}

fn candidate(report: &Value) -> Result<()> {
    for field in ["commit", "base"] {
        let sha = report[field]
            .as_str()
            .context(format!("Candidate {field} missing"))?;
        ensure!(
            sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "Full candidate commit/base required"
        );
    }
    ensure!(
        report["validation"]["local_runtime_exercised"] == true,
        "Candidate lacks explicit real local runtime evidence"
    );
    ensure!(
        report["validation"]["acceptance"]
            .as_array()
            .is_some_and(|v| !v.is_empty()
                && v.iter()
                    .all(|s| s["status"] == "observed_pass" && !s["evidence"].is_null())),
        "Candidate lacks observed acceptance evidence"
    );
    ensure!(
        report["remaining"].as_array().is_some_and(|v| v.is_empty()),
        "Candidate reports remaining required work"
    );
    Ok(())
}
fn reviews_pass(context: &Value, technical: &Value) -> bool {
    [context, technical].iter().all(|r| {
        r["decision"] == "pass"
            && r["findings"]
                .as_array()
                .is_some_and(|v| v.iter().all(|f| f["blocking"] == false))
    })
}
fn assignment(s: &Store, a: &Attempt) -> Result<Value> {
    let raw: String = s.connect()?.query_row(
        "SELECT input FROM assignments WHERE job=?1 AND version=?2",
        params![
            a.data["job_id"].as_str().context("Job ID missing")?,
            a.data["assignment_version"]
                .as_i64()
                .context("Assignment version missing")?
        ],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&raw)?)
}
fn correction(
    s: &Store,
    cfg: &Config,
    api: &impl Boxen,
    a: &mut Attempt,
    findings: Value,
) -> Result<()> {
    let fingerprint=hash(&json!({"context":findings["context"]["findings"],"technical":findings["technical"]["findings"],"condition":findings["condition"],"detail":findings["detail"],"target":findings["target_commit"],"commit":a.data["candidate"]["commit"]}).to_string());
    if a.data["last_returned_findings"] == fingerprint {
        a.data["pause_reason"] = json!(
            "The same candidate returned with the same unresolved findings; human/support decision needed"
        );
        return save(s, a, "pausing");
    }
    a.data["last_returned_findings"] = json!(fingerprint);
    let mut input = assignment(s, a)?;
    let coder = a.data["coder_id"].as_str().context("Coder missing")?;
    ensure!(
        idle(api, coder)?,
        "Correction waits for idle coder execution"
    );
    let version = a.data["assignment_version"]
        .as_i64()
        .context("Version missing")?;
    input["idempotency_key"] = json!(format!("workflow:{}:correction:{version}", a.id));
    input["new_copy"] = json!(false);
    input["coder_id"] = json!(coder);
    input["continuation"] = json!({"expected_version":version,"expected_coder":coder,"reason":"Authenticated exact-commit review findings return to the same coder"});
    input["context"]["review_findings"] = findings;
    input["assignment"]["task"]["review_findings"] = input["context"]["review_findings"].clone();
    input["assignment"]["task"]["candidate_contract"]["pr_description"] =
        json!(PR_DESCRIPTION_GUIDANCE);
    input["assignment"]["assignment_revision"] =
        json!(format!("workflow-correction-{}", version + 1));
    let result = transport::continue_assignment(s, cfg, api, &input)?;
    a.data["assignment_version"] = result["version"].clone();
    a.data["candidate"] = Value::Null;
    save(s, a, "coding")
}
/// Original operator feedback returns through interpretation and the existing
/// same-coder allocation path. The previous candidate and reviews stay retained.
pub(crate) fn operator_return(
    s: &Store,
    cfg: &Config,
    api: &impl Boxen,
    settings: &Settings,
    id: &str,
    input: &Value,
    basis: &str,
) -> Result<()> {
    let prior: Value = serde_json::from_str(basis)?;
    let mut a = load(s, prior["id"].as_str().context("Feedback attempt missing")?)?;
    if a.data["operator_feedback"]["id"] == id {
        return Ok(());
    }
    ensure!(
        settings.enabled,
        "Workflow is disabled; feedback remains saved"
    );
    if a.stage != prior["stage"] || a.data != prior["data"] || a.digest != prior["source_digest"] {
        return Err(crate::operator::ReconciliationRequired("Work changed after feedback acceptance; original feedback remains retained for reconciliation").into());
    }
    if let Some(coder) = a.data["coder_id"].as_str() {
        ensure!(
            idle(api, coder)?,
            "Return waits for the existing coder to become idle"
        );
        let (version, state, held): (i64, String, bool) = s.connect()?.query_row(
            "SELECT version,state,holds_slot FROM work WHERE id=?1",
            [a.data["job_id"].as_str().context("Job missing")?],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        ensure!(
            a.data["assignment_version"] == version
                && matches!(state.as_str(), "finished" | "paused")
                && !held,
            "Return waits for the expected assignment's resource release"
        );
    }
    let root = s
        .path
        .parent()
        .unwrap()
        .join("workflow-evidence")
        .join(&a.id);
    fs::create_dir_all(&root)?;
    let previous_data = if prior["delivery"].is_object() {
        &prior["delivery"]
    } else {
        &a.data
    };
    let previous = retained_prior_delivery(&root, &previous_data.to_string())?;
    let packet = s.packet(a.item)?;
    if packet["source_digest"] != a.digest {
        return Err(crate::operator::ReconciliationRequired(
            "Source changed; original feedback remains retained for reconciliation",
        )
        .into());
    }
    a.data = json!({"packet":packet,"authority":settings.authority,
        "allocation_request_key":format!("operator:{id}:allocation"),
        "operator_prior_delivery":previous,
        "operator_feedback":{"id":id,"original_text":input["text"],"author":{"type":"local_operator","machine":cfg.owner},"subject_commit":previous_data["candidate"]["commit"],"meaning":"Original operator feedback concerns the retained candidate. Fresh interpretation determines the correction scope while existing holds, owner roles and publication limits remain in force."}});
    save(s, &a, "detected")
}

fn advance(
    s: &Store,
    cfg: &Config,
    settings: &Settings,
    api: &impl Boxen,
    driver: &mut impl Driver,
    a: &mut Attempt,
) -> Result<()> {
    if a.stage == "ready" {
        let key = allocation_key(a);
        let saved: Option<(String, i64)> = s
            .connect()?
            .query_row(
                "SELECT job,version FROM assignment_requests WHERE k=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((job, version)) = saved {
            let (coder, current): (String, i64) = s.connect()?.query_row(
                "SELECT coder,version FROM work WHERE id=?1",
                [&job],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(
                current == version,
                "Accepted workflow assignment was superseded externally"
            );
            a.data["job_id"] = json!(job);
            a.data["coder_id"] = json!(coder);
            a.data["assignment_version"] = json!(version);
            return save(s, a, "awaiting_allocation");
        }
    }
    if matches!(
        a.stage.as_str(),
        "detected"
            | "interpreting"
            | "ready"
            | "awaiting_allocation"
            | "coding"
            | "request_reviews"
            | "reviewing"
            | "publish_ready"
            | "push_pending"
            | "pr_lookup"
    ) {
        let (digest, observed, eligible) = source_observation(s, a.item)?;
        if digest == a.digest && eligible && now() - observed >= 30 * 60_000 {
            notify(
                s,
                cfg,
                settings,
                a,
                "mirror_stale",
                json!({"meaning":"A fresh source observation precedes the next workflow transition; no task effects are replayed"}),
            )?;
            return Ok(());
        }
    }
    if matches!(
        a.stage.as_str(),
        "detected"
            | "interpreting"
            | "ready"
            | "awaiting_allocation"
            | "request_reviews"
            | "reviewing"
            | "publish_ready"
            | "push_pending"
            | "pr_lookup"
    ) && !fresh(s, a)?
    {
        return if a.data["job_id"].is_string() {
            held(
                s,
                cfg,
                settings,
                a,
                "Source changed; a fresh interpretation follows owned-resource cleanup",
            )
        } else {
            save(s, a, "superseded")
        };
    }
    if matches!(a.stage.as_str(), "push_pending" | "pr_lookup") && !settings.publication {
        return held(
            s,
            cfg,
            settings,
            a,
            "Automatic publication was disabled before the next external write",
        );
    }
    match a.stage.as_str() {
        "detected" => {
            let mut root = s
                .path
                .parent()
                .unwrap()
                .join("workflow-evidence")
                .join(&a.id);
            if let Some(id) = a.data["operator_feedback"]["id"].as_str() {
                root = root.join(format!("operator-{id}"));
            }
            fs::create_dir_all(&root)?;
            let packet_path = root.join("source.json");
            let packet = &a.data["packet"];
            let bytes = packet.to_string();
            fs::write(&packet_path, &bytes)?;
            let current: Option<(String, String, i64, String)> = s
                .connect()?
                .query_row(
                    "SELECT id,coder,version,state FROM work WHERE source=?1 AND item=?2",
                    params![ado::config(s)?.source(), a.item],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let current=current.map(|(job,coder,version,state)|->Result<Value>{
                let raw:String=s.connect()?.query_row("SELECT input FROM assignments WHERE job=?1 AND version=?2",params![job,version],|r|r.get(0))?;
                let input:Value=serde_json::from_str(&raw)?;
                Ok(json!({"job_id":job,"coder_id":coder,"assignment_version":version,"state":state,"worktree":input["assignment"]["worktree"],"branch":input["assignment"]["branch"],"base_commit":input["assignment"]["base_commit"]}))
            }).transpose()?;
            let prior_delivery:Option<String>=s.connect()?.query_row("SELECT data FROM workflow_attempts WHERE source=?1 AND item=?2 AND stage='published' ORDER BY created DESC LIMIT 1",params![ado::config(s)?.source(),a.item],|r|r.get(0)).optional()?;
            let mut prior_delivery = prior_delivery
                .map(|v| retained_prior_delivery(&root, &v))
                .transpose()?;
            if a.data["operator_prior_delivery"].is_object() {
                prior_delivery = Some(a.data["operator_prior_delivery"].clone());
            }
            let body = json!({"protocol":"boxen.interpretation-request.v1","packet_path":packet_path,"packet_sha256":hash(&bytes),"work_item_revision":packet["revision"],"existing_work":current,"prior_delivery":prior_delivery,
                "operator_feedback":a.data["operator_feedback"],
                "contract":{"protocol":"boxen.interpretation.v1","attempt_id":a.id,"work_item_id":a.item,"source_digest":a.digest,"decision":"ready|clarification_required|no_action|superseded","intended_outcome":"Concrete user-visible behavior","scenarios":[{"id":"scenario-id","role":"role","starting_state":"state/data","action":"user action","expected_result":"observable result"}],"preservation_targets":[],"authoritative_answers":[],"answer_contract":{"kind":"ado_comment|ado_revision","comment_id":"original ID","version":"original version","revision":"original revision","field":"changed requirement field for revision answers","authority_role":"product_owner|engineering_owner"},"questions":[],"question_contract":{"id":"stable across source refreshes","question":"Product question","existing_comment_id":"optional original question already posted; prevents re-posting","route":"work_item|operator|support","authority_needed":"decision owner"},"holds":[],"existing_work":{"unresolved_ownership":false},"evidence_refs":["Inspected source/comment identity"]},
                "meaning":"An explicit JSON response to the authenticated reply_to with matching request_id, attempt_id, source_digest and in_reply_to records interpretation. Ready assesses intent; worker creation and publication are service decisions under configured human authority. Current holds, original comment authors and unknown ownership remain distinguishable. Private final narration is not a response."});
            let mid = enqueue(
                s,
                cfg,
                a,
                "interpretation",
                json!({"type":"resident","id":settings.context_resident}),
                body,
            )?;
            a.data["interpretation_request"] = json!(mid);
            a.data["decision_due"] = json!(now() + 2 * 60 * 60_000);
            save(s, a, "interpreting")
        }
        "interpreting" => {
            if now() > a.data["decision_due"].as_i64().unwrap_or(i64::MAX) {
                return held(
                    s,
                    cfg,
                    settings,
                    a,
                    "Interpretation response deadline elapsed; original request remains retained",
                );
            }

            let Some(answer) = response(s, a.data["interpretation_request"].as_str().unwrap())?
            else {
                return Ok(());
            };
            a.data["interpretation"] = answer.clone();
            if answer["decision"] == "clarification_required"
                && settings.post_clarifications
                && let Some(questions) = answer["questions"].as_array()
            {
                for question in questions.iter().filter(|q| q["route"] == "work_item") {
                    let qid = question["id"]
                        .as_str()
                        .filter(|v| !v.is_empty() && v.len() < 128)
                        .context("Stable question ID missing")?;
                    let text = question["question"]
                        .as_str()
                        .filter(|v| !v.trim().is_empty() && v.len() <= 4000)
                        .context("Question text missing or too large")?;
                    let key = hash(&format!(
                        "question:{}:{}:{qid}",
                        ado::config(s)?.source(),
                        a.item
                    ));
                    let payload = json!({"item":a.item,"key":key,"question":text}).to_string();
                    let c = s.connect()?;
                    let old: Option<(String, String, Option<String>)> = c
                        .query_row(
                            "SELECT request,state,receipt FROM workflow_questions WHERE id=?1",
                            [&key],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .optional()?;
                    let existing = existing_question_comment(
                        question,
                        &a.data["packet"]["evidence"]["entities"],
                    )?;
                    if let (Some((_, state, receipt)), Some((id, original))) = (&old, &existing) {
                        let receipt: Value =
                            serde_json::from_str(receipt.as_deref().unwrap_or("null"))?;
                        ensure!(
                            state == "posted"
                                && receipt["comment_id"] == *id
                                && receipt["comment_version"] == original["version"],
                            "Stable question reference differs from its posted receipt"
                        );
                        // An explicit, verified reference reuses the canonical asked question.
                        // Interpretation wording is a summary, never a replacement or new post.
                        continue;
                    }
                    ensure!(
                        old.as_ref().is_none_or(|v| v.0 == payload),
                        "Stable question ID has changed text"
                    );
                    if let Some((existing, original)) = existing {
                        let receipt = json!({"comment_id":existing,"comment_version":original["version"],"service_authored":false});
                        c.execute("INSERT OR IGNORE INTO workflow_questions(id,item,request,state,receipt,attempt)VALUES(?1,?2,?3,'posted',?4,?5)",params![key,a.item,payload,receipt.to_string(),a.id])?;
                    } else {
                        c.execute("INSERT OR IGNORE INTO workflow_questions(id,item,request,state,receipt,attempt) VALUES(?1,?2,?3,'queued',NULL,?4)",params![key,a.item,payload,a.id])?;
                    }
                }
            }

            match answer["decision"].as_str() {
                Some("no_action" | "superseded") => save(s, a, "no_action"),
                Some("ready") => {
                    ensure!(
                        answer["holds"].as_array().is_some_and(|v| v.is_empty())
                            && answer["questions"].as_array().is_some_and(|v| v.is_empty()),
                        "Ready interpretation still has holds/questions"
                    );
                    ensure!(
                        answer["scenarios"]
                            .as_array()
                            .is_some_and(|v| !v.is_empty()),
                        "Ready interpretation requires a concrete acceptance scenario"
                    );
                    ensure!(
                        answer["evidence_refs"]
                            .as_array()
                            .is_some_and(|v| !v.is_empty()),
                        "Interpretation evidence references missing"
                    );
                    if answer["existing_work"]["unresolved_ownership"] != false {
                        return held(
                            s,
                            cfg,
                            settings,
                            a,
                            "Existing ownership requires reconciliation",
                        );
                    }
                    let scenarios = answer["scenarios"].as_array().unwrap();
                    let mut ids = std::collections::HashSet::new();
                    for scenario in scenarios {
                        for field in ["id", "role", "starting_state", "action", "expected_result"] {
                            ensure!(
                                scenario[field]
                                    .as_str()
                                    .is_some_and(|v| !v.trim().is_empty()),
                                "Acceptance scenario {field} missing"
                            );
                        }
                        ensure!(
                            ids.insert(scenario["id"].as_str().unwrap()),
                            "Acceptance scenario IDs must be distinct"
                        );
                    }
                    ensure!(
                        answer["intended_outcome"]
                            .as_str()
                            .is_some_and(|v| !v.trim().is_empty()),
                        "Concrete intended outcome missing"
                    );
                    let mut answers = Vec::new();
                    if let Some(refs) = answer["authoritative_answers"].as_array() {
                        for reference in refs {
                            let role = reference["authority_role"]
                                .as_str()
                                .context("Answer authority role missing")?;
                            let allowed = settings.project["decision_authorities"][role]
                                .as_array()
                                .context("Answer authority role is not configured")?;
                            let original = if reference["kind"] == "ado_comment" {
                                let original = a.data["packet"]["evidence"]["entities"]
                                    .as_array()
                                    .context("Source entities missing")?
                                    .iter()
                                    .find(|v| {
                                        v["id"] == reference["comment_id"]
                                            && v["version"] == reference["version"]
                                            && v["isDeleted"] != true
                                    })
                                    .context(
                                        "Answer comment/version is not in the frozen source",
                                    )?;
                                ensure!(
                                    allowed.contains(&original["createdBy"]["id"]),
                                    "Answer author is not configured for this decision role"
                                );
                                let posted:bool=s.connect()?.query_row("SELECT EXISTS(SELECT 1 FROM workflow_questions WHERE json_extract(receipt,'$.comment_id')=?1 AND json_extract(receipt,'$.comment_version')=?2 AND json_extract(receipt,'$.service_authored')=1)",params![original["id"].as_i64().unwrap_or(-1),original["version"].as_i64().unwrap_or(-1)],|r|r.get(0))?;
                                ensure!(
                                    !posted,
                                    "A service-authored question is not an authority answer"
                                );
                                original
                            } else {
                                ensure!(
                                    reference["kind"] == "ado_revision",
                                    "Unsupported answer source"
                                );
                                let field = reference["field"]
                                    .as_str()
                                    .context("Answer revision field missing")?;
                                ensure!(
                                    matches!(
                                        field,
                                        "System.Description"
                                            | "System.Title"
                                            | "Microsoft.VSTS.Common.AcceptanceCriteria"
                                    ),
                                    "Answer revision does not change a requirement field"
                                );
                                let original = a.data["packet"]["evidence"]["revisions"]
                                    .as_array()
                                    .context("Source revisions missing")?
                                    .iter()
                                    .find(|v| {
                                        v["rev"] == reference["revision"]
                                            && !v["fields"][field].is_null()
                                    })
                                    .context("Answer revision/field not found")?;
                                ensure!(
                                    allowed.contains(&original["revisedBy"]["id"]),
                                    "Revision author is not configured for this decision role"
                                );
                                original
                            };
                            answers.push(json!({"authority_role":role,"reference":reference,"original":original}));
                        }
                    }
                    a.data["authoritative_answers"] = json!(answers);
                    save(s, a, "ready")
                }
                Some("clarification_required") => {
                    let questions = answer["questions"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(|q| q["question"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n- ")
                        })
                        .unwrap_or_default();
                    held(
                        s,
                        cfg,
                        settings,
                        a,
                        &format!("Clarification needed:\n- {questions}"),
                    )
                }
                _ => held(
                    s,
                    cfg,
                    settings,
                    a,
                    "Interpretation response has no supported decision",
                ),
            }
        }
        "ready" => {
            let c = s.connect()?;
            let existing: Option<(String, String, i64, String)> = c
                .query_row(
                    "SELECT id,coder,version,state FROM work WHERE source=?1 AND item=?2",
                    params![ado::config(s)?.source(), a.item],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if existing.is_none() && !settings.allow_new_copies {
                return held(s, cfg, settings, a, "Automatic new copies are not enabled");
            }
            let previous = existing
                .as_ref()
                .map(|v| -> Result<Value> {
                    let raw: String = c.query_row(
                        "SELECT input FROM assignments WHERE job=?1 AND version=?2",
                        params![v.0, v.2],
                        |r| r.get(0),
                    )?;
                    Ok(serde_json::from_str(&raw)?)
                })
                .transpose()?;
            if existing.as_ref().is_some_and(|v| v.3 == "paused")
                && previous
                    .as_ref()
                    .is_none_or(|v| !v["context"]["workflow_id"].is_string())
            {
                return held(
                    s,
                    cfg,
                    settings,
                    a,
                    "Manually paused ownership remains paused until an explicit resume decision",
                );
            }
            let job = hash(&format!("work:{}:{}", ado::config(s)?.source(), a.item));
            let version = existing.as_ref().map_or(1, |v| v.2 + 1);
            let plan=driver.call("plan",&json!({"attempt":a.id,"job_id":job,"version":version,"item":a.item,"previous":previous,"interpretation":a.data["interpretation"]}))?;
            let mut assigned = plan["assignment"].clone();
            assigned["schema_version"] = json!(1);
            assigned["requester"] = json!({"type":"client","machine":cfg.owner,"id":cfg.client_id});
            assigned["job_id"] = json!(job);
            assigned["assignment_revision"] = json!(format!("workflow-{version}"));
            let manifest: Value =
                serde_json::from_slice(&fs::read(cfg.pack_root.join("pack.manifest.json"))?)?;
            assigned["project_pack"] = json!({"id":manifest["id"],"version":manifest["version"],"manifest_sha256":cfg.pack_hash});
            assigned["work_item"] = json!({"id":a.item,"revision":a.data["packet"]["revision"],"source_digest":a.digest});
            assigned["delivery"] = json!({"push":false,"create_or_update_pr":false,"merge":false,"deploy":false,"autocomplete":false});
            assigned["task"] = json!({"request":a.data["interpretation"]["intended_outcome"],"acceptance_criteria":a.data["interpretation"]["scenarios"],"preservation_targets":a.data["interpretation"]["preservation_targets"],"known_holds":[],"workflow":"Normal full local repository/SQL/application/authenticated-browser reproduction, implementation and validation. This assignment is the implementation stage. The service requests exact-commit context and independent technical reviews after a candidate handoff, returns findings to this same coder, and publishes a ready candidate as a full non-draft PR. No investigation-only restriction is introduced.","candidate_contract":{"protocol":"boxen.work-report.v1","commit":"full exact SHA","base":"current target base SHA","validation":{"local_runtime_exercised":true,"acceptance":[{"id":"scenario-id","status":"observed_pass","evidence":"retained real app observation reference"}]},"cleanup":{"no_owned_runtime_resources":true}},"cleanup":"Owned application processes, allocated synthetic database and temporary source configuration are cleaned after verified local work. The service-owned persistent browser/profile remains available for subsequent jobs. A candidate report distinguishes source checks, real observations, review-pending state and remaining required implementation work."});
            assigned["task"]["candidate_contract"]["pr_description"] =
                json!(PR_DESCRIPTION_GUIDANCE);
            if settings.project["visual_evidence_required"] == true {
                assigned["task"]["candidate_contract"]["visual_evidence"] = json!({
                    "screenshots":[{"file":"JPEG/PNG filename directly in the assignment evidence directory","sha256":"full lowercase SHA-256","caption":"what this screenshot demonstrates at the candidate commit"}],
                    "not_applicable_reason":"Specific explanation when screenshots would not demonstrate the change. Exactly one form applies. Include only the minimum screenshots needed, using synthetic data and framing that excludes account details, secrets and unrelated content."
                });
            }
            assigned["task"]["operator_feedback"] = a.data["operator_feedback"].clone();
            if let Some(browser) = settings.project["browser_workflow"].as_str() {
                assigned["task"]["browser_workflow"] = json!(browser);
            }
            if !crate::managed_browser::configure(&mut assigned, &settings.project)?
                && let Some(policy) = approvals::Policy::load(s)?.filter(|p| p.enabled)
            {
                assigned["runtime"]["browser_launch_command"] =
                    json!(approvals::launch_script(&policy, &assigned)?);
                assigned["task"]["browser_approval"] = json!(
                    "The exact runtime.browser_launch_command, executed in the assigned worktree using the configured shell, is eligible for a one-time Boxen approval while this current assignment holds its browser reservation. Codex uses on-request with Full Access. Unmatched approval requests remain pending for manual review; they do not imply a rejected launch or completed validation. A changed/compound launch is outside this rule."
                );
                if policy.native_edge_app_access {
                    assigned["task"]["native_browser_approval"] = json!(
                        "The operator also enabled one-time approval of Codex native Computer Use app access to Microsoft Edge for the current assigned coder with its browser reservation held. App access can include other Edge windows; interact only with the assigned browser/work. Other apps and unrelated permission requests remain manual. Approval does not establish browser acceptance or repair the browser connector host."
                    );
                }
            }
            let mut input = json!({"item":a.item,"source_digest":a.digest,"new_copy":existing.is_none(),"idempotency_key":allocation_key(a),"context":{"decision":if existing.is_some(){"return_to_coder"}else{"new_implementation"},"workflow_id":a.id,"operator_resume":a.data["operator_resume"],"authority":settings.authority,"authoritative_answers":a.data["authoritative_answers"],"evidence":[a.data["interpretation_request"]],"interpretation":a.data["interpretation"]},"legacy_reconciliation":a.data["interpretation"].to_string(),"assignment":assigned});
            if let Some((_, coder, _, _)) = existing {
                input["coder_id"] = json!(coder);
            }
            let result = s.assign(cfg, &input)?;
            a.data["job_id"] = result["job_id"].clone();
            a.data["coder_id"] = result["coder_id"].clone();
            a.data["assignment_version"] = result["version"].clone();
            save(s, a, "awaiting_allocation")
        }
        "awaiting_allocation" => {
            let state: String = s.connect()?.query_row(
                "SELECT state FROM work WHERE id=?1",
                [a.data["job_id"].as_str().unwrap()],
                |r| r.get(0),
            )?;
            if state == "needs_refresh" {
                return save(s, a, "superseded");
            }
            if state != "reserved" {
                return Ok(());
            }
            let input = assignment(s, a)?;
            driver.call(
                "prepare",
                &json!({"assignment":input["assignment"],"pack_root":cfg.pack_root}),
            )?;
            save(s, a, "coding")
        }
        "coding" => {
            let raw: Option<String> = s
                .connect()?
                .query_row(
                    "SELECT report FROM supervision WHERE job=?1 AND version=?2",
                    params![
                        a.data["job_id"].as_str().unwrap(),
                        a.data["assignment_version"].as_i64().unwrap()
                    ],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let Some(raw) = raw else {
                let closed:Option<bool>=s.connect()?.query_row("SELECT closed=1 AND phase='unresolved' FROM supervision WHERE job=?1 AND version=?2",params![a.data["job_id"].as_str().unwrap(),a.data["assignment_version"].as_i64().unwrap()],|r|r.get(0)).optional()?;
                if closed == Some(true) {
                    return held(
                        s,
                        cfg,
                        settings,
                        a,
                        "Execution has no recoverable structured handoff; no implementation effects are replayed",
                    );
                }
                return Ok(());
            };
            let coder = a.data["coder_id"].as_str().unwrap();
            if !idle(api, coder)? {
                return Ok(());
            }
            let report: Value = serde_json::from_str(&raw)?;
            let input = assignment(s, a)?;
            let cleanup=driver.call("verify_cleanup",&json!({"assignment":input["assignment"],"require_database_removed":report["outcome"]=="completed"}))?;
            ensure!(
                cleanup["quiescent"] == true,
                "Owned runtime resources are not quiescent"
            );
            a.data["cleanup_readback"] = cleanup;
            if report["outcome"] != "completed" || !fresh(s, a)? {
                transport::release_with_basis(
                    s,
                    api,
                    a.data["job_id"].as_str().unwrap(),
                    "paused",
                    true,
                    "Automatic workflow independent project-driver quiescence readback; blocked/stale implementation is retained",
                    (
                        "workflow_cleanup_readback",
                        a.data["assignment_version"].as_i64(),
                    ),
                )?;
                return held(
                    s,
                    cfg,
                    settings,
                    a,
                    &format!("Implementation outcome/source changed: {}", report),
                );
            }
            a.data["candidate"] = report.clone();
            let expected = a.data["interpretation"]["scenarios"]
                .as_array()
                .context("Acceptance scenarios missing")?;
            let observed = report["validation"]["acceptance"].as_array();
            let coverage = observed.is_some_and(|rows| {
                expected.iter().all(|scenario| {
                    rows.iter()
                        .filter(|row| {
                            row["id"] == scenario["id"] && row["status"] == "observed_pass"
                        })
                        .count()
                        == 1
                })
            });
            if !coverage {
                return correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"condition":"acceptance_coverage","detail":"Every assigned scenario needs its own observed-pass result and retained real runtime evidence"}),
                );
            }

            if let Err(error) = candidate(&report).and_then(|_| pr_description(&report)) {
                return correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"condition":"candidate_contract","detail":error.to_string(),"meaning":"A corrected structured report can reuse actual retained evidence at the same commit; this finding requests no replay of completed effects"}),
                );
            }
            let inspection = driver.call(
                "inspect_candidate",
                &json!({"assignment":input["assignment"],"candidate":report}),
            )?;
            if inspection["ready"] != true {
                return correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"condition":inspection["reason"],"detail":inspection["detail"],"target_commit":inspection["target_commit"],"meaning":"Current target integration and refreshed checks/reviews precede publication"}),
                );
            }
            a.data["candidate"] = report;
            save(s, a, "request_reviews")
        }
        "request_reviews" => {
            ensure!(fresh(s, a)?, "Source changed before review dispatch");
            let reviewer = hash(&format!("technical-review:{}", a.id));
            ensure!(
                Some(reviewer.as_str()) != a.data["coder_id"].as_str()
                    && Some(settings.context_resident.as_str()) != a.data["coder_id"].as_str(),
                "Coder cannot satisfy independent review"
            );
            api.call(json!({"op":"resident_definition","args":{"action":"instantiate","spec":{"id":reviewer,"definition_id":settings.review_definition,"definition_revision":settings.review_revision,"overrides":{"name":format!("technical-reviewer~{reviewer}")}}}}))?;
            for (role, to) in [
                ("context", settings.context_resident.clone()),
                ("technical", reviewer),
            ] {
                let body = json!({"protocol":"boxen.review-request.v1","role":role,"review_round":a.data["assignment_version"],"reviewed_commit":a.data["candidate"]["commit"],"base_commit":a.data["candidate"]["base"],"interpretation":a.data["interpretation"],"candidate":a.data["candidate"],"project_pack_root":cfg.pack_root,"assignment":assignment(s,a)?["assignment"],
                    "contract":{"protocol":"boxen.review.v1","role":role,"reviewed_commit":a.data["candidate"]["commit"],"base_commit":a.data["candidate"]["base"],"decision":"pass|changes_required|blocked","findings":[{"id":"stable-finding","blocking":false,"evidence":"source/check reference","requested_change":"specific correction"}]},
                    "meaning":"An explicit JSON response to the authenticated reply_to, with matching request_id, attempt_id, work_item_id, source_digest, role, reviewed_commit, base_commit and envelope in_reply_to, records this review. Pass covers this exact commit and role only. Context review assesses intent/preservation; technical review independently inspects the actual diff and failure paths. Private narration is not a decision; review does not authorize source edits, merge or deployment."});
                let mut body = body;
                if settings.project["visual_evidence_required"] == true {
                    body["visual_evidence_review"] = json!(
                        "Read the candidate's proposed PR screenshots from the assigned evidence directory. Review whether they demonstrate the changed behavior with minimal frames, match this commit, and exclude credentials, account details and non-synthetic data. If screenshots do not apply, review the specific reason. Missing, inappropriate or insufficient evidence is a blocking finding."
                    );
                }
                let mid = enqueue(
                    s,
                    cfg,
                    a,
                    &format!("review:{role}"),
                    json!({"type":"resident","id":to}),
                    body,
                )?;
                a.data[format!("{role}_request")] = json!(mid);
            }
            a.data["decision_due"] = json!(now() + 2 * 60 * 60_000);
            save(s, a, "reviewing")
        }
        "reviewing" => {
            if now() > a.data["decision_due"].as_i64().unwrap_or(i64::MAX) {
                return held(
                    s,
                    cfg,
                    settings,
                    a,
                    "Required review response deadline elapsed; no publication approval is inferred",
                );
            }

            let Some(context) = response(s, a.data["context_request"].as_str().unwrap())? else {
                return Ok(());
            };
            let Some(technical) = response(s, a.data["technical_request"].as_str().unwrap())?
            else {
                return Ok(());
            };
            a.data["context_review"] = context.clone();
            a.data["technical_review"] = technical.clone();
            if !fresh(s, a)? {
                return held(
                    s,
                    cfg,
                    settings,
                    a,
                    "Source changed during review; candidate retained and publication blocked",
                );
            }
            if reviews_pass(&context, &technical) {
                save(s, a, "publish_ready")
            } else if context["decision"] == "blocked" || technical["decision"] == "blocked" {
                held(s, cfg, settings, a, "Required review is blocked")
            } else {
                correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"context":context,"technical":technical}),
                )
            }
        }
        "pausing" => {
            let coder = a.data["coder_id"]
                .as_str()
                .context("Paused coder missing")?;
            if !idle(api, coder)? {
                return Ok(());
            }
            let input = assignment(s, a)?;
            let proof = driver.call(
                "verify_cleanup",
                &json!({"assignment":input["assignment"],"require_database_removed":false}),
            )?;
            ensure!(
                proof["quiescent"] == true,
                "Paused resources are not quiescent"
            );
            transport::release_with_basis(
                s,
                api,
                a.data["job_id"].as_str().unwrap(),
                "paused",
                true,
                "Automatic workflow project-driver quiescence readback; data/evidence retained",
                (
                    "workflow_pause_readback",
                    a.data["assignment_version"].as_i64(),
                ),
            )?;
            a.data["cleanup_readback"] = proof;
            notify(
                s,
                cfg,
                settings,
                a,
                "paused",
                json!({"reason":a.data["pause_reason"],"job_id":a.data["job_id"]}),
            )?;
            save(s, a, "held")
        }
        "publish_ready" => {
            ensure!(settings.publication, "Automatic PR publication is disabled");
            ensure!(fresh(s, a)?, "Source changed before publication");
            if let Err(error) = pr_description(&a.data["candidate"]) {
                return correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"condition":"pr_description","detail":error.to_string(),"meaning":"A corrected report can describe the same candidate without repeating validation"}),
                );
            }
            let inspection=driver.call("inspect_candidate",&json!({"assignment":assignment(s,a)?["assignment"],"candidate":a.data["candidate"]}))?;
            if inspection["ready"] != true {
                return correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"condition":inspection["reason"],"detail":inspection["detail"],"target_commit":inspection["target_commit"]}),
                );
            }
            a.data["publication_grant"] = json!({"authority":settings.authority,"draft":false,"merge":false,"deploy":false,"autocomplete":false});
            save(s, a, "push_pending")
        }
        "push_pending" => {
            ensure!(fresh(s, a)?, "Source changed before push");
            driver.call("push",&json!({"assignment":assignment(s,a)?["assignment"],"candidate":a.data["candidate"]}))?;
            save(s, a, "pr_lookup")
        }
        "pr_lookup" | "pr_creating" => {
            let result=driver.call("find_pr",&json!({"assignment":assignment(s,a)?["assignment"],"candidate":a.data["candidate"]}))?;
            let prs = result["prs"].as_array().context("PR list required")?;
            ensure!(
                prs.len() <= 1,
                "Multiple matching PRs require reconciliation"
            );
            if let Some(pr) = prs.first() {
                a.data["pr_id"] = pr["id"].clone();
                return save(s, a, "pr_readback");
            }
            if a.stage == "pr_creating" {
                return held(
                    s,
                    cfg,
                    settings,
                    a,
                    "PR creation outcome remains uncertain; no matching PR is visible and creation is not repeated automatically",
                );
            }
            ensure!(fresh(s, a)?, "Source changed before PR creation");
            if let Err(error) = pr_description(&a.data["candidate"]) {
                return correction(
                    s,
                    cfg,
                    api,
                    a,
                    json!({"condition":"pr_description","detail":error.to_string(),"meaning":"A corrected report can describe the same candidate without repeating validation"}),
                );
            }
            // Intent is committed BEFORE creation. A crash returns to read-only reconciliation.
            save(s, a, "pr_creating")?;
            let result=driver.call("create_pr",&json!({"assignment":assignment(s,a)?["assignment"],"candidate":a.data["candidate"],"interpretation":a.data["interpretation"],"context_review":a.data["context_review"],"technical_review":a.data["technical_review"]}))?;
            a.data["pr_id"] = result["id"].clone();
            save(s, a, "pr_readback")
        }
        "pr_readback" => {
            let receipt=driver.call("verify_pr",&json!({"assignment":assignment(s,a)?["assignment"],"candidate":a.data["candidate"],"id":a.data["pr_id"]}))?;
            ensure!(
                receipt["is_draft"] == false
                    && (receipt["active"] == true || receipt["completed"] == true)
                    && receipt["commit"] == a.data["candidate"]["commit"]
                    && receipt["work_item_id"] == a.item
                    && receipt["autocomplete"] == false,
                "PR readback does not match approved candidate/full PR contract"
            );
            let mut receipt = receipt;
            if settings.project["visual_evidence_required"] == true {
                ensure!(
                    settings.publication,
                    "Automatic PR evidence publication is disabled"
                );
                let evidence = driver.call(
                    "attach_pr_evidence",
                    &json!({"assignment":assignment(s,a)?["assignment"],"candidate":a.data["candidate"],"id":a.data["pr_id"]}),
                )?;
                ensure!(
                    evidence["verified"] == true,
                    "PR visual evidence readback required"
                );
                receipt["visual_evidence"] = evidence;
            }
            a.data["publication_receipt"] = receipt.clone();
            let latest = s.packet(a.item)?;
            let source_changed_after_publication =
                latest["source_digest"] != a.digest || latest["eligible"] != true;
            a.data["source_changed_after_publication"] = json!(source_changed_after_publication);
            transport::release_with_basis(
                s,
                api,
                a.data["job_id"].as_str().unwrap(),
                if source_changed_after_publication {
                    "paused"
                } else {
                    "finished"
                },
                true,
                "Automatic workflow required reviews, project cleanup readback and full PR publication receipt",
                (
                    "workflow_full_pr_readback",
                    a.data["assignment_version"].as_i64(),
                ),
            )?;
            notify(s, cfg, settings, a, "published", receipt)?;
            save(s, a, "published")
        }
        _ => Ok(()),
    }
}

pub fn tick(
    s: &Store,
    cfg: &Config,
    api: &impl Boxen,
    settings: &Settings,
    driver: &mut impl Driver,
) -> Result<()> {
    if !settings.enabled {
        return Ok(());
    }
    let source = ado::config(s)?;
    ensure!(
        settings.project["azure_organization"]
            .as_str()
            .is_some_and(|v| v.trim_end_matches('/') == source.organization.trim_end_matches('/'))
            && (settings.project["azure_project"] == source.project
                || settings.project["azure_project"] == source.project_id),
        "Workflow project differs from the configured Azure mirror"
    );
    ensure!(
        cfg.supervision.is_some(),
        "Automatic workflow requires structured handoff supervision"
    );
    if let Err(error) = questions_tick(s, cfg, settings, driver) {
        let c = s.connect()?;
        let row:Option<(String,String,Option<String>)>=c.query_row("SELECT id,attempt,error FROM workflow_questions WHERE state IN('queued','creating') AND next_due<=?1 ORDER BY rowid LIMIT 1",[now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, attempt, old)) = row {
            let detail = error.to_string();
            if old.as_deref() != Some(&detail) {
                notify(
                    s,
                    cfg,
                    settings,
                    &load(s, &attempt)?,
                    &format!("question_error:{}:{}", &id[..12], &hash(&detail)[..12]),
                    json!({"question":id,"detail":detail}),
                )?;
            }
            c.execute(
                "UPDATE workflow_questions SET error=?2,next_due=?3 WHERE id=?1",
                params![id, detail, now() + 60_000],
            )?;
        }
    }
    let capability = api
        .call(json!({"op":"thread_activity","id":cfg.owner}))
        .context("Compatible Boxen core with compact thread_activity support is required")?;
    ensure!(
        capability["thread_id"] == cfg.owner && capability["idle"].is_boolean(),
        "Boxen thread activity metadata is unavailable"
    );
    intake(s, cfg, api)?;
    crate::operator::tick(s, cfg, api, settings)?;
    discover(s, settings)?;
    flush(s, cfg, settings, api)?;
    let c = s.connect()?;
    let ids=c.prepare("SELECT id FROM workflow_attempts WHERE stage NOT IN('published','held','no_action','superseded') AND next_due<=?1 ORDER BY checked,created,id LIMIT 32")?.query_map([now()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        let mut a = load(s, &id)?;
        c.execute(
            "UPDATE workflow_attempts SET checked=?2 WHERE id=?1",
            params![id, now()],
        )?;
        if let Err(error) = advance(s, cfg, settings, api, driver, &mut a) {
            let detail = error.to_string();
            let old: Option<String> = c.query_row(
                "SELECT error FROM workflow_attempts WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if old.as_deref() != Some(&detail) {
                Store::log(
                    &c,
                    a.data["job_id"].as_str().unwrap_or(&id),
                    "workflow.error",
                    json!({"attempt":id,"stage":a.stage,"detail":detail}),
                )?;
                notify(
                    s,
                    cfg,
                    settings,
                    &a,
                    &format!("error:{}", &hash(&detail)[..12]),
                    json!({"stage":a.stage,"detail":detail}),
                )?;
            }
            c.execute(
                "UPDATE workflow_attempts SET error=?2,next_due=?3 WHERE id=?1",
                params![id, detail, now() + 60_000],
            )?;
        }
    }
    flush(s, cfg, settings, api)?;
    Ok(())
}

fn questions_tick(
    s: &Store,
    cfg: &Config,
    settings: &Settings,
    driver: &mut impl Driver,
) -> Result<()> {
    let c = s.connect()?;
    let row:Option<(String,String,String,String)>=c.query_row("SELECT id,request,state,attempt FROM workflow_questions WHERE state IN('queued','creating') AND next_due<=?1 ORDER BY rowid LIMIT 1",[now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((id, raw, state, attempt)) = row else {
        return Ok(());
    };
    let request: Value = serde_json::from_str(&raw)?;
    let existing = driver.call("find_question", &request)?;
    if existing["found"] == true {
        c.execute(
            "UPDATE workflow_questions SET state='posted',receipt=?2 WHERE id=?1",
            params![id, existing.to_string()],
        )?;
        return Ok(());
    }
    if state == "creating" {
        c.execute(
            "UPDATE workflow_questions SET state='uncertain' WHERE id=?1",
            [&id],
        )?;
        return notify(
            s,
            cfg,
            settings,
            &load(s, &attempt)?,
            "question_delivery_uncertain",
            json!({"question":id,"meaning":"No second comment is posted while the first write remains uncertain"}),
        );
    }
    if !settings.post_clarifications {
        return Ok(());
    }
    c.execute(
        "UPDATE workflow_questions SET state='creating' WHERE id=?1",
        [&id],
    )?;
    let result = driver.call("post_question", &request)?;
    c.execute(
        "UPDATE workflow_questions SET state='posted',receipt=?2 WHERE id=?1",
        params![id, result.to_string()],
    )?;
    Ok(())
}
