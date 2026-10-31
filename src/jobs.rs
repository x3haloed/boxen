//! The executing daemon is the sole authority for runs and their ordered events.
use crate::logging::{self, Context, Level, Target};
use crate::{Result, codex, state};
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
        let path =
            std::env::temp_dir().join(format!("boxen-journal-{}", state::random_hex().unwrap()));
        let owner = state::random_hex().unwrap();
        let id = state::random_hex().unwrap();
        state::private_dir(&path.join(&id)).unwrap();
        let record = Record {
            owner: owner.clone(),
            created_at: now(),
            spec: Spec {
                id: id.clone(),
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
        let jobs = Jobs::open(path.clone(), "missing-codex".into()).unwrap();
        assert!(Jobs::open(path.clone(), "missing-codex".into()).is_err());
        let run = jobs.get(&owner, &id).unwrap();
        let (events, snapshot) = run.batch(0, false).unwrap();
        assert_eq!(snapshot.status, "interrupted");
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert!(run.batch(3, false).is_err());
        assert!(jobs.get(&state::random_hex().unwrap(), &id).is_err());
        drop(run);
        drop(jobs);
        let reopened = Jobs::open(path.clone(), "missing-codex".into()).unwrap();
        assert_eq!(reopened.get(&owner, &id).unwrap().snapshot().last_seq, 2);
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }
}
pub fn valid_id(id: &str) -> Result<()> {
    state::key(id).map(|_| ())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Spec {
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
    pub spec: Spec,
    pub owner: String,
    pub created_at: u64,
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

pub struct Jobs {
    directory: PathBuf,
    runs: Mutex<HashMap<String, Arc<Run>>>,
    codex_path: String,
    _lock: File,
}

impl Jobs {
    pub fn open(directory: PathBuf, codex_path: String) -> Result<Arc<Self>> {
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
        let jobs = Arc::new(Self {
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

    pub fn start(self: &Arc<Self>, owner: &str, spec: Spec) -> Result<Snapshot> {
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
                return Err("thread already has an active run; steer it instead".into());
            }
            if prior.record.spec.cwd != spec.cwd {
                return Err("follow-up must retain the thread working directory".into());
            }
            if prior.status == "interrupted" {
                return Err(
                    "thread has an uncertain prior execution; inspect it and create a new thread"
                        .into(),
                );
            }
        }
        let resume = previous.iter().rev().find_map(|s| s.backend_thread.clone());
        let path = self.directory.join(&spec.id);
        state::private_dir(&path)?;
        let record = Record {
            spec: spec.clone(),
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
        std::thread::spawn(move || {
            if let Err(error) = codex::execute(&binary, &spec, resume, run.clone(), receiver) {
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
        });
        Ok(snapshot)
    }
}
