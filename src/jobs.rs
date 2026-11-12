//! The executing daemon is the sole authority for runs and their ordered events.
use crate::logging::{self, Context, Level, Target};
use crate::{
    Result, codex,
    mailbox::{Address, Mailboxes, Message, Send},
    residents::{Create, Resident, Residents},
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[path = "inference_health.rs"]
pub mod inference_health;
#[path = "promotion.rs"]
pub mod promotion;
#[path = "thread_import.rs"]
pub mod thread_import;
#[path = "transcript_import.rs"]
pub mod transcript_import;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_pages_keep_large_retained_runs_below_the_transport_limit() {
        let root =
            std::env::temp_dir().join(format!("boxen-summaries-{}", state::random_hex().unwrap()));
        let path = root.join("runs");
        let owner = state::random_hex().unwrap();
        let prompt = "private prompt body".repeat(4096);
        for index in 0..130 {
            let id = format!("{index:064x}");
            let run = path.join(&id);
            state::private_dir(&run).unwrap();
            state::write_new(&run.join("record.json"), &serde_json::to_vec(&json!({
                "owner": owner,
                "created_at": 1,
                "spec": {"id":id,"thread_id":id,"backend":"codex","cwd":"/fixture","model":"fixture","prompt":prompt}
            })).unwrap()).unwrap();
            state::write_new(
                &run.join("events.jsonl"),
                b"{\"seq\":1,\"at\":1,\"kind\":\"status\",\"data\":{\"status\":\"completed\"}}\n",
            )
            .unwrap();
        }
        let jobs = Jobs::open(path, "missing-codex".into(), owner.clone()).unwrap();
        assert!(serde_json::to_vec(&jobs.operator_list()).unwrap().len() > 8 * 1024 * 1024);
        let first = jobs.operator_summary_page(None).unwrap();
        assert_eq!(first["summaries"].as_array().unwrap().len(), 128);
        assert!(first.to_string().len() < 128 * 1024);
        assert_eq!(
            first["summaries"][0]["title"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            72
        );
        assert!(first["summaries"][0]["record"]["spec"]["prompt"].is_null());
        let second = jobs
            .operator_summary_page(serde_json::from_value(first["next_after"].clone()).unwrap())
            .unwrap();
        assert_eq!(second["summaries"].as_array().unwrap().len(), 2);
        assert!(second["next_after"].is_null());
        let ids: Vec<_> = [first, second]
            .iter()
            .flat_map(|page| page["summaries"].as_array().unwrap())
            .map(|row| row["record"]["spec"]["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            ids,
            (0..130)
                .map(|index| format!("{index:064x}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            jobs.operator_run(&ids[0])
                .unwrap()
                .snapshot()
                .record
                .spec
                .prompt,
            prompt
        );
        drop(jobs);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_tail_recovery_keeps_sequence_and_single_authority() {
        let root =
            std::env::temp_dir().join(format!("boxen-journal-{}", state::random_hex().unwrap()));
        let path = root.join("runs");
        let owner = state::random_hex().unwrap();
        let id = state::random_hex().unwrap();
        state::private_dir(&path.join(&id)).unwrap();
        let record = Record {
            automatic_cwd: false,
            after: None,
            execution_change: None,
            runtime_policy: Default::default(),
            imported: None,
            transcript_import: None,
            observations: Vec::new(),
            scheduled: None,
            owner: owner.clone(),
            resident: None,
            recovery: None,
            mailbox: None,
            created_at: now(),
            spec: Spec {
                attachments: vec![],
                context: None,
                id: id.clone(),
                resident_id: None,
                thread_id: id.clone(),
                backend: "codex".into(),
                cwd: path.to_string_lossy().into(),
                model: String::new(),
                prompt: "hello".into(),
            },
        };
        state::write_new(
            &path.join(&id).join("record.json"),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let event = Event {
            seq: 1,
            at: now(),
            kind: "status".into(),
            data: json!({"status":"running"}),
        };
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.extend_from_slice(b"\n{\"seq\":2");
        state::write_new(&path.join(&id).join("events.jsonl"), &bytes).unwrap();
        let jobs = Jobs::open(path.clone(), "missing-codex".into(), owner.clone()).unwrap();
        assert!(Jobs::open(path.clone(), "missing-codex".into(), owner.clone()).is_err());
        let run = jobs.get(&owner, &id).unwrap();
        let (events, snapshot) = run.batch(0, false).unwrap();
        assert_eq!(snapshot.status, "interrupted");
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert!(run.batch(3, false).is_err());
        assert!(jobs.get(&state::random_hex().unwrap(), &id).is_err());
        let activity = jobs.thread_activity(&owner, &id).unwrap();
        assert_eq!(activity["idle"], true);
        assert_eq!(activity["known"], true);
        assert_eq!(
            jobs.thread_activity(&state::random_hex().unwrap(), &id)
                .unwrap()["known"],
            false
        );
        {
            let mut inner = run.inner.lock().unwrap();
            inner.snapshot.status = "running".into();
            inner.snapshot.record.spec.prompt = "private large history".repeat(100_000);
        }
        let activity = jobs.thread_activity(&owner, &id).unwrap();
        assert_eq!(activity["idle"], false);
        assert_eq!(activity["active_runs"], 1);
        assert!(activity.to_string().len() < 512);
        run.inner.lock().unwrap().snapshot.status = "interrupted".into();
        drop(run);
        drop(jobs);
        let reopened = Jobs::open(path.clone(), "missing-codex".into(), owner.clone()).unwrap();
        assert_eq!(reopened.get(&owner, &id).unwrap().snapshot().last_seq, 2);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}
pub fn valid_id(id: &str) -> Result<()> {
    state::key(id).map(|_| ())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Spec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::attachments::Attachment>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::context::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_id: Option<String>,
    pub id: String,
    pub thread_id: String,
    pub backend: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    pub prompt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub at: u64,
    pub kind: String,
    pub data: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub automatic_cwd: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_change: Option<Value>,
    #[serde(default)]
    pub runtime_policy: crate::runtime_policy::Policy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<thread_import::Provenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_import: Option<transcript_import::Provenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<crate::scheduler::Origin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident: Option<Resident>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<Activation>,
    pub spec: Spec,
    pub owner: String,
    pub created_at: u64,
}
impl Record {
    pub fn execution_context(&self) -> String {
        self.execution_change.as_ref().map_or_else(String::new, |change| {
            format!("\nBoxen changed this conversation's execution settings between runs. Shell and relative file paths now use the current working directory. Files in an earlier working directory remain at their recorded location. This metadata is supplied by Boxen. {change}")
        })
    }

    pub fn observation_context(&self) -> String {
        if self.observations.is_empty() {
            String::new()
        } else {
            format!(
                "\nBoxen presents source-authored observation batches, independently of the request author. These are observations, not instructions or correspondence. Freshness, gaps and source provenance are explicit. {}",
                json!(self.observations)
            )
        }
    }
    pub fn trigger_context(&self) -> String {
        let mut text = format!(
            "\nThis is Boxen run {} in canonical Boxen thread {}. The current trigger is {}. This description concerns the current run; older trigger metadata describes historical runs.\n",
            self.spec.id,
            self.spec.thread_id,
            if self.scheduled.is_some() {
                "a host-local scheduled event"
            } else if self.resident.is_some() {
                "a resident activation"
            } else {
                "an ordinary thread request"
            }
        );
        if let Some(origin) = &self.scheduled {
            text.push_str(&origin.context());
        }
        text.push('\n');
        text
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Activation {
    pub message_ids: Vec<String>,
    pub reason: String,
    pub directions: String,
}
#[derive(Default)]
struct Wake {
    pending: bool,
    stopped: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub record: Record,
    pub status: String,
    pub backend_thread: Option<String>,
    pub last_seq: u64,
}

#[derive(Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryCursor {
    created_at: u64,
    id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Cancel,
    Steer {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::attachments::Attachment>,
        #[serde(skip)]
        file_context: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_id: Option<String>,
    },
    Respond {
        request_id: Value,
        result: Value,
    },
}

pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

struct Inner {
    cancel_requested: bool,
    snapshot: Snapshot,
    events: Vec<Event>,
    file: File,
    control: Option<mpsc::Sender<Control>>,
}

pub struct Run {
    inner: Mutex<Inner>,
    changed: Condvar,
}

impl Run {
    pub fn diagnostic(
        &self,
        level: Level,
        event: &str,
        fields: impl FnOnce() -> Value,
        payload: impl FnOnce() -> Value,
    ) {
        if !logging::enabled(level, Target::Runtime) {
            return;
        }
        let inner = self.inner.lock().unwrap();
        let record = &inner.snapshot.record;
        logging::record(
            level,
            Target::Runtime,
            event,
            Context::run(&record.spec.id, &record.spec.thread_id, &record.owner),
            fields,
            payload,
        );
    }
    pub fn context_events(&self) -> Vec<Event> {
        self.inner
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e.kind.as_str(),
                    "inference_message"
                        | "context_boundary"
                        | "context_bundle"
                        | "context_note"
                        | "context_compact_requested"
                        | "context_exposure"
                )
            })
            .cloned()
            .collect()
    }
    pub fn cancellation_requested(&self) -> bool {
        self.inner.lock().unwrap().cancel_requested
    }
    pub fn retained_events(&self, kind: &str) -> Vec<Event> {
        self.inner
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|e| e.kind == kind)
            .cloned()
            .collect()
    }
    pub fn owner(&self) -> String {
        self.inner.lock().unwrap().snapshot.record.owner.clone()
    }
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().snapshot.clone()
    }
    pub fn append(&self, kind: &str, data: Value) -> Result<Event> {
        let mut inner = self.inner.lock().unwrap();
        self.append_locked(&mut inner, kind, data)
    }
    fn append_locked(&self, inner: &mut Inner, kind: &str, data: Value) -> Result<Event> {
        let event = Event {
            seq: inner.snapshot.last_seq + 1,
            at: now(),
            kind: kind.into(),
            data,
        };
        let mut bytes = serde_json::to_vec(&event)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("backend event exceeds 4 MiB".into());
        }
        bytes.push(b'\n');
        // Retain before publication. Readers never observe an unretained event.
        inner.file.write_all(&bytes)?;
        inner.file.sync_data()?;
        project(&mut inner.snapshot, &event);
        inner.events.push(event.clone());
        self.changed.notify_all();
        let record = &inner.snapshot.record;
        logging::record(
            if kind == "status" {
                Level::Info
            } else {
                Level::Trace
            },
            Target::Workload,
            "event.retained",
            Context::run(&record.spec.id, &record.spec.thread_id, &record.owner),
            || json!({"sequence":event.seq,"kind":kind,"bytes":bytes.len(),"status":inner.snapshot.status,"method":event.data.get("method")}),
            || serde_json::to_value(&event).unwrap_or(Value::Null),
        );
        Ok(event)
    }
    pub fn reserve_message(&self, call_id: &str, fingerprint: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if terminal(&inner.snapshot.status) {
            return Err("ended runs cannot accept new message attempts".into());
        }
        if let Some(prior) = inner
            .events
            .iter()
            .find(|e| e.kind == "message_send_reserved" && e.data["call_id"] == call_id)
        {
            return if prior.data["fingerprint"] == fingerprint {
                Ok(())
            } else {
                Err("message call ID already identifies different input".into())
            };
        }
        let count = inner
            .events
            .iter()
            .filter(|e| e.kind == "message_send_reserved")
            .count();
        let limit = inner.snapshot.record.runtime_policy.max_message_sends;
        if limit.is_some_and(|limit| count >= limit as usize) {
            if !inner
                .events
                .iter()
                .any(|e| e.kind == "message_send_limited")
            {
                self.append_locked(&mut inner, "message_send_limited", json!({"limit":limit,"detail":"Further new message attempts in this run are paused. Accepted messages remain accepted; a later activation has a new budget."}))?;
            }
            drop(inner);
            self.diagnostic(
                Level::Warn,
                "agent.message_rejected",
                || json!({"limit":limit,"reason":"run_budget"}),
                || Value::Null,
            );
            return Err("this run's outgoing message-attempt budget is exhausted; no new message was sent. Other tools remain available; a later activation has its own budget".into());
        }
        self.append_locked(
            &mut inner,
            "message_send_reserved",
            json!({"call_id":call_id,"fingerprint":fingerprint,"attempt":count+1,"limit":limit}),
        )?;
        drop(inner);
        self.diagnostic(
            Level::Debug,
            "agent.message_reserved",
            || json!({"call_id":call_id,"attempt":count+1,"limit":limit}),
            || Value::Null,
        );
        Ok(())
    }
    pub fn status(&self, status: &str, detail: &str) -> Result<()> {
        self.append("status", json!({"status":status,"detail":detail}))?;
        Ok(())
    }
    pub fn batch(&self, after: u64, wait: bool) -> Result<(Vec<Event>, Snapshot)> {
        let mut inner = self.inner.lock().unwrap();
        if after > inner.snapshot.last_seq {
            return Err("event cursor is ahead of this run".into());
        }
        if wait && after == inner.snapshot.last_seq && !terminal(&inner.snapshot.status) {
            inner = self
                .changed
                .wait_timeout(inner, Duration::from_secs(2))
                .unwrap()
                .0;
        }
        let mut size = 0;
        let events = inner
            .events
            .iter()
            .skip(after as usize)
            .take(128)
            .take_while(|event| {
                size += serde_json::to_vec(event).map(|v| v.len()).unwrap_or(0);
                size <= 5 * 1024 * 1024
            })
            .cloned()
            .collect();
        let record = &inner.snapshot.record;
        logging::record(
            Level::Trace,
            Target::Workload,
            "events.read",
            Context::run(&record.spec.id, &record.spec.thread_id, &record.owner),
            || json!({"after":after,"last_sequence":inner.snapshot.last_seq,"status":inner.snapshot.status}),
            || Value::Null,
        );
        Ok((events, inner.snapshot.clone()))
    }
    pub fn control(&self, control: Control) -> Result<()> {
        self.diagnostic(Level::Debug,"control.requested",||json!({"type":match &control{Control::Cancel=>"cancel",Control::Steer{..}=>"steer",Control::Respond{..}=>"respond"}}),||serde_json::to_value(&control).unwrap_or(Value::Null));
        if matches!(&control, Control::Steer { .. }) && self.snapshot().record.resident.is_some() {
            return Err("resident communication enters its mailbox; operational cancellation and backend input responses remain separate controls".into());
        }
        if let Control::Steer {
            text, attachments, ..
        } = &control
            && ((text.is_empty() && attachments.is_empty()) || text.len() > 256 * 1024)
        {
            return Err("steering text must contain 1–262144 bytes".into());
        }
        let mut inner = self.inner.lock().unwrap();
        let cancel = matches!(control, Control::Cancel);
        if terminal(&inner.snapshot.status) {
            return Err("run is no longer active".into());
        }
        inner
            .control
            .as_ref()
            .ok_or("runtime is not available")?
            .send(control)?;
        if cancel {
            inner.cancel_requested = true;
        }
        Ok(())
    }
}

fn project(snapshot: &mut Snapshot, event: &Event) {
    snapshot.last_seq = event.seq;
    if event.kind == "status" {
        if let Some(status) = event.data["status"].as_str() {
            snapshot.status = status.into();
        }
    } else if event.kind == "backend_thread" {
        snapshot.backend_thread = event.data["id"].as_str().map(str::to_owned);
    }
}

pub struct History {
    directory: PathBuf,
    thread_id: String,
    runs: Vec<Arc<Run>>,
    pub rebuild_reason: Option<String>,
    pub native_import: Option<thread_import::Provenance>,
}
impl History {
    pub fn journal_directory(&self) -> &std::path::Path {
        &self.directory
    }
    pub fn source(&self, current: &Arc<Run>) -> crate::context::Source {
        let mut runs = self.runs.clone();
        runs.push(current.clone());
        crate::context::Source::from_runs(&runs)
    }
    pub fn settings(&self, spec: &Spec) -> crate::context::Settings {
        spec.context
            .clone()
            .or_else(|| {
                self.runs
                    .iter()
                    .rev()
                    .find_map(|r| r.snapshot().record.spec.context)
            })
            .unwrap_or_default()
    }
    pub fn inference_health_context(&self) -> String {
        inference_health::context(&self.runs)
    }
    pub fn transcript_context(&self) -> String {
        let sources:Vec<_>=self.runs.iter().filter_map(|r|r.snapshot().record.transcript_import).map(|p|json!({"source":p.source,"message_count":p.message_ids.len(),"history_sha256":p.request_sha256})).collect();
        if sources.is_empty() {
            String::new()
        } else {
            format!(
                "\nThis canonical conversation includes operator-imported text history. Its user and assistant roles preserve the source speakers; imported assistant messages originate from the declared source, rather than the currently selected model. Original text and source attribution remain retained; hidden source prompts, reasoning, audio and provider session state are not reconstructed. {}",
                json!(sources)
            )
        }
    }
    pub fn reconstruction(&self) -> Result<String> {
        let mut recent = std::collections::VecDeque::new();
        let mut bytes = 0usize;
        let mut total = 0usize;
        for run in &self.runs {
            let inner = run.inner.lock().unwrap();
            let id = &inner.snapshot.record.spec.id;
            let journal = self.directory.join(id).join("events.jsonl");
            for event in &inner.events {
                total += 1;
                if event.kind == "backend"
                    && !matches!(
                        event.data["method"].as_str(),
                        Some(
                            "item/completed" | "turn/plan/updated" | "turn/diff/updated" | "error"
                        )
                    )
                {
                    continue;
                }
                let value = json!({"run_id":id,"event":event});
                let size = serde_json::to_vec(&value)?.len();
                let value = if size > 16 * 1024 {
                    json!({"run_id":id,"seq":event.seq,"kind":event.kind,"payload_omitted":true,"bytes":size,"journal":journal})
                } else {
                    value
                };
                bytes += serde_json::to_vec(&value)?.len();
                recent.push_back(value);
                while bytes > 48 * 1024 {
                    if let Some(old) = recent.pop_front() {
                        bytes = bytes.saturating_sub(serde_json::to_vec(&old)?.len());
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(json!({"canonical_thread":self.thread_id,"retained_runs_directory":self.directory,"total_prior_events":total,"included_recent_events":recent.len(),"omitted_older_events":total.saturating_sub(recent.len()),"recent_events":recent}).to_string())
    }
}

pub struct Jobs {
    pub attachments: crate::attachments::Store,
    pub network: Mutex<Option<Arc<crate::state::State>>>,
    directory: PathBuf,
    runs: Mutex<HashMap<String, Arc<Run>>>,
    codex_path: String,
    pub inference: crate::inference::Config,
    _lock: File,
    pub residents: Arc<Residents>,
    pub definitions: crate::resident_definitions::Definitions,
    transitions: Mutex<()>,
    pub mailboxes: Arc<Mailboxes>,
    pub connectors: crate::messaging::Connectors,
    pub schedules: crate::scheduler::Schedules,
    pub observations: Arc<crate::events::Events>,
    wake: Arc<(Mutex<Wake>, Condvar)>,
}

enum ActivationCause {
    Mail,
    Manual,
    Schedule(crate::scheduler::Origin),
    Observations,
}
impl Jobs {
    pub(crate) fn runtime_home(&self) -> Option<&Path> {
        self.directory.parent()
    }

    fn scheduled_thread(&self, owner: &str, id: &str) -> Result<Spec> {
        valid_id(id)?;
        if self.residents.get(owner, id).is_ok() {
            return Err("resident schedules and subscriptions address a resident target, not an anonymous thread target".into());
        }
        let mut records: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .map(|r| r.snapshot())
            .filter(|s| s.record.spec.thread_id == id)
            .collect();
        records.sort_by_key(|s| s.record.created_at);
        if records.iter().any(|s| s.record.owner != owner) {
            return Err("thread belongs to a different peer".into());
        }
        let last = records
            .last()
            .ok_or("thread schedule addresses an existing identity-less thread")?;
        if last.record.resident.is_some() {
            return Err("resident schedules use the resident activation handler".into());
        }
        Ok(last.record.spec.clone())
    }
    fn reconcile_schedule(
        &self,
        job: &crate::scheduler::Job,
        cancel_missing: bool,
    ) -> Result<bool> {
        let Some(origin) = &job.claimed else {
            return Ok(false);
        };
        let id = origin.run_id();
        if let Ok(run) = self.get(&job.owner, &id) {
            let snapshot = run.snapshot();
            let matches = snapshot.record.scheduled.as_ref() == Some(origin);
            self.schedules.finish(job,if matches{"dispatched"}else{"uncertain"},json!({"run_id":id,"recovered":true,"status_at_reconciliation":snapshot.status,"origin_matches":matches,"detail":"The existing accepted run was linked without repeating its workload. An interrupted outcome remains interrupted."}))?;
            logging::record(
                Level::Info,
                Target::Workload,
                "schedule.reconciled",
                Context::run(&id, job.definition.target.id(), &job.owner),
                || json!({"schedule_id":job.definition.id,"origin_matches":matches,"execution_status":snapshot.status}),
                || Value::Null,
            );
            return Ok(true);
        }
        if self.directory.join(&id).join("record.json").exists() {
            self.schedules.finish(job,"uncertain",json!({"run_id":id,"detail":"An accepted run record exists outside the active catalog. Its evidence is preserved; this occurrence is not resubmitted. Later occurrences remain eligible."}))?;
            return Ok(true);
        }
        if cancel_missing {
            self.schedules.finish(job,"cancelled_before_acceptance",json!({"detail":"The unaccepted claim was cancelled by a definition change. No accepted run record exists for it."}))?;
            return Ok(true);
        }
        Ok(false)
    }
    pub fn schedule_operation(self: &Arc<Self>, owner: &str, args: Value) -> Result<Value> {
        let _transition = self.transitions.lock().unwrap();
        let action = args["action"].as_str().unwrap_or("list");
        let result = (|| -> Result<Value> {
            match action {
                "list" => {
                    let mut page = self.schedules.page(owner, None, &args);
                    page["issues"] = json!(self.schedules.issues());
                    Ok(page)
                }
                "get" => Ok(json!(
                    self.schedules
                        .get(owner, args["id"].as_str().ok_or("schedule id required")?)?
                )),
                "history" => self.schedules.history(
                    owner,
                    args["id"].as_str().ok_or("schedule id required")?,
                    args["after"].as_u64().unwrap_or(0),
                ),
                "create" => {
                    let definition: crate::scheduler::Definition =
                        serde_json::from_value(args["spec"].clone())?;
                    match &definition.target {
                        crate::scheduler::Handler::Resident { id } => {
                            self.residents.get(owner, id)?;
                        }
                        crate::scheduler::Handler::Thread { id } => {
                            self.scheduled_thread(owner, id)?;
                        }
                        crate::scheduler::Handler::Source { .. }
                        | crate::scheduler::Handler::Delivery { .. } => return Err(
                            "observation timers are managed by source/subscription registration"
                                .into(),
                        ),
                    }
                    Ok(json!(self.schedules.create(owner, definition)?))
                }
                "update" => {
                    let id = args["id"].as_str().ok_or("schedule id required")?;
                    let patch: crate::scheduler::Patch =
                        serde_json::from_value(args["patch"].clone())?;
                    let job = self.schedules.get(owner, id)?;
                    if matches!(
                        job.definition.target,
                        crate::scheduler::Handler::Source { .. }
                            | crate::scheduler::Handler::Delivery { .. }
                    ) {
                        return Err("observation timer definitions are managed by the events API; source/subscription controls preserve their delivery contract".into());
                    }
                    if patch.expected_revision != job.revision {
                        return Err(
                            "schedule revision conflict; a read returns the current definition"
                                .into(),
                        );
                    }
                    self.schedules.validate_patch(owner, id, &patch)?;
                    self.reconcile_schedule(&job, true)?;
                    Ok(json!(self.schedules.patch(owner, id, patch)?))
                }
                _ => Err("schedule actions are list, get, history, create, and update".into()),
            }
        })();
        logging::record(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::Workload,
            "schedule.operation",
            Context::peer(owner),
            || json!({"action":action,"schedule_id":args.get("id").or_else(||args.get("spec").and_then(|s|s.get("id"))),"accepted":result.is_ok()}),
            || args.clone(),
        );
        if result.is_ok() && matches!(action, "create" | "update") {
            self.notify();
        }
        result
    }
    fn dispatch_schedules(self: &Arc<Self>) {
        let mut jobs = self.schedules.all();
        jobs.sort_by_key(|j| (j.next_at_ms, j.definition.id.clone()));
        for initial in jobs {
            let _transition = self.transitions.lock().unwrap();
            let result = (|| -> Result<()> {
                let job = self.schedules.get(&initial.owner, &initial.definition.id)?;
                if matches!(
                    job.definition.target,
                    crate::scheduler::Handler::Source { .. }
                        | crate::scheduler::Handler::Delivery { .. }
                ) {
                    if job.retry_at_ms > now() {
                        return Ok(());
                    }
                    if !self.observations.available() {
                        self.schedules.defer(&job,false,"Observation storage unavailable; only observation timers are deferred.")?;
                        return Ok(());
                    }
                    if job.enabled && !job.deleted && job.next_at_ms.is_some_and(|t| t <= now()) {
                        let claimed = self.schedules.claim(&job, now())?;
                        match &job.definition.target {
                            crate::scheduler::Handler::Source { id } => {
                                let observations = self.observations.clone();
                                let owner = job.owner.clone();
                                let id = id.clone();
                                let weak = Arc::downgrade(self);
                                std::thread::spawn(move || {
                                    if let Err(error) = observations.collect(&owner, &id) {
                                        logging::record(
                                            Level::Warn,
                                            Target::Workload,
                                            "observation.collect_failed",
                                            Context::peer(&owner),
                                            || json!({"source_id":id,"error":error.to_string()}),
                                            || Value::Null,
                                        );
                                    }
                                    if let Some(j) = weak.upgrade() {
                                        j.notify();
                                    }
                                });
                            }
                            crate::scheduler::Handler::Delivery { id } => {
                                if let Err(error) = self.observations.ready(&job.owner, id) {
                                    self.schedules.defer(
                                        &claimed,
                                        false,
                                        "Observation batch preparation failed; retry is bounded.",
                                    )?;
                                    return Err(error);
                                }
                            }
                            _ => unreachable!(),
                        }
                        self.schedules.finish(&claimed,"timed",json!({"detail":"Observation timer handled; source collection and ready batches are distinct from inference acceptance."}))?;
                    }
                    return Ok(());
                }
                if self.reconcile_schedule(&job, false)? {
                    return Ok(());
                }
                if job.deleted || !job.enabled {
                    return Ok(());
                }
                let time = now();
                if job.next_at_ms.is_none_or(|due| due > time)
                    || (!job.blocked && job.retry_at_ms > time)
                {
                    return Ok(());
                }
                let runs: Vec<_> = self
                    .runs
                    .lock()
                    .unwrap()
                    .values()
                    .map(|r| r.snapshot())
                    .collect();
                if runs.iter().any(|s| {
                    s.record.spec.thread_id == job.definition.target.id() && !terminal(&s.status)
                }) || runs.iter().filter(|s| !terminal(&s.status)).count() >= 8
                {
                    if !job.blocked || job.retry_at_ms <= time {
                        self.schedules.defer(&job,true,"Target is active or the host's execution slots are occupied; the occurrence remains pending until capacity is available.")?;
                        logging::record(
                            Level::Info,
                            Target::Workload,
                            "schedule.waiting",
                            Context::peer(&job.owner),
                            || json!({"schedule_id":job.definition.id,"thread_id":job.definition.target.id()}),
                            || Value::Null,
                        );
                    }
                    return Ok(());
                }
                let claimed = self.schedules.claim(&job, time)?;
                if self.reconcile_schedule(&claimed, false)? {
                    return Ok(());
                }
                let origin = claimed.claimed.as_ref().ok_or("claim unavailable")?.clone();
                let run_id = origin.run_id();
                logging::record(
                    Level::Info,
                    Target::Workload,
                    "schedule.dispatching",
                    Context::run(&run_id, job.definition.target.id(), &job.owner),
                    || json!({"schedule_id":job.definition.id,"due_at_ms":origin.due_at_ms,"coalesced_occurrences":origin.coalesced_occurrences}),
                    || Value::Null,
                );
                let dispatched = match &job.definition.target {
                    crate::scheduler::Handler::Resident { id } => self
                        .activate_locked(
                            &job.owner,
                            id,
                            run_id.clone(),
                            job.definition.directions.clone(),
                            ActivationCause::Schedule(origin.clone()),
                        )
                        .and_then(|s| {
                            s.ok_or_else(|| "scheduled activation was not accepted".into())
                        }),
                    crate::scheduler::Handler::Source { .. }
                    | crate::scheduler::Handler::Delivery { .. } => unreachable!(),
                    crate::scheduler::Handler::Thread { id } => {
                        self.scheduled_thread(&job.owner, id).and_then(|mut spec| {
                            spec.id = run_id.clone();
                            spec.prompt = job.definition.directions.clone();
                            self.start_locked(&job.owner, spec, None, Some(origin.clone()), None)
                        })
                    }
                };
                match dispatched {
                    Ok(snapshot) => {
                        self.schedules.finish(&claimed,"dispatched",json!({"run_id":run_id,"status_at_acceptance":snapshot.status,"detail":"Accepted execution is distinct from completion; the run journal owns its outcome."}))?;
                    }
                    Err(error) => {
                        if !self.reconcile_schedule(&claimed, false)? {
                            self.schedules.defer(&claimed, false, &error.to_string())?;
                        }
                        logging::record(
                            Level::Warn,
                            Target::Workload,
                            "schedule.dispatch_failed",
                            Context::run(&run_id, job.definition.target.id(), &job.owner),
                            || json!({"schedule_id":job.definition.id,"error":error.to_string()}),
                            || Value::Null,
                        );
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                logging::record(
                    Level::Error,
                    Target::Workload,
                    "schedule.dispatch_error",
                    Context::peer(&initial.owner),
                    || json!({"schedule_id":initial.definition.id,"error":error.to_string()}),
                    || Value::Null,
                );
            }
        }
    }
    pub fn event_operation(self: &Arc<Self>, owner: &str, args: Value) -> Result<Value> {
        let action = args["action"].as_str().unwrap_or("list");
        let result = (|| -> Result<Value> {
            match action {
            "list"=>Ok(self.observations.list(owner)),
            "source"=>Ok(json!(self.observations.source(owner,args["id"].as_str().ok_or("source id required")?)?)),
            "subscription"=>Ok(json!(self.observations.subscription(owner,args["id"].as_str().ok_or("subscription id required")?)?)),
            "create_source"=>{
                let s=self.observations.create_source(owner,serde_json::from_value(args["spec"].clone())?)?;
                if !s.spec.command.is_empty(){self.schedules.create(owner,crate::scheduler::Definition{id:crate::events::timer_id("source",&s.spec.id),name:format!("Collect {}",s.spec.id),target:crate::scheduler::Handler::Source{id:s.spec.id.clone()},directions:String::new(),at_ms:args["at_ms"].as_u64().unwrap_or(s.created_at_ms),interval_ms:Some(s.spec.interval_ms)})?;}
                Ok(json!(s))
            },
            "subscribe"=>{
                let spec:crate::events::SubscriptionSpec=serde_json::from_value(args["spec"].clone())?;
                match &spec.target {crate::scheduler::Handler::Resident{id}=>{self.residents.get(owner,id)?;},crate::scheduler::Handler::Thread{id}=>{self.scheduled_thread(owner,id)?;},_=>return Err("subscriptions target residents or threads".into())}
                let sub=self.observations.subscribe(owner,spec)?;
                self.schedules.create(owner,crate::scheduler::Definition{id:crate::events::timer_id("delivery",&sub.spec.id),name:format!("Batch {}",sub.spec.id),target:crate::scheduler::Handler::Delivery{id:sub.spec.id.clone()},directions:String::new(),at_ms:args["at_ms"].as_u64().unwrap_or(sub.created_at_ms+sub.spec.interval_ms),interval_ms:Some(sub.spec.interval_ms)})?;
                Ok(json!(sub))
            },
            "update_source"|"update_subscription"=>{let _t=self.transitions.lock().unwrap();self.observations.update(owner,if action=="update_source"{"source"}else{"subscription"},args["id"].as_str().ok_or("id required")?,&args["patch"])},
            "publish"=>self.observations.publish(owner,args["id"].as_str().ok_or("source id required")?,args["generation"].as_u64().ok_or("source generation required")?,Some(args["sequence"].as_u64().ok_or("producer sequence required")?),args["response"].clone()),
            "read"=>{let sub=self.observations.subscription(owner,args["id"].as_str().ok_or("subscription id required")?)?;Ok(json!({"batches":self.observations.batches(owner,sub.spec.target.id()).into_iter().filter(|b|b["subscription_id"]==sub.spec.id).collect::<Vec<_>>(),"presentation":"Read-only preview; positions advance only after input is retained in a workload journal."}))},
            _=>Err("observation actions are list, source, subscription, create_source, subscribe, update_source, update_subscription, publish, and read".into())
        }
        })();
        logging::record(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::Workload,
            "observation.operation",
            Context::peer(owner),
            || json!({"action":action,"id":args.get("id").or_else(||args.get("spec").and_then(|s|s.get("id"))),"accepted":result.is_ok()}),
            || args.clone(),
        );
        if result.is_ok() && !matches!(action, "list" | "source" | "subscription" | "read") {
            self.notify();
        }
        result
    }
    pub fn present_observations(&self, run: &Arc<Run>, call_id: &str, id: &str) -> Result<Value> {
        let _transition = self.transitions.lock().unwrap();
        let record = run.snapshot().record;
        let sub = self.observations.subscription(&record.owner, id)?;
        if sub.spec.target.id() != record.spec.thread_id {
            return Err("subscription belongs to a different thread".into());
        }
        self.observations.ready(&record.owner, id)?;
        let batches: Vec<_> = self
            .observations
            .batches(&record.owner, &record.spec.thread_id)
            .into_iter()
            .filter(|b| b["subscription_id"] == id)
            .collect();
        run.append(
            "observation_batch",
            json!({"batches":batches,"call_id":call_id}),
        )?;
        self.observations
            .acknowledge(&record.owner, &batches, true)?;
        Ok(json!({"batches":batches}))
    }
    fn dispatch_observations(self: &Arc<Self>) {
        for (owner, target) in self.observations.pending_targets() {
            let _t = self.transitions.lock().unwrap();
            if !self.observations.wake_allowed(&owner, target.id())
                || self.runs.lock().unwrap().values().any(|r| {
                    r.snapshot().record.spec.thread_id == target.id()
                        && !terminal(&r.snapshot().status)
                })
            {
                continue;
            }
            let result = (|| -> Result<()> {
                let id = state::random_hex()?;
                let directions = if matches!(target, crate::scheduler::Handler::Resident { .. }) {
                    "Ready source observations accompany this resident execution opportunity. Their provenance, freshness and retention gaps are included separately. Assistant output remains private narration; outgoing mailbox communication is explicit."
                } else {
                    "The attached source observations are input to this ordinary workload request. Assistant output is its conventional thread response, retained for authorized clients and distinct from mailbox sending. Provenance, freshness and any retention gaps are included separately."
                };
                let run = match target {
                    crate::scheduler::Handler::Resident { id: ref target } => self
                        .activate_locked(
                            &owner,
                            target,
                            id,
                            directions.into(),
                            ActivationCause::Observations,
                        )?
                        .ok_or("observation activation was not accepted")?,
                    crate::scheduler::Handler::Thread { id: ref target } => {
                        let mut spec = self.scheduled_thread(&owner, target)?;
                        spec.id = id;
                        spec.prompt = directions.into();
                        self.start_locked(&owner, spec, None, None, None)?
                    }
                    _ => return Err("observation target is unavailable".into()),
                };
                logging::record(
                    Level::Info,
                    Target::Workload,
                    "observation.activated",
                    Context::run(&run.record.spec.id, target.id(), &owner),
                    || json!({"batches":run.record.observations.len()}),
                    || Value::Null,
                );
                Ok(())
            })();
            if let Err(error) = result {
                logging::record(
                    Level::Warn,
                    Target::Workload,
                    "observation.activation_deferred",
                    Context::peer(&owner),
                    || json!({"thread_id":target.id(),"error":error.to_string()}),
                    || Value::Null,
                );
            }
        }
    }
    pub fn context_operation(&self, owner: &str, thread: &str, args: Value) -> Result<Value> {
        let _transition = self.transitions.lock().unwrap();
        valid_id(thread)?;
        let mut runs: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.snapshot().record.spec.thread_id == thread)
            .cloned()
            .collect();
        runs.sort_by_key(|r| r.snapshot().record.created_at);
        let last = runs.last().ok_or("thread has no accepted run")?;
        for run in &runs {
            if run.snapshot().record.owner != owner {
                return Err("thread belongs to a different peer".into());
            }
        }
        if last.snapshot().record.spec.backend != "openrouter" {
            return Err("Boxen history projection tools apply to the built-in inference runtime; managed runtimes retain native history controls".into());
        }
        let mutating = terminal(&last.snapshot().status);
        let result = crate::context::Source::from_runs(&runs).operation(
            last,
            args.clone(),
            mutating,
            "operator",
            49152,
        );
        last.diagnostic(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            "context.operation",
            || json!({"action":args["action"],"accepted":result.is_ok(),"external":true}),
            || Value::Null,
        );
        result
    }
    pub fn runtime_info(&self) -> Value {
        json!({"native_thread_import":{"backend":"codex","history":"public_turn_item_snapshot","starts_execution":false,"resident_promotion":true},"messaging":{"protocol":"boxen-messaging-v1","instance_scoped":true,"builtin":"boxen","agent_tool":"boxen_messages","managed_tools":"registered_on_new_native_sessions"},"observations":{"protocol":"boxen-adapter-v1","input":"bounded_json","delivery":["activation","explicit_tool_read","adapter_result"],"managed_tools":"registered_on_new_native_sessions","pending_store_bytes":4194304},"backends":[{"id":"codex","kind":"managed"},{"id":"openrouter","kind":"built_in","configured":self.inference.ready().is_ok(),"default_model":crate::inference::DEFAULT_MODEL,"input":["text","image","audio","video","file"],"native_read":true,"context_policies":["summary","verbatim"],"default_ephemeral_context":"summary","default_resident_context":"verbatim","history_tool":true,"model_provider_support_required":true,"tools":true,"full_access":true}]})
    }

    pub(crate) fn artifact_root(&self) -> &std::path::Path {
        &self.directory
    }
    pub fn machine_id(&self) -> &str {
        self.residents.machine_id()
    }
    #[cfg(test)]
    pub fn open(directory: PathBuf, codex_path: String, machine_id: String) -> Result<Arc<Self>> {
        let config = crate::inference::Config {
            endpoint: crate::chat::ENDPOINT.into(),
            key_file: directory
                .parent()
                .ok_or("runs parent required")?
                .join("credentials/openrouter.key"),
        };
        Self::open_with_inference(directory, codex_path, machine_id, config)
    }
    pub fn open_with_inference(
        directory: PathBuf,
        codex_path: String,
        machine_id: String,
        inference: crate::inference::Config,
    ) -> Result<Arc<Self>> {
        logging::record(
            Level::Info,
            Target::State,
            "journal.opening",
            Context::default(),
            || json!({"directory":directory}),
            || Value::Null,
        );
        state::private_dir(&directory)?;
        let lock_path = directory.join("authority.lock");
        if !lock_path.exists() {
            let _ = state::write_new(&lock_path, b"");
        }
        let lock = OpenOptions::new().read(true).write(true).open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| "another daemon already owns this data directory")?;
        let residents = Arc::new(Residents::open(
            directory
                .parent()
                .ok_or("runs directory requires a parent")?
                .join("residents"),
            machine_id.clone(),
        )?);
        let mailboxes = Arc::new(Mailboxes::open(
            directory
                .parent()
                .ok_or("runs parent required")?
                .join("mailboxes"),
            machine_id,
        )?);
        for resident in residents.all() {
            mailboxes.register(&resident.owner, &resident.id)?;
        }
        let schedules = crate::scheduler::Schedules::open(
            directory
                .parent()
                .ok_or("runs parent required")?
                .join("schedules"),
        )?;
        let observations = Arc::new(crate::events::Events::open(
            directory
                .parent()
                .ok_or("runs parent required")?
                .join("observations"),
        )?);
        let connectors = crate::messaging::Connectors::open(
            directory
                .parent()
                .ok_or("runs parent required")?
                .join("connectors"),
        )?;
        let definitions = crate::resident_definitions::Definitions::open(
            directory
                .parent()
                .ok_or("runs parent required")?
                .join("resident-definitions"),
            residents.machine_id().into(),
        );
        let attachment_root = directory
            .parent()
            .ok_or("runs parent required")?
            .canonicalize()?
            .join("attachments");
        let jobs = Arc::new(Self {
            attachments: crate::attachments::Store::new(attachment_root),
            network: Mutex::new(None),
            definitions,
            connectors,
            observations,
            schedules,
            residents,
            mailboxes,
            wake: Arc::new((Mutex::new(Wake::default()), Condvar::new())),
            transitions: Mutex::new(()),
            directory,
            runs: Mutex::new(HashMap::new()),
            codex_path,
            inference,
            _lock: lock,
        });
        for entry in fs::read_dir(&jobs.directory)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            valid_id(id)?;
            // An interrupted directory creation without a record never accepted a run.
            if !path.join("record.json").exists() {
                continue;
            }
            let record: Record = serde_json::from_slice(&fs::read(path.join("record.json"))?)?;
            if record.spec.id != id {
                return Err("run record identity mismatch".into());
            }
            let log_path = path.join("events.jsonl");
            if !log_path.exists() {
                state::write_new(&log_path, b"")?;
            }
            let mut snapshot = Snapshot {
                record,
                status: "starting".into(),
                backend_thread: None,
                last_seq: 0,
            };
            let mut events = Vec::new();
            let mut complete_bytes = 0;
            let mut reader = BufReader::new(File::open(&log_path)?);
            loop {
                let mut line = Vec::new();
                if reader.read_until(b'\n', &mut line)? == 0 {
                    break;
                }
                if !line.ends_with(b"\n") {
                    // An uncommitted final record was never published.
                    OpenOptions::new()
                        .write(true)
                        .open(&log_path)?
                        .set_len(complete_bytes)?;
                    break;
                }
                let event: Event =
                    serde_json::from_slice(&line).map_err(|_| "corrupt event log")?;
                if event.seq != snapshot.last_seq + 1 {
                    return Err("event log sequence mismatch".into());
                }
                complete_bytes += line.len() as u64;
                project(&mut snapshot, &event);
                events.push(event);
            }
            let run = Arc::new(Run {
                inner: Mutex::new(Inner {
                    cancel_requested: false,
                    snapshot,
                    events,
                    file: OpenOptions::new().append(true).open(log_path)?,
                    control: None,
                }),
                changed: Condvar::new(),
            });
            if !terminal(&run.snapshot().status) {
                logging::record(
                    Level::Warn,
                    Target::State,
                    "run.recovery_uncertain",
                    Context::default(),
                    || json!({"run_id":id}),
                    || Value::Null,
                );
                run.status(
                    "interrupted",
                    "Daemon restarted. Prior execution outcome is uncertain; it was not retried.",
                )?;
            }
            let accepted = run.snapshot().record;
            jobs.observations
                .acknowledge(&accepted.owner, &accepted.observations, true)
                .unwrap_or_else(|e| {
                    logging::record(
                        Level::Error,
                        Target::State,
                        "observation.reconcile_failed",
                        Context::peer(&accepted.owner),
                        || json!({"error":e.to_string()}),
                        || Value::Null,
                    )
                });
            for event in run.retained_events("observation_batch") {
                if let Some(batches) = event.data["batches"].as_array()
                    && let Err(error) =
                        jobs.observations
                            .acknowledge(&accepted.owner, batches, true)
                {
                    logging::record(
                        Level::Error,
                        Target::State,
                        "observation.reconcile_failed",
                        Context::peer(&accepted.owner),
                        || json!({"error":error.to_string()}),
                        || Value::Null,
                    );
                }
            }
            jobs.runs.lock().unwrap().insert(id.into(), run);
        }
        Ok(jobs)
    }

    pub fn notify(&self) {
        let mut wake = self.wake.0.lock().unwrap();
        wake.pending = true;
        self.wake.1.notify_one();
    }
    pub fn start_dispatcher(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let signal = self.wake.clone();
        std::thread::spawn(move || {
            let mut delay = Duration::from_secs(60);
            loop {
                {
                    let mut wake = signal.0.lock().unwrap();
                    let (next, _) = signal
                        .1
                        .wait_timeout_while(wake, delay, |w| !w.pending && !w.stopped)
                        .unwrap();
                    wake = next;
                    if wake.stopped {
                        return;
                    }
                    wake.pending = false;
                }
                let Some(jobs) = weak.upgrade() else {
                    return;
                };
                if let Err(error) = jobs.observations.expire() {
                    logging::record(
                        Level::Error,
                        Target::State,
                        "observation.expiry_failed",
                        Context::default(),
                        || json!({"error":error.to_string()}),
                        || Value::Null,
                    );
                }
                jobs.dispatch_schedules();
                jobs.dispatch_observations();
                for resident in jobs.residents.all() {
                    if !resident.auto_activate {
                        continue;
                    }
                    if let Err(error) = jobs.activate(
                        &resident.owner,
                        &resident.id,
                        state::random_hex().unwrap_or_default(),
                        String::new(),
                        false,
                    ) {
                        logging::record(
                            Level::Warn,
                            Target::Workload,
                            "mailbox.activation_deferred",
                            Context {
                                peer_id: Some(&resident.owner),
                                thread_id: Some(&resident.id),
                                ..Context::default()
                            },
                            || json!({"error":error.to_string()}),
                            || Value::Null,
                        );
                    }
                }
                delay = jobs.schedules.delay(now());
            }
        });
        self.notify();
    }
    fn validate_address(&self, owner: &str, address: &Address) -> Result<()> {
        address.validate()?;
        match address {
            Address::Resident { id } => {
                self.residents.get(owner, id)?;
            }
            Address::Connector { instance, .. } => {
                self.connectors.get(owner, instance)?;
            }
            Address::Client { machine, .. } if machine != owner => return Err(
                "this host routes client mail only to the authenticated owner's client namespace"
                    .into(),
            ),
            _ => {}
        }
        Ok(())
    }
    pub fn send_mail(
        &self,
        owner: &str,
        sender: Address,
        spec: Send,
        source: String,
    ) -> Result<Message> {
        self.validate_address(owner, &sender)?;
        if let Some(prior) = self.mailboxes.retry(&sender, &spec, &source)? {
            return Ok(prior);
        }
        self.attachments.require(&spec.attachments)?;
        let network = self.network.lock().unwrap().clone();
        let (recipient_owner, destination) = if !matches!(spec.to, Address::Connector { .. }) {
            if let Some(state) = &network {
                crate::network::route(state, self, &spec.to)?
            } else {
                self.validate_address(owner, &spec.to)?;
                (owner.to_owned(), None)
            }
        } else {
            self.validate_address(owner, &spec.to)?;
            (owner.to_owned(), None)
        };
        if let Some(reply) = &spec.reply_to {
            if let Some(state) = &network {
                if !matches!(reply, Address::Connector { .. }) {
                    crate::network::route(state, self, reply)?;
                } else {
                    self.validate_address(owner, reply)?;
                }
            } else {
                self.validate_address(owner, reply)?;
            }
        }
        if matches!(&sender, Address::Connector { .. }) {
            return Err("native connector senders enter through scoped intake".into());
        }
        if let Address::Connector {
            instance,
            conversation,
        } = &spec.to
        {
            if !spec.attachments.is_empty() {
                return Err(
                    "file references are supported by Boxen mail, not external connector sends"
                        .into(),
                );
            }
            let Address::Resident { id } = &sender else {
                return Err("external sends have a bound resident sender".into());
            };
            let connector = if let Some(service) = &spec.service {
                let connector = self.connectors.get(owner, instance)?;
                let tool = crate::message_service::declared(
                    &connector,
                    service["operation"]
                        .as_str()
                        .ok_or("service operation required")?,
                )?;
                crate::messaging::authorize_action(
                    self,
                    owner,
                    id,
                    instance,
                    conversation,
                    &tool.permission,
                )?
            } else {
                crate::messaging::authorize(self, owner, id, instance, conversation, true)?
            };
            if !spec.subject.is_empty()
                && !connector
                    .definition
                    .capabilities
                    .iter()
                    .any(|c| c == "subjects")
            {
                return Err("this connector does not transport message subjects".into());
            }
            if let Some(reply) = &spec.in_reply_to {
                if !connector
                    .definition
                    .capabilities
                    .iter()
                    .any(|c| c == "replies")
                {
                    return Err("connector instance does not declare replies".into());
                }
                let original = self.mailboxes.get(owner, reply)?;
                if original.to != sender && original.sender != sender {
                    return Err("external reply reference is unavailable to this sender".into());
                }
                let origin = original
                    .origin
                    .ok_or("external reply references a message with native provenance")?;
                if origin["instance"] != *instance || origin["conversation"] != *conversation {
                    return Err("reply provenance belongs to another connector conversation".into());
                }
            }
        }
        let message = if destination.is_none() {
            self.mailboxes
                .send(&recipient_owner, sender, spec, source)?
        } else {
            self.mailboxes.accept_routed(
                &recipient_owner,
                sender,
                spec,
                source,
                None,
                destination.as_deref(),
            )?
        };
        self.notify();
        Ok(message)
    }
    pub fn messaging_notify(&self) {
        self.notify();
    }
    fn presented(&self, owner: &str, resident: &str) -> HashMap<String, String> {
        self.runs
            .lock()
            .unwrap()
            .values()
            .filter_map(|r| {
                let s = r.snapshot();
                if s.record.owner != owner || s.record.spec.resident_id.as_deref() != Some(resident)
                {
                    return None;
                }
                s.record.mailbox.map(|a| (s.record.spec.id, a.message_ids))
            })
            .flat_map(|(run, ids)| ids.into_iter().map(move |id| (id, run.clone())))
            .collect()
    }
    pub fn inbox(&self, owner: &str, address: Address, after: u64) -> Result<Value> {
        self.mailboxes.check()?;
        self.validate_address(owner, &address)?;
        let presented = match &address {
            Address::Resident { id } => self.presented(owner, id),
            _ => HashMap::new(),
        };
        let messages = self.mailboxes.messages(owner, &address, after);
        let next_after = messages.last().map(|m| m.seq).unwrap_or(after);
        let values: Vec<_> = messages
            .iter()
            .map(|m| {
                let mut v = serde_json::to_value(m).unwrap();
                v["presented_in"] = json!(presented.get(&m.id));
                v["delivery"] = json!(if presented.contains_key(&m.id) {
                    "presented"
                } else {
                    "queued"
                });
                v
            })
            .collect();
        Ok(json!({"address":address,"messages":values,"next_after":next_after}))
    }
    pub fn activate(
        self: &Arc<Self>,
        owner: &str,
        id: &str,
        run_id: String,
        directions: String,
        manual: bool,
    ) -> Result<Option<Snapshot>> {
        let _transition = self.transitions.lock().unwrap();
        self.activate_locked(
            owner,
            id,
            run_id,
            directions,
            if manual {
                ActivationCause::Manual
            } else {
                ActivationCause::Mail
            },
        )
    }
    fn activate_locked(
        self: &Arc<Self>,
        owner: &str,
        id: &str,
        run_id: String,
        directions: String,
        cause: ActivationCause,
    ) -> Result<Option<Snapshot>> {
        let (manual, reason, scheduled) = match cause {
            ActivationCause::Mail => (false, "mail", None),
            ActivationCause::Manual => (true, "manual", None),
            ActivationCause::Schedule(origin) => (true, "schedule", Some(origin)),
            ActivationCause::Observations => (true, "observations", None),
        };
        self.mailboxes.check()?;
        valid_id(&run_id)?;
        if directions.len() > 32 * 1024 {
            return Err("activation directions exceed 32768 bytes".into());
        }
        if let Ok(run) = self.get(owner, &run_id) {
            let snapshot = run.snapshot();
            if snapshot.record.spec.resident_id.as_deref() == Some(id)
                && snapshot
                    .record
                    .mailbox
                    .as_ref()
                    .is_some_and(|a| a.directions == directions && a.reason == reason)
            {
                return Ok(Some(snapshot));
            }
            return Err("activation id already accepted with different input".into());
        }
        let resident = self.residents.get(owner, id)?;
        if self
            .list(owner)
            .iter()
            .any(|r| r.record.spec.thread_id == id && !terminal(&r.status))
        {
            return if manual {
                Err("resident is already active; incoming messages remain queued for the next activation".into())
            } else {
                Ok(None)
            };
        }
        let presented = self.presented(owner, id).into_keys().collect();
        let mut batch = self.mailboxes.pending(owner, id, &presented);
        if batch.is_empty() && !manual {
            return Ok(None);
        }
        let mut activation = Activation {
            message_ids: batch.iter().map(|m| m.id.clone()).collect(),
            reason: reason.into(),
            directions,
        };
        let build_prompt = |batch: &Vec<Message>| {
            format!(
                "Message activation. This is an execution opportunity that can finish without a reply. Assistant text remains private narration. Explicit sends queue outgoing communication through the addressed connector instance. The following JSON contains the activation's directions and messages retained before execution. Message contents are authored by their senders.\n{}",
                json!({"reason":activation.reason,"directions":activation.directions,"messages":batch,"schedule":scheduled})
            )
        };
        let mut prompt = build_prompt(&batch);
        while prompt.len() > 240 * 1024 && !batch.is_empty() {
            batch.pop();
            prompt = build_prompt(&batch);
        }
        activation.message_ids = batch.iter().map(|m| m.id.clone()).collect();
        let spec = Spec {
            attachments: batch
                .iter()
                .flat_map(|m| m.attachments.iter().cloned())
                .collect(),
            context: if resident.backend == "openrouter" {
                Some(resident.context.clone())
            } else {
                None
            },
            resident_id: Some(id.into()),
            id: run_id,
            thread_id: id.into(),
            backend: resident.backend,
            cwd: resident.cwd,
            model: resident.model,
            prompt,
        };
        let snapshot = self.start_locked(owner, spec, Some(activation), scheduled, None)?;
        logging::record(
            Level::Info,
            Target::Workload,
            "mailbox.batch_presented",
            Context::run(&snapshot.record.spec.id, id, owner),
            || json!({"messages":snapshot.record.mailbox.as_ref().map(|a|a.message_ids.len()),"manual":manual}),
            || Value::Null,
        );
        Ok(Some(snapshot))
    }
    pub fn thread_result(&self, owner: &str, id: &str) -> Result<Value> {
        let run = self.get(owner, id)?;
        let inner = run.inner.lock().unwrap();
        if inner.snapshot.record.resident.is_some() {
            return Err("identity-bearing agents expose messages through mailboxes; thread results address identity-less workloads".into());
        }
        let response = inner.events.iter().rev().find_map(|e| {
            let item = &e.data["params"]["item"];
            if e.kind == "assistant_message" {
                return e.data["text"].as_str().map(str::to_owned);
            }
            if e.kind == "backend"
                && e.data["method"] == "item/completed"
                && item["type"] == "agentMessage"
                && (item["phase"].is_null() || item["phase"] == "final_answer")
            {
                item["text"].as_str().map(str::to_owned)
            } else {
                None
            }
        });
        Ok(
            json!({"snapshot":inner.snapshot,"done":terminal(&inner.snapshot.status),"response":if terminal(&inner.snapshot.status){response}else{None}}),
        )
    }
    pub fn get(&self, owner: &str, id: &str) -> Result<Arc<Run>> {
        valid_id(id)?;
        let run = self
            .runs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or("run not found")?;
        if run.snapshot().record.owner != owner {
            return Err("run belongs to a different peer".into());
        }
        Ok(run)
    }

    /// Instantaneous metadata, not a lease against future messages/activations.
    pub fn thread_activity(&self, owner: &str, id: &str) -> Result<Value> {
        valid_id(id)?;
        let (count, active) = {
            let runs = self.runs.lock().unwrap();
            let mut count = 0_u64;
            let mut active = 0_u64;
            for run in runs.values() {
                let inner = run.inner.lock().unwrap();
                let snapshot = &inner.snapshot;
                if snapshot.record.owner == owner && snapshot.record.spec.thread_id == id {
                    count += 1;
                    if !terminal(&snapshot.status) {
                        active += 1;
                    }
                }
            }
            (count, active)
        };
        logging::record(
            Level::Debug,
            Target::Workload,
            "thread.activity_read",
            Context::peer(owner),
            || json!({"thread_id":id,"known_runs":count,"active_runs":active}),
            || Value::Null,
        );
        Ok(
            json!({"thread_id":id,"known":count>0,"run_count":count,"active_runs":active,"idle":active==0}),
        )
    }

    pub(crate) fn resident_activity(&self, owner: &str, ids: &[String]) -> (usize, usize) {
        let runs = self.runs.lock().unwrap();
        let mut active = std::collections::HashSet::new();
        let mut queued = std::collections::HashSet::new();
        let mut presented: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for run in runs.values() {
            let inner = run.inner.lock().unwrap();
            let s = &inner.snapshot;
            if s.record.owner != owner {
                continue;
            }
            let Some(id) = s
                .record
                .spec
                .resident_id
                .as_ref()
                .filter(|id| ids.contains(id))
            else {
                continue;
            };
            if s.status == "running" {
                active.insert(id.clone());
            } else if s.status == "starting" {
                queued.insert(id.clone());
            }
            if let Some(batch) = &s.record.mailbox {
                presented
                    .entry(id.clone())
                    .or_default()
                    .extend(batch.message_ids.iter().cloned());
            }
        }
        drop(runs);
        for id in ids {
            if self
                .mailboxes
                .has_pending(owner, id, presented.entry(id.clone()).or_default())
            {
                queued.insert(id.clone());
            }
        }
        (active.len(), queued.len())
    }
    pub fn operator_list(&self) -> Vec<Snapshot> {
        let mut records: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .map(|r| r.snapshot())
            .collect();
        records.sort_by_key(|s| (s.record.created_at, s.record.spec.id.clone()));
        records
    }
    pub fn operator_summary_page(&self, after: Option<SummaryCursor>) -> Result<Value> {
        const PAGE_SIZE: usize = 128;
        if let Some(cursor) = &after {
            valid_id(&cursor.id)?;
        }
        let mut page: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .filter_map(|run| {
                let inner = run.inner.lock().unwrap();
                let record = &inner.snapshot.record;
                let cursor = SummaryCursor {
                    created_at: record.created_at,
                    id: record.spec.id.clone(),
                };
                (after.as_ref().is_none_or(|after| &cursor > after))
                    .then(|| (cursor, Arc::clone(run)))
            })
            .collect();
        page.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let has_more = page.len() > PAGE_SIZE;
        page.truncate(PAGE_SIZE);
        let summaries: Vec<_> = page
            .iter()
            .map(|(_, run)| {
                let inner = run.inner.lock().unwrap();
                let snapshot = &inner.snapshot;
                let record = &snapshot.record;
                let spec = &record.spec;
                let resident_id = spec.resident_id.as_deref()
                    .or_else(|| record.resident.as_ref().map(|resident| resident.id.as_str()));
                let title: String = spec.prompt.lines().next().filter(|line| !line.trim().is_empty())
                    .or_else(|| spec.attachments.first().map(|file| file.name.as_str()))
                    .unwrap_or("Untitled thread").chars().take(72).collect();
                json!({
                    "record": {
                        "spec": {"id":spec.id,"thread_id":spec.thread_id,"backend":spec.backend,
                                 "resident_id":resident_id},
                        "created_at": record.created_at,
                        "mailbox": record.mailbox.as_ref().map(|m| json!({"reason":m.reason.chars().take(128).collect::<String>()})),
                        "imported": record.imported.as_ref().map(|_| true),
                        "transcript_import": record.transcript_import.as_ref().map(|_| true)
                    },
                    "title": title,
                    "status": snapshot.status,
                    "last_seq": snapshot.last_seq
                })
            })
            .collect();
        let next_after = page.last().filter(|_| has_more).map(|(cursor, _)| cursor);
        Ok(json!({"summaries":summaries,"next_after":next_after}))
    }
    pub fn operator_run(&self, id: &str) -> Result<Arc<Run>> {
        valid_id(id)?;
        self.runs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| "run not found on this machine".into())
    }
    pub fn thread_owner(&self, id: &str) -> Result<Option<String>> {
        valid_id(id)?;
        let runs = self.runs.lock().unwrap();
        let mut owners = std::collections::BTreeSet::new();
        for r in runs.values() {
            let inner = r.inner.lock().unwrap();
            if inner.snapshot.record.spec.thread_id == id {
                owners.insert(inner.snapshot.record.owner.clone());
            }
        }
        if owners.len() > 1 {
            return Err("thread identity has conflicting historical namespaces".into());
        }
        Ok(owners.into_iter().next())
    }
    pub fn owners(&self) -> Vec<String> {
        let mut owners: std::collections::BTreeSet<String> =
            self.residents.all().into_iter().map(|r| r.owner).collect();
        owners.insert(self.machine_id().into());
        owners.extend(
            self.runs
                .lock()
                .unwrap()
                .values()
                .map(|r| r.inner.lock().unwrap().snapshot.record.owner.clone()),
        );
        owners.extend(self.definitions.owners());
        owners.extend(self.connectors.owners());
        owners.extend(self.mailboxes.owners());
        owners.extend(self.schedules.owners());
        owners.extend(self.observations.owners());
        owners.into_iter().collect()
    }
    pub fn list(&self, owner: &str) -> Vec<Snapshot> {
        let mut records: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .map(|run| run.snapshot())
            .filter(|snapshot| snapshot.record.owner == owner)
            .collect();
        records.sort_by_key(|s| (s.record.created_at, s.record.spec.id.clone()));
        records
    }

    pub fn definition_operation(&self, owner: &str, args: Value) -> Result<Value> {
        let action = args["action"].as_str().unwrap_or("list");
        let id = args["id"].as_str().unwrap_or("");
        let outcome = (|| -> Result<Value> {
            match action {
                "list" => Ok(self.definitions.list(owner)),
                "get" => Ok(serde_json::to_value(self.definitions.get(
                    owner,
                    id,
                    args["revision"].as_u64(),
                )?)?),
                "history" => self.definitions.history(
                    owner,
                    id,
                    args["after"].as_u64().unwrap_or(0),
                    args["limit"].as_u64().unwrap_or(20) as usize,
                ),
                "create" => Ok(serde_json::to_value(
                    self.definitions
                        .create(owner, serde_json::from_value(args["spec"].clone())?)?,
                )?),
                "update" => Ok(serde_json::to_value(
                    self.definitions.update(
                        owner,
                        id,
                        args["expected_revision"]
                            .as_u64()
                            .ok_or("expected_revision required")?,
                        args["patch"]
                            .as_object()
                            .ok_or("definition patch is an object")?,
                    )?,
                )?),
                "instantiate" => {
                    let request: crate::resident_definitions::Instantiate =
                        serde_json::from_value(args["spec"].clone())?;
                    valid_id(&request.id)?;
                    valid_id(&request.definition_id)?;
                    let _transition = self.transitions.lock().unwrap();
                    if let Some(existing) = self.residents.existing_instance(owner, &request)? {
                        self.mailboxes.register(owner, &existing.id)?;
                        return Ok(existing.value());
                    }
                    if self
                        .runs
                        .lock()
                        .unwrap()
                        .values()
                        .any(|r| r.snapshot().record.spec.thread_id == request.id)
                    {
                        return Err("thread id is already in use".into());
                    }
                    let source = self.definitions.get(
                        owner,
                        &request.definition_id,
                        Some(request.definition_revision),
                    )?;
                    let mut initial =
                        crate::resident_definitions::patched(&source.spec, &request.overrides)?;
                    initial.id = request.id.clone();
                    let origin = crate::resident_definitions::Origin {
                        id: request.definition_id.clone(),
                        instantiated_revision: source.revision,
                        initial_overrides: request.overrides.keys().cloned().collect(),
                        last_applied_fields: crate::resident_definitions::FIELDS
                            .iter()
                            .filter(|f| !request.overrides.contains_key(**f))
                            .map(|f| (f.to_string(), source.revision))
                            .collect(),
                    };
                    let resident = self.residents.create_with_origin(
                        owner,
                        initial,
                        Some(request.clone()),
                        Some(origin),
                        None,
                    )?;
                    self.mailboxes.register(owner, &resident.id)?;
                    logging::record(
                        Level::Info,
                        Target::State,
                        "resident_definition.instantiated",
                        Context {
                            thread_id: Some(&resident.id),
                            peer_id: Some(owner),
                            ..Context::default()
                        },
                        || json!({"definition_id":request.definition_id,"definition_revision":source.revision,"override_fields":request.overrides.keys().collect::<Vec<_>>()}),
                        || Value::Null,
                    );
                    Ok(resident.value())
                }
                "instances" => {
                    valid_id(id)?;
                    Ok(json!(
                        self.residents
                            .list(owner)
                            .iter()
                            .filter(|r| r.definition.as_ref().is_some_and(|d| d.id == id))
                            .map(|r| r.value())
                            .collect::<Vec<_>>()
                    ))
                }
                "apply" => {
                    let _transition = self.transitions.lock().unwrap();
                    let resident_id = args["resident_id"].as_str().ok_or("resident_id required")?;
                    let current = self.residents.get(owner, resident_id)?;
                    let source = current
                        .definition
                        .as_ref()
                        .ok_or("standalone resident has no definition")?;
                    let revision = args["definition_revision"]
                        .as_u64()
                        .ok_or("definition_revision required")?;
                    let definition = self.definitions.get(owner, &source.id, Some(revision))?;
                    let fields: Vec<String> = serde_json::from_value(args["fields"].clone())?;
                    let has_history = self
                        .runs
                        .lock()
                        .unwrap()
                        .values()
                        .any(|r| r.snapshot().record.spec.thread_id == resident_id);
                    let resident = self.residents.apply_definition(
                        owner,
                        resident_id,
                        args["expected_revision"]
                            .as_u64()
                            .ok_or("expected_revision required")?,
                        &definition,
                        &fields,
                        has_history,
                    )?;
                    self.notify();
                    Ok(resident.value())
                }
                _ => Err("unknown resident definition operation".into()),
            }
        })();
        logging::record(
            if outcome.is_ok() {
                Level::Debug
            } else {
                Level::Warn
            },
            Target::State,
            "resident_definition.operation",
            Context::peer(owner),
            || json!({"action":action,"definition_id":id,"instance_id":args["resident_id"],"accepted":outcome.is_ok(),"error":outcome.as_ref().err().map(|e|e.to_string())}),
            || Value::Null,
        );
        outcome
    }
    pub fn create_resident(&self, owner: &str, spec: Create) -> Result<Resident> {
        let _transition = self.transitions.lock().unwrap();
        if self
            .runs
            .lock()
            .unwrap()
            .values()
            .any(|r| r.snapshot().record.spec.thread_id == spec.id)
        {
            // Retrying creation after the first conversation is still idempotent.
            if self.residents.get(owner, &spec.id).is_err() {
                return Err("thread id is already in use".into());
            }
        }
        let resident = self.residents.create(owner, spec)?;
        self.mailboxes.register(owner, &resident.id)?;
        Ok(resident)
    }
    pub fn start(self: &Arc<Self>, owner: &str, spec: Spec) -> Result<Snapshot> {
        self.start_after(owner, spec, None)
    }
    pub fn start_after(
        self: &Arc<Self>,
        owner: &str,
        spec: Spec,
        after: Option<&str>,
    ) -> Result<Snapshot> {
        let _transition = self.transitions.lock().unwrap();
        self.start_locked(owner, spec, None, None, after)
    }
    fn start_locked(
        self: &Arc<Self>,
        owner: &str,
        mut spec: Spec,
        activation: Option<Activation>,
        scheduled: Option<crate::scheduler::Origin>,
        after: Option<&str>,
    ) -> Result<Snapshot> {
        logging::record(
            Level::Info,
            Target::Workload,
            "submission.received",
            Context::run(&spec.id, &spec.thread_id, owner),
            || json!({"backend":spec.backend,"cwd":spec.cwd,"model":spec.model,"prompt_bytes":spec.prompt.len()}),
            || json!({"prompt":spec.prompt}),
        );
        valid_id(&spec.id)?;
        valid_id(&spec.thread_id)?;
        let mut runs = self.runs.lock().unwrap();
        if let Some(existing) = runs.get(&spec.id) {
            let snapshot = existing.snapshot();
            let mut accepted_input = snapshot.record.spec.clone();
            if snapshot.record.automatic_cwd {
                accepted_input.cwd.clear();
            }
            if snapshot.record.owner == owner
                && accepted_input == spec
                && snapshot.record.after.as_deref() == after
            {
                logging::record(
                    Level::Info,
                    Target::Workload,
                    "submission.reused",
                    Context::run(&spec.id, &spec.thread_id, owner),
                    || json!({"status":snapshot.status}),
                    || Value::Null,
                );
                return Ok(snapshot);
            }
            return Err("run id already exists with different input or owner".into());
        }
        if spec.resident_id.is_some() && activation.is_none() {
            return Err("identity-bearing agents receive mailbox messages; direct turn submission is unavailable".into());
        }
        let resident =
            self.residents
                .context(owner, &spec.thread_id, spec.resident_id.as_deref())?;
        let automatic_cwd = resident.is_none() && spec.cwd.is_empty();
        if let Some(r) = &resident
            && (spec.cwd != r.cwd || spec.model != r.model || spec.backend != r.backend)
        {
            return Err("resident working directory and model are pinned to its definition".into());
        }
        if !matches!(spec.backend.as_str(), "codex" | "openrouter") {
            return Err("unsupported backend".into());
        }
        if spec.backend == "openrouter" && activation.is_none() {
            self.inference.ready()?;
        }
        if let Some(settings) = &spec.context {
            settings.validate()?;
            if spec.backend != "openrouter" {
                return Err("Boxen context policies apply to the built-in runtime; managed runtimes own their native compaction".into());
            }
        }
        if activation.is_none() {
            self.attachments.require(&spec.attachments)?;
        }
        if (spec.prompt.is_empty() && spec.attachments.is_empty()) || spec.prompt.len() > 256 * 1024
        {
            return Err("prompt must contain 1–262144 bytes".into());
        }
        let automatic_path = if automatic_cwd {
            let root = self.directory.parent().ok_or("run directory has no home")?;
            let path = root.canonicalize()?.join("threads").join(&spec.thread_id);
            spec.cwd = path
                .to_str()
                .ok_or("automatic working directory requires a UTF-8 path")?
                .into();
            Some(path)
        } else {
            None
        };
        if !automatic_cwd
            && (!std::path::Path::new(&spec.cwd).is_absolute()
                || !std::path::Path::new(&spec.cwd).is_dir())
        {
            return Err(
                "working directory must exist on the executing machine and be absolute".into(),
            );
        }
        if runs
            .values()
            .filter(|run| !terminal(&run.snapshot().status))
            .count()
            >= 8
        {
            return Err("this prototype allows eight active runs per machine".into());
        }
        let mut previous: Vec<_> = runs
            .values()
            .map(|r| r.snapshot())
            .filter(|r| r.record.spec.thread_id == spec.thread_id)
            .collect();
        previous.sort_by_key(|s| (s.record.created_at, s.record.spec.id.clone()));
        for prior in &previous {
            if prior.record.owner != owner {
                return Err("thread belongs to a different peer".into());
            }
            if !terminal(&prior.status) {
                return Err("thread already has an active run; another request is accepted after it ends, and steering is a separate control operation".into());
            }
        }
        let head = previous.last();
        if let Some(after) = after {
            valid_id(after)?;
            if resident.is_some() || head.is_none_or(|head| head.record.spec.id != after) {
                return Err("follow-up source is no longer the latest anonymous run".into());
            }
        }
        if let Some(head) = head {
            let old = &head.record.spec;
            if old.backend != spec.backend {
                return Err("this conversation's runtime is fixed".into());
            }
            if after.is_none() {
                if spec.backend == "openrouter" && resident.is_none() && old.model != spec.model {
                    return Err("this slice pins a thread’s inference model unless the latest run ID is supplied".into());
                }
                if old.cwd != spec.cwd {
                    return Err("changing the working directory requires the latest run ID".into());
                }
            }
        }
        let execution_change = head.filter(|head| {
            head.record.spec.cwd != spec.cwd || head.record.spec.model != spec.model
        }).filter(|_| resident.is_none()).map(|head| {
            json!({
                "previous_run_id":head.record.spec.id,
                "previous":{"machine":self.machine_id(),"cwd":head.record.spec.cwd,"model":head.record.spec.model},
                "current":{"machine":self.machine_id(),"cwd":spec.cwd,"model":spec.model}
            })
        });
        let resume = previous.iter().rev().find_map(|s| s.backend_thread.clone());
        let recovery = previous.last().filter(|r|r.status=="interrupted").map(|r|json!({"prior_run_id":r.record.spec.id,"detail":"Previous execution and tool effects are uncertain. This run continues the canonical conversation with a new message. Boxen has not resubmitted the previous prompt or replayed its tool actions; repeating those actions can duplicate effects that already occurred."}));
        let history = History {
            native_import: previous.iter().find_map(|r| r.record.imported.clone()),
            directory: self.directory.clone(),
            thread_id: spec.thread_id.clone(),
            rebuild_reason:previous.iter().rev().find(|r|r.backend_thread.is_some()).filter(|r|spec.resident_id.is_some()&&r.record.mailbox.is_none()&&resident.as_ref().is_none_or(|r|r.promotion.is_none())).map(|_|"Resident communication migrated from direct turns to mailbox activations; tools are registered in a new backend session beneath the same canonical conversation.".into()),
            runs: previous
                .iter()
                .map(|r| runs[&r.record.spec.id].clone())
                .collect(),
        };
        let runtime_policy = crate::runtime_policy::Policy::load(
            self.directory.parent().ok_or("run directory has no home")?,
        )?;
        if let Some(path) = automatic_path {
            for dir in [path.parent().expect("thread has a parent"), path.as_path()] {
                if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err("automatic working directory cannot be a symbolic link".into());
                }
                state::private_dir(dir)?;
            }
        }
        let path = self.directory.join(&spec.id);
        state::private_dir(&path)?;
        let record = Record {
            automatic_cwd,
            after: after.map(str::to_owned),
            execution_change,
            runtime_policy,
            imported: None,
            transcript_import: None,
            observations: self.observations.batches(owner, &spec.thread_id),
            scheduled,
            spec: spec.clone(),
            resident,
            mailbox: activation,
            recovery,
            owner: owner.into(),
            // Preserve acceptance order even for submissions in the same millisecond
            // or after a wall-clock correction. Follow-ups must resume the latest run.
            created_at: now().max(
                runs.values()
                    .map(|r| r.snapshot().record.created_at)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1),
            ),
        };
        state::publish_new(&path.join("record.json"), &serde_json::to_vec(&record)?)?;
        state::write_new(&path.join("events.jsonl"), b"")?;
        let (sender, receiver) = mpsc::channel();
        let run = Arc::new(Run {
            inner: Mutex::new(Inner {
                cancel_requested: false,
                snapshot: Snapshot {
                    record,
                    status: "starting".into(),
                    backend_thread: None,
                    last_seq: 0,
                },
                events: Vec::new(),
                file: OpenOptions::new()
                    .append(true)
                    .open(path.join("events.jsonl"))?,
                control: Some(sender),
            }),
            changed: Condvar::new(),
        });
        run.status("starting", "Accepted by executing machine")?;
        if let Some(change) = &run.snapshot().record.execution_change {
            run.append("execution_settings_changed", change.clone())?;
        }
        if let Some(origin) = &run.snapshot().record.scheduled {
            run.append("scheduled_event", serde_json::to_value(origin)?)?;
        }
        runs.insert(spec.id.clone(), run.clone());
        if let Err(error) =
            self.observations
                .acknowledge(owner, &run.snapshot().record.observations, true)
        {
            run.append("observation_incident",json!({"detail":"Accepted input retained; pending-position update failed. Other work remains available.","error":error.to_string()}))?;
        }
        let snapshot = run.snapshot();
        logging::record(
            Level::Info,
            Target::Workload,
            "submission.accepted",
            Context::run(&spec.id, &spec.thread_id, owner),
            || json!({"backend":spec.backend,"cwd":spec.cwd,"automatic_cwd":automatic_cwd,"runtime_policy":snapshot.record.runtime_policy}),
            || Value::Null,
        );
        let binary = self.codex_path.clone();
        let jobs = self.clone();
        std::thread::spawn(move || {
            let result = if spec.backend == "openrouter" {
                crate::inference::execute(&spec, run.clone(), receiver, jobs.clone(), history)
            } else {
                codex::execute(
                    &binary,
                    &spec,
                    resume,
                    run.clone(),
                    receiver,
                    jobs.clone(),
                    history,
                )
            };
            if let Err(error) = result {
                run.diagnostic(
                    Level::Error,
                    "execution.failed",
                    || json!({"error":error.to_string()}),
                    || Value::Null,
                );
                let status = if matches!(run.snapshot().status.as_str(), "running" | "waiting") {
                    "interrupted"
                } else {
                    "failed"
                };
                let _ = run.status(status, &error.to_string());
            }
            run.inner.lock().unwrap().control = None;
            jobs.notify();
        });
        Ok(snapshot)
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        let mut wake = self.wake.0.lock().unwrap();
        wake.stopped = true;
        self.wake.1.notify_all();
    }
}
