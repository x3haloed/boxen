//! Antigravity's native ACP session. Boxen's journal owns run settlement.
use crate::{
    Result,
    jobs::{Control, History, Jobs, Run, RunOutcome, Spec},
    logging::Level,
    resident_bridge::Bridge,
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

const MAX_LINE: u64 = 4 * 1024 * 1024;
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

struct Process {
    child: Child,
    run: Arc<Run>,
}

impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: this child leads a dedicated group created by launch.
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let exit = self.child.wait();
        self.run.diagnostic(
            Level::Info,
            "antigravity.process_stopped",
            || json!({"pid":self.child.id(),"exit":exit.ok().and_then(|status|status.code())}),
            || Value::Null,
        );
    }
}

struct Rpc {
    input: ChildStdin,
    output: mpsc::Receiver<Result<Value>>,
    next: u64,
    run: Arc<Run>,
}

impl Rpc {
    fn launch(binary: &str, spec: &Spec, run: Arc<Run>) -> Result<(Process, Self)> {
        let execution = run.snapshot().record.execution;
        let mut command = execution
            .as_ref()
            .map_or_else(|| Command::new(binary), |e| e.command(binary));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        run.diagnostic(
            Level::Info,
            "antigravity.process_starting",
            || json!({"cwd":spec.cwd}),
            || Value::Null,
        );
        let child = command
            .args(["acp", "--process-per-session", "--disable-telemetry"])
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Cannot start Antigravity: {error}"))?;
        let mut process = Process {
            child,
            run: run.clone(),
        };
        let input = process
            .child
            .stdin
            .take()
            .ok_or("missing Antigravity stdin")?;
        let output = process
            .child
            .stdout
            .take()
            .ok_or("missing Antigravity stdout")?;
        let (tx, output_rx) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            let mut line = Vec::new();
            loop {
                line.clear();
                let item = match reader
                    .by_ref()
                    .take(MAX_LINE + 1)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => break,
                    Ok(_)
                        if line.starts_with(
                            b"Open the following link to authenticate the ACP server: ",
                        ) =>
                    {
                        Err("Antigravity requires sign-in on the executing host".into())
                    }
                    Ok(_) if line.len() as u64 > MAX_LINE => {
                        Err("Antigravity ACP frame exceeds 4 MiB".into())
                    }
                    Ok(_) => serde_json::from_slice(&line)
                        .map_err(|_| "Antigravity returned an invalid ACP frame".into()),
                    Err(error) => Err(error.into()),
                };
                let failed = item.is_err();
                if tx.send(item).is_err() || failed {
                    break;
                }
            }
        });
        run.diagnostic(
            Level::Info,
            "antigravity.process_started",
            || json!({"pid":process.child.id()}),
            || Value::Null,
        );
        Ok((
            process,
            Self {
                input,
                output: output_rx,
                next: 1,
                run,
            },
        ))
    }

    fn send(&mut self, message: Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, &message)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        // ACP setup frames may contain the MCP capability, including in header values.
        self.run.diagnostic(
            Level::Trace,
            "antigravity.rpc_sent",
            || json!({"method":message["method"],"id":message["id"]}),
            || Value::Null,
        );
        Ok(())
    }

    fn call(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        Ok(id)
    }

    fn notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn reply(&mut self, id: &Value, result: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    fn unsupported(&mut self, id: &Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"The client does not provide this ACP operation"}}))
    }

    fn receive(&self) -> Result<Option<Value>> {
        match self.output.recv_timeout(Duration::from_millis(25)) {
            Ok(item) => {
                let message = item?;
                self.run.diagnostic(Level::Trace, "antigravity.rpc_received",
                    || json!({"method":message["method"],"id":message["id"],"error_code":message["error"]["code"]}),
                    || Value::Null);
                Ok(Some(message))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("Antigravity ACP stream closed".into())
            }
        }
    }

    fn setup(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.call(method, params)?;
        let deadline = Instant::now() + SETUP_TIMEOUT;
        while Instant::now() < deadline {
            if self.run.cancellation_requested() {
                return Err("Cancellation requested before the Antigravity prompt".into());
            }
            if let Some(message) = self.receive()? {
                if message.get("method").is_none() && message["id"] == id {
                    return Ok(message);
                }
                if message.get("method").is_some()
                    && let Some(id) = message.get("id")
                {
                    self.unsupported(id)?;
                }
            }
        }
        Err(format!("Antigravity {method} did not respond before the setup deadline").into())
    }
}

