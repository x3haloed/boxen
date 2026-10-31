//! One app-server process per active run; completed threads resume on demand.
use crate::logging::Level;
use crate::{
    Result,
    jobs::{Control, History, Run, Spec},
    residents::{Resident, Residents},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct BackendRejected(String);
impl std::fmt::Display for BackendRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for BackendRejected {}

struct Process(Child, Arc<Run>);
impl Drop for Process {
    fn drop(&mut self) {
        self.1.diagnostic(
            Level::Info,
            "process.stopping",
            || json!({"pid":self.0.id()}),
            || Value::Null,
        );
        let _ = self.0.kill();
        let outcome = self.0.wait();
        self.1.diagnostic(Level::Info,"process.stopped",||json!({"pid":self.0.id(),"status":outcome.as_ref().map(|s|s.to_string()).unwrap_or_else(|e|e.to_string())}),||Value::Null);
    }
}

enum Incoming {
    Message(Value),
    Closed(String),
}

struct Rpc {
    stdin: ChildStdin,
    incoming: mpsc::Receiver<Incoming>,
    controls: mpsc::Receiver<Control>,
    run: Arc<Run>,
    next_id: u64,
    replies: HashMap<u64, Value>,
    awaiting: HashSet<u64>,
    pending: HashSet<String>,
    thread: Option<String>,
    turn: Option<String>,
    finished: bool,
    cancel_requested: bool,
    cancel_sent_at: Option<Instant>,
    resident: Option<(Arc<Residents>, Resident)>,
}

fn read_lines(
    reader: impl Read + Send + 'static,
    sender: mpsc::SyncSender<Incoming>,
    stderr: bool,
    run: Arc<Run>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            // Bound a backend line before parsing or retaining it.
            match reader
                .by_ref()
                .take(4 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) => {
                    if !stderr {
                        let _ =
                            sender.send(Incoming::Closed("Codex app-server closed stdout".into()));
                    }
                    break;
                }
                Ok(_) if line.len() > 4 * 1024 * 1024 => {
                    let _ = sender.send(Incoming::Closed("Codex output line exceeds 4 MiB".into()));
                    break;
                }
                Ok(_) => {
                    if stderr {
                        run.diagnostic(
                            Level::Debug,
                            "process.stderr",
                            || json!({"bytes":line.len()}),
                            || json!({"text":String::from_utf8_lossy(&line).trim()}),
                        );
                        continue;
                    }
                    let item = match serde_json::from_slice(&line) {
                        Ok(value) => Incoming::Message(value),
                        Err(error) => {
                            run.diagnostic(
                                Level::Error,
                                "process.invalid_json",
                                || json!({"error":error.to_string(),"bytes":line.len()}),
                                || json!({"text":String::from_utf8_lossy(&line)}),
                            );
                            Incoming::Closed(format!("Invalid Codex app-server JSON: {error}"))
                        }
                    };
                    if sender.send(item).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Incoming::Closed(error.to_string()));
                    break;
                }
            }
        }
    });
}

