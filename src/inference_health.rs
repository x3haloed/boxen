//! Coalesced projections of existing workload journals; no separate incident store.
use crate::{
    Result,
    jobs::{self, Jobs, Run, Snapshot, Spec},
    logging::{self, Context, Level, Target},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) fn project(runs: &[Arc<Run>]) -> Vec<Value> {
    let mut ordered = runs.to_vec();
    ordered.sort_by_key(|r| {
        let s = r.snapshot();
        (s.record.created_at, s.record.spec.id)
    });
    let mut rows: BTreeMap<(String, String, String), Value> = BTreeMap::new();
    for run in ordered {
        let inner = run.inner.lock().unwrap();
        let s = &inner.snapshot;
        if s.record.spec.backend != "openrouter" || s.record.transcript_import.is_some() {
            continue;
        }
        let mut headers: BTreeMap<(u64, String), Option<u64>> = BTreeMap::new();
        for e in &inner.events {
            let purpose = e.data["purpose"].as_str().unwrap_or("workload").to_owned();
            if e.kind == "inference_exchange_completed" {
                for ((thread, p, _), v) in rows.iter_mut() {
                    if thread == &s.record.spec.thread_id && p == &purpose && v["active"] == true {
                        v["active"] = json!(false);
                        v["resolved_at"] = json!(e.at);
                        v["resolved_run_id"] = json!(s.record.spec.id);
                    }
                }
                continue;
            }
            if e.kind != "inference_frame" {
                continue;
            }
            let frame = &e.data["event"];
            let key = (e.data["step"].as_u64().unwrap_or(0), purpose.clone());
            if frame["type"] == "response_headers" {
                headers.insert(key, frame["status"].as_u64());
                continue;
            }
            if frame["type"] != "exchange_failed" {
                continue;
            }
            let status = headers.get(&key).copied().flatten();
            let category = match status {
                Some(402) => "payment",
                Some(401 | 403) => "authorization",
                Some(429) => "rate_limit",
                Some(400..=499) => "request_rejected",
                Some(500..=599) => "provider_unavailable",
                _ if frame["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains("credential") =>
                {
                    "credential"
                }
                _ => "transport_or_protocol",
            };
            let description = match category {
                "payment" => {
                    "The provider rejected inference with HTTP 402. Payment, credit or the request's token allowance can prevent acceptance."
                }
                "authorization" => {
                    "The provider rejected authorization. Host credentials or provider permissions may need correction."
                }
                "rate_limit" => "The provider rejected inference because of a rate limit.",
                "request_rejected" => "The provider rejected this inference request.",
                "provider_unavailable" => "The provider reported a server failure.",
                "credential" => {
                    "The host credential was unavailable or malformed; no provider request was made."
                }
                _ => {
                    "Inference transport or protocol failed; retained frames describe the observed exchange."
                }
            };
            let v=rows.entry((s.record.spec.thread_id.clone(),purpose.clone(),category.into())).or_insert_with(||json!({"thread_id":s.record.spec.thread_id,"resident_id":s.record.spec.resident_id,"backend":s.record.spec.backend,"category":category,"purpose":purpose,"first_at":e.at,"count":0}));
            v["count"] = json!(v["count"].as_u64().unwrap_or(0) + 1);
            v["last_at"] = json!(e.at);
            v["last_run_id"] = json!(s.record.spec.id);
            v["http_status"] = json!(status);
            v["model"] = json!(s.record.spec.model);
            v["description"] = json!(description);
            v["active"] = json!(true);
            v["resolved_at"] = Value::Null;
            v["resolved_run_id"] = Value::Null;
        }
    }
    rows.into_values().collect()
}
pub(crate) fn context(runs: &[Arc<Run>]) -> String {
    let mut rows = project(runs);
    if rows.is_empty() {
        return String::new();
    }
    rows.sort_by_key(|v| std::cmp::Reverse(v["last_at"].as_u64().unwrap_or(0)));
    let total = rows.len();
    rows.truncate(8);
    format!(
        "\nInference health is a coalesced projection of retained execution records, not correspondence. Counts describe observed failed exchanges. A resolved incident means a later exchange completed, not that earlier work succeeded. Earlier tool effects are not automatically repeated. Recent incident categories: {} of {}. {}",
        rows.len(),
        total,
        json!(rows)
    )
}
impl Jobs {
    pub fn inference_health(&self, owner: &str, offset: usize) -> Value {
        let runs: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.snapshot().record.owner == owner)
            .cloned()
            .collect();
        let mut rows = project(&runs);
        rows.sort_by_key(|v| {
            (
                v["active"] != true,
                std::cmp::Reverse(v["last_at"].as_u64().unwrap_or(0)),
            )
        });
        let total = rows.len();
        let page: Vec<_> = rows.into_iter().skip(offset).take(50).collect();
        let next = offset.saturating_add(page.len());
        logging::record(
            Level::Debug,
            Target::Workload,
            "inference.health_read",
            Context::peer(owner),
            || json!({"total":total,"offset":offset,"returned":page.len()}),
            || Value::Null,
        );
        json!({"incidents":page,"total":total,"next_offset":if next<total{Some(next)}else{None}})
    }
    pub fn resume_inference(
        self: &Arc<Self>,
        owner: &str,
        failed: &str,
        id: String,
    ) -> Result<Snapshot> {
        let result = (|| {
            jobs::valid_id(&id)?;
            let _transition = self.transitions.lock().unwrap();
            let prior = self.get(owner, failed)?.snapshot();
            if prior.record.spec.backend != "openrouter" {
                return Err("inference continuation applies to the built-in runtime".into());
            }
            let directions = format!(
                "Continuation after interrupted inference in run {failed}. Accepted history retains the original request. Earlier tool effects and outgoing messages are not automatically repeated; unknown effects remain unknown."
            );
            if let Ok(r) = self.get(owner, &id) {
                let s = r.snapshot();
                if s.record.spec.thread_id == prior.record.spec.thread_id
                    && (s.record.spec.prompt == directions
                        || s.record
                            .mailbox
                            .as_ref()
                            .is_some_and(|a| a.directions == directions))
                {
                    return Ok(s);
                }
                return Err("continuation ID belongs to different input".into());
            }
            if prior.status != "interrupted" {
                return Err(
                    "continuation requires an interrupted run; completed work is not retried"
                        .into(),
                );
            }
            let latest = self
                .list(owner)
                .into_iter()
                .filter(|r| r.record.spec.thread_id == prior.record.spec.thread_id)
                .max_by_key(|r| (r.record.created_at, r.record.spec.id.clone()));
            if latest.is_none_or(|r| r.record.spec.id != failed) {
                return Err("a later run already exists; the current conversation can continue without replaying this incident".into());
            }
            if let Some(resident) = &prior.record.spec.resident_id {
                return self
                    .activate_locked(
                        owner,
                        resident,
                        id,
                        directions,
                        jobs::ActivationCause::Manual,
                    )?
                    .ok_or_else(|| "continuation did not accept an activation".into());
            }
            let mut spec: Spec = prior.record.spec;
            spec.id = id;
            spec.prompt = directions;
            self.start_locked(owner, spec, None, None)
        })();
        logging::record(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::Workload,
            "inference.resume_outcome",
            Context::peer(owner),
            || json!({"source_run_id":failed,"accepted":result.is_ok(),"run_id":result.as_ref().ok().map(|s|&s.record.spec.id),"error":result.as_ref().err().map(|e|e.to_string())}),
            || Value::Null,
        );
        result
    }
}
