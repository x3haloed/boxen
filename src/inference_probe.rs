//! Opt-in transport experiment, not a daemon backend or a tool executor.
use crate::{
    Result,
    logging::{self, Context, Level, Target},
    take_option,
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

const FRAME_LIMIT: usize = 1024 * 1024;
const JOURNAL_LIMIT: u64 = 16 * 1024 * 1024;
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

pub fn run(command: &str, mut args: Vec<String>) -> Result<()> {
    let journal = take_option(&mut args, "--journal")?.ok_or("--journal PATH is required")?;
    if command == "inference-replay" {
        if !args.is_empty() {
            return Err("unexpected replay arguments".into());
        }
        println!("{}", replay(Path::new(&journal))?);
        return Ok(());
    }
    let request = take_option(&mut args, "--request")?.ok_or("--request PATH is required")?;
    let endpoint = take_option(&mut args, "--endpoint")?.unwrap_or_else(|| ENDPOINT.into());
    let key_file = take_option(&mut args, "--key-file")?;
    let seconds: u64 = take_option(&mut args, "--timeout-seconds")?
        .unwrap_or_else(|| "120".into())
        .parse()?;
    if !args.is_empty() || !(1..=300).contains(&seconds) {
        return Err("unexpected arguments or timeout outside 1–300 seconds".into());
    }
    // Test overrides cannot receive a real credential. No redirects or arbitrary remote endpoints.
    let local = endpoint
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| endpoint.strip_prefix("https://127.0.0.1:"))
        .is_some_and(|s| {
            s.split_once('/')
                .is_some_and(|(port, _)| port.parse::<u16>().is_ok())
        });
    if endpoint != ENDPOINT && (!local || key_file.is_some()) {
        return Err("endpoint override accepts only unauthenticated IPv4 loopback fixtures".into());
    }
    let key = match key_file {
        Some(path) => {
            let bytes = fs::read(path)?;
            if bytes.len() > 4096 {
                return Err("credential file exceeds 4096 bytes".into());
            }
            let value = String::from_utf8(bytes)?.trim().to_string();
            if value.is_empty() || value.contains(['\r', '\n']) {
                return Err("credential file contains no key or multiple lines".into());
            }
            Some(value)
        }
        None if !local => return Err("--key-file PATH is required for OpenRouter".into()),
        None => None,
    };
    if fs::metadata(&request)?.len() > FRAME_LIMIT as u64 {
        return Err("probe request exceeds 1 MiB".into());
    }
    let mut body: Value = serde_json::from_slice(&fs::read(request)?)?;
    if !body.is_object() {
        return Err("request is not an object".into());
    }
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage":true});
    let mut journal = Journal::create(Path::new(&journal))?;
    journal.append(
        json!({"type":"exchange_started","version":1,"endpoint":endpoint,"request":body}),
    )?;
    logging::record(
        Level::Info,
        Target::Runtime,
        "inference_probe.started",
        Context::default(),
        || json!({"endpoint":endpoint}),
        || Value::Null,
    );
    let result = crate::chat::exchange(&endpoint, key.as_deref(), &body, seconds, &mut |event| {
        journal.append(event)
    });
    // Transport setup errors are sanitized at the boundary; HTTP error bodies are not formatted.
    if let Err(error) = &result {
        let reason = error.to_string();
        let reason = key.as_deref().map_or_else(
            || reason.clone(),
            |key| reason.replace(key, "[credential redacted]"),
        );
        journal.append(json!({"type":"exchange_failed","reason":reason}))?;
    }
    logging::record(
        Level::Info,
        Target::Runtime,
        "inference_probe.finished",
        Context::default(),
        || json!({"ok":result.is_ok()}),
        || Value::Null,
    );
    result
}

struct Journal {
    file: File,
    bytes: u64,
    seq: u64,
}
impl Journal {
    fn create(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: options.open(path)?,
            bytes: 0,
            seq: 0,
        })
    }
    fn append(&mut self, mut event: Value) -> Result<()> {
        self.seq += 1;
        event["seq"] = json!(self.seq);
        event["at_ms"] = json!(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        );
        let mut data = serde_json::to_vec(&event)?;
        data.push(b'\n');
        self.bytes += data.len() as u64;
        if self.bytes > JOURNAL_LIMIT {
            return Err("probe journal exceeds 16 MiB".into());
        }
        self.file.write_all(&data)?;
        self.file.sync_data()?;
        println!("{event}");
        std::io::stdout().flush()?;
        Ok(())
    }
}

fn replay(path: &Path) -> Result<Value> {
    if fs::metadata(path)?.len() > JOURNAL_LIMIT {
        return Err("probe journal exceeds 16 MiB".into());
    }
    let bytes = fs::read(path)?;
    let mut events = Vec::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let event: Value = serde_json::from_slice(line)?;
        if event["seq"] != (events.len() + 1) {
            return Err("noncontiguous probe journal".into());
        }
        events.push(event);
    }
    crate::chat::project(events)
}