impl Rpc {
    fn write(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes)?;
        self.stdin.flush()?;
        self.run.diagnostic(
            Level::Trace,
            "rpc.sent",
            || json!({"method":value["method"],"id":value["id"],"bytes":bytes.len()}),
            || value.clone(),
        );
        Ok(())
    }
    fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"id":id,"method":method,"params":params}))?;
        Ok(id)
    }
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params)?;
        self.awaiting.insert(id);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(reply) = self.replies.remove(&id) {
                if !reply["error"].is_null() {
                    return Err(BackendRejected(format!("{method}: {}", reply["error"])).into());
                }
                return Ok(reply["result"].clone());
            }
            if Instant::now() > deadline {
                self.run.diagnostic(
                    Level::Error,
                    "rpc.timeout",
                    || json!({"method":method,"id":id}),
                    || Value::Null,
                );
                return Err(format!("Codex {method} timed out").into());
            }
            self.tick()?;
        }
    }
    fn tick(&mut self) -> Result<()> {
        while let Ok(control) = self.controls.try_recv() {
            match control {
                Control::Cancel => {
                    self.cancel_requested = true;
                    self.run
                        .append("control", json!({"type":"cancel_requested"}))?;
                }
                Control::Steer { text } => {
                    if text.is_empty() || text.len() > 256 * 1024 {
                        return Err("invalid steering text".into());
                    }
                    if let (Some(thread), Some(turn)) = (&self.thread, &self.turn) {
                        let params = json!({"threadId":thread,"expectedTurnId":turn,"input":[{"type":"text","text":text}]});
                        let id = self.send("turn/steer", params)?;
                        self.run.append(
                            "control",
                            json!({"type":"steer_requested","request_id":id,"text":text}),
                        )?;
                    } else {
                        self.run.append(
                            "control_error",
                            json!({"message":"Turn is still starting; send again when running"}),
                        )?;
                    }
                }
                Control::Respond { request_id, result } => {
                    if !self.pending.remove(&request_id.to_string()) {
                        self.run.append("control_error", json!({"message":"Request is no longer pending","request_id":request_id}))?;
                    } else {
                        self.write(json!({"id":request_id,"result":result}))?;
                        self.run
                            .append("request_answered", json!({"request_id":request_id}))?;
                        if self.pending.is_empty() && !self.finished {
                            self.run.status("running", "Input sent to backend")?;
                        }
                    }
                }
            }
        }
        if self.cancel_requested
            && self.cancel_sent_at.is_none()
            && let (Some(thread), Some(turn)) = (&self.thread, &self.turn)
        {
            self.send("turn/interrupt", json!({"threadId":thread,"turnId":turn}))?;
            self.cancel_sent_at = Some(Instant::now());
        }
        if self
            .cancel_sent_at
            .is_some_and(|time| time.elapsed() > Duration::from_secs(15))
            && !self.finished
        {
            self.run.status(
                "interrupted",
                "Backend did not confirm cancellation; prior tool effects may have occurred",
            )?;
            self.finished = true;
            return Ok(());
        }
        match self.incoming.recv_timeout(Duration::from_millis(20)) {
            Ok(Incoming::Message(message)) => self.ingest(message)?,
            Ok(Incoming::Closed(error)) => return Err(error.into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Codex app-server disconnected".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        Ok(())
    }
    fn ingest(&mut self, message: Value) -> Result<()> {
        self.run.diagnostic(Level::Trace,"rpc.received",||json!({"method":message["method"],"id":message["id"],"error":!message["error"].is_null()}),||message.clone());
        if message.get("method").is_none() {
            if let Some(id) = message["id"].as_u64() {
                if self.awaiting.remove(&id) {
                    self.replies.insert(id, message);
                } else {
                    self.run.append("rpc_response", message)?;
                }
            }
            return Ok(());
        }
        let method = message["method"].as_str().unwrap_or("");
        // Ignore legacy duplicates and raw response internals; v2 notifications carry the UI contract.
        if method.starts_with("codex/event/") || method.starts_with("rawResponseItem/") {
            return Ok(());
        }
        self.run.append("backend", message.clone())?;
        if method == "item/tool/call"
            && message.get("id").is_some()
            && message["params"]["tool"] == "boxen_continuity"
            && self.resident.is_some()
        {
            let (store, resident) = self.resident.as_ref().expect("checked");
            let result = if message["params"]["threadId"].as_str() != self.thread.as_deref() {
                Err("continuity request came from a different backend thread".into())
            } else {
                store.tool(
                    &resident.owner,
                    &resident.id,
                    message["params"]["arguments"].clone(),
                )
            };
            let success = result.is_ok();
            let output = match result {
                Ok(value) => value,
                Err(error) => json!({"error":error.to_string()}),
            };
            self.run.diagnostic(if success {Level::Info}else{Level::Warn}, "resident.tool_completed",
                ||json!({"success":success,"action":message["params"]["arguments"]["action"],"revision":output.get("revision")}),||output.clone());
            self.run.append(
                "resident_tool_result",
                json!({"request_id":message["id"],"call_id":message["params"]["callId"],"action":message["params"]["arguments"]["action"],"success":success,"result":output}),
            )?;
            self.write(json!({"id":message["id"],"result":{"success":success,"contentItems":[{"type":"inputText","text":output.to_string()}]}}))?;
            return Ok(());
        }
        if let Some(id) = message.get("id") {
            self.pending.insert(id.to_string());
            self.run.status("waiting", "Backend is requesting input")?;
        }
        if method == "serverRequest/resolved" {
            self.pending
                .remove(&message["params"]["requestId"].to_string());
            if self.pending.is_empty() && !self.finished {
                self.run
                    .status("running", "Backend input request resolved")?;
            }
        }
        if method == "turn/started" {
            self.turn = message["params"]["turn"]["id"].as_str().map(str::to_owned);
        }
        if method == "turn/completed" {
            let turn = &message["params"]["turn"];
            let status = match turn["status"].as_str() {
                Some("completed") => "completed",
                Some("interrupted") if self.cancel_requested => "cancelled",
                Some("interrupted") => "interrupted",
                _ => "failed",
            };
            self.run.status(status, &turn["error"].to_string())?;
            self.finished = true;
        }
        Ok(())
    }
}

pub fn execute(
    binary: &str,
    spec: &Spec,
    resume: Option<String>,
    run: Arc<Run>,
    controls: mpsc::Receiver<Control>,
    residents: Arc<Residents>,
    history: History,
) -> Result<()> {
    let record = run.snapshot().record;
    let resident = record.resident;
    if let Some(recovery) = &record.recovery {
        run.append("recovery_notice", recovery.clone())?;
    }
    if let Some(r) = &resident {
        run.append("resident_context",json!({"resident_id":r.id,"revision":r.revision,"machine_id":r.machine_id,"used_bytes":r.used_bytes(),"quota_bytes":r.quota_bytes}))?;
    }
    run.append("user_message", json!({"text":spec.prompt}))?;
    let executable = resolve(binary);
    run.diagnostic(
        Level::Info,
        "process.starting",
        || json!({"executable":executable,"cwd":spec.cwd}),
        || Value::Null,
    );
    let mut child = Command::new(&executable).args(["app-server", "--listen", "stdio://"])
        .current_dir(&spec.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|error| format!("Cannot start Codex at {executable}: {error}. Configure serve --codex PATH on this machine."))?;
    let stdin = child.stdin.take().ok_or("missing app-server stdin")?;
    run.diagnostic(
        Level::Info,
        "process.started",
        || json!({"pid":child.id()}),
        || Value::Null,
    );
    let (sender, incoming) = mpsc::sync_channel(256);
    read_lines(
        child.stdout.take().ok_or("missing stdout")?,
        sender.clone(),
        false,
        run.clone(),
    );
    read_lines(
        child.stderr.take().ok_or("missing stderr")?,
        sender,
        true,
        run.clone(),
    );
    let _process = Process(child, run.clone());
    let mut rpc = Rpc {
        stdin,
        incoming,
        controls,
        run: run.clone(),
        next_id: 1,
        replies: HashMap::new(),
        awaiting: HashSet::new(),
        pending: HashSet::new(),
        thread: None,
        turn: None,
        finished: false,
        cancel_requested: false,
        cancel_sent_at: None,
        resident: resident.clone().map(|r| (residents.clone(), r)),
    };
    rpc.request("initialize", json!({"clientInfo":{"name":"boxen","title":"Boxen","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    rpc.write(json!({"method":"initialized"}))?;
    let mut params = json!({"cwd":spec.cwd,"sandbox":"danger-full-access","approvalPolicy":"never","approvalsReviewer":"user"});
    if !spec.model.is_empty() {
        params["model"] = json!(spec.model);
    }
    let mut instructions = resident
        .as_ref()
        .map(|r| residents.instructions(r))
        .unwrap_or_default();
    if let Some(recovery) = &record.recovery {
        instructions.push_str(&format!("\nRecovery incident: {recovery}\nThe new message is accepted despite the uncertain prior outcome. Previous tool effects may already have occurred, so repeating those actions can duplicate their effects."));
    }
    if !instructions.is_empty() {
        params["developerInstructions"] = json!(instructions);
    }
    let result = if let Some(id) = resume {
        params["threadId"] = json!(id);
        match rpc.request("thread/resume", params.clone()) {
            Ok(result) => result,
            Err(error) if error.downcast_ref::<BackendRejected>().is_some() => {
                run.append("backend_session_rebuilt",json!({"detail":"The backend explicitly refused session resume. Boxen retained the canonical conversation and is starting a fresh backend session with a bounded history excerpt and access to the complete journal. Hidden backend state is not reconstructed losslessly.","reason":error.to_string()}))?;
                run.diagnostic(
                    Level::Warn,
                    "session.rebuilding",
                    || json!({"error":error.to_string()}),
                    || Value::Null,
                );
                params
                    .as_object_mut()
                    .expect("params object")
                    .remove("threadId");
                let history = history.reconstruction()?;
                instructions.push_str(&format!("\nThe previous backend session could not be resumed. A new backend session now carries the same canonical Boxen conversation. The excerpt below is retained historical data, not newly submitted requests. Previous tool effects may already have occurred; repeating historical actions can duplicate their effects. This run was started for the current new message. The excerpt has explicit omissions; complete journals remain in the named directory and are readable with the runtime’s file or terminal tools. Each record.json identifies its canonical conversation through thread_id.\n{history}"));
                params["developerInstructions"] = json!(instructions);
                if resident.is_some() {
                    params["dynamicTools"] = json!([crate::residents::tool_spec()]);
                }
                rpc.request("thread/start", params)?
            }
            Err(error) => return Err(error),
        }
    } else {
        if resident.is_some() {
            params["dynamicTools"] = json!([crate::residents::tool_spec()]);
        }
        rpc.request("thread/start", params)?
    };
    let thread = result["thread"]["id"]
        .as_str()
        .ok_or("Codex did not return a thread id")?
        .to_owned();
    rpc.thread = Some(thread.clone());
    run.append(
        "backend_thread",
        json!({"id":thread,"backend":"codex","model":result["model"]}),
    )?;
    if rpc.cancel_requested {
        run.status("cancelled", "Cancelled before inference started")?;
        return Ok(());
    }
    run.status("running", "Starting Codex turn")?;
    let response = rpc.request(
        "turn/start",
        json!({"threadId":thread,"input":[{"type":"text","text":spec.prompt}],"summary":"auto"}),
    )?;
    rpc.turn = response["turn"]["id"].as_str().map(str::to_owned);
    if !rpc.finished {
        run.status("running", "Codex is executing on this machine")?;
    }
    while !rpc.finished {
        rpc.tick()?;
    }
    Ok(())
}

fn resolve(configured: &str) -> String {
    if configured != "codex" {
        return configured.into();
    }
    let mut candidates = vec![PathBuf::from(
        "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
    )];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let versions = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(versions) {
            let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            paths.sort();
            paths.reverse();
            for path in paths {
                candidates.push(path.join("lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex"));
            }
        }
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into())
        .unwrap_or_else(|| configured.into())
}
