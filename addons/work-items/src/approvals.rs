//! Operator-owned assigned-browser policy over retained Boxen requests and controls.
use crate::{transport::Boxen, *};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub rule_id: String,
    pub revision: i64,
    pub authority: String,
    pub browser_executable: PathBuf,
    pub shells: Vec<PathBuf>,
}
impl Policy {
    pub fn load(s: &Store) -> Result<Option<Self>> {
        let p = s.path.parent().unwrap().join("approval-policy.json");
        if !p.exists() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        fs::File::open(p)?.take(16385).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 16384, "Approval policy exceeds 16 KiB");
        let p: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            p.revision > 0
                && !p.rule_id.is_empty()
                && p.rule_id.len() <= 80
                && !p.authority.trim().is_empty()
                && p.authority.len() <= 4096,
            "Approval rule identity/authority required"
        );
        ensure!(
            p.browser_executable.is_absolute()
                && (1..=4).contains(&p.shells.len())
                && p.shells.iter().all(|s| s.is_absolute()),
            "Approval executable/shell paths must be absolute"
        );
        Ok(Some(p))
    }
}
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS approval_streams(run TEXT PRIMARY KEY,position INTEGER NOT NULL,projection TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS approval_decisions(run TEXT,request TEXT,digest TEXT,job TEXT,version INTEGER,rule TEXT,revision INTEGER,state TEXT,reason TEXT,detail TEXT,created INTEGER,PRIMARY KEY(run,request));")?;
    Ok(())
}
fn path(p: &str) -> Result<PathBuf> {
    Ok(fs::canonicalize(p)?)
}
fn display_path(p: &Path) -> String {
    p.to_string_lossy().trim_start_matches(r"\\?\").to_string()
}
fn literal(v: &str) -> Result<String> {
    ensure!(
        !v.is_empty() && !v.chars().any(|c| c == '\'' || c.is_control()),
        "Browser argument contains unsupported quoting/control characters"
    );
    Ok(format!("'{v}'"))
}
pub fn launch_script(policy: &Policy, a: &Value) -> Result<String> {
    let exe = display_path(&path(
        policy
            .browser_executable
            .to_str()
            .context("Browser path encoding")?,
    )?);
    let profile = display_path(&path(
        a["runtime"]["browser_context"]
            .as_str()
            .context("Assigned browser profile required")?,
    )?);
    let port = a["runtime"]["frontend_port"]
        .as_u64()
        .filter(|n| (1024..=65535).contains(n))
        .context("Assigned frontend port required")?;
    let tenant = a["runtime"]["synthetic_tenant_id"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= i32::MAX as u64)
        .context("Assigned synthetic tenant required")?;
    Ok(format!(
        "Start-Process -FilePath {} -ArgumentList {},'--no-first-run',{} -WindowStyle Normal",
        literal(&exe)?,
        literal(&format!("--user-data-dir=\"{profile}\""))?,
        literal(&format!("http://localhost:{port}/?clientId={tenant}"))?
    ))
}
fn command_matches(policy: &Policy, a: &Value, command: &str) -> Result<bool> {
    let Some(words) = shlex::split(command) else {
        return Ok(false);
    };
    if !(3..=7).contains(&words.len()) {
        return Ok(false);
    }
    let shell = match path(&words[0]) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    let allowed = policy
        .shells
        .iter()
        .any(|p| fs::canonicalize(p).is_ok_and(|p| p == shell));
    if !allowed
        || !matches!(
            words[words.len() - 2].to_ascii_lowercase().as_str(),
            "-command" | "-c"
        )
    {
        return Ok(false);
    }
    if !words[1..words.len() - 2].iter().all(|s| {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "-nologo" | "-noprofile" | "-noninteractive"
        )
    }) {
        return Ok(false);
    }
    Ok(words.last() == Some(&launch_script(policy, a)?))
}