fn result<'a>(message: &'a Value, method: &str) -> Result<&'a Value> {
    message.get("result").ok_or_else(|| {
        let code = message["error"]["code"].as_i64();
        if code == Some(-32000) {
            format!("Antigravity rejected {method} (authentication required on the executing host)")
        } else {
            format!("Antigravity rejected {method} (ACP error code {code:?})")
        }
        .into()
    })
}

fn missing_session(response: &Value) -> bool {
    let text = response["error"]["message"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    response.get("error").is_some()
        && [
            "session not found",
            "unknown session",
            "no such session",
            "conversation not found",
        ]
        .iter()
        .any(|phrase| text.contains(phrase))
}

fn config<'a>(options: &'a Value, id: &str) -> Option<&'a Value> {
    options.as_array()?.iter().find(|option| option["id"] == id)
}

fn offered(options: &Value, value: &str) -> bool {
    options.as_array().is_some_and(|options| {
        options
            .iter()
            .any(|entry| entry["value"] == value || offered(&entry["options"], value))
    })
}

fn select(rpc: &mut Rpc, session: &str, options: &mut Value, key: &str, value: &str) -> Result<()> {
    let option =
        config(options, key).ok_or_else(|| format!("Antigravity session has no {key} selector"))?;
    if !offered(&option["options"], value) {
        return Err(
            format!("The requested Antigravity {key} is unavailable in this session").into(),
        );
    }
    if option["currentValue"] != value {
        let selected = rpc.setup(
            "session/set_config_option",
            json!({"sessionId":session,"configId":key,"value":value}),
        )?;
        *options = result(&selected, "session/set_config_option")?["configOptions"].clone();
        if config(options, key).is_none_or(|option| option["currentValue"] != value) {
            return Err(format!("Antigravity did not confirm the requested {key}").into());
        }
    }
    Ok(())
}

fn prompt_parts(text: &str, context: &str) -> Vec<Value> {
    [text, context]
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(|part| json!({"type":"text","text":part}))
        .collect()
}

struct Question {
    id: Value,
    choices: HashSet<String>,
}

struct Turn<'a> {
    rpc: &'a mut Rpc,
    session: &'a str,
    run: &'a Run,
    secret: &'a str,
    tools: OutstandingWork,
    questions: HashMap<String, Question>,
    steering: HashMap<u64, Value>,
    prompt: Option<u64>,
    stop_reason: Option<String>,
    awaiting_wake: bool,
    cancel_sent: Option<Instant>,
    answer: String,
}

