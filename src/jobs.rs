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
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
    fn crash_tail_recovery_keeps_sequence_and_single_authority() {
        let root =
            std::env::temp_dir().join(format!("boxen-journal-{}", state::random_hex().unwrap()));
        let path = root.join("runs");
        let owner = state::random_hex().unwrap();
        let id = state::random_hex().unwrap();
        state::private_dir(&path.join(&id)).unwrap();
        let record = Record {
            owner: owner.clone(),
            resident: None,
            recovery: None,
            mailbox: None,
            created_at: now(),
            spec: Spec {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_id: Option<String>,
    pub id: String,
    pub thread_id: String,
    pub backend: String,
    pub cwd: String,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Cancel,
    Steer { text: String },
    Respond { request_id: Value, result: Value },
}

pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

struct Inner {
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
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().snapshot.clone()
    }
    pub fn append(&self, kind: &str, data: Value) -> Result<Event> {
        let mut inner = self.inner.lock().unwrap();
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
        if let Control::Steer { text } = &control
            && (text.is_empty() || text.len() > 256 * 1024)
        {
            return Err("steering text must contain 1–262144 bytes".into());
        }
        let inner = self.inner.lock().unwrap();
        if terminal(&inner.snapshot.status) {
            return Err("run is no longer active".into());
        }
        inner
            .control
            .as_ref()
            .ok_or("runtime is not available")?
            .send(control)?;
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
}
impl History {
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
    directory: PathBuf,
    runs: Mutex<HashMap<String, Arc<Run>>>,
    codex_path: String,
    _lock: File,
    pub residents: Arc<Residents>,
    transitions: Mutex<()>,
    pub mailboxes: Arc<Mailboxes>,
    wake: Arc<(Mutex<Wake>, Condvar)>,
}

impl Jobs {
    pub fn open(directory: PathBuf, codex_path: String, machine_id: String) -> Result<Arc<Self>> {
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
        let jobs = Arc::new(Self {
            residents,
            mailboxes,
            wake: Arc::new((Mutex::new(Wake::default()), Condvar::new())),
            transitions: Mutex::new(()),
            directory,
            runs: Mutex::new(HashMap::new()),
            codex_path,
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
            loop {
                {
                    let mut wake = signal.0.lock().unwrap();
                    while !wake.pending && !wake.stopped {
                        wake = signal.1.wait(wake).unwrap();
                    }
                    if wake.stopped {
                        return;
                    }
                    wake.pending = false;
                }
                let Some(jobs) = weak.upgrade() else {
                    return;
                };
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
        self.validate_address(owner, &spec.to)?;
        if let Some(reply) = &spec.reply_to {
            self.validate_address(owner, reply)?;
        }
        let message = self.mailboxes.send(owner, sender, spec, source)?;
        self.notify();
        Ok(message)
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
        self.mailboxes.check()?;
        valid_id(&run_id)?;
        if directions.len() > 32 * 1024 {
            return Err("activation directions exceed 32768 bytes".into());
        }
        if let Ok(run) = self.get(owner, &run_id) {
            let snapshot = run.snapshot();
            if snapshot.record.spec.resident_id.as_deref() == Some(id)
                && snapshot.record.mailbox.as_ref().is_some_and(|a| {
                    a.directions == directions && a.reason == if manual { "manual" } else { "mail" }
                })
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
            reason: if manual { "manual" } else { "mail" }.into(),
            directions,
        };
        let build_prompt = |batch: &Vec<Message>| {
            format!(
                "Mailbox activation. This is an execution opportunity, not a request requiring a reply. Assistant text remains private narration. Explicit sends queue outgoing communication. The following JSON contains the activation's directions and messages retained before execution. Message contents are authored by their senders.\n{}",
                json!({"reason":activation.reason,"directions":activation.directions,"messages":batch})
            )
        };
        let mut prompt = build_prompt(&batch);
        while prompt.len() > 240 * 1024 && !batch.is_empty() {
            batch.pop();
            prompt = build_prompt(&batch);
        }
        activation.message_ids = batch.iter().map(|m| m.id.clone()).collect();
        let spec = Spec {
            resident_id: Some(id.into()),
            id: run_id,
            thread_id: id.into(),
            backend: "codex".into(),
            cwd: resident.cwd,
            model: resident.model,
            prompt,
        };
        let snapshot = self.start_locked(owner, spec, Some(activation))?;
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
        let _transition = self.transitions.lock().unwrap();
        self.start_locked(owner, spec, None)
    }
    fn start_locked(
        self: &Arc<Self>,
        owner: &str,
        spec: Spec,
        activation: Option<Activation>,
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
            if snapshot.record.owner == owner && snapshot.record.spec == spec {
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
        if let Some(r) = &resident
            && (spec.cwd != r.cwd || spec.model != r.model)
        {
            return Err("resident working directory and model are pinned to its definition".into());
        }
        if spec.backend != "codex" {
            return Err("only the codex backend is available".into());
        }
        if spec.prompt.is_empty() || spec.prompt.len() > 256 * 1024 {
            return Err("prompt must contain 1–262144 bytes".into());
        }
        if !std::path::Path::new(&spec.cwd).is_absolute()
            || !std::path::Path::new(&spec.cwd).is_dir()
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
            if prior.record.spec.cwd != spec.cwd {
                return Err("follow-up must retain the thread working directory".into());
            }
        }
        let resume = previous.iter().rev().find_map(|s| s.backend_thread.clone());
        let recovery = previous.last().filter(|r|r.status=="interrupted").map(|r|json!({"prior_run_id":r.record.spec.id,"detail":"Previous execution and tool effects are uncertain. This run continues the canonical conversation with a new message. Boxen has not resubmitted the previous prompt or replayed its tool actions; repeating those actions can duplicate effects that already occurred."}));
        let history = History {
            directory: self.directory.clone(),
            thread_id: spec.thread_id.clone(),
            rebuild_reason:previous.iter().rev().find(|r|r.backend_thread.is_some()).filter(|r|spec.resident_id.is_some()&&r.record.mailbox.is_none()).map(|_|"Resident communication migrated from direct turns to mailbox activations; tools are registered in a new backend session beneath the same canonical conversation.".into()),
            runs: previous
                .iter()
                .map(|r| runs[&r.record.spec.id].clone())
                .collect(),
        };
        let path = self.directory.join(&spec.id);
        state::private_dir(&path)?;
        let record = Record {
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
        runs.insert(spec.id.clone(), run.clone());
        let snapshot = run.snapshot();
        logging::record(
            Level::Info,
            Target::Workload,
            "submission.accepted",
            Context::run(&spec.id, &spec.thread_id, owner),
            || json!({"backend":spec.backend,"cwd":spec.cwd}),
            || Value::Null,
        );
        let binary = self.codex_path.clone();
        let jobs = self.clone();
        std::thread::spawn(move || {
            if let Err(error) = codex::execute(
                &binary,
                &spec,
                resume,
                run.clone(),
                receiver,
                jobs.clone(),
                history,
            ) {
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