#[derive(Default, Serialize, Deserialize)]
struct Projection {
    turn: Option<String>,
    items: BTreeMap<String, Value>,
    pending: BTreeMap<String, Value>,
}
impl Projection {
    fn ingest(&mut self, e: &Value) {
        if e["kind"] == "request_answered" {
            self.pending.remove(&e["data"]["request_id"].to_string());
        }
        if e["kind"] == "control" && e["data"]["type"] == "respond" {
            self.pending.remove(&e["data"]["request_id"].to_string());
        }
        if e["kind"] != "backend" {
            return;
        }
        let d = &e["data"];
        let p = &d["params"];
        match d["method"].as_str() {
            Some("turn/started") => {
                self.turn = p["turn"]["id"].as_str().map(str::to_string);
                self.pending.clear();
                self.items.clear();
            }
            Some("turn/completed") => {
                self.turn = None;
                self.pending.clear();
                self.items.clear();
            }
            Some("item/started") if p["item"]["type"] == "commandExecution" => {
                if let Some(id) = p["item"]["id"].as_str().filter(|v| v.len() <= 256) {
                    let command = p["item"]["command"].as_str().filter(|s| s.len() <= 16384);
                    self.items.insert(id.into(), json!({"command":command}));
                }
            }
            Some("item/completed") => {
                if let Some(id) = p["item"]["id"].as_str() {
                    self.items.remove(id);
                    self.pending.retain(|_, r| r["params"]["itemId"] != id);
                }
            }
            Some("item/commandExecution/requestApproval") => {
                if (d["id"].is_string() || d["id"].is_number()) && d["id"].to_string().len() <= 256
                {
                    let request = if d.to_string().len() <= 65536 {
                        d.clone()
                    } else {
                        json!({"id":d["id"],"method":d["method"],"params":{},"oversized":true})
                    };
                    self.pending.insert(d["id"].to_string(), request);
                }
            }
            Some("serverRequest/resolved") => {
                self.pending.remove(&p["requestId"].to_string());
            }
            _ => {}
        }
    }
}
fn projection(s: &Store, cfg: &Config, run: &str) -> Result<Option<Projection>> {
    key(run)?;
    let c = s.connect()?;
    let stored: Option<(i64, String)> = c
        .query_row(
            "SELECT position,projection FROM approval_streams WHERE run=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (mut position, mut state) = match stored {
        Some((p, v)) => (u64::try_from(p)?, serde_json::from_str(&v)?),
        None => (0, Projection::default()),
    };
    let mut file = fs::File::open(cfg.boxen_home.join("runs").join(run).join("events.jsonl"))?;
    ensure!(
        position <= file.metadata()?.len(),
        "Approval journal was truncated; manual review required"
    );
    file.seek(SeekFrom::Start(position))?;
    let mut reader = BufReader::new(file);
    let mut total = 0;
    let mut caught_up = false;
    for _ in 0..512 {
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            caught_up = true;
            break;
        }
        ensure!(
            n <= 4 * 1024 * 1024,
            "Approval journal line exceeds bound; manual review required"
        );
        if !line.ends_with(b"\n") {
            break;
        }
        state.ingest(&serde_json::from_slice(&line)?);
        ensure!(
            state.items.len() <= 64 && state.pending.len() <= 32,
            "Approval projection exceeds bound; manual review required"
        );
        position += n as u64;
        total += n;
        if total >= 8 * 1024 * 1024 {
            break;
        }
    }
    c.execute("INSERT INTO approval_streams VALUES(?1,?2,?3) ON CONFLICT(run) DO UPDATE SET position=excluded.position,projection=excluded.projection",params![run,i64::try_from(position)?,serde_json::to_string(&state)?])?;
    Ok(caught_up.then_some(state))
}

fn matches(
    policy: &Policy,
    cfg: &Config,
    a: &Value,
    snapshot: &Value,
    projected: &Projection,
    request: &Value,
) -> Result<bool> {
    let p = &request["params"];
    if request["method"] != "item/commandExecution/requestApproval"
        || snapshot["status"] != "waiting"
        || p["threadId"] != snapshot["backend_thread"]
        || p["turnId"].as_str() != projected.turn.as_deref()
        || !p["threadId"].is_string()
        || projected.turn.is_none()
        || !p["additionalPermissions"].is_null()
        || !p["networkApprovalContext"].is_null()
        || a["runtime"]["required"] != true
    {
        return Ok(false);
    }
    if p["availableDecisions"]
        .as_array()
        .is_some_and(|v| !v.iter().any(|x| x == "accept"))
    {
        return Ok(false);
    }
    let Some(item) = p["itemId"].as_str().and_then(|id| projected.items.get(id)) else {
        return Ok(false);
    };
    let Some(cmd) = p["command"].as_str().filter(|s| s.len() <= 16384) else {
        return Ok(false);
    };
    if item["command"] != cmd {
        return Ok(false);
    }
    let Some(cwd) = p["cwd"].as_str().and_then(|s| path(s).ok()) else {
        return Ok(false);
    };
    let worktree = path(
        a["worktree"]
            .as_str()
            .context("Assigned worktree required")?,
    )?;
    if cwd != fs::canonicalize(&cfg.pack_root)? && !cwd.starts_with(worktree) {
        return Ok(false);
    }
    command_matches(policy, a, cmd)
}