impl Turn<'_> {
    fn append_text(&mut self, update: &Value, reasoning: bool) -> Result<()> {
        let Some(text) = update["content"]["text"].as_str() else {
            return Ok(());
        };
        let text = text.replace(self.secret, "[redacted]");
        if !reasoning {
            if self.answer.len() + text.len() > 3 * 1024 * 1024 {
                return Err("Antigravity response exceeds Boxen's retained message limit".into());
            }
            self.answer.push_str(&text);
        }
        self.run.append(if reasoning {"reasoning_delta"} else {"assistant_delta"},
            json!({"id":if reasoning {"acp-thinking"} else {"acp-answer"},"text":text,"backend":"antigravity"}))?;
        Ok(())
    }

    fn update(&mut self, update: &Value) -> Result<()> {
        let kind = update["sessionUpdate"].as_str().unwrap_or("");
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                if self.prompt.is_none() {
                    self.awaiting_wake = true;
                }
                self.append_text(update, kind == "agent_thought_chunk")?;
            }
            "tool_call" | "tool_call_update" => {
                let id = update["toolCallId"]
                    .as_str()
                    .ok_or("ACP tool update has no call ID")?;
                let status = update["status"].as_str();
                let terminal = matches!(status, Some("completed" | "failed" | "cancelled"));
                if terminal {
                    self.tools.finish(id);
                } else if kind == "tool_call" || status.is_some() {
                    self.tools.start(id)?;
                }
                let output = update["rawOutput"]["combinedOutput"]
                    .as_str()
                    .or_else(|| update["rawOutput"]["combined_output"].as_str())
                    .map(|text| text.replace(self.secret, "[redacted]"));
                let title = update["title"]
                    .as_str()
                    .map(|text| text.replace(self.secret, "[redacted]"));
                let data = json!({"call_id":id,"tool":title,"status":status,"backend":"antigravity",
                    "result":output,"output_kind":output.as_ref().map(|_|"aggregate"),
                    "exit_code":update["rawOutput"]["exitCode"].as_i64()});
                self.run.append(
                    if terminal {
                        "tool_result"
                    } else if kind == "tool_call" {
                        "tool_started"
                    } else {
                        "tool_status"
                    },
                    data,
                )?;
            }
            "usage_update" => {
                if self.prompt.is_none() && self.tools.is_empty() && self.questions.is_empty() {
                    self.awaiting_wake = false;
                }
                self.run.append(
                    "backend_usage",
                    json!({"backend":"antigravity",
                    "used":update["used"].as_u64(),"size":update["size"].as_u64()}),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn permission(&mut self, message: &Value) -> Result<()> {
        let id = &message["id"];
        let params = &message["params"];
        let options = params["options"]
            .as_array()
            .ok_or("ACP permission options are invalid")?;
        let tool = &params["toolCall"];
        let is_question = tool["toolCallId"]
            .as_str()
            .is_some_and(|id| id.starts_with("interaction_"));
        if !is_question || self.cancel_sent.is_some() {
            let selected = if self.cancel_sent.is_none() {
                options
                    .iter()
                    .find(|option| option["kind"] == "allow_once")
                    .and_then(|o| o["optionId"].as_str())
            } else {
                None
            };
            let outcome = selected.map_or_else(
                || json!({"outcome":"cancelled"}),
                |value| json!({"outcome":"selected","optionId":value}),
            );
            self.rpc.reply(id, json!({"outcome":outcome}))?;
            self.run.append("tool_approval_decision",json!({"request_id":id,"backend":"antigravity","automatic":true,"action":if selected.is_some() {"accept"} else {"cancel"}}))?;
            return Ok(());
        }
        let choices: Vec<_> = options.iter().filter_map(|o| {
            let key = o["optionId"].as_str()?;
            Some(json!({"id":key,"label":o["name"].as_str().unwrap_or(key).replace(self.secret,"[redacted]")}))
        }).collect();
        let values: HashSet<_> = choices
            .iter()
            .filter_map(|o| o["id"].as_str().map(str::to_owned))
            .collect();
        if choices.is_empty() || values.len() != options.len() || values.contains("") {
            return Err("Antigravity question has ambiguous choices".into());
        }
        let title = tool["title"]
            .as_str()
            .unwrap_or("Antigravity is requesting input")
            .replace(self.secret, "[redacted]");
        if self.questions.len() >= 64 && !self.questions.contains_key(&id.to_string()) {
            return Err("Antigravity exceeded the pending question limit".into());
        }
        self.questions.insert(
            id.to_string(),
            Question {
                id: id.clone(),
                choices: values,
            },
        );
        self.run.append("input_requested",json!({"request_id":id,"backend":"antigravity","question":title,"choices":choices,"answer":{"option_id":"offered choice ID"}}))?;
        self.run
            .status("waiting", "Antigravity is requesting input")?;
        Ok(())
    }

    fn ingest(&mut self, message: Value) -> Result<()> {
        if let Some(method) = message["method"].as_str() {
            if message["params"]["sessionId"] == self.session {
                if method == "session/update" {
                    self.update(&message["params"]["update"])?;
                    return Ok(());
                }
                if method == "session/request_permission" && message.get("id").is_some() {
                    return self.permission(&message);
                }
            }
            if let Some(id) = message.get("id") {
                self.rpc.unsupported(id)?;
            }
            return Ok(());
        }
        let Some(id) = message["id"].as_u64() else {
            return Ok(());
        };
        if let Some(update) = self.steering.remove(&id) {
            if message.get("error").is_some() {
                self.run.append(
                    "control_error",
                    json!({"message":"Antigravity rejected the steer","update":update}),
                )?;
                return Ok(());
            }
            self.run.append("user_message", update)?;
            if self.prompt.is_some_and(|prompt| id > prompt) || self.prompt.is_none() {
                self.prompt = Some(id);
            }
        }
        if self.prompt == Some(id) {
            let end = result(&message, "session/prompt")?;
            let stop = end["stopReason"]
                .as_str()
                .ok_or("Antigravity prompt response has no stop reason")?;
            self.prompt = None;
            self.stop_reason = Some(stop.to_owned());
            self.awaiting_wake = stop == "end_turn" && !self.tools.is_empty();
            self.run.append("backend_prompt_ended",json!({"backend":"antigravity","reason":stop,"pending_tools":self.tools.len(),"pending_questions":self.questions.len()}))?;
        }
        Ok(())
    }

    fn controls(&mut self, receiver: &mpsc::Receiver<Control>) -> Result<()> {
        for _ in 0..64 {
            let Ok(control) = receiver.try_recv() else {
                break;
            };
            match control {
                Control::Cancel => {
                    if self.cancel_sent.is_none() {
                        self.run
                            .append("control", json!({"type":"cancel_requested"}))?;
                        self.rpc
                            .notification("session/cancel", json!({"sessionId":self.session}))?;
                        self.cancel_sent = Some(Instant::now());
                        for (_, question) in self.questions.drain() {
                            self.rpc
                                .reply(&question.id, json!({"outcome":{"outcome":"cancelled"}}))?;
                            self.run.append(
                                "request_cancelled",
                                json!({"backend":"antigravity","request_id":question.id}),
                            )?;
                        }
                    }
                }
                Control::Steer {
                    text,
                    attachments,
                    file_context,
                    update_id,
                } => {
                    if self.prompt.is_none()
                        || self.cancel_sent.is_some()
                        || !self.steering.is_empty()
                    {
                        self.run.append("control_error",json!({"message":"Antigravity steering requires an active prompt and no pending steer or cancellation","update":{"update_id":update_id}}))?;
                    } else {
                        let id = self.rpc.call("session/prompt", json!({"sessionId":self.session,"prompt":prompt_parts(&text,&file_context)}))?;
                        let update = json!({"text":text,"attachments":attachments,"steering":true,"update_id":update_id});
                        self.run.append(
                            "control",
                            json!({"type":"steer_requested","request_id":id,"message":update}),
                        )?;
                        self.steering.insert(id, update);
                    }
                }
                Control::Respond { request_id, result } => {
                    let key = request_id.to_string();
                    let option = result["option_id"].as_str();
                    if let Some(question) = self.questions.get(&key)
                        && let Some(option) = option.filter(|id| question.choices.contains(*id))
                    {
                        self.rpc.reply(
                            &question.id,
                            json!({"outcome":{"outcome":"selected","optionId":option}}),
                        )?;
                        self.questions.remove(&key);
                        self.run.append(
                            "request_answered",
                            json!({"backend":"antigravity","request_id":request_id}),
                        )?;
                        if self.questions.is_empty() {
                            self.run.status("running", "Answer sent to Antigravity")?;
                        }
                    } else {
                        self.run.append("control_error",json!({"message":"The answer must select an offered option for a pending Antigravity question","request_id":request_id}))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn outcome(&self, resident: bool) -> Option<RunOutcome> {
        if self
            .cancel_sent
            .is_some_and(|time| time.elapsed() >= STOP_TIMEOUT)
        {
            return Some(RunOutcome::Interrupted("Antigravity did not confirm that outstanding work stopped; prior effects remain uncertain".into()));
        }
        if self.prompt.is_some()
            || !self.steering.is_empty()
            || !self.tools.is_empty()
            || !self.questions.is_empty()
            || self.awaiting_wake
        {
            return None;
        }
        Some(match self.stop_reason.as_deref()? {
            "end_turn" if resident || !self.answer.trim().is_empty() => {
                RunOutcome::Completed("Antigravity work completed".into())
            }
            "end_turn" => {
                RunOutcome::Failed("Antigravity did not return a conventional text response".into())
            }
            "cancelled" if self.cancel_sent.is_some() => {
                RunOutcome::Cancelled("Antigravity confirmed cancellation".into())
            }
            "cancelled" => RunOutcome::Interrupted(
                "Antigravity stopped the prompt; prior effects remain uncertain".into(),
            ),
            "refusal" | "max_tokens" | "max_turn_requests" => {
                RunOutcome::Failed("Antigravity ended the request before completion".into())
            }
            _ => RunOutcome::Interrupted("Antigravity returned an unrecognized stop reason".into()),
        })
    }
}

pub fn execute(
    binary: &str,
    spec: &Spec,
    resume: Option<String>,
    run: Arc<Run>,
    controls: mpsc::Receiver<Control>,
    jobs: Arc<Jobs>,
    history: History,
) -> Result<RunOutcome> {
    let record = run.snapshot().record;
    let resident = record.resident.as_ref();
    let bridge = match resident {
        Some(resident) => Bridge::start(jobs.clone(), run.clone(), resident.clone())?,
        None => Bridge::start_work(jobs.clone(), run.clone())?,
    };
    let (_process, mut rpc) = Rpc::launch(binary, spec, run.clone())?;
    let prepared = (|| -> Result<(String, Value, Option<String>)> {
        let init = rpc.setup("initialize",json!({"protocolVersion":1,
            "clientInfo":{"name":"boxen","title":"Boxen","version":env!("CARGO_PKG_VERSION")},
            "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}))?;
        let init = result(&init, "initialize")?;
        let capabilities = &init["agentCapabilities"];
        if init["protocolVersion"] != 1 || capabilities["mcpCapabilities"]["http"] != true {
            return Err("Antigravity requires ACP v1 with HTTP MCP support".into());
        }
        let version = init["agentInfo"]["version"].as_str().unwrap_or("");
        let mut parts = version.split(['.', '-']).map(str::parse::<u32>);
        let supported = match (parts.next(), parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) => {
                (major, minor, patch) >= (1, 5, 1)
            }
            _ => false,
        };
        if !supported {
            return Err("Antigravity 1.5.1 or later is required for native prompt steering".into());
        }
        let mut setup = json!({"cwd":spec.cwd,"mcpServers":[bridge.acp_config()]});
        let mut rebuild = history.rebuild_reason.clone();
        let mut native_id = None;
        let mut session_value = None;
        if let Some(resume) = resume.as_ref().filter(|_| rebuild.is_none()) {
            if capabilities["sessionCapabilities"]["resume"].is_null() {
                return Err("Antigravity did not advertise native resume".into());
            }
            setup["sessionId"] = json!(resume);
            let response = rpc.setup("session/resume", setup.clone())?;
            setup.as_object_mut().unwrap().remove("sessionId");
            if missing_session(&response) {
                rebuild =
                    Some("Antigravity reported that the native session is unavailable.".into());
            } else {
                native_id = Some(resume.clone());
                session_value = Some(result(&response, "session/resume")?.clone());
            }
        }
        let state = match session_value {
            Some(state) => state,
            None => {
                let response = rpc.setup("session/new", setup)?;
                result(&response, "session/new")?.clone()
            }
        };
        let id = native_id
            .or_else(|| state["sessionId"].as_str().map(str::to_owned))
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or("Antigravity returned an invalid session ID")?;
        let mut options = state["configOptions"].clone();
        if !spec.model.is_empty() {
            select(&mut rpc, &id, &mut options, "model", &spec.model)?;
        }
        if let Some(level) = crate::reasoning::requested(record.reasoning_effort.as_deref()) {
            select(&mut rpc, &id, &mut options, "thought_level", level)?;
        }
        Ok((id, options, rebuild))
    })();
    if run.cancellation_requested() {
        run.append("control", json!({"type":"cancel_requested"}))?;
        return Ok(RunOutcome::Cancelled(
            "Cancelled before the Antigravity prompt".into(),
        ));
    }
    let (session, options, rebuild) = prepared?;
    let mut context = record.trigger_context()
        + &record.observation_context()
        + &run.runtime_context()?
        + &jobs.attachments.context(&spec.attachments);
    if let Some(recovery) = &record.recovery {
        run.append("recovery_notice", recovery.clone())?;
        context.push_str(&format!("\nRecovery incident: {recovery}"));
    }
    if let Some(reason) = rebuild {
        run.append(
            "backend_session_rebuilt",
            json!({"backend":"antigravity","previous_backend_thread":resume,"reason":reason}),
        )?;
        context.push_str(&format!("\n{reason} Boxen supplies retained conversation evidence. Hidden state and omitted payloads remain unavailable. Historical effects are retained as evidence and are not replayed.\n{}",history.reconstruction()?));
    }
    if let Some(resident) = resident {
        context.push_str(&jobs.residents.instructions(resident));
        context.push_str("\nBoxen resident operations are available through the boxen MCP connection. Assistant narration remains private. Explicit messaging tool calls deliver correspondence.");
        run.append("resident_context",json!({"resident_id":resident.id,"revision":resident.revision,"machine_id":resident.machine_id,"used_bytes":resident.used_bytes(),"quota_bytes":resident.quota_bytes}))?;
    }
    run.append(
        if resident.is_some() {
            "activation_input"
        } else {
            "user_message"
        },
        json!({"text":spec.prompt,"attachments":spec.attachments}),
    )?;
    let model = config(&options, "model").map(|model| &model["currentValue"]);
    run.append(
        "backend_thread",
        json!({"id":session,"backend":"antigravity","model":model}),
    )?;
    run.diagnostic(
        Level::Info,
        "antigravity.session_ready",
        || json!({"session_id":session,"model":model}),
        || Value::Null,
    );
    if run.cancellation_requested() {
        run.append("control", json!({"type":"cancel_requested"}))?;
        return Ok(RunOutcome::Cancelled(
            "Cancelled before the Antigravity prompt".into(),
        ));
    }
    run.status("running", "Antigravity is executing on this machine")?;
    let prompt = rpc.call("session/prompt", json!({"sessionId":session,"prompt":prompt_parts(
        &spec.prompt,
        &format!("Boxen-authored runtime context, separate from the request author:\n{context}"),
    )}))?;
    let mut turn = Turn {
        rpc: &mut rpc,
        session: &session,
        run: &run,
        secret: &bridge.token,
        tools: OutstandingWork::default(),
        questions: HashMap::new(),
        steering: HashMap::new(),
        prompt: Some(prompt),
        stop_reason: None,
        awaiting_wake: false,
        cancel_sent: None,
        answer: String::new(),
    };
    loop {
        turn.controls(&controls)?;
        if let Some(message) = turn.rpc.receive()? {
            turn.ingest(message)?;
        }
        if let Some(outcome) = turn.outcome(resident.is_some()) {
            if matches!(outcome, RunOutcome::Completed(_)) {
                run.append("assistant_message",json!({"id":"acp-answer","text":turn.answer,"private":resident.is_some(),"backend":"antigravity"}))?;
            }
            return Ok(outcome);
        }
    }
}
