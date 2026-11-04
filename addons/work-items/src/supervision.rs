//! Workflow supervision consumes authenticated mail and Boxen run metadata.
//! It never infers an outcome from private narration or replays implementation.
use crate::transport::{Boxen, release_with_basis};
use crate::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub notify_to: Value,
    #[serde(default = "start_timeout")]
    pub start_timeout_seconds: i64,
    #[serde(default = "idle_timeout")]
    pub inactivity_seconds: i64,
    #[serde(default = "report_grace")]
    pub report_grace_seconds: i64,
}
fn start_timeout() -> i64 {
    300
}
fn idle_timeout() -> i64 {
    1800
}
fn report_grace() -> i64 {
    60
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=86400).contains(&self.start_timeout_seconds)
                && (1..=86400).contains(&self.inactivity_seconds)
                && (1..=3600).contains(&self.report_grace_seconds),
            "Supervision timeouts are bounded positive seconds"
        );
        let t = &self.notify_to;
        ensure!(
            matches!(t["type"].as_str(), Some("client" | "resident")),
            "Notification destination is a typed client or resident address"
        );
        key(t["id"]
            .as_str()
            .context("Notification address ID required")?)?;
        if t["type"] == "client" {
            key(t["machine"].as_str().context("Client machine required")?)?;
        }
        Ok(())
    }
}
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS supervision(job TEXT,version INTEGER,coder TEXT,message TEXT,settings TEXT,phase TEXT NOT NULL DEFAULT 'waiting',run TEXT,last_seq INTEGER NOT NULL DEFAULT 0,activity INTEGER NOT NULL,start_at INTEGER NOT NULL DEFAULT 0,terminal_at INTEGER NOT NULL DEFAULT 0,followup TEXT,report TEXT,checked INTEGER NOT NULL DEFAULT 0,closed INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(job,version));
    CREATE TABLE IF NOT EXISTS report_receipts(message TEXT PRIMARY KEY,job TEXT,version INTEGER,accepted INTEGER,reason TEXT);
    CREATE TABLE IF NOT EXISTS supervision_outbox(id TEXT PRIMARY KEY,job TEXT,version INTEGER,purpose TEXT,request TEXT,accepted INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_due INTEGER NOT NULL DEFAULT 0,error TEXT);
    CREATE INDEX IF NOT EXISTS supervision_due ON supervision(closed,checked);")?;
    Ok(())
}
pub fn contract(job: &str, version: i64, mid: &str) -> String {
    format!(
        "\nStructured handoff protocol boxen.work-report.v1: explicit Boxen mail to this assignment's authenticated reply_to with in_reply_to={mid} and a JSON object body records the outcome. Example: {}. outcome is completed, blocked, needs_clarification or failed. summary, evidence and remaining describe the result and what is needed next. cleanup.no_owned_runtime_resources is the worker's explicit attestation, not independent cleanup proof. Progress can remain ordinary mail. A report is attributed to the assigned resident/version; private final narration is not a report. Completion applies to this assignment, not automatic work-item closure or publication.\n",
        json!({"protocol":"boxen.work-report.v1","job_id":job,"assignment_version":version,"outcome":"completed","summary":"Result and scope","evidence":["retained evidence reference"],"remaining":[],"cleanup":{"no_owned_runtime_resources":false}})
    )
}
fn enqueue(
    c: &Connection,
    cfg: &Config,
    job: &str,
    version: i64,
    purpose: &str,
    delivery: (&Value, String, Option<&str>),
) -> Result<()> {
    let (to, body, reply) = delivery;
    let mid = hash(&format!("supervision:{job}:{version}:{purpose}"));
    let mut message = json!({"id":mid,"to":to,"subject":format!("Assignment supervision: {purpose}"),"body":body});
    if let Some(reply) = reply {
        message["in_reply_to"] = json!(reply);
    }
    let request = json!({"op":"mailbox_send","client_id":cfg.client_id,"message":message});
    c.execute("INSERT OR IGNORE INTO supervision_outbox(id,job,version,purpose,request) VALUES(?1,?2,?3,?4,?5)",params![mid,job,version,purpose,request.to_string()])?;
    Ok(())
}
fn notify(
    c: &Connection,
    cfg: &Config,
    job: &str,
    version: i64,
    settings: &Settings,
    kind: &str,
    detail: Value,
) -> Result<()> {
    let body=json!({"protocol":"boxen.work-notice.v1","job_id":job,"assignment_version":version,"condition":kind,"detail":detail,"meaning":"Workflow observation or attributed report, not work-item closure. No acknowledgment gates unrelated work."}).to_string();
    enqueue(
        c,
        cfg,
        job,
        version,
        kind,
        (&settings.notify_to, body, None),
    )?;
    Ok(())
}
/// Advances the source cursor only with the receipt and any derived notification.
fn intake(s: &Store, cfg: &Config, api: &impl Boxen) -> Result<()> {
    for _ in 0..4 {
        let mut c = s.connect()?;
        let after: i64 = c
            .query_row(
                "SELECT COALESCE((SELECT v FROM meta WHERE k='supervision:mail_cursor'),'0')",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()?;
        let page =
            api.call(json!({"op":"client_inbox","client_id":cfg.client_id,"after":after}))?;
        let messages = page["messages"]
            .as_array()
            .context("Report inbox unavailable")?;
        if messages.is_empty() {
            break;
        }
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for m in messages {
            let Some(body) = m["body"].as_str() else {
                continue;
            };
            let report = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
            if report["protocol"] != "boxen.work-report.v1" {
                // Ordinary attributed progress stays correspondence, never a decision/report.
                // Forwarding it prevents access questions disappearing into a service inbox.
                if let (Some(mid), Some(reply), Some(coder)) = (
                    m["id"].as_str(),
                    m["in_reply_to"].as_str(),
                    m["sender"]["id"].as_str(),
                ) {
                    let watch: Option<(String, i64, String)> = tx.query_row(
                        "SELECT s.job,s.version,s.settings FROM supervision s JOIN work w ON w.id=s.job AND w.version=s.version AND w.coder=s.coder WHERE s.coder=?1 AND (s.message=?2 OR s.followup=?2)",
                        params![coder, reply], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    ).optional()?;
                    if let Some((job, version, settings)) = watch
                        && m["sender"] == json!({"type":"resident","id":coder})
                        && m["owner"] == cfg.owner
                        && m["to"]
                            == json!({"type":"client","machine":cfg.owner,"id":cfg.client_id})
                    {
                        let settings: Settings = serde_json::from_str(&settings)?;
                        notify(
                            &tx,
                            cfg,
                            &job,
                            version,
                            &settings,
                            &format!("progress:{mid}"),
                            json!({"message_id":mid,"sender":m["sender"],"in_reply_to":reply,"subject":m["subject"],"body":body,"meaning":"Attributed worker correspondence; no completion, approval or operator action is inferred"}),
                        )?;
                    }
                }
                continue;
            }
            let mid = m["id"].as_str().context("Report message ID missing")?;
            let seen: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM report_receipts WHERE message=?1)",
                [mid],
                |r| r.get(0),
            )?;
            if seen {
                continue;
            }
            let job = report["job_id"].as_str().unwrap_or("");
            let version = report["assignment_version"].as_i64().unwrap_or(0);
            let watch:Option<IntakeRow>=tx.query_row("SELECT coder,message,followup,settings,report FROM supervision WHERE job=?1 AND version=?2",params![job,version],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let mut reason = "Unknown supervised assignment";
            let mut accepted = false;
            if let Some((coder, assignment, followup, settings, old)) = watch {
                let settings: Settings = serde_json::from_str(&settings)?;
                let current: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM work WHERE id=?1 AND version=?2 AND coder=?3)",
                    params![job, version, coder],
                    |r| r.get(0),
                )?;
                let authored = m["sender"] == json!({"type":"resident","id":coder})
                    && m["owner"] == cfg.owner
                    && m["to"] == json!({"type":"client","machine":cfg.owner,"id":cfg.client_id});
                let linked = m["in_reply_to"] == assignment
                    || followup.as_ref().is_some_and(|f| m["in_reply_to"] == *f);
                let valid = matches!(
                    report["outcome"].as_str(),
                    Some("completed" | "blocked" | "needs_clarification" | "failed")
                ) && report["summary"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 8192)
                    && report["evidence"].as_array().is_some_and(|a| {
                        a.len() <= 64
                            && a.iter()
                                .all(|v| v.as_str().is_some_and(|s| s.len() <= 2048))
                    })
                    && report["remaining"].as_array().is_some_and(|a| {
                        a.len() <= 64
                            && a.iter()
                                .all(|v| v.as_str().is_some_and(|s| s.len() <= 2048))
                    })
                    && report["cleanup"]["no_owned_runtime_resources"].is_boolean();
                reason = if !authored {
                    "Sender or destination does not match assignment"
                } else if !current {
                    "Stale assignment version"
                } else if !linked {
                    "Report is not linked to its assignment"
                } else if !valid {
                    "Malformed report fields"
                } else if old.as_ref().is_some_and(|r| r != &report.to_string()) {
                    "Conflicting final report; prior report retained"
                } else {
                    "Accepted authenticated assignment report"
                };
                accepted = authored
                    && current
                    && linked
                    && valid
                    && old.as_ref().is_none_or(|r| r == &report.to_string());
                if accepted && old.is_none() {
                    tx.execute("UPDATE supervision SET report=?3,closed=0,phase='report_received' WHERE job=?1 AND version=?2",params![job,version,report.to_string()])?;
                    notify(
                        &tx,
                        cfg,
                        job,
                        version,
                        &settings,
                        "report_received",
                        json!({"report":report,"sender":m["sender"],"message_id":mid,"execution_cleanup":"Pending independent run/reservation checks"}),
                    )?;
                } else if authored && !accepted {
                    notify(
                        &tx,
                        cfg,
                        job,
                        version,
                        &settings,
                        "report_rejected",
                        json!({"reason":reason,"message_id":mid}),
                    )?;
                }
            }
            tx.execute(
                "INSERT INTO report_receipts VALUES(?1,?2,?3,?4,?5)",
                params![mid, job, version, accepted, reason],
            )?;
            Store::log(
                &tx,
                job,
                if accepted {
                    "report.accepted"
                } else {
                    "report.rejected"
                },
                json!({"version":version,"message_id":mid,"reason":reason}),
            )?;
        }
        let next = page["next_after"]
            .as_i64()
            .context("Report inbox cursor missing")?;
        ensure!(next > after, "Report inbox cursor did not advance");
        tx.execute("INSERT INTO meta VALUES('supervision:mail_cursor',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",[next.to_string()])?;
        tx.commit()?;
    }
    Ok(())
}
type IntakeRow = (String, String, Option<String>, String, Option<String>);
type WatchRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
);
fn find_mail(api: &impl Boxen, coder: &str, mid: &str) -> Result<Option<Value>> {
    let mut after = 0;
    for _ in 0..32 {
        let page = api.call(json!({"op":"resident_inbox","id":coder,"after":after}))?;
        let messages = page["messages"]
            .as_array()
            .context("Assignment inbox unavailable")?;
        if let Some(m) = messages.iter().find(|m| m["id"] == mid) {
            return Ok(Some(m.clone()));
        }
        if messages.is_empty() {
            return Ok(None);
        }
        let next = page["next_after"]
            .as_u64()
            .context("Assignment inbox cursor missing")?;
        ensure!(next > after, "Assignment inbox cursor did not advance");
        after = next;
    }
    anyhow::bail!("Assignment inbox exceeds bounded scan; outcome remains unresolved")
}
fn watch(
    s: &Store,
    cfg: &Config,
    api: &impl Boxen,
    job: &str,
    version: i64,
    at: i64,
) -> Result<()> {
    let mut c = s.connect()?;
    let (coder,mid,settings,mut run,seq,mut activity,mut start,mut terminal,followup,report):WatchRow=c.query_row("SELECT coder,message,settings,run,last_seq,activity,start_at,terminal_at,followup,report FROM supervision WHERE job=?1 AND version=?2",params![job,version],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    let settings: Settings = serde_json::from_str(&settings)?;
    let (current,state,packet):(i64,String,String)=c.query_row("SELECT w.version,w.state,a.packet FROM work w JOIN assignments a ON a.job=w.id AND a.version=?2 WHERE w.id=?1",params![job,version],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if current != version || matches!(state.as_str(), "paused" | "finished") {
        c.execute(
            "UPDATE supervision SET closed=1,phase='operator_closed' WHERE job=?1 AND version=?2",
            params![job, version],
        )?;
        return Ok(());
    }
    if state == "queued" {
        return Ok(());
    }
    if state == "needs_refresh" {
        notify(
            &c,
            cfg,
            job,
            version,
            &settings,
            "assignment_needs_refresh",
            json!({"meaning":"Mirrored source changed or became stale. Current assignment is retained; a new operator/context decision is needed. No automatic implementation retry."}),
        )?;
    }
    if start == 0 {
        start = at;
        activity = at;
    }
    let mail = find_mail(api, &coder, followup.as_deref().unwrap_or(&mid))?;
    if let Some(id) = mail.as_ref().and_then(|m| m["presented_in"].as_str()) {
        run = Some(id.to_owned());
    }
    let snapshot = if let Some(id) = &run {
        Some(api.call(json!({"op":"get","id":id}))?)
    } else {
        None
    };
    if let Some(r) = &snapshot {
        ensure!(
            r["record"]["spec"]["thread_id"] == coder && r["record"]["owner"] == cfg.owner,
            "Observed run owner/coder mismatch"
        );
    }
    let new_seq = snapshot
        .as_ref()
        .and_then(|r| r["last_seq"].as_i64())
        .unwrap_or(0);
    if new_seq != seq {
        activity = at;
    }
    let status = snapshot
        .as_ref()
        .and_then(|r| r["status"].as_str())
        .unwrap_or("not_started");
    let idle = matches!(status, "completed" | "failed" | "interrupted" | "cancelled");
    if idle && terminal == 0 {
        terminal = at;
    }
    if !idle {
        terminal = 0;
    }
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE supervision SET run=?3,last_seq=?4,activity=?5,start_at=?6,terminal_at=?7,phase=?8 WHERE job=?1 AND version=?2",params![job,version,run,new_seq,activity,start,terminal,if report.is_some(){"report_received"}else{status}])?;
    if report.is_none() {
        if status == "not_started" && at - start >= settings.start_timeout_seconds * 1000 {
            notify(
                &tx,
                cfg,
                job,
                version,
                &settings,
                "start_overdue",
                json!({"state":state,"meaning":"No assignment activation observed; no automatic task retry or replacement"}),
            )?;
        } else if !idle
            && status != "not_started"
            && at - activity >= settings.inactivity_seconds * 1000
        {
            notify(
                &tx,
                cfg,
                job,
                version,
                &settings,
                "activity_overdue",
                json!({"run":run,"status":status,"meaning":"No new retained run activity observed within configured interval; not proof of failure"}),
            )?;
        } else if idle && at - terminal >= settings.report_grace_seconds * 1000 {
            if status == "completed" && followup.is_none() {
                let fid = hash(&format!("supervision:{job}:{version}:report_request"));
                enqueue(
                    &tx,
                    cfg,
                    job,
                    version,
                    "report_request",
                    (
                        &json!({"type":"resident","id":coder}),
                        format!(
                            "The observed assignment run completed without a structured handoff. A report of the existing outcome closes that communication gap. This follow-up requests no implementation, test rerun, replay of previous tool effects or new work. Original assignment {mid}. {}",
                            contract(job, version, &mid)
                        ),
                        Some(&mid),
                    ),
                )?;
                tx.execute("UPDATE supervision SET followup=?3,run=NULL,last_seq=0,terminal_at=0,start_at=?4,activity=?4 WHERE job=?1 AND version=?2",params![job,version,fid,at])?;
                notify(
                    &tx,
                    cfg,
                    job,
                    version,
                    &settings,
                    "missing_report",
                    json!({"run":run,"status":status,"recovery":"One durable report-only follow-up; no repeated task effects"}),
                )?;
            } else {
                notify(
                    &tx,
                    cfg,
                    job,
                    version,
                    &settings,
                    "unresolved_outcome",
                    json!({"run":run,"status":status,"meaning":"No structured handoff; reservation retained. No automatic implementation retry."}),
                )?;
                tx.execute("UPDATE supervision SET closed=1,phase='unresolved' WHERE job=?1 AND version=?2",params![job,version])?;
            }
        }
    }
    tx.commit()?;
    if let Some(raw) = report {
        let report: Value = serde_json::from_str(&raw)?;
        let packet: Value = serde_json::from_str(&packet)?;
        if idle
            && packet["assignment"]["runtime"]["required"] == false
            && report["cleanup"]["no_owned_runtime_resources"] == true
        {
            if report["outcome"] == "completed" && status != "completed" {
                notify(
                    &s.connect()?,
                    cfg,
                    job,
                    version,
                    &settings,
                    "report_run_conflict",
                    json!({"reported_outcome":"completed","run_status":status,"meaning":"Completion report retained as attributed input; failed/interrupted execution is not certified successful"}),
                )?;
            }
            let outcome = if report["outcome"] == "completed"
                && state != "needs_refresh"
                && status == "completed"
            {
                "finished"
            } else {
                "paused"
            };
            match release_with_basis(
                s,
                api,
                job,
                outcome,
                true,
                "Authenticated structured report; no runtime allocation; all coder runs idle and accepted assignment presented",
                ("worker_report_and_run_readback", Some(version)),
            ) {
                Ok(_) => {
                    let mut c = s.connect()?;
                    let tx = c.transaction()?;
                    tx.execute("UPDATE supervision SET closed=1,phase='released' WHERE job=?1 AND version=?2",params![job,version])?;
                    notify(
                        &tx,
                        cfg,
                        job,
                        version,
                        &settings,
                        "allocation_released",
                        json!({"outcome":outcome,"report_outcome":report["outcome"],"work_item_closed":false,"cleanup_basis":"Worker attestation plus no runtime allocation and authoritative run/mail readback; source checkout/evidence retained"}),
                    )?;
                    tx.commit()?;
                }
                Err(e) => {
                    notify(
                        &s.connect()?,
                        cfg,
                        job,
                        version,
                        &settings,
                        "cleanup_pending",
                        json!({"reason":e.to_string()}),
                    )?;
                }
            }
        } else if idle {
            notify(
                &s.connect()?,
                cfg,
                job,
                version,
                &settings,
                "cleanup_pending",
                json!({"meaning":"Owned runtime resources are not verified absent; reservation retained for operator cleanup"}),
            )?;
        }
    }
    Ok(())
}
fn flush(s: &Store, cfg: &Config, api: &impl Boxen, at: i64) -> Result<()> {
    let c = s.connect()?;
    let rows=c.prepare("SELECT id,request FROM supervision_outbox WHERE accepted=0 AND next_due<=?1 ORDER BY rowid LIMIT 16")?.query_map([at],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let budget = std::time::Instant::now();
    for (id, raw) in rows {
        if budget.elapsed().as_secs() >= 2 {
            break;
        }
        let request: Value = serde_json::from_str(&raw)?;
        let result = api.call(request).and_then(|r| {
            ensure!(
                r["id"] == id
                    && r["sender"]
                        == json!({"type":"client","machine":cfg.owner,"id":cfg.client_id}),
                "Notification acceptance identity mismatch"
            );
            Ok(())
        });
        match result {
            Ok(()) => {
                c.execute("UPDATE supervision_outbox SET accepted=1,error=NULL,attempts=attempts+1 WHERE id=?1",[&id])?;
                event("supervision.mail_accepted", json!({"message_id":id}));
            }
            Err(e) => {
                c.execute("UPDATE supervision_outbox SET attempts=attempts+1,next_due=?2,error=?3 WHERE id=?1",params![id,at+30_000,e.to_string()])?;
            }
        }
    }
    Ok(())
}
pub fn tick(s: &Store, cfg: &Config, api: &impl Boxen, at: i64) -> Result<()> {
    intake(s, cfg, api)?;
    let c = s.connect()?;
    let rows = c
        .prepare(
            "SELECT job,version FROM supervision WHERE closed=0 ORDER BY checked,rowid LIMIT 32",
        )?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let budget = std::time::Instant::now();
    for (job, version) in rows {
        if budget.elapsed().as_secs() >= 2 {
            break;
        }
        c.execute(
            "UPDATE supervision SET checked=?3 WHERE job=?1 AND version=?2",
            params![job, version, at],
        )?;
        if let Err(e) = watch(s, cfg, api, &job, version, at) {
            let settings: String = c.query_row(
                "SELECT settings FROM supervision WHERE job=?1 AND version=?2",
                params![job, version],
                |r| r.get(0),
            )?;
            notify(
                &c,
                cfg,
                &job,
                version,
                &serde_json::from_str(&settings)?,
                "observation_unavailable",
                json!({"reason":e.to_string(),"meaning":"Outcome unknown; no implementation retry"}),
            )?;
        }
    }
    flush(s, cfg, api, at)
}
pub fn snapshot(s: &Store) -> Result<Value> {
    let c = s.connect()?;
    let watches=c.prepare("SELECT job,version,phase,run,report,closed FROM supervision ORDER BY rowid DESC LIMIT 200")?.query_map([],|r|{let report:Option<String>=r.get(4)?;Ok(json!({"job_id":r.get::<_,String>(0)?,"version":r.get::<_,i64>(1)?,"phase":r.get::<_,String>(2)?,"run":r.get::<_,Option<String>>(3)?,"report":report.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"closed":r.get::<_,bool>(5)?}))})?.collect::<rusqlite::Result<Vec<_>>>()?;
    let pending: i64 = c.query_row(
        "SELECT count(*) FROM supervision_outbox WHERE accepted=0",
        [],
        |r| r.get(0),
    )?;
    let health: Option<String> = c
        .query_row("SELECT v FROM meta WHERE k='supervision:health'", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(
        json!({"assignments":watches,"pending_notifications":pending,"health":health.and_then(|v|serde_json::from_str::<Value>(&v).ok())}),
    )
}

pub fn delivery_test(s: &Store, cfg: &Config, request_key: &str) -> Result<String> {
    ensure!(
        !request_key.is_empty() && request_key.len() <= 200,
        "Delivery-test key contains 1–200 bytes"
    );
    let settings = cfg
        .supervision
        .as_ref()
        .context("Notification supervision is not configured")?;
    let job = hash(&format!("delivery-test:{request_key}"));
    notify(
        &s.connect()?,
        cfg,
        &job,
        0,
        settings,
        "delivery_test",
        json!({"meaning":"Explicit operator notification-route diagnostic. No coder work, incident or work-item change is represented."}),
    )?;
    Ok(hash(&format!("supervision:{job}:0:delivery_test")))
}

/// Retained service health is separate from assignment outcomes and cannot
/// claim successful observation while the backend is unavailable.
pub fn health(s: &Store, cfg: &Config, error: Option<&str>) -> Result<()> {
    let mut c = s.connect()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old: Option<String> = tx
        .query_row("SELECT v FROM meta WHERE k='supervision:health'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let old: Value = old
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or(json!({"available":true,"episode":0,"failures":0}));
    let available = error.is_none();
    let episode =
        old["episode"].as_i64().unwrap_or(0) + i64::from(!available && old["available"] == true);
    let record = json!({"available":available,"episode":episode,"failures":old["failures"].as_i64().unwrap_or(0)+i64::from(!available),"last_observed":now(),"error":error.map(|e|e.chars().take(512).collect::<String>())});
    if old["available"] != available {
        if let Some(settings) = &cfg.supervision {
            notify(
                &tx,
                cfg,
                &hash(&format!("supervision-health:{episode}")),
                0,
                settings,
                if available {
                    "service_recovered"
                } else {
                    "service_unavailable"
                },
                record.clone(),
            )?;
        }
        Store::log(&tx, "", "supervision.health_changed", record.clone())?;
    }
    tx.execute("INSERT INTO meta VALUES('supervision:health',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",[record.to_string()])?;
    tx.commit()?;
    Ok(())
}
