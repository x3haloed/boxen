use crate::*;
use fs2::FileExt;
use std::io::Write;

fn job_lock(s: &Store, job: &str) -> Result<fs::File> {
    key(job)?;
    let folder = s.path.parent().unwrap().join("locks");
    fs::create_dir_all(&folder)?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(folder.join(format!("{job}.lock")))?;
    f.try_lock_exclusive()
        .context("Another dispatch/release operation owns this job")?;
    Ok(f)
}

pub trait Boxen {
    fn call(&self, request: Value) -> Result<Value>;
}
pub struct Control {
    cfg: Config,
    token: String,
    agent: ureq::Agent,
}
impl Control {
    pub fn new(cfg: &Config) -> Result<Self> {
        cfg.validate()?;
        let token = fs::read_to_string(cfg.boxen_home.join("control-token"))
            .context("Boxen control credential unavailable")?
            .trim()
            .to_string();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(std::time::Duration::from_secs(15)))
            .build()
            .into();
        let mut response = agent
            .get(format!("{}/api/status", cfg.url))
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .map_err(|_| anyhow::anyhow!("Boxen API unavailable"))?;
        ensure!(
            response.status().as_u16() == 200,
            "Boxen authentication/status rejected"
        );
        let status: Value = response
            .body_mut()
            .with_config()
            .limit(512 * 1024)
            .read_json()?;
        ensure!(
            status["identity"] == cfg.owner,
            "Boxen identity differs from pinned owner"
        );
        Ok(Self {
            cfg: cfg.clone(),
            token,
            agent,
        })
    }
}
impl Boxen for Control {
    fn call(&self, request: Value) -> Result<Value> {
        let mut r = self
            .agent
            .post(format!("{}/api/operation", self.cfg.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(json!({"peer":self.cfg.owner,"address":"","request":request}))
            .map_err(|_| {
                anyhow::anyhow!("Boxen transport unavailable; outcome may be uncertain")
            })?;
        ensure!(
            r.status().as_u16() == 200,
            "Boxen rejected operation (HTTP {})",
            r.status().as_u16()
        );
        Ok(r.body_mut()
            .with_config()
            .limit(2 * 1024 * 1024)
            .read_json()?)
    }
}
fn effect(
    s: &Store,
    job: &str,
    version: i64,
    kind: &str,
    state: &str,
    detail: Option<&str>,
) -> Result<()> {
    let mut c = s.connect()?;
    let tx = c.transaction()?;
    tx.execute("INSERT INTO effects VALUES(?1,?2,?3,?4,?5) ON CONFLICT(job,version,kind) DO UPDATE SET state=excluded.state,detail=excluded.detail",params![job,version,kind,state,detail])?;
    Store::log(
        &tx,
        job,
        &format!("effect.{state}"),
        json!({"version":version,"kind":kind}),
    )?;
    tx.commit()?;
    Ok(())
}
pub fn dispatch(s: &Store, cfg: &Config, api: &impl Boxen, job: &str) -> Result<()> {
    let _lock = job_lock(s, job)?;
    let c = s.connect()?;
    let (source, item, coder, state, version): (String, i64, String, String, i64) = c.query_row(
        "SELECT source,item,coder,state,version FROM work WHERE id=?1",
        [job],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if state != "reserved" {
        return Ok(());
    }
    let (digest, packet, instantiate, message): (String, String, String, String) = c.query_row(
        "SELECT digest,packet,instantiate,message FROM assignments WHERE job=?1 AND version=?2",
        params![job, version],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let latest = packet_on(&c, &source, item)?;
    if latest["source_digest"] != digest
        || !crate::assignment_eligible(
            &latest,
            &serde_json::from_str::<Value>(&packet)?["eligibility_override"],
        )
        || now() - latest["observed"].as_i64().unwrap_or(0) > 30 * 60_000
    {
        c.execute("UPDATE work SET state='needs_refresh',error='Source changed or is stale; operator refresh required' WHERE id=?1 AND version=?2",params![job,version])?;
        return Ok(());
    }
    // Derived files are published before mailbox submission. SQLite retains the
    // authoritative immutable version and can regenerate a missing projection.
    let path = s
        .path
        .parent()
        .unwrap()
        .join("evidence")
        .join(job)
        .join(version.to_string())
        .join("packet.json");
    fs::create_dir_all(path.parent().unwrap())?;
    if path.exists() {
        ensure!(
            fs::read_to_string(&path)? == packet,
            "Derived packet conflicts with immutable assignment"
        );
    } else {
        let temp = path.with_extension(format!("{}.tmp", id()));
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        f.write_all(packet.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::hard_link(&temp, &path)?;
        fs::remove_file(temp)?;
    }
    let create: Value = serde_json::from_str(&instantiate)?;
    if !create.is_null() {
        effect(s, job, version, "instantiate", "submitting", None)?;
        let r = match api.call(create) {
            Ok(v) => v,
            Err(e) => {
                effect(
                    s,
                    job,
                    version,
                    "instantiate",
                    "uncertain",
                    Some(&e.to_string()),
                )?;
                return Err(e);
            }
        };
        ensure!(
            r["id"] == coder
                && r["owner"] == cfg.owner
                && r["definition"]["id"] == cfg.definition_id
                && r["definition"]["instantiated_revision"] == cfg.definition_revision,
            "Instantiated coder identity/definition mismatch"
        );
        effect(s, job, version, "instantiate", "accepted", None)?;
    } else {
        let r = api.call(json!({"op":"resident_get","id":coder}))?;
        let native = latest["legacy"]["ownership"]["coder_thread_id"].as_str();
        ensure!(
            r["owner"] == cfg.owner
                && (r["definition"]["id"] == cfg.definition_id
                    || native.is_some_and(|n| r["promotion"]["backend_thread_id"] == n)),
            "Adopted coder does not match the definition or recorded legacy native thread"
        );
    }
    // A poll can invalidate the assignment during a remote create. That retains
    // the exact coder binding but prevents submitting an obsolete assignment.
    let still: bool = c.query_row(
        "SELECT state='reserved' AND version=?2 FROM work WHERE id=?1",
        params![job, version],
        |r| r.get(0),
    )?;
    if !still {
        return Ok(());
    }
    let msg: Value = serde_json::from_str(&message)?;
    effect(s, job, version, "mail", "submitting", None)?;
    let accepted =
        match api.call(json!({"op":"mailbox_send","client_id":cfg.client_id,"message":msg})) {
            Ok(v) => v,
            Err(e) => {
                effect(s, job, version, "mail", "uncertain", Some(&e.to_string()))?;
                return Err(e);
            }
        };
    ensure!(
        accepted["id"] == msg["id"]
            && accepted["sender"]
                == json!({"type":"client","machine":cfg.owner,"id":cfg.client_id}),
        "Assignment mail acceptance identity mismatch"
    );
    effect(s, job, version, "mail", "accepted", None)?;
    c.execute("UPDATE work SET state='delivered',error=NULL WHERE id=?1 AND version=?2 AND state='reserved'",params![job,version])?;
    event(
        "assignment.mail_accepted",
        json!({"job":job,"version":version,"coder":coder}),
    );
    Ok(())
}
pub fn continue_assignment(
    s: &Store,
    cfg: &Config,
    api: &impl Boxen,
    input: &Value,
) -> Result<Value> {
    let source = ado::config(s)?.source();
    let c = s.connect()?;
    let (job, coder, version, held): (String, String, i64, i64) = c.query_row(
        "SELECT id,coder,version,holds_slot FROM work WHERE source=?1 AND item=?2",
        params![source, input["item"].as_i64().context("Item required")?],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let _lock = job_lock(s, &job)?;
    ensure!(
        held == 1
            && input["continuation"]["expected_version"] == version
            && input["continuation"]["expected_coder"] == coder,
        "Continuation owner/version changed"
    );
    let runs = api.call(json!({"op":"list"}))?;
    for run in runs.as_array().context("Boxen run catalog unavailable")? {
        if run["record"]["spec"]["thread_id"] == coder {
            ensure!(
                matches!(
                    run["status"].as_str(),
                    Some("completed" | "failed" | "interrupted" | "cancelled")
                ),
                "Continuation waits for idle coder execution; resources remain held"
            );
        }
    }
    let message: String = c.query_row(
        "SELECT message FROM assignments WHERE job=?1 AND version=?2",
        params![job, version],
        |r| r.get(0),
    )?;
    let mid: Value = serde_json::from_str(&message)?;
    let mut after = 0;
    let mut presented = false;
    let mut complete = false;
    for _ in 0..100 {
        let page = api.call(json!({"op":"resident_inbox","id":coder,"after":after}))?;
        let messages = page["messages"]
            .as_array()
            .context("Resident inbox unavailable")?;
        for m in messages {
            ensure!(
                m["delivery"] == "presented",
                "Continuation waits for queued mailbox presentation"
            );
            if m["id"] == mid["id"] {
                presented = true;
            }
        }
        let next = page["next_after"].as_u64().unwrap_or(after);
        if messages.is_empty() || next <= after {
            complete = true;
            break;
        }
        after = next;
    }
    ensure!(
        presented && complete,
        "Continuation requires prior assignment presentation"
    );
    s.assign(cfg, input)
}
pub fn release(
    s: &Store,
    api: &impl Boxen,
    job: &str,
    outcome: &str,
    cleaned: bool,
    note: &str,
) -> Result<Value> {
    release_with_basis(s, api, job, outcome, cleaned, note, ("operator", None))
}
pub(crate) fn release_with_basis(
    s: &Store,
    api: &impl Boxen,
    job: &str,
    outcome: &str,
    cleaned: bool,
    note: &str,
    basis: (&str, Option<i64>),
) -> Result<Value> {
    let _lock = job_lock(s, job)?;
    key(job)?;
    ensure!(
        matches!(outcome, "paused" | "finished") && cleaned && !note.trim().is_empty(),
        "Release requires paused/finished, confirmed cleanup with attributed basis, and a note"
    );
    let c = s.connect()?;
    let (coder, version, state): (String, i64, String) = c.query_row(
        "SELECT coder,version,state FROM work WHERE id=?1",
        [job],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        basis.1.is_none_or(|expected| expected == version),
        "Assignment changed before automatic release"
    );
    let runs = api.call(json!({"op":"list"}))?;
    for r in runs.as_array().context("Boxen run catalog unavailable")? {
        if r["record"]["spec"]["thread_id"] == coder {
            ensure!(
                matches!(
                    r["status"].as_str(),
                    Some("completed" | "failed" | "interrupted" | "cancelled")
                ),
                "Coder still has active execution; resources remain held"
            );
        }
    }
    // An idle run catalog alone does not prove accepted mail cannot start later.
    // A queued assignment is retained until it is presented/reconciled.
    let message: String = c.query_row(
        "SELECT message FROM assignments WHERE job=?1 AND version=?2",
        params![job, version],
        |r| r.get(0),
    )?;
    let mid = serde_json::from_str::<Value>(&message)?["id"].clone();
    let mail_effect: Option<String> = c
        .query_row(
            "SELECT state FROM effects WHERE job=?1 AND version=?2 AND kind='mail'",
            params![job, version],
            |r| r.get(0),
        )
        .optional()?;
    if mail_effect.is_some() {
        let mut after = 0_u64;
        let mut found = None;
        for _ in 0..1000 {
            let page = api.call(json!({"op":"resident_inbox","id":coder,"after":after}))?;
            let messages = page["messages"]
                .as_array()
                .context("Coder inbox unavailable")?;
            for m in messages {
                if m["id"] == mid {
                    found = Some(m.clone());
                    break;
                }
            }
            if found.is_some() || messages.is_empty() {
                break;
            }
            let next = page["next_after"]
                .as_u64()
                .context("Inbox cursor missing")?;
            ensure!(next > after, "Inbox cursor did not advance");
            after = next;
        }
        if let Some(mail) = found {
            ensure!(
                mail["delivery"] == "presented",
                "Accepted assignment mail is queued; resources remain held until presentation/reconciliation"
            );
        } else {
            ensure!(
                mail_effect.as_deref() != Some("accepted"),
                "Accepted assignment is absent from bounded inbox readback; resources remain held"
            );
            ensure!(
                outcome == "paused",
                "An uncertain mail effect can only be paused after explicit absence readback"
            );
        }
    }
    ensure!(
        state != "needs_refresh" || outcome == "paused",
        "Stale assignment can be paused, not certified finished"
    );
    let mut c = s.connect()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: (i64, String) =
        tx.query_row("SELECT version,state FROM work WHERE id=?1", [job], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    ensure!(
        current == (version, state),
        "Job changed during release; operator readback required"
    );
    tx.execute("DELETE FROM reservations WHERE job=?1", [job])?;
    tx.execute(
        "UPDATE work SET state=?2,holds_slot=0,error=NULL WHERE id=?1",
        params![job, outcome],
    )?;
    Store::log(
        &tx,
        job,
        "capacity.released",
        json!({"version":version,"outcome":outcome,"note":note,"cleanup_basis":basis.0,"cleanup_confirmed_by_operator":basis.0=="operator"}),
    )?;
    tx.commit()?;
    Ok(json!({"job_id":job,"state":outcome,"resources_released":true}))
}
