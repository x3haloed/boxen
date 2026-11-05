//! Authenticated machine operators share network access. Runtime tools and connector
//! runners keep their existing identity-scoped entry points. Stored owners remain
//! continuity provenance rather than being rewritten during network admission.
use crate::{
    Result,
    jobs::{Jobs, Run},
    logging::{self, Context, Level, Target},
};
use serde_json::{Value, json};
use std::sync::Arc;
pub fn run(jobs: &Jobs, id: &str) -> Result<Arc<Run>> {
    jobs.operator_run(id)
}
fn namespace(jobs: &Jobs, actor: &str, request: &Value) -> Result<String> {
    let op = request["op"].as_str().unwrap_or("");
    let resident = match op {
        "resident_get" | "resident_update" | "resident_activate" | "resident_inbox" => {
            request["id"].as_str()
        }
        "transcript_import" => request["spec"]["resident_id"].as_str(),
        "resident_create" => request["spec"]["id"]
            .as_str()
            .filter(|id| jobs.residents.all().iter().any(|r| r.id == *id)),
        "resident_definition" if request["args"]["action"] == "apply" => {
            request["args"]["resident_id"].as_str()
        }
        _ => None,
    };
    if let Some(id) = resident {
        return jobs
            .residents
            .all()
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.owner.clone())
            .ok_or_else(|| "resident not found on this machine".into());
    }
    let run_id = match op {
        "get" | "control" | "thread_result" => request["id"].as_str(),
        "inference_resume" => request["failed_run_id"].as_str(),
        "thread_import" => request["spec"]["id"]
            .as_str()
            .filter(|id| jobs.operator_run(id).is_ok()),
        _ => None,
    };
    if let Some(id) = run_id {
        return Ok(run(jobs, id)?.owner());
    }
    let thread = match op {
        "context" => request["thread_id"].as_str(),
        "start" => request["spec"]["thread_id"].as_str(),
        "thread_activity" => request["id"].as_str(),
        "resident_promote" => request["spec"]["thread_id"].as_str(),
        _ => None,
    };
    if let Some(id) = thread
        && let Some(owner) = jobs.thread_owner(id)?
    {
        return Ok(owner);
    }
    if op == "schedule" {
        let args = &request["args"];
        if let Some(id) = args["id"].as_str().or_else(|| args["spec"]["id"].as_str()) {
            for owner in jobs.owners() {
                if jobs.schedules.get(&owner, id).is_ok() {
                    return Ok(owner);
                }
            }
        }
        if let Some(id) = args["spec"]["target"]["id"].as_str() {
            if let Some(r) = jobs.residents.all().into_iter().find(|r| r.id == id) {
                return Ok(r.owner);
            }
            if let Some(owner) = jobs.thread_owner(id)? {
                return Ok(owner);
            }
        }
    }
    if op == "events" {
        let args = &request["args"];
        if let Some(id) = args["id"].as_str() {
            for owner in jobs.owners() {
                if jobs.observations.source(&owner, id).is_ok()
                    || jobs.observations.subscription(&owner, id).is_ok()
                {
                    return Ok(owner);
                }
            }
        }
        if let Some(id) = args["spec"]["target"]["id"].as_str() {
            if let Some(r) = jobs.residents.all().into_iter().find(|r| r.id == id) {
                return Ok(r.owner);
            }
            if let Some(owner) = jobs.thread_owner(id)? {
                return Ok(owner);
            }
        }
    }
    if op == "resident_definition" {
        let args = &request["args"];
        if args["action"] == "instantiate"
            && let Some(id) = args["spec"]["id"].as_str()
            && let Some(r) = jobs.residents.all().into_iter().find(|r| r.id == id)
        {
            return Ok(r.owner);
        }
        let id = if args["action"] == "instantiate" {
            args["spec"]["definition_id"].as_str()
        } else if args["action"] == "create" {
            args["spec"]["id"].as_str()
        } else {
            args["id"].as_str()
        };
        if let Some(id) = id {
            for owner in jobs.owners() {
                if jobs.definitions.get(&owner, id, None).is_ok() {
                    return Ok(owner);
                }
            }
        }
    }
    if op == "client_inbox"
        && let Some(machine) = request["machine"].as_str()
    {
        crate::state::key(machine)?;
        return Ok(machine.into());
    }
    if op == "messaging" {
        let args = &request["args"];
        if let Some(id) = args["resident"].as_str()
            && let Some(r) = jobs.residents.all().into_iter().find(|r| r.id == id)
        {
            return Ok(r.owner);
        }
        if let Some(id) = args["to"]["instance"].as_str() {
            for owner in jobs.owners() {
                if jobs.connectors.get(&owner, id).is_ok() {
                    return Ok(owner);
                }
            }
        }
    }
    if op == "messaging"
        && request["args"]["action"] == "status"
        && let Some(id) = request["args"]["id"].as_str()
    {
        for owner in jobs.owners() {
            if jobs.mailboxes.get(&owner, id).is_ok() {
                return Ok(owner);
            }
        }
    }
    if op == "messaging"
        && let Some(id) = request["args"]["instance"].as_str()
    {
        for owner in jobs.owners() {
            if jobs.connectors.get(&owner, id).is_ok() {
                return Ok(owner);
            }
        }
    }
    Ok(actor.into())
}
pub fn operation(jobs: &Arc<Jobs>, actor: &str, request: &Value) -> Result<Value> {
    let op = request["op"].as_str().unwrap_or("");
    let result = (|| -> Result<Value> {
        match op {
            "resident_list" => {
                return Ok(json!(
                    jobs.residents
                        .all()
                        .iter()
                        .map(|r| r.value())
                        .collect::<Vec<_>>()
                ));
            }
            "list" => return Ok(json!(jobs.operator_list())),
            "resident_definition"
                if request["args"]["action"].as_str().unwrap_or("list") == "list" =>
            {
                let mut all = Vec::new();
                let mut issues = Vec::new();
                for owner in jobs.owners() {
                    let v = jobs.definitions.list(&owner);
                    all.extend(v["definitions"].as_array().cloned().unwrap_or_default());
                    issues = v["issues"].as_array().cloned().unwrap_or_default();
                }
                return Ok(json!({"definitions":all,"issues":issues}));
            }
            "inference_health" => {
                let mut rows = Vec::new();
                for owner in jobs.owners() {
                    let mut offset = 0;
                    loop {
                        let page = jobs.inference_health(&owner, offset);
                        rows.extend(page["incidents"].as_array().cloned().unwrap_or_default());
                        if let Some(next) = page["next_offset"].as_u64() {
                            offset = next as usize;
                        } else {
                            break;
                        }
                    }
                }
                rows.sort_by_key(|v| {
                    (
                        v["active"] != true,
                        std::cmp::Reverse(v["last_at"].as_u64().unwrap_or(0)),
                    )
                });
                let total = rows.len();
                let offset = request["offset"].as_u64().unwrap_or(0) as usize;
                let page: Vec<_> = rows.into_iter().skip(offset).take(50).collect();
                let next = offset.saturating_add(page.len());
                return Ok(
                    json!({"incidents":page,"total":total,"next_offset":if next<total{Some(next)}else{None}}),
                );
            }
            "schedule" if request["args"]["action"].as_str().unwrap_or("list") == "list" => {
                let args = &request["args"];
                let mut rows = Vec::new();
                let mut source_more = false;
                for owner in jobs.owners() {
                    let page = jobs.schedules.page(&owner, None, args);
                    source_more |= !page["next_after_id"].is_null();
                    rows.extend(page["jobs"].as_array().cloned().unwrap_or_default());
                }
                rows.sort_by_key(|v| v["definition"]["id"].as_str().unwrap_or("").to_owned());
                let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
                let more = rows.len() > limit || source_more;
                rows.truncate(limit);
                let next = if more {
                    rows.last().map(|v| v["definition"]["id"].clone())
                } else {
                    None
                };
                return Ok(
                    json!({"jobs":rows,"next_after_id":next,"issues":jobs.schedules.issues()}),
                );
            }
            "events" if request["args"]["action"].as_str().unwrap_or("list") == "list" => {
                let mut sources = Vec::new();
                let mut subs = Vec::new();
                let mut unavailable = Value::Null;
                for owner in jobs.owners() {
                    let p = jobs.observations.list(&owner);
                    sources.extend(p["sources"].as_array().cloned().unwrap_or_default());
                    subs.extend(p["subscriptions"].as_array().cloned().unwrap_or_default());
                    unavailable = p["unavailable"].clone();
                }
                return Ok(
                    json!({"sources":sources,"subscriptions":subs,"unavailable":unavailable}),
                );
            }
            "messaging" if request["args"]["action"].as_str().unwrap_or("list") == "list" => {
                let mut instances = vec![
                    json!({"id":"boxen","provider":"boxen","capabilities":["receive","send","replies","subjects"],"account":actor}),
                ];
                let mut issues = Vec::new();
                for owner in jobs.owners() {
                    let page = crate::messaging::operation(
                        jobs,
                        &owner,
                        None,
                        json!({"action":"list"}),
                        String::new(),
                    )?;
                    instances.extend(
                        page["instances"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|v| v["id"] != "boxen")
                            .cloned(),
                    );
                    issues.extend(page["issues"].as_array().cloned().unwrap_or_default());
                }
                issues.sort_by_key(Value::to_string);
                issues.dedup();
                return Ok(json!({"instances":instances,"issues":issues}));
            }
            "mailbox_send" => {
                let mut message = crate::protocol::operation(jobs, actor, request)?;
                if let Some(id) = message["id"].as_str().map(str::to_owned) {
                    message["delivery"] = jobs
                        .mailboxes
                        .receipt(&id)
                        .unwrap_or(json!({"status":"accepted","accepted_on":jobs.machine_id()}));
                }
                return Ok(message);
            }
            _ => {}
        }
        let owner = namespace(jobs, actor, request)?;
        crate::protocol::operation(jobs, &owner, request)
    })();
    logging::record(
        if result.is_ok() {
            Level::Debug
        } else {
            Level::Warn
        },
        Target::Daemon,
        "operator.operation",
        Context::peer(actor),
        || json!({"op":op,"accepted":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string())}),
        || Value::Null,
    );
    result
}
