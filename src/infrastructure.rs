//! Bounded, payload-free evidence at infrastructure boundaries.
use crate::{
    Result,
    logging::{self, Context, Level, Target},
    state,
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
const PAGE_SIZE: usize = 128;
static HISTORY: OnceLock<History> = OnceLock::new();

struct History {
    directory: PathBuf,
    machine: String,
    session: String,
    attempts: AtomicU64,
    writer: Mutex<Writer>,
}

#[derive(Default)]
struct Writer {
    sequence: u64,
    initialized: bool,
    failed_writes: u64,
    recovered_partial_rows: u64,
}

pub fn install(directory: &Path, machine: &str) -> Result<()> {
    let history = History {
        directory: directory.join("infrastructure"),
        machine: machine.into(),
        session: state::random_hex()?,
        attempts: AtomicU64::new(0),
        writer: Mutex::new(Writer::default()),
    };
    HISTORY
        .set(history)
        .map_err(|_| "infrastructure history already installed")?;
    let attempt = Attempt::new("daemon", "startup", None, None, None, None);
    attempt.record("startup", "ready", None);
    Ok(())
}

impl History {
    fn snapshot(&self, name: &str) -> Result<Option<(File, u64)>> {
        let file = match File::open(self.directory.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let length = file.metadata()?.len();
        if length > SEGMENT_BYTES {
            return Err("infrastructure history segment exceeds its storage bound".into());
        }
        Ok(Some((file, length)))
    }

    fn scan(snapshot: Option<(File, u64)>, mut visit: impl FnMut(Value)) -> Result<(u64, u64)> {
        let Some((file, length)) = snapshot else {
            return Ok((0, 0));
        };
        let mut reader = BufReader::new(file.take(length));
        let mut line = Vec::new();
        let mut invalid = 0;
        let mut complete = 0;
        while reader.read_until(b'\n', &mut line)? != 0 {
            if !line.ends_with(b"\n") {
                invalid += 1;
                break;
            }
            complete += line.len() as u64;
            match serde_json::from_slice::<Value>(&line) {
                Ok(value) if value["seq"].is_u64() => visit(value),
                _ => invalid += 1,
            }
            line.clear();
        }
        Ok((invalid, complete))
    }

    fn append(&self, writer: &mut Writer, mut record: Value) -> Result<()> {
        let current = self.directory.join("current.jsonl");
        if !writer.initialized {
            state::private_dir(&self.directory)?;
            let mut recover_sequence = |row: Value| {
                writer.sequence = writer.sequence.max(row["seq"].as_u64().unwrap());
            };
            Self::scan(self.snapshot("previous.jsonl")?, &mut recover_sequence)?;
            let (_, complete) = Self::scan(self.snapshot("current.jsonl")?, recover_sequence)?;
            if current.exists() {
                let file = OpenOptions::new().write(true).open(&current)?;
                if file.metadata()?.len() != complete {
                    file.set_len(complete)?;
                    writer.recovered_partial_rows += 1;
                }
            }
            writer.initialized = true;
        }
        writer.sequence += 1;
        record["seq"] = json!(writer.sequence);
        record["write_failures_since_start"] = json!(writer.failed_writes);
        record["recovered_partial_rows_since_start"] = json!(writer.recovered_partial_rows);
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        let length = match fs::metadata(&current) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        if length + bytes.len() as u64 > SEGMENT_BYTES {
            let previous = self.directory.join("previous.jsonl");
            match fs::remove_file(&previous) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            fs::rename(&current, previous)?;
        }
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&current)?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        #[cfg(unix)]
        if length == 0 || length + bytes.len() as u64 > SEGMENT_BYTES {
            File::open(&self.directory)?.sync_all()?;
        }
        Ok(())
    }

    fn write(&self, record: Value) -> bool {
        let mut writer = self.writer.lock().unwrap();
        if self.append(&mut writer, record).is_ok() {
            true
        } else {
            writer.initialized = false;
            writer.failed_writes += 1;
            false
        }
    }
}

pub fn page(request: &Value) -> Result<Value> {
    let history = HISTORY.get().ok_or("infrastructure history unavailable")?;
    let after = request
        .get("after")
        .map(|value| value.as_u64().ok_or("after must be an unsigned sequence"))
        .transpose()?
        .unwrap_or(0);
    let (previous, current, failed_writes, recovered_partial_rows) = {
        let writer = history.writer.lock().unwrap();
        (
            history.snapshot("previous.jsonl")?,
            history.snapshot("current.jsonl")?,
            writer.failed_writes,
            writer.recovered_partial_rows,
        )
    };
    let mut records = Vec::new();
    let (mut oldest, mut newest) = (None, None);
    let mut more = false;
    let mut visit = |row: Value| {
        let seq = row["seq"].as_u64().unwrap();
        oldest.get_or_insert(seq);
        newest = Some(seq);
        if seq > after {
            if records.len() < PAGE_SIZE {
                records.push(row);
            } else {
                more = true;
            }
        }
    };
    let (invalid_previous, _) = History::scan(previous, &mut visit)?;
    let (invalid_current, _) = History::scan(current, visit)?;
    let invalid = invalid_previous + invalid_current;
    let next = more.then(|| records.last().unwrap()["seq"].as_u64().unwrap());
    Ok(json!({
        "machine_id":history.machine, "records":records, "next_after":next,
        "oldest_seq":oldest, "newest_seq":newest, "invalid_rows":invalid,
        "history_before_cursor_unavailable":oldest.is_some_and(|seq| seq > after.saturating_add(1)),
        "write_failures_since_start":failed_writes, "recovered_partial_rows_since_start":recovered_partial_rows
    }))
}

fn identifier(value: Option<&str>) -> Option<String> {
    value
        .filter(|id| {
            id.len() == 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        })
        .map(str::to_owned)
}

// Polling and streaming are high volume and have their own opt-in diagnostics.
pub fn operation(request: &Value) -> Option<&'static str> {
    Some(match request["op"].as_str()? {
        "start" => "start",
        "control" => "control",
        "resident_activate" => "resident_activate",
        "inference_resume" => "inference_resume",
        "thread_move" => "thread_move",
        "thread_move_receive" => match request["action"].as_str() {
            Some("prepare") => "thread_move_prepare",
            Some("finish") => "thread_move_finish",
            _ => return None,
        },
        "thread_workspace" => "thread_workspace",
        "thread_workspace_offer" => "thread_workspace_offer",
        "network_mail" => "network_mail",
        "mailbox_send" => "mailbox_send",
        _ => return None,
    })
}

