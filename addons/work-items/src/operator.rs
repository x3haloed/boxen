//! Local operator API. Workflow records remain the execution authority; feedback
//! retains original operator text and its exact observed basis before dispatch.
use crate::{transport::Boxen, *};

#[derive(Debug)]
pub(crate) struct ReconciliationRequired(pub &'static str);
impl std::fmt::Display for ReconciliationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ReconciliationRequired {}

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS operator_feedback(id TEXT PRIMARY KEY,input TEXT NOT NULL,item INTEGER NOT NULL,attempt TEXT NOT NULL,basis TEXT NOT NULL,created INTEGER NOT NULL,state TEXT NOT NULL,receipt TEXT NOT NULL,error TEXT,next_due INTEGER NOT NULL DEFAULT 0);")?;
    let has_due = c
        .prepare("PRAGMA table_info(operator_feedback)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|n| n == "next_due");
    if !has_due {
        c.execute_batch(
            "ALTER TABLE operator_feedback ADD COLUMN next_due INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

fn latest(s: &Store, item: i64) -> Result<Option<Value>> {
    let source = ado::config(s)?.source();
    let row: Option<(String, String, String, String, Option<String>)> = s.connect()?.query_row(
        "SELECT id,digest,stage,data,error FROM workflow_attempts WHERE source=?1 AND item=?2 ORDER BY created DESC,rowid DESC LIMIT 1",
        params![source,item], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let value=row.map(|(id,digest,stage,data,error)| -> Result<Value> {
        Ok(json!({"id":id,"source_digest":digest,"stage":stage,"data":serde_json::from_str::<Value>(&data)?,"error":error}))
    }).transpose()?;
    if value.is_some() {
        return Ok(value);
    }
    let manual:Option<(String,String,i64,String,String,Option<String>)> = s.connect()?.query_row("SELECT w.id,w.coder,w.version,w.state,a.digest,u.report FROM work w JOIN assignments a ON a.job=w.id AND a.version=w.version LEFT JOIN supervision u ON u.job=w.id AND u.version=w.version WHERE w.source=?1 AND w.item=?2",params![ado::config(s)?.source(),item],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    if let Some((job, coder, version, state, digest, report)) = manual {
        return Ok(Some(
            json!({"id":format!("manual:{job}"),"source_digest":digest,"stage":format!("manual_{state}"),"error":null,"data":{"job_id":job,"coder_id":coder,"assignment_version":version,"candidate":report.map(|v|serde_json::from_str::<Value>(&v)).transpose()?}}),
        ));
    }
    let packet = s.packet(item)?;
    Ok(Some(
        json!({"id":format!("observed:{item}"),"source_digest":packet["source_digest"],"stage":"observed","data":{},"error":null}),
    ))
}

fn summary(s: &Store, item: i64, title: String, observed: i64) -> Result<Value> {
    let a = latest(s, item)?.unwrap_or(Value::Null);
    let d = &a["data"];
    let delivery = delivery(s, item)?;
    let candidate = if d["candidate"].is_object() {
        &d["candidate"]
    } else {
        &delivery["candidate"]
    };
    let pr = if d["publication_receipt"].is_object() {
        &d["publication_receipt"]
    } else {
        &delivery["publication_receipt"]
    };
    let stage = a["stage"].as_str().unwrap_or("observed");
    let feedback_pending: bool = s.connect()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM operator_feedback WHERE item=?1 AND state IN('queued','return_accepted'))",[item],|r| r.get(0))?;
    let label = if feedback_pending {
        "Feedback pending"
    } else if !a["error"].is_null() {
        "Needs attention"
    } else {
        match stage {
            "published" => "PR delivered",
            "held" | "pausing" => "Blocked",
            "no_action" if pr.is_object() => "PR delivered · no further action",
            "no_action" | "superseded" => "No current action",
            "request_reviews" | "reviewing" => "In review",
            "ready" | "awaiting_allocation" => "Waiting for capacity",
            "coding" => "With coder",
            "manual_finished" => "Local handoff completed",
            "manual_paused" | "manual_needs_refresh" => "Paused assignment",
            "manual_queued" | "manual_reserved" => "Waiting for capacity",
            "manual_delivered" => "Assignment delivered",
            "detected" | "interpreting" => "Interpreting",
            "observed" => "Observed",
            _ => "Preparing publication",
        }
    };
    let mut reason = if !a["error"].is_null() {
        a["error"].clone()
    } else {
        d["pause_reason"].clone()
    };
    if reason.is_null() && stage == "held" {
        let interpretation = &d["interpretation"];
        let reasons = interpretation["holds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["reason"].as_str())
            .chain(
                interpretation["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["question"].as_str()),
            )
            .collect::<Vec<_>>();
        if !reasons.is_empty() {
            reason = json!(reasons.join("\n\n"));
        }
    }
    if let Some(raw) = reason.as_str()
        && let Some(start) = raw.find('{')
        && let Ok(report) = serde_json::from_str::<Value>(&raw[start..])
        && let Some(summary) = report["summary"].as_str()
    {
        let remaining = report["remaining"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        reason = json!(format!("{summary}\n{remaining}"));
    }
    Ok(
        json!({"item":item,"title":title,"status":label,"stage":stage,"reason":reason,
        "observed_at":observed,"observation_stale":now()-observed>=30*60_000,
        "attempt_id":a["id"],"source_digest":a["source_digest"],
        "assignment_version":d["assignment_version"],"coder_id":d["coder_id"],
        "commit":candidate["commit"],"pr":pr,
        "can_return":matches!(stage,"published"|"held"|"no_action") && !feedback_pending,
        "basis":hash(&a.to_string())}),
    )
}

fn delivery(s: &Store, item: i64) -> Result<Value> {
    let raw:Option<String> = s.connect()?.query_row("SELECT data FROM workflow_attempts WHERE source=?1 AND item=?2 AND stage='published' ORDER BY created DESC,rowid DESC LIMIT 1",params![ado::config(s)?.source(),item],|r|r.get(0)).optional()?;
    raw.map(|v| serde_json::from_str(&v).map_err(Into::into))
        .transpose()
        .map(|v| v.unwrap_or(Value::Null))
}

pub fn list(s: &Store) -> Result<Value> {
    let source = ado::config(s)?.source();
    let c = s.connect()?;
    let rows = c
        .prepare(
            "SELECT id,title,observed FROM ado_items WHERE source=?1 ORDER BY id DESC LIMIT 201",
        )?
        .query_map([source], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let items = rows
        .iter()
        .take(200)
        .map(|(id, title, at)| summary(s, *id, title.clone(), *at))
        .collect::<Result<Vec<_>>>()?;
    let health: Value = c.query_row("SELECT last_success,error FROM ado_sync LIMIT 1",[],|r| Ok(json!({"last_success":r.get::<_,Option<i64>>(0)?,"error":r.get::<_,Option<String>>(1)?}))).optional()?.unwrap_or(Value::Null);
    Ok(
        json!({"protocol":"boxen.operator.v1","observed_at":now(),"items":items,"more":rows.len()>200,"mirror":health,
        "meaning":"Status is retained host-local evidence, not a fresh remote PR or deployment check."}),
    )
}

pub fn detail(s: &Store, item: i64) -> Result<Value> {
    let source = ado::config(s)?.source();
    let c = s.connect()?;
    let (title, observed): (String, i64) = c.query_row(
        "SELECT title,observed FROM ado_items WHERE source=?1 AND id=?2",
        params![source, item],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let a = latest(s, item)?.unwrap_or(Value::Null);
    let history = c.prepare("SELECT id,input,created,state,receipt,error FROM operator_feedback WHERE item=?1 ORDER BY created DESC,rowid DESC LIMIT 50")?
        .query_map([item],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?)))?
        .map(|row| {let(id,input,at,state,receipt,error)=row?;Ok(json!({"id":id,"input":serde_json::from_str::<Value>(&input)?,"created_at":at,"state":state,"receipt":serde_json::from_str::<Value>(&receipt)?,"error":error}))}).collect::<Result<Vec<_>>>()?;
    let mut result = summary(s, item, title, observed)?;
    ensure!(
        result["basis"] == hash(&a.to_string()),
        "Work changed during status projection; a fresh read supplies a consistent view"
    );
    result["workflow"] = a["data"].clone();
    let delivery = delivery(s, item)?;
    for field in [
        "candidate",
        "context_review",
        "technical_review",
        "publication_receipt",
    ] {
        if result["workflow"][field].is_null() {
            result["workflow"][field] = delivery[field].clone();
        }
    }
    result["feedback"] = json!(history);
    Ok(result)
}

/// Submission is a local operator action, never an instruction parsed from mail.
/// The CLI holds workflow.lock across this operation and the desktop uses the same API.
pub fn submit(s: &Store, cfg: &Config, input: &Value) -> Result<Value> {
    let id = input["id"].as_str().context("Feedback ID required")?;
    key(id)?;
    let item = input["item"]
        .as_i64()
        .filter(|n| *n > 0)
        .context("Work item required")?;
    let kind = input["kind"].as_str().context("Feedback kind required")?;
    ensure!(
        matches!(kind, "question" | "correction"),
        "Feedback kind is question or correction"
    );
    let text = input["text"].as_str().context("Feedback text required")?;
    ensure!(
        !text.trim().is_empty() && text.len() <= 8192,
        "Feedback contains 1–8192 UTF-8 bytes"
    );
    let mut c = s.connect()?;
    if let Some((original, receipt)) = c
        .query_row(
            "SELECT input,receipt FROM operator_feedback WHERE id=?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        ensure!(
            serde_json::from_str::<Value>(&original)? == *input,
            "Feedback ID has different input; the original remains retained"
        );
        return Ok(serde_json::from_str(&receipt)?);
    }
    let settings = workflow::Settings::read(s)?.context("Project workflow is not configured")?;
    ensure!(
        settings.items.is_empty() || settings.items.contains(&item),
        "Work item is outside configured workflow scope"
    );
    let current = detail(s, item)?;
    ensure!(
        input["basis"] == current["basis"],
        "Work changed after this view; refreshing supplies the current feedback basis"
    );
    let mut a = latest(s, item)?.context("No workflow interpretation exists for this item")?;
    if kind == "correction" {
        ensure!(
            settings.enabled && current["can_return"] == true,
            "A correction return requires terminal workflow work with no pending feedback; questions remain available during execution"
        );
        let packet = s.packet(item)?;
        ensure!(
            packet["source_digest"] == a["source_digest"],
            "Source changed; fresh interpretation precedes a correction return"
        );
    }
    a["delivery"] = delivery(s, item)?;
    a["question_recipient"] = json!(settings.context_resident);
    a["subject_commit"] = current["commit"].clone();
    let receipt = json!({"id":id,"item":item,"kind":kind,"accepted_at":now(),"subject_commit":current["commit"],"state":"accepted",
        "meaning":"Feedback is durably saved. Acceptance does not establish a reply, correction or completed work."});
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: i64 = tx.query_row(
        "SELECT count(*) FROM operator_feedback WHERE state='queued'",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        pending < 256,
        "Pending feedback capacity is full; existing submissions remain retained"
    );
    tx.execute("INSERT INTO operator_feedback(id,input,item,attempt,basis,created,state,receipt) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7)",params![id,input.to_string(),item,a["id"].as_str().unwrap(),a.to_string(),now(),receipt.to_string()])?;
    Store::log(
        &tx,
        id,
        "operator.feedback_accepted",
        json!({"item":item,"kind":kind,"operator_machine":cfg.owner,"attempt":a["id"],"subject_commit":current["commit"]}),
    )?;
    tx.commit()?;
    event(
        "operator.feedback_accepted",
        json!({"id":id,"item":item,"kind":kind}),
    );
    Ok(receipt)
}

pub fn locked_submit(s: &Store, cfg: &Config, input: &Value) -> Result<Value> {
    use fs2::FileExt;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(s.path.parent().unwrap().join("workflow.lock"))?;
    lock.try_lock_exclusive()
        .context("Workflow is advancing; an explicit retry retains this feedback ID")?;
    submit(s, cfg, input)
}

pub fn replies(api: &impl Boxen, cfg: &Config, detail: &mut Value) -> Result<()> {
    let ids: Vec<&str> = detail["feedback"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["id"].as_str())
        .collect();
    let mut messages = Vec::new();
    let mut after = 0;
    let mut limited = false;
    for _ in 0..4 {
        let page = api.call(json!({"op":"client_inbox","client_id":cfg.owner,"after":after}))?;
        let batch = page["messages"]
            .as_array()
            .context("Operator inbox unavailable")?;
        if batch.is_empty() {
            limited = false;
            break;
        }
        messages.extend(
            batch
                .iter()
                .filter(|m| {
                    m["in_reply_to"]
                        .as_str()
                        .is_some_and(|id| ids.contains(&id))
                })
                .cloned(),
        );
        let next = page["next_after"]
            .as_i64()
            .context("Inbox cursor missing")?;
        ensure!(next > after, "Inbox cursor did not advance");
        after = next;
        limited = true;
    }
    detail["replies"] = json!(messages);
    detail["replies_limited"] = json!(limited);
    Ok(())
}

pub fn tick(
    s: &Store,
    cfg: &Config,
    api: &impl Boxen,
    settings: &workflow::Settings,
) -> Result<()> {
    let rows = s.connect()?.prepare("SELECT id,input,attempt,basis FROM operator_feedback WHERE state='queued' AND next_due<=?1 ORDER BY created,rowid LIMIT 8")?
        .query_map([now()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, raw, attempt, basis) in rows {
        let input: Value = serde_json::from_str(&raw)?;
        let result = if input["kind"] == "correction" {
            workflow::operator_return(s, cfg, api, settings, &id, &input, &basis)
        } else {
            let message = json!({"op":"mailbox_send","client_id":cfg.owner,"message":{"id":id,"to":{"type":"resident","id":serde_json::from_str::<Value>(&basis)?["question_recipient"]},"subject":format!("Work item {}: operator question",input["item"]),"body":json!({"protocol":"boxen.operator-question.v1","work_item_id":input["item"],"original_text":input["text"],"observed_basis":{"attempt":attempt,"commit":serde_json::from_str::<Value>(&basis)?["subject_commit"]},"meaning":"The original text is from a local operator submission. An explicit reply reaches the operator mailbox; private narration is not delivered. This question does not authorize new implementation or publication."}).to_string()}});
            api.call(message).map(|_| ())
        };
        let c = s.connect()?;
        match result {
            Ok(()) => {
                c.execute(
                    "UPDATE operator_feedback SET state=?2,error=NULL WHERE id=?1",
                    params![
                        id,
                        if input["kind"] == "correction" {
                            "return_accepted"
                        } else {
                            "mail_accepted"
                        }
                    ],
                )?;
            }
            Err(error) => {
                let state = if error.downcast_ref::<ReconciliationRequired>().is_some() {
                    "needs_reconciliation"
                } else {
                    "queued"
                };
                let detail = error.to_string();
                let old: Option<String> = c.query_row(
                    "SELECT error FROM operator_feedback WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )?;
                if old.as_ref() != Some(&detail) {
                    Store::log(
                        &c,
                        &id,
                        "operator.feedback_waiting",
                        json!({"item":input["item"],"attempt":attempt,"detail":detail}),
                    )?;
                }
                c.execute(
                    "UPDATE operator_feedback SET error=?2,next_due=?3,state=?4 WHERE id=?1",
                    params![id, detail, now() + 15_000, state],
                )?;
            }
        }
    }
    Ok(())
}
