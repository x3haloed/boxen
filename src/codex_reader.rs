//! Short-lived, read-only connection to the executing account's Codex app-server.
use crate::{
    Result,
    execution::Execution,
    logging::{self, Context, Level, Target},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// Only read-only operations use this connection. No start/resume/turn calls.
pub(crate) struct NativeReader {
    child: Child,
    stdin: ChildStdin,
    incoming: mpsc::Receiver<std::result::Result<Value, String>>,
    next_id: u64,
    owner: String,
    purpose: &'static str,
    deadline: Option<Instant>,
}
impl NativeReader {
    pub fn open(
        binary: &str,
        owner: &str,
        execution: &Execution,
        purpose: &'static str,
        deadline: Option<Instant>,
    ) -> Result<Self> {
        let mut child = crate::codex_executable::launch(binary, crate::platform::host_os(),
            |selected| {
                let mut command = execution.command(&selected.path);
                if purpose == "model_catalog" && !execution.home.is_empty() {
                    command.current_dir(&execution.home);
                }
                command.args(["app-server", "--listen", "stdio://"])
                    .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
            },
            |selected, retry| logging::record(Level::Debug, Target::Runtime,
                if purpose == "thread_import" { "thread_import.reader_starting" } else { "codex_models.process_starting" }, Context::peer(owner),
                ||json!({"purpose":purpose,"executable":selected.path,"version":selected.version,"automatic":selected.automatic,"probe_fallback":selected.probe_fallback,"skipped_candidates":selected.skipped_candidates,"rediscovered":retry}), ||Value::Null),
        ).inspect_err(|error| logging::record(Level::Error, Target::Runtime,
            if purpose == "thread_import" { "thread_import.reader_start_failed" } else { "codex_models.process_start_failed" }, Context::peer(owner),
            ||json!({"purpose":purpose,"detail":error.to_string()}), ||Value::Null))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("native reader stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("native reader stdout unavailable")?;
        let (send, incoming) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let result=match reader.by_ref().take(4*1024*1024+1).read_until(b'\n',&mut line){
                    Ok(0)=>break,
                    Ok(_) if line.len()>4*1024*1024=>Err("native read response exceeds 4 MiB; smaller history pages may be available".into()),
                    Ok(_)=>serde_json::from_slice(&line).map_err(|e|format!("invalid native read response: {e}")),
                    Err(e)=>Err(e.to_string()),
                };
                let failed = result.is_err();
                if send.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut this = Self {
            child,
            stdin,
            incoming,
            next_id: 1,
            owner: owner.into(),
            purpose,
            deadline,
        };
        this.request("initialize",json!({"clientInfo":{"name":"boxen","title":"Boxen","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        this.write(json!({"method":"initialized"}))?;
        Ok(this)
    }
    fn write(&mut self, v: Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, &v)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        logging::record(
            Level::Debug,
            Target::Runtime,
            if self.purpose == "thread_import" {
                "thread_import.native_request"
            } else {
                "codex_models.request"
            },
            Context::peer(&self.owner),
            || json!({"purpose":self.purpose,"method":method,"backend_thread_id":params["threadId"],"request_id":id}),
            || Value::Null,
        );
        self.write(json!({"id":id,"method":method,"params":params}))?;
        let deadline = self
            .deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("native {method} timed out"))?;
            let message = self
                .incoming
                .recv_timeout(remaining)
                .map_err(|e| format!("native {method} unavailable: {e}"))?
                .map_err(|e| format!("native {method}: {e}"))?;
            if message["id"] == id {
                logging::record(
                    Level::Debug,
                    Target::Runtime,
                    if self.purpose == "thread_import" {
                        "thread_import.native_reply"
                    } else {
                        "codex_models.reply"
                    },
                    Context::peer(&self.owner),
                    || json!({"purpose":self.purpose,"method":method,"request_id":id,"accepted":message["error"].is_null(),"error_code":message["error"]["code"]}),
                    || Value::Null,
                );
                if !message["error"].is_null() {
                    return Err(format!("native {method} rejected: {}", message["error"]).into());
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "native read result unavailable".into());
            }
            if message.get("id").is_some() && message.get("method").is_some() {
                return Err(
                    "native read unexpectedly requested a client action; no action was executed"
                        .into(),
                );
            }
        }
    }
}
impl Drop for NativeReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let status = self.child.wait();
        logging::record(
            Level::Debug,
            Target::Runtime,
            if self.purpose == "thread_import" {
                "thread_import.reader_stopped"
            } else {
                "codex_models.process_stopped"
            },
            Context::peer(&self.owner),
            || json!({"purpose":self.purpose,"pid":self.child.id(),"status":status.map(|v|v.to_string()).unwrap_or_else(|e|e.to_string())}),
            || Value::Null,
        );
    }
}

pub fn models(binary: &str, owner: &str, execution: &Execution) -> Result<Value> {
    use std::collections::HashSet;
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut reader = NativeReader::open(binary, owner, execution, "model_catalog", Some(deadline))?;
    let mut cursors = HashSet::new();
    let mut slugs = HashSet::new();
    let mut models = Vec::new();
    let mut params = json!({"includeHidden":false,"limit":100});
    for _ in 0..8 {
        let page = reader.request("model/list", params.clone())?;
        let entries = page["data"]
            .as_array()
            .ok_or("Codex returned an invalid model list")?;
        for entry in entries.iter().filter(|entry| entry["hidden"] != true) {
            let model = entry["model"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 256)
                .ok_or("Codex returned an invalid model ID")?;
            if slugs.insert(model.to_owned()) {
                let label = entry["displayName"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty() && s.len() <= 256)
                    .unwrap_or(model);
                models.push(json!({"model":model,"label":label}));
                if models.len() > 800 {
                    return Err("Codex model list exceeds its size bound".into());
                }
            }
        }
        match page["nextCursor"].as_str() {
            Some(cursor) if cursor.len() <= 4096 && cursors.insert(cursor.to_owned()) => {
                params["cursor"] = json!(cursor);
            }
            None if page["nextCursor"].is_null() => return Ok(json!({"models":models})),
            _ => return Err("Codex returned an invalid model list cursor".into()),
        }
    }
    Err("Codex model list exceeds its page bound".into())
}
