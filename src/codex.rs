//! One app-server process per active run; completed threads resume on demand.
use crate::logging::Level;
use crate::{
    Result,
    agent_tools::{resident_tool, resident_tools},
    jobs::{Control, History, Jobs, Run, RunOutcome, Spec},
    residents::Resident,
    runtime::OutstandingWork,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read, Write},
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
    config_reply: Option<u64>,
    steering: HashMap<u64, Value>,
    pending: HashSet<String>,
    thread: Option<String>,
    turn: Option<String>,
    prompt_submitted: bool,
    response_seen: bool,
    native_outcome: Option<RunOutcome>,
    tools: OutstandingWork,
    outcome: Option<RunOutcome>,
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
        if method == "config/read" {
            self.config_reply = Some(id);
        }
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
            // A terminal turn notification may precede the start acknowledgement.
            // Once the outcome is observed, an absent acknowledgement cannot change it.
            if method == "turn/start" && (self.native_outcome.is_some() || self.outcome.is_some()) {
                self.awaiting.remove(&id);
                return Ok(Value::Null);
            }
        }
    }
    fn default_effort(&mut self, cwd: &str, session: &Value) -> Result<String> {
        let config = self.request("config/read", json!({"cwd":cwd,"includeLayers":false}))?;
        if let Some(effort) = config["effort"].as_str() {
            return Ok(effort.into());
        }
        let model = session["model"]
            .as_str()
            .ok_or("Codex did not identify the session model")?;
        let mut params = json!({"includeHidden":true,"limit":100});
        if let Some(provider) = session["modelProvider"].as_str() {
            params["modelProvider"] = json!(provider);
        }
        for _ in 0..8 {
            let catalog = self.request("model/list", params.clone())?;
            if let Some(effort) = catalog["data"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry["model"] == model || entry["id"] == model)
                .and_then(|entry| entry["defaultReasoningEffort"].as_str())
            {
                return Ok(effort.into());
            }
            let Some(cursor) = catalog["nextCursor"].as_str() else {
                break;
            };
            params["cursor"] = json!(cursor);
        }
        Err("Codex reported no reasoning default for this model; an explicit supported level can start the next turn".into())
    }
    fn tick(&mut self) -> Result<()> {
        while let Ok(control) = self.controls.try_recv() {
            match control {
                Control::Cancel => {
                    self.cancel_requested = true;
                    self.run
                        .append("control", json!({"type":"cancel_requested"}))?;
                }
                Control::Steer {
                    text,
                    attachments,
                    file_context,
                    update_id,
                } => {
                    if self.native_outcome.is_none()
                        && let (Some(thread), Some(turn)) = (&self.thread, &self.turn)
                    {
                        let mut input = Vec::new();
                        if !text.is_empty() {
                            input.push(json!({"type":"text","text":text}));
                        }
                        if !file_context.is_empty() {
                            input.push(json!({"type":"text","text":file_context}));
                        }
                        let params = json!({"threadId":thread,"expectedTurnId":turn,"input":input});
                        let id = self.send("turn/steer", params)?;
                        let message = json!({"text":text,"attachments":attachments,"steering":true,"update_id":update_id});
                        self.run.append(
                            "control",
                            json!({"type":"steer_requested","request_id":id,"message":message}),
                        )?;
                        self.steering.insert(id, message);
                    } else {
                        self.run.append(
                            "control_error",
                            json!({"message":"Steering requires an active Codex turn","dispatch":"not_sent","update":{"update_id":update_id}}),
                        )?;
                    }
                }
                Control::Respond { request_id, result, attempt } => {
                    if !self.pending.remove(&request_id.to_string()) {
                        self.run.response_progress("response_refused", &request_id, attempt.as_ref(), "request_not_pending")?;
                        self.run.append("control_error", json!({"message":"Request is no longer pending","request_id":request_id}))?;
                    } else {
                        self.run.response_progress("response_write_started", &request_id, attempt.as_ref(), "writing_to_codex")?;
                        self.write(json!({"id":request_id,"result":result}))?;
                        self.run.append("request_answered", json!({
                            "request_id":request_id,"attempt_id":attempt.as_ref().map(|attempt| &attempt.id)
                        }))?;
                        if self.pending.is_empty() && self.outcome.is_none() {
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
            && self.outcome.is_none()
        {
            self.outcome = Some(RunOutcome::Interrupted(
                "Backend did not confirm cancellation; prior tool effects may have occurred".into(),
            ));
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
        self.settle_native_turn();
        Ok(())
    }
    fn settle_native_turn(&mut self) {
        if self.outcome.is_some() {
            return;
        }
        let Some(outcome) = self.native_outcome.as_ref() else {
            return;
        };
        let pending =
            !self.tools.is_empty() || !self.pending.is_empty() || !self.steering.is_empty();
        if !pending {
            self.outcome = if matches!(outcome, RunOutcome::Completed(_))
                && self.resident.is_none()
                && !self.response_seen
            {
                Some(RunOutcome::Failed(
                    "Codex ended without a conventional text response.".into(),
                ))
            } else {
                self.native_outcome.take()
            };
        } else if !matches!(outcome, RunOutcome::Completed(_)) {
            self.outcome = Some(RunOutcome::Interrupted(
                "Codex ended the turn without confirming the outcome of outstanding work or controls.".into(),
            ));
        }
    }

    fn tracks_turn(&self, params: &Value) -> bool {
        self.prompt_submitted
            && params["threadId"]
                .as_str()
                .is_none_or(|thread| self.thread.as_deref() == Some(thread))
            && params["turnId"]
                .as_str()
                .or_else(|| params["turn"]["id"].as_str())
                .is_none_or(|turn| self.turn.as_deref().is_none_or(|active| active == turn))
    }

    fn observe_turn(&mut self, method: &str, params: &Value) -> Result<()> {
        if !self.tracks_turn(params) {
            return Ok(());
        }
        let item = &params["item"];
        match method {
            "turn/started" => {
                if let Some(turn) = params["turn"]["id"].as_str() {
                    self.turn.get_or_insert_with(|| turn.to_owned());
                }
            }
            "item/started"
                if matches!(
                    item["type"].as_str(),
                    Some(
                        "commandExecution"
                            | "dynamicToolCall"
                            | "mcpToolCall"
                            | "collabAgentToolCall"
                            | "fileChange"
                    )
                ) =>
            {
                let id = item["id"].as_str().ok_or("Codex work item has no ID")?;
                self.tools.start(id)?;
            }
            "item/completed" => {
                if item["type"] == "agentMessage"
                    && (item["phase"].is_null() || item["phase"] == "final_answer")
                    && item["text"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
                {
                    self.response_seen = true;
                }
                if let Some(id) = item["id"].as_str() {
                    self.tools.finish(id);
                }
            }
            "turn/completed" if self.native_outcome.is_none() => {
                let turn = &params["turn"];
                if let Some(id) = turn["id"].as_str() {
                    self.turn.get_or_insert_with(|| id.to_owned());
                }
                let detail = turn["error"].to_string();
                let status = turn["status"].as_str();
                self.native_outcome = Some(match status {
                    Some("completed") => RunOutcome::Completed(detail),
                    Some("interrupted") if self.cancel_requested => RunOutcome::Cancelled(detail),
                    Some("interrupted") => RunOutcome::Interrupted(detail),
                    _ => RunOutcome::Failed(detail),
                });
                self.run.append("backend_prompt_ended", json!({"backend":"codex","reason":status,
                    "pending_tools":self.tools.len(),"pending_questions":self.pending.len(),"pending_steering":self.steering.len()}))?;
                self.run.diagnostic(Level::Info, "backend.prompt_ended",
                    || json!({"turn_id":turn["id"],"reason":status,"pending_tools":self.tools.len(),
                        "pending_questions":self.pending.len(),"pending_steering":self.steering.len()}),
                    || Value::Null);
            }
            _ => {}
        }
        Ok(())
    }

    fn ingest(&mut self, mut message: Value) -> Result<()> {
        if let Some(id) = self.config_reply
            && message.get("method").is_none()
            && message["id"] == id
        {
            self.config_reply = None;
            // Only this value is needed. Host configuration can carry credentials in
            // arbitrary fields; the full response must never reach diagnostic payloads.
            message = if message["error"].is_null() {
                let effort = message["result"]["config"]["model_reasoning_effort"]
                    .as_str()
                    .filter(|effort| crate::reasoning::validate("codex", effort).is_ok());
                json!({"id":id,"result":{"effort":effort}})
            } else {
                json!({"id":id,"error":{"message":"Codex did not return its effective configuration"}})
            };
        }
        self.run.diagnostic(Level::Trace,"rpc.received",||json!({"method":message["method"],"id":message["id"],"error":!message["error"].is_null()}),||message.clone());
        if message.get("method").is_none() {
            if let Some(id) = message["id"].as_u64() {
                if self.awaiting.remove(&id) {
                    self.replies.insert(id, message);
                } else if let Some(update) = self.steering.remove(&id) {
                    if message.get("error").is_some() {
                        self.run.append("control_error", json!({"request_id":id,"message":"Codex rejected the update","update":update,"error":message["error"]}))?;
                    } else {
                        self.run.append("user_message", update)?;
                    }
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
        if self.prompt_submitted
            && (method.starts_with("item/") || method.starts_with("turn/"))
            && !self.tracks_turn(&message["params"])
        {
            self.run.append("backend_other_turn", message.clone())?;
            self.run.diagnostic(Level::Debug, "backend.other_turn",
                || json!({"method":method,"thread_id":message["params"]["threadId"],"turn_id":message["params"]["turnId"]}),
                || Value::Null);
            if let Some(id) = message.get("id") {
                self.write(json!({"id":id,"error":{"code":-32602,"message":"The operation belongs to a different native turn."}}))?;
            }
            return Ok(());
        }
        self.run.append("backend", message.clone())?;
        if !self.cancel_requested {
            match self.jobs.tool_approvals.automatic(&message) {
                Ok(Some(category)) => {
                    self.run.append("tool_approval_decision", json!({"request_id":message["id"],"category":category,"automatic":true,"action":"accept"}))?;
                    self.run.diagnostic(
                        Level::Info,
                        "tool_approval.accepted",
                        || json!({"request_id":message["id"],"category":category,"automatic":true}),
                        || Value::Null,
                    );
                    self.write(
                        json!({"id":message["id"],"result":{"action":"accept","content":{}}}),
                    )?;
                    self.run.append(
                        "request_answered",
                        json!({"request_id":message["id"],"automatic":true,"category":category}),
                    )?;
                    return Ok(());
                }
                Err(error) => {
                    self.run.append(
                        "tool_approval_error",
                        json!({"request_id":message["id"],"detail":error.to_string()}),
                    )?;
                    self.run.diagnostic(
                        Level::Warn,
                        "tool_approval.settings_unavailable",
                        || json!({"request_id":message["id"],"error":error.to_string()}),
                        || Value::Null,
                    );
                }
                Ok(None) => {}
            }
        }
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
                        | "boxen_thread_files"
                )
            )
            && (self.resident.is_some()
                || matches!(
                    message["params"]["tool"].as_str(),
                    Some("boxen_observations" | "boxen_adapter" | "boxen_thread_files")
                ))
        {
            let result = if message["params"]["threadId"].as_str() != self.thread.as_deref() {
                Err("continuity request came from a different backend thread".into())
            } else if message["params"]["tool"] == "boxen_thread_files" {
                crate::work_tools::call(
                    &self.jobs,
                    &self.run,
                    message["params"]["callId"].as_str().unwrap_or("missing"),
                    "boxen_thread_files",
                    message["params"]["arguments"].clone(),
                )
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
            if self.pending.is_empty() && self.outcome.is_none() {
                self.run
                    .status("running", "Backend input request resolved")?;
            }
        }
        self.observe_turn(method, &message["params"])?;
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
) -> Result<RunOutcome> {
    let residents = jobs.residents.clone();
    let record = run.snapshot().record;
    if let (Some(execution), Some(id)) = (&record.execution, &resume)
        && let Some(path) = history.native_session_path(id)?
    {
        execution.own_native_session(&path, id)?;
        run.diagnostic(
            Level::Info,
            "execution.native_session_prepared",
            || json!({"account":execution.account,"backend_thread_id":id}),
            || Value::Null,
        );
    }
    let browser = crate::browser_tools::for_run(&jobs, &record).inspect_err(|error| {
        run.diagnostic(
            Level::Error,
            "browser_assignment.rejected",
            || json!({"detail":error.to_string()}),
            || Value::Null,
        );
    })?;
    let promoted = record
        .resident
        .as_ref()
        .is_some_and(|r| r.promotion.is_some());
    let mut trigger_context = record.trigger_context() + &record.observation_context();
    if let Some(browser) = &browser {
        run.append("browser_assignment_attached", browser.identity())?;
        run.diagnostic(
            Level::Info,
            "browser_assignment.attached",
            || browser.identity(),
            || Value::Null,
        );
        trigger_context.push('\n');
        trigger_context.push_str(browser.context());
    }
    if (history.native_import.is_some() || promoted)
        && let Some(recovery) = &record.recovery
    {
        trigger_context.push_str(&format!("\nRecovery incident: {recovery}\nPrevious tool effects may already have occurred. This new request does not replay those effects."));
    }
    let execution_context = run.runtime_context()?;
    let resident = record.resident;
    if promoted {
        trigger_context.push_str(&format!("\nResident attachment (Boxen-authored runtime context, separate from the message authors and the saved native instructions):\n{}",residents.instructions(resident.as_ref().unwrap())));
        trigger_context.push_str("\nBoxen resident operations are available through the boxen_resident MCP connection. Historical assistant output remains earlier activity; explicit messaging tools deliver new correspondence.");
    }
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
        json!({"text":spec.prompt,"attachments":spec.attachments}),
    )?;
    let bridge = if promoted {
        Some(crate::resident_bridge::Bridge::start(
            jobs.clone(),
            run.clone(),
            resident.as_ref().unwrap().clone(),
        )?)
    } else {
        Some(crate::resident_bridge::Bridge::start_work(
            jobs.clone(),
            run.clone(),
        )?)
    };
    let mut launch = crate::infrastructure::Attempt::new(
        "runtime",
        "codex_launch",
        None,
        Some(&spec.id),
        Some(&spec.thread_id),
        None,
    );
    launch.runtime(None, &record.runtime_policy);
    launch.record("launch", "attempted", None);
    let mut child = crate::codex_executable::launch(
        binary, crate::platform::host_os(),
        |selected| {
            let mut command = record.execution.as_ref().map_or_else(
                || Command::new(&selected.path),
                |execution| execution.command(&selected.path),
            );
            if let Some(bridge) = &bridge {
                command.env(crate::resident_bridge::TOKEN_ENV, &bridge.token);
            }
            command.args(["app-server", "--listen", "stdio://"])
                .current_dir(&spec.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        },
        |selected, retry| {
            launch.runtime(selected.version.as_deref(), &record.runtime_policy);
            run.diagnostic(Level::Info, "process.starting",
                || json!({"executable":selected.path,"version":selected.version,"automatic":selected.automatic,"probe_fallback":selected.probe_fallback,"rediscovered":retry,"cwd":spec.cwd}), || Value::Null);
        },
    ).map_err(|error| {
        launch.record("launch", "failed", Some(crate::infrastructure::reason(&error)));
        run.diagnostic(Level::Error, "process.start_failed", ||json!({"detail":error.to_string()}), ||Value::Null);
        format!("Cannot start Codex: {error}")
    })?;
    launch.record("launch", "completed", None);
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
        config_reply: None,
        steering: HashMap::new(),
        pending: HashSet::new(),
        thread: None,
        turn: None,
        prompt_submitted: false,
        response_seen: false,
        native_outcome: None,
        tools: OutstandingWork::default(),
        outcome: None,
        cancel_requested: false,
        cancel_sent_at: None,
        resident: resident.clone(),
        jobs,
    };
    rpc.request("initialize", json!({"clientInfo":{"name":"boxen","title":"Boxen","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    rpc.write(json!({"method":"initialized"}))?;
    let mut params = json!({"cwd":spec.cwd,"sandbox":"danger-full-access","approvalPolicy":"on-request","approvalsReviewer":"user"});
    run.append("backend_execution_policy", json!({"sandbox":"danger-full-access","approval_policy":"on-request","approvals_reviewer":"user"}))?;
    run.diagnostic(Level::Info, "backend.execution_policy", || json!({"sandbox":"danger-full-access","approval_policy":"on-request","approvals_reviewer":"user"}), || Value::Null);
    if let Some(bridge) = &bridge {
        let server = if promoted {
            "boxen_resident"
        } else {
            "boxen_work"
        };
        let scope = if promoted { "activation" } else { "run" };
        params["config"] = json!({format!("mcp_servers.{server}"):bridge.config()});
        run.append(
            if promoted {
                "resident_bridge_approval_policy"
            } else {
                "work_bridge_approval_policy"
            },
            json!({"server":server,"default_tools_approval_mode":"approve","scope":scope}),
        )?;
        run.diagnostic(
            Level::Info,
            "backend.resident_bridge_approval_policy",
            || json!({"server":server,"default_tools_approval_mode":"approve","scope":scope}),
            || Value::Null,
        );
    }
    if let Some(browser) = &browser {
        params["config"]["mcp_servers.boxen_browser"] = browser.config();
    }
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
        .filter(|_| !promoted)
        .map(|r| residents.instructions(r))
        .unwrap_or_default();
    if let Some(recovery) = &record.recovery
        && history.native_import.is_none()
        && !promoted
    {
        instructions.push_str(&format!("\nRecovery incident: {recovery}\nThe new message is accepted despite the uncertain prior outcome. Previous tool effects may already have occurred, so repeating those actions can duplicate their effects."));
    }
    if let Some(reason) = &history.rebuild_reason {
        let context = history.reconstruction()?;
        run.append("backend_session_rebuilt",json!({"detail":reason,"reconstruction":"Bounded retained history; full journals remain available."}))?;
        instructions.push_str(&format!("\nSession change: {reason} The following is retained historical data. The current request is the new input. Repeating historical actions can duplicate effects that already occurred. The bounded excerpt reports omissions; complete journals remain in the named directory.\n{context}"));
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
    if resident.is_none() {
        dynamic.push(crate::jobs::thread_workspace::tool());
    }
    let result = if let Some(id) = resume {
        params["threadId"] = json!(id);
        // Resuming needs session metadata, not an inline copy of every old turn.
        // History remains in Codex and Boxen's retained journals.
        let mut resume_params = params.clone();
        resume_params["excludeTurns"] = json!(true);
        run.diagnostic(
            Level::Debug,
            "backend.resume.requested",
            || json!({"backend_thread_id":id,"exclude_turns":true}),
            || Value::Null,
        );
        match rpc.request("thread/resume", resume_params) {
            Ok(result) => result,
            Err(error)
                if error.downcast_ref::<BackendRejected>().is_some()
                    && history.native_import.is_none()
                    && !promoted =>
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
    if let Some(promotion) = resident.as_ref().and_then(|r| r.promotion.as_ref())
        && thread != promotion.backend_thread_id
    {
        return Err(
            "promoted session resumed a different native thread; no turn was started".into(),
        );
    }
    rpc.thread = Some(thread.clone());
    if bridge.is_some() {
        let server = if promoted {
            "boxen_resident"
        } else {
            "boxen_work"
        };
        let status = rpc.request(
            "mcpServerStatus/list",
            json!({"threadId":thread,"serverName":server,"detail":"toolsAndAuthOnly"}),
        )?;
        let ready = status["data"].as_array().into_iter().flatten().any(|s| {
            s["name"] == server
                && s["tools"].get("boxen_machines").is_some()
                && s["tools"].get("boxen_work").is_some()
                && (!promoted
                    || (s["tools"].get("boxen_continuity").is_some()
                        && s["tools"].get("boxen_send_message").is_some()))
        });
        if !ready {
            return Err(
                "The native session did not expose its run-scoped Boxen work tools; no turn was started"
                    .into(),
            );
        }
        run.append(
            if promoted {
                "resident_bridge_ready"
            } else {
                "work_bridge_ready"
            },
            json!({"backend_thread_id":thread,"server":server}),
        )?;
    }
    if let Some(browser) = &browser {
        let status = rpc.request(
            "mcpServerStatus/list",
            json!({"threadId":thread,"serverName":"boxen_browser","detail":"toolsAndAuthOnly"}),
        )?;
        let ready = status["data"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|entry| {
                entry["name"] == "boxen_browser"
                    && browser
                        .tools()
                        .iter()
                        .all(|name| entry["tools"].get(*name).is_some())
            });
        if !ready {
            run.diagnostic(
                Level::Error,
                "browser_assignment.unavailable",
                || browser.identity(),
                || Value::Null,
            );
            return Err(
                "the assigned browser MCP tools were unavailable; no turn was started".into(),
            );
        }
        run.append("browser_assignment_ready", browser.identity())?;
        run.diagnostic(
            Level::Info,
            "browser_assignment.ready",
            || browser.identity(),
            || Value::Null,
        );
    }
    run.append(
        "backend_thread",
        json!({"id":thread,"backend":"codex","model":result["model"]}),
    )?;
    if rpc.cancel_requested {
        return Ok(RunOutcome::Cancelled(
            "Cancelled before inference started".into(),
        ));
    }
    let mut input = Vec::new();
    if !spec.prompt.is_empty() {
        input.push(json!({"type":"text","text":spec.prompt}));
    }
    input.push(json!({"type":"text","text":format!("Boxen provides the following current-run metadata independently of the request author. {}{}{}",trigger_context,execution_context,rpc.jobs.attachments.context(&spec.attachments))}));
    // An omitted turn override resumes the previous effort. Explicit reset therefore
    // resolves and sends a fresh default without changing the native thread.
    let effort = match record.reasoning_effort.as_deref() {
        Some("default") => Some(rpc.default_effort(&spec.cwd, &result)?),
        value => value.map(str::to_owned),
    };
    if rpc.cancel_requested {
        return Ok(RunOutcome::Cancelled(
            "Cancelled before inference started".into(),
        ));
    }
    run.status("running", "Starting Codex turn")?;
    let mut turn = json!({"threadId":thread,"input":input,"summary":"auto"});
    if let Some(effort) = &effort {
        turn["effort"] = json!(effort);
    }
    run.diagnostic(
        Level::Info,
        "backend.turn_requested",
        || json!({"backend_thread_id":thread,"reasoning_effort":effort}),
        || Value::Null,
    );
    rpc.prompt_submitted = true;
    let response = rpc.request("turn/start", turn)?;
    if let Some(id) = response["turn"]["id"].as_str() {
        if rpc.turn.as_deref().is_some_and(|observed| observed != id) {
            return Err("Codex acknowledged a different active turn".into());
        }
        rpc.turn.get_or_insert_with(|| id.to_owned());
    }
    if rpc.outcome.is_none() && rpc.native_outcome.is_none() && rpc.pending.is_empty() {
        run.status("running", "Codex is executing on this machine")?;
    }
    loop {
        if let Some(outcome) = rpc.outcome.take() {
            return Ok(outcome);
        }
        rpc.tick()?;
    }
}