pub struct Attempt {
    id: Option<String>,
    boundary: &'static str,
    operation: &'static str,
    peer: Option<String>,
    run: Option<String>,
    thread: Option<String>,
    started: Instant,
    runtime_version: Option<String>,
    policy_revision: Option<String>,
}

impl Attempt {
    pub fn request(
        boundary: &'static str,
        peer: &str,
        request: &Value,
        id: Option<&str>,
    ) -> Option<Self> {
        HISTORY.get()?;
        let attempt = Self::new(
            boundary,
            operation(request)?,
            Some(peer),
            request["spec"]["id"].as_str().or_else(|| {
                (request["op"] == "control")
                    .then(|| request["id"].as_str())
                    .flatten()
            }),
            request["spec"]["thread_id"]
                .as_str()
                .or(request["thread_id"].as_str()),
            id,
        );
        attempt.record("request", "attempted", None);
        Some(attempt)
    }

    pub fn new(
        boundary: &'static str,
        operation: &'static str,
        peer: Option<&str>,
        run: Option<&str>,
        thread: Option<&str>,
        id: Option<&str>,
    ) -> Self {
        Self {
            id: identifier(id).or_else(|| {
                HISTORY.get().map(|history| {
                    format!(
                        "{}{:016x}",
                        &history.session[..48],
                        history.attempts.fetch_add(1, Ordering::Relaxed)
                    )
                })
            }),
            boundary,
            operation,
            peer: identifier(peer),
            run: identifier(run),
            thread: identifier(thread),
            started: Instant::now(),
            runtime_version: None,
            policy_revision: None,
        }
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn since(mut self, started: Instant) -> Self {
        self.started = started;
        self
    }

    pub fn runtime(&mut self, version: Option<&str>, policy: &crate::runtime_policy::Policy) {
        use sha2::{Digest, Sha256};
        self.runtime_version = version
            .map(|v| v.strip_prefix("codex-cli ").unwrap_or(v))
            .filter(|v| {
                v.len() <= 64
                    && v.as_bytes().first().is_some_and(u8::is_ascii_digit)
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
            })
            .map(str::to_owned);
        self.policy_revision = serde_json::to_vec(policy)
            .ok()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
    }

    pub fn record(&self, stage: &'static str, outcome: &'static str, reason: Option<&'static str>) {
        let Some(history) = HISTORY.get() else {
            return;
        };
        let elapsed_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let success = history.write(json!({
            "v":1, "machine_id":history.machine, "session_id":history.session,
            "daemon_version":env!("CARGO_PKG_VERSION"), "platform":crate::platform::host_os(),
            "timestamp_ms":crate::jobs::now(), "elapsed_ms":elapsed_ms,
            "request_id":self.id, "peer_id":self.peer, "run_id":self.run, "thread_id":self.thread,
            "boundary":self.boundary, "operation":self.operation, "stage":stage, "outcome":outcome,
            "reason":reason, "runtime_version":self.runtime_version,
            "runtime_policy_revision":self.policy_revision,
        }));
        if !success {
            logging::record(
                Level::Warn,
                Target::State,
                "infrastructure.write_failed",
                Context {
                    peer_id: self.peer.as_deref(),
                    run_id: self.run.as_deref(),
                    thread_id: self.thread.as_deref(),
                    ..Context::default()
                },
                || json!({"stage":stage,"request_id":self.id}),
                || Value::Null,
            );
        }
    }
}

pub fn reason(error: &(dyn std::error::Error + 'static)) -> &'static str {
    match error.to_string().as_str() {
        "peer unreachable; delivery is not confirmed" => return "peer_unreachable",
        "remote identity differs from the selected peer; no message sent" => {
            return "peer_identity_mismatch";
        }
        "working directory must exist on the executing machine and be absolute" => {
            return "invalid_working_directory";
        }
        "run id already exists with different input or owner" => return "run_id_conflict",
        "follow-up source is no longer the latest anonymous run" => return "stale_follow_up",
        _ => {}
    }
    match error.downcast_ref::<io::Error>().map(io::Error::kind) {
        Some(io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => "timeout",
        Some(io::ErrorKind::ConnectionRefused) => "connection_refused",
        Some(
            io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe,
        ) => "connection_closed",
        Some(io::ErrorKind::NotFound) => "not_found",
        Some(io::ErrorKind::PermissionDenied) => "permission_denied",
        Some(_) => "io_error",
        None => "operation_error",
    }
}