pub fn tick(s: &Store, cfg: &Config, api: &impl Boxen) -> Result<()> {
    let Some(policy) = Policy::load(s)?.filter(|p| p.enabled) else {
        return Ok(());
    };
    let c = s.connect()?;
    let rows=c.prepare("SELECT w.id,w.coder,w.version,v.run,a.input,a.claims FROM work w JOIN supervision v ON v.job=w.id AND v.version=w.version JOIN assignments a ON a.job=w.id AND a.version=w.version WHERE w.state='delivered' AND w.holds_slot=1 AND v.closed=0 AND v.run IS NOT NULL ORDER BY v.checked LIMIT 16")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (job, coder, version, run, raw, claims) in rows {
        let snapshot = api.call(json!({"op":"get","id":run}))?;
        ensure!(
            snapshot["record"]["owner"] == cfg.owner
                && snapshot["record"]["spec"]["thread_id"] == coder
                && snapshot["record"]["spec"]["id"] == run,
            "Approval run ownership mismatch"
        );
        if snapshot["status"] != "waiting" {
            continue;
        }
        let Some(projected) = projection(s, cfg, &run)? else {
            continue;
        };
        let a: Value = serde_json::from_str(&raw)?;
        let a = &a["assignment"];
        for (id, request) in &projected.pending {
            if c.query_row(
                "SELECT EXISTS(SELECT 1 FROM approval_decisions WHERE run=?1 AND request=?2)",
                params![run, id],
                |r| r.get::<_, bool>(0),
            )? {
                continue;
            }
            let _guard = transport::job_lock(s, &job)?;
            let current:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM work WHERE id=?1 AND coder=?2 AND version=?3 AND state='delivered' AND holds_slot=1)",params![job,coder,version],|r|r.get(0))?;
            let required: Vec<String> = serde_json::from_str(&claims)?;
            let reserved = c
                .prepare("SELECT resource FROM reservations WHERE job=?1 ORDER BY resource")?
                .query_map([&job], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let policy_current = Policy::load(s)?.is_some_and(|p| p == policy && p.enabled);
            let allowed = policy_current
                && current
                && required == reserved
                && required.iter().any(|v| v.starts_with("browser:"))
                && matches(&policy, cfg, a, &snapshot, &projected, request)?;
            let state = if allowed { "intent" } else { "manual" };
            let reason = if allowed {
                "Exact assigned browser launch with current owner/version/resource reservations"
            } else {
                "Request does not match the assigned-browser rule; existing manual review remains available"
            };
            let mut connection = s.connect()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO approval_decisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    run,
                    id,
                    hash(&request.to_string()),
                    job,
                    version,
                    policy.rule_id,
                    policy.revision,
                    state,
                    reason,
                    json!({"request":request,"policy":policy}).to_string(),
                    now()
                ],
            )?;
            Store::log(
                &tx,
                &job,
                "approval.evaluated",
                json!({"run":run,"request_id":request["id"],"version":version,"rule":policy.rule_id,"revision":policy.revision,"state":state,"reason":reason}),
            )?;
            if !allowed {
                notice(
                    &tx,
                    cfg,
                    &job,
                    version,
                    (&run, &request["id"], "manual", reason),
                )?;
            }
            tx.commit()?;
            event(
                "approval.evaluated",
                json!({"job":job,"run":run,"version":version,"rule":policy.rule_id,"state":state}),
            );
            if !allowed {
                continue;
            }
            // Durable intent precedes control. Unknown delivery never causes another auto-response.
            let result=api.call(json!({"op":"control","id":run,"action":{"type":"respond","request_id":request["id"],"result":{"decision":"accept"}}}));
            let (state,reason)=match result {Ok(v) if v["accepted"]==true=>("submitted","One-time approval submitted; retained backend answer/execution events establish subsequent outcome".to_string()),Ok(_)=>("uncertain","Unexpected control response; manual reconciliation required".into()),Err(e)=>("uncertain",e.to_string())};
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE approval_decisions SET state=?3,reason=?4 WHERE run=?1 AND request=?2",
                params![run, id, state, reason],
            )?;
            Store::log(
                &tx,
                &job,
                "approval.control_outcome",
                json!({"run":run,"request_id":request["id"],"state":state,"reason":reason}),
            )?;
            if state == "uncertain" {
                notice(
                    &tx,
                    cfg,
                    &job,
                    version,
                    (&run, &request["id"], state, &reason),
                )?;
            }
            tx.commit()?;
            event(
                "approval.control_outcome",
                json!({"job":job,"run":run,"state":state}),
            );
        }
    }
    Ok(())
}

fn notice(
    c: &Connection,
    cfg: &Config,
    job: &str,
    version: i64,
    detail: (&str, &Value, &str, &str),
) -> Result<()> {
    let (run, id, state, reason) = detail;
    if let Some(settings) = &cfg.supervision {
        supervision::notify(
            c,
            cfg,
            job,
            version,
            settings,
            &format!("approval_{state}:{}", &hash(&format!("{run}:{id}"))[..16]),
            json!({"run":run,"request_id":id,"reason":reason,"meaning":"Retained Codex request remains available through normal Boxen controls; no automatic retry or session-wide approval follows"}),
        )?;
    }
    Ok(())
}

pub fn snapshot(s: &Store) -> Result<Value> {
    let c = s.connect()?;
    let rows=c.prepare("SELECT run,request,job,version,rule,revision,state,reason,created FROM approval_decisions ORDER BY created DESC LIMIT 50")?.query_map([],|r|Ok(json!({"run":r.get::<_,String>(0)?,"request_id":r.get::<_,String>(1)?,"job":r.get::<_,String>(2)?,"version":r.get::<_,i64>(3)?,"rule":r.get::<_,String>(4)?,"rule_revision":r.get::<_,i64>(5)?,"state":r.get::<_,String>(6)?,"reason":r.get::<_,String>(7)?,"at":r.get::<_,i64>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({"decisions":rows}))
}
