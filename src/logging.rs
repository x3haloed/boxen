//! Opt-in process diagnostics. This is deliberately independent of workload journals.
use crate::{Result, state};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

const QUEUE_RECORDS: usize = 256;
const QUEUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAMS: usize = 8;
const MAX_PAYLOAD: usize = 64 * 1024;
static GLOBAL: OnceLock<Arc<Logger>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    #[default]
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Daemon,
    Peer,
    Http,
    Workload,
    Runtime,
    State,
}
impl Target {
    const ALL: [Self; 6] = [
        Self::Daemon,
        Self::Peer,
        Self::Http,
        Self::Workload,
        Self::Runtime,
        Self::State,
    ];
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub level: Level,
    pub targets: Vec<Target>,
    pub events: Vec<String>,
    pub peer_id: Option<String>,
    pub thread_id: Option<String>,
    pub run_id: Option<String>,
    pub payloads: bool,
}
impl Filter {
    fn validate(&self) -> Result<()> {
        for value in [&self.peer_id, &self.thread_id, &self.run_id]
            .into_iter()
            .flatten()
        {
            state::key(value)?;
        }
        if self.events.len() > 32 || self.events.iter().any(|s| s.is_empty() || s.len() > 100) {
            return Err("event prefixes must contain 1–100 bytes; at most 32 prefixes".into());
        }
        Ok(())
    }
    fn matches(&self, level: Level, target: Target, event: &str, context: Context<'_>) -> bool {
        level >= self.level
            && (self.targets.is_empty() || self.targets.contains(&target))
            && (self.events.is_empty() || self.events.iter().any(|s| event.starts_with(s)))
            && self
                .peer_id
                .as_deref()
                .is_none_or(|v| context.peer_id == Some(v))
            && self
                .thread_id
                .as_deref()
                .is_none_or(|v| context.thread_id == Some(v))
            && self
                .run_id
                .as_deref()
                .is_none_or(|v| context.run_id == Some(v))
    }
    fn mask(&self) -> u64 {
        let mut mask = 0;
        for target in Target::ALL {
            if self.targets.is_empty() || self.targets.contains(&target) {
                for level in self.level as u8..=Level::Error as u8 {
                    mask |= 1 << (target as u8 * 5 + level);
                }
            }
        }
        mask
    }
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct Context<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<u64>,
}
impl<'a> Context<'a> {
    pub fn peer(peer: &'a str) -> Self {
        Self {
            peer_id: Some(peer),
            ..Self::default()
        }
    }
    pub fn run(run: &'a str, thread: &'a str, peer: &'a str) -> Self {
        Self {
            run_id: Some(run),
            thread_id: Some(thread),
            peer_id: Some(peer),
            connection_id: None,
        }
    }
    pub fn connection(id: u64) -> Self {
        Self {
            connection_id: Some(id),
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct Counters {
    queued_bytes: AtomicUsize,
    lost: AtomicU64,
    dropped: AtomicU64,
}
struct Packet {
    value: Value,
    bytes: usize,
}
struct Sink {
    filter: Filter,
    sender: mpsc::SyncSender<Arc<Packet>>,
    counters: Arc<Counters>,
}
struct Inner {
    sinks: HashMap<u64, Sink>,
    next_sink: u64,
    sequence: u64,
    file: Option<(u64, String)>,
    file_error: Option<String>,
}
pub struct Logger {
    machine: String,
    session: String,
    mask: AtomicU64,
    inner: Mutex<Inner>,
    file_worker: Mutex<Option<JoinHandle<()>>>,
    emitted: AtomicU64,
    dropped: AtomicU64,
}

pub enum Delivery {
    Record(Value),
    Idle,
    End,
}
pub struct Subscription {
    logger: Weak<Logger>,
    id: u64,
    receiver: mpsc::Receiver<Arc<Packet>>,
    counters: Arc<Counters>,
    report_loss: AtomicBool,
}
impl Subscription {
    pub fn read(&self, timeout: Duration) -> Delivery {
        let dropped = if self.report_loss.swap(false, Ordering::Relaxed) {
            self.counters.lost.swap(0, Ordering::Relaxed)
        } else {
            0
        };
        if dropped > 0
            && let Some(logger) = self.logger.upgrade()
        {
            return Delivery::Record(
                json!({"v":1,"session_id":logger.session,"timestamp_ms":crate::jobs::now(),
                "machine_id":logger.machine,"pid":std::process::id(),"seq":null,"level":"warn","target":"daemon",
                "event":"logging.dropped","context":{},"fields":{"sink_id":self.id,"dropped":dropped}}),
            );
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(packet) => {
                self.report_loss.store(true, Ordering::Relaxed);
                self.counters
                    .queued_bytes
                    .fetch_sub(packet.bytes, Ordering::Relaxed);
                Delivery::Record(packet.value.clone())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.report_loss.store(true, Ordering::Relaxed);
                Delivery::Idle
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if self.counters.lost.load(Ordering::Relaxed) > 0 {
                    self.report_loss.store(true, Ordering::Relaxed);
                    self.read(Duration::ZERO)
                } else {
                    Delivery::End
                }
            }
        }
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(logger) = self.logger.upgrade() {
            let mut inner = logger.inner.lock().unwrap();
            inner.sinks.remove(&self.id);
            logger.refresh(&inner);
        }
    }
}

impl Logger {
    pub fn new(machine: String) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            machine,
            session: state::random_hex()?,
            mask: AtomicU64::new(0),
            inner: Mutex::new(Inner {
                sinks: HashMap::new(),
                next_sink: 1,
                sequence: 0,
                file: None,
                file_error: None,
            }),
            file_worker: Mutex::new(None),
            emitted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }))
    }
    fn refresh(&self, inner: &Inner) {
        self.mask.store(
            inner
                .sinks
                .values()
                .fold(0, |mask, sink| mask | sink.filter.mask()),
            Ordering::Release,
        );
    }
    pub fn subscribe(self: &Arc<Self>, filter: Filter) -> Result<Subscription> {
        filter.validate()?;
        let mut inner = self.inner.lock().unwrap();
        if inner.sinks.len() >= MAX_STREAMS {
            return Err("at most eight diagnostic sinks may be active".into());
        }
        Ok(self.register(&mut inner, filter))
    }
    fn register(self: &Arc<Self>, inner: &mut Inner, filter: Filter) -> Subscription {
        let id = inner.next_sink;
        inner.next_sink += 1;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_RECORDS);
        let counters = Arc::new(Counters::default());
        inner.sinks.insert(
            id,
            Sink {
                filter,
                sender,
                counters: counters.clone(),
            },
        );
        self.refresh(inner);
        Subscription {
            logger: Arc::downgrade(self),
            id,
            receiver,
            counters,
            report_loss: AtomicBool::new(true),
        }
    }
    pub fn status(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        let mut sinks:Vec<_> = inner.sinks.iter().map(|(id,s)| json!({"id":id,"filter":s.filter,
            "queued_bytes":s.counters.queued_bytes.load(Ordering::Relaxed),"dropped":s.counters.dropped.load(Ordering::Relaxed)})).collect();
        sinks.sort_by_key(|s| s["id"].as_u64());
        json!({"enabled":!inner.sinks.is_empty(),"session_id":self.session,"sinks":sinks,
            "file":inner.file.as_ref().map(|(id,path)| json!({"sink_id":id,"path":path})),"file_error":inner.file_error,
            "emitted":self.emitted.load(Ordering::Relaxed),"dropped":self.dropped.load(Ordering::Relaxed)})
    }
    pub fn off(&self) {
        self.emit(
            Level::Info,
            Target::Daemon,
            "logging.disable_requested",
            Context::default(),
            || json!({"reason":"explicit off"}),
            || Value::Null,
        );
        let mut worker = self.file_worker.lock().unwrap();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.sinks.clear();
            inner.file = None;
            self.refresh(&inner);
        }
        // Close producers first; accepted file records drain without blocking execution.
        if let Some(worker) = worker.take() {
            let _ = worker.join();
        }
    }
    pub fn file(self: &Arc<Self>, path: &str, filter: Filter) -> Result<()> {
        filter.validate()?;
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err("log file path must be absolute on the executing machine".into());
        }
        if path.exists() && !fs::metadata(path)?.is_file() {
            return Err("log destination must be a regular file".into());
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        let mut worker = self.file_worker.lock().unwrap();
        let subscription = {
            let mut inner = self.inner.lock().unwrap();
            let old_id = inner.file.as_ref().map(|(id, _)| *id);
            if inner.sinks.len().saturating_sub(usize::from(
                old_id.is_some_and(|id| inner.sinks.contains_key(&id)),
            )) >= MAX_STREAMS
            {
                return Err("at most eight diagnostic sinks may be active".into());
            }
            let subscription = self.register(&mut inner, filter);
            if let Some(id) = old_id {
                inner.sinks.remove(&id);
            }
            inner.file = Some((subscription.id, path.to_string_lossy().into()));
            inner.file_error = None;
            self.refresh(&inner);
            subscription
        };
        // Producers already route to the new bounded queue while the old writer drains.
        if let Some(old) = worker.take() {
            let _ = old.join();
        }
        let id = subscription.id;
        let logger = Arc::downgrade(self);
        *worker = Some(std::thread::spawn(move || {
            let outcome = (|| -> Result<()> {
                loop {
                    match subscription.read(Duration::from_secs(1)) {
                        Delivery::Record(value) => {
                            let mut bytes = serde_json::to_vec(&value)?;
                            bytes.push(b'\n');
                            file.write_all(&bytes)?;
                        }
                        Delivery::Idle => {}
                        Delivery::End => {
                            file.flush()?;
                            break;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = outcome
                && let Some(logger) = logger.upgrade()
            {
                let mut inner = logger.inner.lock().unwrap();
                let mut lost = 1;
                while let Ok(packet) = subscription.receiver.try_recv() {
                    subscription
                        .counters
                        .queued_bytes
                        .fetch_sub(packet.bytes, Ordering::Relaxed);
                    lost += 1;
                }
                logger.dropped.fetch_add(lost, Ordering::Relaxed);
                inner.file_error = Some(error.to_string());
                if inner
                    .file
                    .as_ref()
                    .is_some_and(|(current, _)| *current == id)
                {
                    inner.file = None;
                }
                inner.sinks.remove(&id);
                logger.refresh(&inner);
                drop(inner);
                logger.emit(
                    Level::Error,
                    Target::Daemon,
                    "logging.file_failed",
                    Context::default(),
                    || json!({"error":error.to_string(),"sink_id":id,"dropped":lost}),
                    || Value::Null,
                );
            }
        }));
        Ok(())
    }
    pub fn emit(
        &self,
        level: Level,
        target: Target,
        event: &str,
        context: Context<'_>,
        fields: impl FnOnce() -> Value,
        payload: impl FnOnce() -> Value,
    ) {
        let bit = 1 << (target as u8 * 5 + level as u8);
        if self.mask.load(Ordering::Acquire) & bit == 0 {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let matches = |sink: &Sink| sink.filter.matches(level, target, event, context);
        if !inner.sinks.values().any(matches) {
            return;
        }
        let capture = inner
            .sinks
            .values()
            .any(|s| matches(s) && s.filter.payloads);
        inner.sequence += 1;
        let mut base = json!({"v":1,"session_id":self.session,"seq":inner.sequence,"timestamp_ms":crate::jobs::now(),
            "pid":std::process::id(),"machine_id":self.machine,"level":level,"target":target,"event":event,
            "context":context,"fields":bounded(redact(fields()))});
        let plain = Arc::new(Packet {
            bytes: serde_json::to_vec(&base).map(|b| b.len()).unwrap_or(0),
            value: base.clone(),
        });
        let rich = if capture {
            base["payload"] = bounded(redact(payload()));
            Some(Arc::new(Packet {
                bytes: serde_json::to_vec(&base).map(|b| b.len()).unwrap_or(0),
                value: base,
            }))
        } else {
            None
        };
        self.emitted.fetch_add(1, Ordering::Relaxed);
        for sink in inner.sinks.values().filter(|s| matches(s)) {
            let packet = if sink.filter.payloads {
                rich.as_ref().unwrap_or(&plain)
            } else {
                &plain
            };
            let previous = sink
                .counters
                .queued_bytes
                .fetch_add(packet.bytes, Ordering::Relaxed);
            if previous + packet.bytes > QUEUE_BYTES
                || sink.sender.try_send(packet.clone()).is_err()
            {
                sink.counters
                    .queued_bytes
                    .fetch_sub(packet.bytes, Ordering::Relaxed);
                sink.counters.lost.fetch_add(1, Ordering::Relaxed);
                sink.counters.dropped.fetch_add(1, Ordering::Relaxed);
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn bounded(value: Value) -> Value {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    if bytes.len() <= MAX_PAYLOAD {
        value
    } else {
        json!({"truncated":true,"original_bytes":bytes.len(),"limit_bytes":MAX_PAYLOAD})
    }
}
fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let normalized: String = key
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect();
                    let secret = normalized.ends_with("token")
                        || normalized.ends_with("apikey")
                        || normalized.ends_with("password")
                        || normalized.ends_with("secret")
                        || matches!(
                            normalized.as_str(),
                            "private"
                                | "privatekey"
                                | "authorization"
                                | "password"
                                | "token"
                                | "accesstoken"
                                | "refreshtoken"
                                | "apikey"
                                | "secret"
                                | "clientsecret"
                                | "capability"
                        );
                    (
                        key,
                        if secret {
                            json!("[redacted]")
                        } else {
                            redact(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact).collect()),
        Value::String(text) => {
            let mut bearer = false;
            Value::String(
                text.split_inclusive(char::is_whitespace)
                    .map(|part| {
                        let word = part.trim_end();
                        let whitespace = &part[word.len()..];
                        let redact = bearer
                            || word.match_indices("sk-").any(|(index, _)| {
                                index == 0 || !word.as_bytes()[index - 1].is_ascii_alphanumeric()
                            });
                        bearer = word.eq_ignore_ascii_case("bearer");
                        if redact {
                            format!("[redacted]{whitespace}")
                        } else {
                            part.into()
                        }
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

pub fn install(machine: &str) -> Result<()> {
    GLOBAL
        .set(Logger::new(machine.into())?)
        .map_err(|_| "logger already installed".into())
}
pub fn global() -> Result<&'static Arc<Logger>> {
    GLOBAL
        .get()
        .ok_or_else(|| "diagnostic logger is not installed".into())
}
pub fn enabled(level: Level, target: Target) -> bool {
    GLOBAL.get().is_some_and(|logger| {
        logger.mask.load(Ordering::Acquire) & (1 << (target as u8 * 5 + level as u8)) != 0
    })
}
pub fn record(
    level: Level,
    target: Target,
    event: &str,
    context: Context<'_>,
    fields: impl FnOnce() -> Value,
    payload: impl FnOnce() -> Value,
) {
    if let Some(logger) = GLOBAL.get() {
        logger.emit(level, target, event, context, fields, payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn disabled_and_filtered_fields_are_lazy_and_payloads_are_per_sink() {
        let logger = Logger::new("test".into()).unwrap();
        let evaluated = AtomicBool::new(false);
        logger.emit(
            Level::Info,
            Target::Peer,
            "test",
            Context::default(),
            || {
                evaluated.store(true, Ordering::Relaxed);
                json!({})
            },
            || panic!("payload evaluated while off"),
        );
        assert!(!evaluated.load(Ordering::Relaxed));
        assert_eq!(logger.status()["enabled"], false);
        let metadata = logger
            .subscribe(Filter {
                level: Level::Info,
                targets: vec![Target::Peer],
                ..Filter::default()
            })
            .unwrap();
        logger.emit(
            Level::Trace,
            Target::Peer,
            "test",
            Context::default(),
            || panic!("filtered fields evaluated"),
            || Value::Null,
        );
        logger.emit(
            Level::Info,
            Target::Peer,
            "test",
            Context::default(),
            || json!({"answer":42}),
            || panic!("payload evaluated without opt-in"),
        );
        assert!(matches!(metadata.read(Duration::ZERO), Delivery::Record(_)));
        let rich = logger
            .subscribe(Filter {
                payloads: true,
                ..Filter::default()
            })
            .unwrap();
        logger.emit(
            Level::Info,
            Target::Peer,
            "test",
            Context::default(),
            || json!({}),
            || json!({"api_key":"secret","content":"Bearer tokenvalue"}),
        );
        if let Delivery::Record(record) = metadata.read(Duration::ZERO) {
            assert!(record.get("payload").is_none());
        } else {
            panic!("missing metadata record");
        }
        if let Delivery::Record(record) = rich.read(Duration::ZERO) {
            assert_eq!(record["payload"]["api_key"], "[redacted]");
            assert!(!record.to_string().contains("tokenvalue"));
        } else {
            panic!("missing rich record");
        }
        logger.off();
        assert!(matches!(rich.read(Duration::ZERO), Delivery::Record(_)));
        assert!(matches!(rich.read(Duration::ZERO), Delivery::End));
    }
    #[test]
    fn slow_consumers_are_bounded_and_receive_loss_notices() {
        let logger = Logger::new("test".into()).unwrap();
        let stream = logger.subscribe(Filter::default()).unwrap();
        for _ in 0..1000 {
            logger.emit(
                Level::Trace,
                Target::Daemon,
                "test",
                Context::default(),
                || json!({}),
                || Value::Null,
            );
        }
        assert!(logger.status()["dropped"].as_u64().unwrap() > 0);
        match stream.read(Duration::ZERO) {
            Delivery::Record(record) => assert_eq!(record["event"], "logging.dropped"),
            _ => panic!("missing loss notice"),
        }
        for _ in 0..100 {
            logger.emit(
                Level::Trace,
                Target::Daemon,
                "test",
                Context::default(),
                || json!({}),
                || Value::Null,
            );
        }
        match stream.read(Duration::ZERO) {
            Delivery::Record(record) => assert_eq!(record["event"], "test"),
            _ => panic!("loss notices starved queued records"),
        }
        drop(stream);
        assert_eq!(logger.status()["enabled"], false);
    }
    #[test]
    fn category_event_and_run_filters_apply_before_field_construction() {
        let logger = Logger::new("test".into()).unwrap();
        let run = state::random_hex().unwrap();
        let thread = state::random_hex().unwrap();
        let peer = state::random_hex().unwrap();
        let stream = logger
            .subscribe(Filter {
                targets: vec![Target::Workload],
                events: vec!["event.".into()],
                run_id: Some(run.clone()),
                ..Filter::default()
            })
            .unwrap();
        logger.emit(
            Level::Info,
            Target::Runtime,
            "event.retained",
            Context::run(&run, &thread, &peer),
            || panic!("category-filtered fields evaluated"),
            || Value::Null,
        );
        logger.emit(
            Level::Info,
            Target::Workload,
            "submission.accepted",
            Context::run(&run, &thread, &peer),
            || panic!("prefix-filtered fields evaluated"),
            || Value::Null,
        );
        logger.emit(
            Level::Info,
            Target::Workload,
            "event.retained",
            Context::default(),
            || panic!("run-filtered fields evaluated"),
            || Value::Null,
        );
        logger.emit(
            Level::Info,
            Target::Workload,
            "event.retained",
            Context::run(&run, &thread, &peer),
            || json!({"kind":"status"}),
            || Value::Null,
        );
        match stream.read(Duration::ZERO) {
            Delivery::Record(record) => {
                assert_eq!(record["context"]["run_id"], run);
                assert_eq!(record["seq"], 1);
            }
            _ => panic!("matching event missing"),
        }
        assert!(matches!(stream.read(Duration::ZERO), Delivery::Idle));
    }
    #[test]
    fn file_switch_and_off_drain_accepted_records_without_restart() {
        let logger = Logger::new("test".into()).unwrap();
        let directory = std::env::temp_dir().join(state::random_hex().unwrap());
        fs::create_dir(&directory).unwrap();
        for name in ["a.jsonl", "b.jsonl"] {
            logger
                .file(directory.join(name).to_str().unwrap(), Filter::default())
                .unwrap();
            logger.emit(
                Level::Info,
                Target::Daemon,
                "test",
                Context::default(),
                || json!({"file":name}),
                || Value::Null,
            );
        }
        logger.off();
        for name in ["a.jsonl", "b.jsonl"] {
            assert!(
                fs::read_to_string(directory.join(name))
                    .unwrap()
                    .contains(name)
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn redirect_preserves_events_while_producers_are_active() {
        let logger = Logger::new("test".into()).unwrap();
        let directory = std::env::temp_dir().join(state::random_hex().unwrap());
        fs::create_dir(&directory).unwrap();
        logger
            .file(
                directory.join("0.jsonl").to_str().unwrap(),
                Filter::default(),
            )
            .unwrap();
        let producer = logger.clone();
        let worker = std::thread::spawn(move || {
            for index in 0..200 {
                producer.emit(
                    Level::Info,
                    Target::Daemon,
                    "test.sequence",
                    Context::default(),
                    || json!({"index":index}),
                    || Value::Null,
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        for index in 1..4 {
            std::thread::sleep(Duration::from_millis(20));
            logger
                .file(
                    directory.join(format!("{index}.jsonl")).to_str().unwrap(),
                    Filter::default(),
                )
                .unwrap();
        }
        worker.join().unwrap();
        logger.off();
        let mut indices = Vec::new();
        for index in 0..4 {
            for line in fs::read_to_string(directory.join(format!("{index}.jsonl")))
                .unwrap()
                .lines()
            {
                let record: Value = serde_json::from_str(line).unwrap();
                if record["event"] == "test.sequence" {
                    indices.push(record["fields"]["index"].as_u64().unwrap());
                }
            }
        }
        indices.sort_unstable();
        assert_eq!(indices, (0..200).collect::<Vec<_>>());
        assert_eq!(logger.status()["dropped"], 0);
        fs::remove_dir_all(directory).unwrap();
    }
}
