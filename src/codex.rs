//! One app-server process per active run; completed threads resume on demand.
use crate::logging::Level;
use crate::{
    Result,
    agent_tools::{resident_tool, resident_tools},
    jobs::{Control, History, Jobs, Run, Spec},
    residents::Resident,
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
    resident: Option<Resident>,
    jobs: Arc<Jobs>,
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
                            json!({"message":"Steering becomes available after the turn starts"}),
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
            && matches!(
                message["params"]["tool"].as_str(),
                Some(
                    "boxen_continuity"
                        | "boxen_schedule"
                        | "boxen_messages"
                        | "boxen_mailbox"
                        | "boxen_send_message"
                        | "boxen_thread_request"
                        | "boxen_thread_result"
                        | "boxen_observations"
                        | "boxen_adapter"
                )
            )
            && (self.resident.is_some()
                || matches!(
                    message["params"]["tool"].as_str(),
                    Some("boxen_observations" | "boxen_adapter")
                ))
        {
            let result = if message["params"]["threadId"].as_str() != self.thread.as_deref() {
                Err("continuity request came from a different backend thread".into())
            } else if matches!(
                message["params"]["tool"].as_str(),
                Some("boxen_observations" | "boxen_adapter")
            ) {
                crate::agent_tools::observation_tool(
                    &self.jobs,
                    &self.run,
                    message["params"]["callId"].as_str().unwrap_or("missing"),
                    message["params"]["tool"].as_str().unwrap_or(""),
                    message["params"]["arguments"].clone(),
                )
            } else {
                resident_tool(
                    &self.jobs,
                    self.resident.as_ref().expect("checked"),
                    &self.run.snapshot().record.spec.id,
                    message["params"]["callId"].as_str().unwrap_or("missing"),
                    message["params"]["tool"].as_str().unwrap_or(""),
                    message["params"]["arguments"].clone(),
                )
            };
            let success = result.is_ok();
            let output = match result {
                Ok(value) => value,
                Err(error) => json!({"error":error.to_string()}),
            };
            self.run.diagnostic(if success {Level::Info}else{Level::Warn}, "resident.tool_completed",
                ||json!({"success":success,"tool":message["params"]["tool"],"call_id":message["params"]["callId"],"action":message["params"]["arguments"]["action"],"revision":output.get("revision")}),||output.clone());
            self.run.append(
                "resident_tool_result",
                json!({"request_id":message["id"],"call_id":message["params"]["callId"],"action":message["params"]["arguments"]["action"],"tool":message["params"]["tool"],"success":success,"result":output}),
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
    mut resume: Option<String>,
    run: Arc<Run>,
    controls: mpsc::Receiver<Control>,
    jobs: Arc<Jobs>,
    history: History,
) -> Result<()> {
    let residents = jobs.residents.clone();
    let record = run.snapshot().record;
    let mut trigger_context = record.trigger_context() + &record.observation_context();
    if history.native_import.is_some()
        && let Some(recovery) = &record.recovery
    {
        trigger_context.push_str(&format!("\nRecovery incident: {recovery}\nPrevious tool effects may already have occurred. This new request does not replay those effects."));
    }
    let resident = record.resident;
    if let Some(recovery) = &record.recovery {
        run.append("recovery_notice", recovery.clone())?;
    }
    if let Some(r) = &resident {
        run.append("resident_context",json!({"resident_id":r.id,"revision":r.revision,"machine_id":r.machine_id,"used_bytes":r.used_bytes(),"quota_bytes":r.quota_bytes}))?;
    }
    run.append(
        if record.mailbox.is_some() {
            "activation_input"
        } else {
            "user_message"
        },
        json!({"text":spec.prompt}),
    )?;
    let executable = resolve(binary);
    run.diagnostic(
        Level::Info,
        "process.starting",
        || json!({"executable":executable,"cwd":spec.cwd}),
        || Value::Null,
    );
    let mut child = Command::new(&executable).args(["app-server", "--listen", "stdio://"])
        .current_dir(&spec.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|error| format!("Cannot start Codex at {executable}: {error}. The executing daemon selects its backend with serve --codex PATH."))?;
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
        resident: resident.clone(),
        jobs,
    };
    rpc.request("initialize", json!({"clientInfo":{"name":"boxen","title":"Boxen","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    rpc.write(json!({"method":"initialized"}))?;
    let mut params = json!({"cwd":spec.cwd,"sandbox":"danger-full-access","approvalPolicy":"never","approvalsReviewer":"user"});
    if !spec.model.is_empty() {
        params["model"] = json!(spec.model);
    }
    if let Some(imported) = &history.native_import
        && let Some(provider) = imported.metadata["modelProvider"].as_str()
    {
        params["modelProvider"] = json!(provider);
    }
    let mut instructions = resident
        .as_ref()
        .map(|r| residents.instructions(r))
        .unwrap_or_default();
    if let Some(recovery) = &record.recovery
        && history.native_import.is_none()
    {
        instructions.push_str(&format!("\nRecovery incident: {recovery}\nThe new message is accepted despite the uncertain prior outcome. Previous tool effects may already have occurred, so repeating those actions can duplicate their effects."));
    }
    if let Some(reason) = &history.rebuild_reason {
        let context = history.reconstruction()?;
        run.append("backend_session_rebuilt",json!({"detail":reason,"reconstruction":"Bounded retained history; full journals remain available. Private narration in legacy runs is historical activity, not newly sent mailbox messages."}))?;
        instructions.push_str(&format!("\nCommunication migration: {reason} The following is retained historical data, not newly submitted mail or requests. The bounded excerpt reports omissions; complete journals remain in the named directory.\n{context}"));
        resume = None;
    }
    if !instructions.is_empty() {
        params["developerInstructions"] = json!(instructions);
    }
    let mut dynamic = if resident.is_some() {
        resident_tools()
    } else {
        Vec::new()
    };
    dynamic.extend(crate::agent_tools::observation_tools());
    let result = if let Some(id) = resume {
        params["threadId"] = json!(id);
        match rpc.request("thread/resume", params.clone()) {
            Ok(result) => result,
            Err(error)
                if error.downcast_ref::<BackendRejected>().is_some()
                    && history.native_import.is_none() =>
            {
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
                params["dynamicTools"] = json!(dynamic);
                rpc.request("thread/start", params)?
            }
            Err(error) => return Err(error),
        }
    } else {
        params["dynamicTools"] = json!(dynamic);
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
        json!({"threadId":thread,"input":[{"type":"text","text":spec.prompt},{"type":"text","text":format!("Boxen provides the following current-run metadata independently of the request author. {}",trigger_context)}],"summary":"auto"}),
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

pub(crate) fn resolve(configured: &str) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let search = std::env::var_os("PATH");
    resolve_on(
        configured,
        crate::platform::host_os(),
        std::env::consts::ARCH,
        home.as_deref(),
        search.as_deref(),
    )
}

fn resolve_on(
    configured: &str,
    os: &str,
    arch: &str,
    home: Option<&std::path::Path>,
    search: Option<&std::ffi::OsStr>,
) -> String {
    if configured != "codex" {
        return configured.into();
    }
    let name = if os == "windows" {
        "codex.exe"
    } else {
        "codex"
    };
    let mut candidates = Vec::new();
    if os == "macos" {
        candidates.push(PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"));
    }
    let mut prefixes = Vec::new();
    if let Some(search) = search {
        for directory in std::env::split_paths(search).filter(|p| !p.as_os_str().is_empty()) {
            candidates.push(directory.join(name));
            if let Some(parent) = directory.parent() {
                prefixes.push(parent.to_owned());
            }
        }
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/bin").join(name));
        candidates.push(home.join(".cargo/bin").join(name));
        prefixes.push(home.join(".npm-global"));
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            paths.sort();
            paths.reverse();
            prefixes.extend(paths);
        }
    }
    prefixes.extend([
        PathBuf::from("/usr/local"),
        PathBuf::from("/usr"),
        PathBuf::from("/opt/homebrew"),
    ]);
    let platform = match (os, arch) {
        ("linux", "x86_64") => Some(("linux-x64", "x86_64-unknown-linux-musl")),
        ("linux", "aarch64") => Some(("linux-arm64", "aarch64-unknown-linux-musl")),
        ("macos", "x86_64") => Some(("darwin-x64", "x86_64-apple-darwin")),
        ("macos", "aarch64") => Some(("darwin-arm64", "aarch64-apple-darwin")),
        ("windows", "x86_64") => Some(("win32-x64", "x86_64-pc-windows-msvc")),
        ("windows", "aarch64") => Some(("win32-arm64", "aarch64-pc-windows-msvc")),
        _ => None,
    };
    if let Some((package, target)) = platform {
        for prefix in prefixes {
            let modules = prefix.join("lib/node_modules/@openai");
            let codex = modules.join("codex");
            let relative = format!("vendor/{target}/bin/{name}");
            candidates.push(
                codex
                    .join(format!("node_modules/@openai/codex-{package}"))
                    .join(&relative),
            );
            candidates.push(modules.join(format!("codex-{package}")).join(&relative));
            candidates.push(codex.join(&relative));
        }
    }
    candidates
        .into_iter()
        .find(|p| {
            if !p.is_file() {
                return false;
            }
            #[cfg(unix)]
            if os != "windows" {
                use std::os::unix::fs::PermissionsExt;
                return p
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
            }
            true
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| configured.into())
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    #[test]
    fn linux_standalone_and_native_npm_are_discovered_without_desktop_or_node_path() {
        let root = std::env::temp_dir().join(format!(
            "boxen-codex-discovery-{}",
            crate::state::random_hex().unwrap()
        ));
        let binary = root.join(".local/bin/codex");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            PathBuf::from(resolve_on("codex", "linux", "x86_64", Some(&root), None)),
            binary
        );
        assert_eq!(
            resolve_on("/explicit/backend", "linux", "x86_64", Some(&root), None),
            "/explicit/backend"
        );
        std::fs::remove_file(binary).unwrap();
        let binary = root.join(".nvm/versions/node/v22/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            PathBuf::from(resolve_on("codex", "linux", "x86_64", Some(&root), None)),
            binary
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
