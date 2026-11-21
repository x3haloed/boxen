//! Claude Code stream-json integration. Claude owns native sessions; Boxen owns accepted runs.
use crate::{
    Result,
    jobs::{Control, History, Jobs, Run, RunOutcome},
    logging::{self, Context, Level, Target},
    resident_bridge::Bridge,
    runtime::OutstandingWork,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const MAX_FRAME: u64 = 4 * 1024 * 1024;
const WAIT: Duration = Duration::from_millis(25);
const SETUP: Duration = Duration::from_secs(15);
const STOP: Duration = Duration::from_secs(10);

pub fn discover(home: &Path) -> Option<String> {
    let relative = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let home = crate::execution::Execution::capture(home)
        .ok()
        .map(|account| account.home);
    let from_path = std::env::var_os("PATH")
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(relative))
        .find(|path| path.is_file());
    from_path
        .or_else(|| {
            let path = PathBuf::from(home?).join(".local/bin").join(relative);
            path.is_file().then_some(path)
        })
        .map(|path| path.to_string_lossy().into_owned())
}

enum Scope<'a> {
    Run(&'a Run),
    Catalog(&'a str),
}

impl Scope<'_> {
    fn diagnostic(&self, level: Level, event: &str, fields: impl FnOnce() -> Value) {
        match self {
            Self::Run(run) => run.diagnostic(level, event, fields, || Value::Null),
            Self::Catalog(owner) => logging::record(
                level,
                Target::Runtime,
                event,
                Context::peer(owner),
                fields,
                || Value::Null,
            ),
        }
    }

    fn cancelled(&self) -> bool {
        matches!(self, Self::Run(run) if run.cancellation_requested())
    }
}

enum Frame {
    Data(Value),
    Idle,
    Closed,
}

struct Stream<'a> {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Result<Value>>,
    scope: Scope<'a>,
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        self.input.take();
        #[cfg(unix)]
        // The CLI was started in an isolated process group.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let status = self.child.wait();
        self.scope.diagnostic(
            Level::Debug,
            "claude.process_stopped",
            || json!({"pid":self.child.id(),"exit":status.ok().and_then(|status|status.code())}),
        );
    }
}

impl<'a> Stream<'a> {
    fn launch(mut command: Command, cwd: &Path, scope: Scope<'a>) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        scope.diagnostic(Level::Info, "claude.process_starting", || json!({}));
        let mut child = command
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--strict-mcp-config",
            ])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .inspect_err(|error| {
                scope.diagnostic(
                    Level::Warn,
                    "claude.process_start_failed",
                    || json!({"error_kind":format!("{:?}",error.kind())}),
                );
            })?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().expect("piped Claude stdout");
        let (tx, output) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                let frame = match reader
                    .by_ref()
                    .take(MAX_FRAME + 1)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => break,
                    Ok(_) if line.len() as u64 > MAX_FRAME => {
                        Err("Claude Code stream frame exceeds 4 MiB".into())
                    }
                    Ok(_) => serde_json::from_slice(&line)
                        .map_err(|_| "Claude Code returned an invalid stream frame".into()),
                    Err(error) => Err(error.into()),
                };
                let failed = frame.is_err();
                if tx.send(frame).is_err() || failed {
                    break;
                }
            }
        });
        scope.diagnostic(
            Level::Info,
            "claude.process_started",
            || json!({"pid":child.id()}),
        );
        Ok(Self {
            child,
            input,
            output,
            scope,
        })
    }

    fn send(&mut self, frame: Value) -> Result<()> {
        let input = self.input.as_mut().ok_or("Claude Code input is closed")?;
        serde_json::to_writer(&mut *input, &frame)?;
        input.write_all(b"\n")?;
        input.flush()?;
        self.scope.diagnostic(Level::Trace, "claude.frame_sent", || {
            json!({"type":frame["type"],"subtype":frame["request"]["subtype"],"request_id":frame["request_id"]})
        });
        Ok(())
    }

    fn request(&mut self, id: &str, request: Value) -> Result<()> {
        self.send(json!({"type":"control_request","request_id":id,"request":request}))
    }

    fn receive(&self) -> Result<Frame> {
        match self.output.recv_timeout(WAIT) {
            Ok(frame) => {
                let frame = frame?;
                self.scope
                    .diagnostic(Level::Trace, "claude.frame_received", || {
                        json!({"type":frame["type"],"subtype":frame["subtype"],
                        "request_id":frame["response"]["request_id"]})
                    });
                Ok(Frame::Data(frame))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(Frame::Idle),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(Frame::Closed),
        }
    }

    fn setup(&mut self, id: &str, request: Value, resume: Option<&str>) -> Result<Setup> {
        // A missing native session can terminate the process before it reads stdin.
        let sent = self.request(id, request);
        let deadline = Instant::now() + SETUP;
        while !self.scope.cancelled() && Instant::now() < deadline {
            let frame = match self.receive() {
                Ok(Frame::Data(frame)) => frame,
                Ok(Frame::Idle) => continue,
                Ok(Frame::Closed) => {
                    return sent.and(Err("Claude Code stream closed during setup".into()));
                }
                Err(error) => return sent.and(Err(error)),
            };
            if frame["type"] == "result" {
                if let Some(session) = resume
                    && frame["subtype"] == "error_during_execution"
                    && frame["errors"].as_array().is_some_and(|errors| {
                        errors.iter().any(|error| {
                            error.as_str().and_then(|text| {
                                text.strip_prefix("No conversation found with session ID: ")
                            }) == Some(session)
                        })
                    })
                {
                    return Ok(Setup::MissingSession);
                }
                return Err(
                    "Claude Code ended during setup; the executing account's CLI must be usable"
                        .into(),
                );
            }
            let reply = &frame["response"];
            if frame["type"] == "control_response" && reply["request_id"] == id {
                if reply["subtype"] != "success" {
                    return Err(format!("Claude Code rejected setup operation {id}").into());
                }
                let response = &reply["response"];
                if id == "initialize" && signed_out(&response["account"]) {
                    return Err("Claude Code sign-in is required on the executing machine".into());
                }
                return Ok(Setup::Ready(response.clone()));
            }
        }
        Err(format!("Claude Code setup operation {id} did not complete").into())
    }
}

enum Setup {
    Ready(Value),
    MissingSession,
}

fn signed_out(account: &Value) -> bool {
    account["apiProvider"] == "firstParty"
        && account["tokenSource"] == "none"
        && !account["subscriptionType"].is_string()
        && (account["apiKeySource"].is_null() || account["apiKeySource"] == "none")
}

pub fn models(mut command: Command, cwd: &Path, owner: &str) -> Result<Value> {
    command.args([
        "--no-session-persistence",
        "--tools",
        "",
        "--setting-sources",
        "user",
        "--settings",
        r#"{"disableAllHooks":true}"#,
    ]);
    let mut stream = Stream::launch(command, cwd, Scope::Catalog(owner))?;
    let Setup::Ready(init) = stream.setup("initialize", json!({"subtype":"initialize"}), None)?
    else {
        return Err("Claude Code returned no catalog session".into());
    };
    let entries = init["models"]
        .as_array()
        .ok_or("Claude Code did not return a model catalog")?;
    if entries.len() > 1024 {
        return Err("Claude Code model catalog exceeds 1024 entries".into());
    }
    let mut seen = HashSet::new();
    let mut choices = Vec::new();
    for entry in entries {
        let id = entry["value"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or("Claude Code returned an invalid model ID")?;
        if id == "default" || !seen.insert(id) {
            continue;
        }
        let label = entry["displayName"]
            .as_str()
            .filter(|label| !label.is_empty() && label.len() <= 256)
            .unwrap_or(id);
        choices.push(json!({"model":id,"label":label}));
    }
    Ok(json!({"models":choices}))
}

struct Turn<'a> {
    run: &'a Run,
    private: bool,
    secret: &'a str,
    session: Option<String>,
    answer: String,
    join_text: bool,
    tools: OutstandingWork,
    tasks: OutstandingWork,
    stopped_task: bool,
}

impl Turn<'_> {
    fn redact(&self, text: &str) -> String {
        text.replace(self.secret, "[redacted]")
    }

    fn text(&mut self, text: &str, reasoning: bool) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let text = self.redact(text);
        let text = if !reasoning && self.join_text {
            self.join_text = false;
            format!("\n\n{text}")
        } else {
            text
        };
        if !reasoning {
            if self.answer.len() + text.len() > 3 * 1024 * 1024 {
                return Err("Claude Code response exceeds Boxen's retained message limit".into());
            }
            self.answer.push_str(&text);
        }
        let (kind, id) = if reasoning {
            ("reasoning_delta", "claude-thinking")
        } else {
            ("assistant_delta", "claude-answer")
        };
        self.run.append(
            kind,
            json!({"id":id,"text":text,"backend":"claude","private":self.private}),
        )?;
        Ok(())
    }

    fn project(&mut self, frame: &Value) -> Result<()> {
        match (frame["type"].as_str(), frame["subtype"].as_str()) {
            (Some("system"), Some("init")) => {
                if self.session.is_none() {
                    let session = bounded_id(&frame["session_id"])?;
                    let model = frame["model"]
                        .as_str()
                        .filter(|model| model.len() <= 256)
                        .map(|model| self.redact(model));
                    self.run.append(
                        "backend_thread",
                        json!({"id":session,"backend":"claude","model":model}),
                    )?;
                    Scope::Run(self.run).diagnostic(
                        Level::Info,
                        "claude.session_ready",
                        || json!({"session_id":session,"model":model}),
                    );
                    self.session = Some(session.into());
                }
            }
            (Some("stream_event"), _) if frame["parent_tool_use_id"].is_null() => {
                self.stream_event(&frame["event"])?;
            }
            (Some("assistant"), _) => {
                if let Some(blocks) = frame["message"]["content"].as_array() {
                    for block in blocks.iter().filter(|block| block["type"] == "tool_use") {
                        let id = bounded_id(&block["id"])?;
                        self.tools.start(id)?;
                        let input = self.redact(&serde_json::to_string(&block["input"])?);
                        let name = block["name"].as_str().map(|name| self.redact(name));
                        self.run.append(
                            "tool_started",
                            json!({"call_id":id,"tool":name,"backend":"claude",
                            "arguments":serde_json::from_str::<Value>(&input)?,"status":"running",
                            "parent_call_id":frame["parent_tool_use_id"]}),
                        )?;
                    }
                }
            }
            (Some("tool_progress"), _) => {
                let id = bounded_id(&frame["tool_use_id"])?;
                self.run.append(
                    "tool_status",
                    json!({"call_id":id,"backend":"claude",
                    "status":"running","elapsed_seconds":frame["elapsed_time_seconds"]}),
                )?;
            }
            (Some("user"), _) => {
                if let Some(blocks) = frame["message"]["content"].as_array() {
                    for block in blocks.iter().filter(|block| block["type"] == "tool_result") {
                        let id = bounded_id(&block["tool_use_id"])?;
                        self.tools.finish(id);
                        let output = match &block["content"] {
                            Value::String(text) => self.redact(text),
                            value => self.redact(&serde_json::to_string(value)?),
                        };
                        self.run.append(
                            "tool_result",
                            json!({"call_id":id,"backend":"claude",
                            "status":if block["is_error"] == true {"failed"} else {"completed"},
                            "result":output,"output_kind":"aggregate","success":block["is_error"] != true}),
                        )?;
                    }
                }
            }
            (Some("system"), Some("task_started" | "task_updated" | "task_notification")) => {
                let task = bounded_id(&frame["task_id"])?;
                let status = frame["status"].as_str().unwrap_or("running");
                let done = frame["subtype"] == "task_notification"
                    && matches!(status, "completed" | "failed" | "stopped" | "cancelled");
                if done {
                    self.tasks.finish(task);
                    self.stopped_task |= matches!(status, "stopped" | "cancelled");
                } else {
                    self.tasks.start(task)?;
                }
                let description = frame["description"]
                    .as_str()
                    .or_else(|| frame["summary"].as_str())
                    .map(|text| self.redact(text));
                self.run.append(
                    if done {"tool_result"} else if frame["subtype"] == "task_started" {"tool_started"} else {"tool_status"},
                    json!({"call_id":format!("task:{task}"),"native_task_id":task,"parent_call_id":frame["tool_use_id"],
                        "tool":"Background task","backend":"claude","status":status,"result":description,
                        "success":done.then_some(status == "completed")}),
                )?;
                Scope::Run(self.run).diagnostic(
                    Level::Debug,
                    "claude.task_activity",
                    || json!({"task_id":task,"settled":done}),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn stream_event(&mut self, event: &Value) -> Result<()> {
        let block = match event["type"].as_str() {
            Some("content_block_start") => &event["content_block"],
            Some("content_block_delta") => &event["delta"],
            _ => return Ok(()),
        };
        if let Some(text) = block["text"].as_str() {
            self.text(text, false)?;
        }
        if let Some(text) = block["thinking"].as_str() {
            self.text(text, true)?;
        }
        Ok(())
    }

    fn finish(&self, result: &Value) -> Result<RunOutcome> {
        if interrupted(result) {
            return Ok(RunOutcome::Interrupted(
                "Claude Code reported an interrupted prompt".into(),
            ));
        }
        if result["subtype"] != "success" || result["is_error"] == true {
            let detail = match result["api_error_status"].as_u64() {
                Some(401 | 403) => {
                    "Claude Code could not authorize this request. The executing account's sign-in and selected model determine access."
                }
                Some(429) => "Claude Code reported a usage or rate limit.",
                _ => {
                    "Claude Code reported an unsuccessful prompt. Its completed tool activity remains in the run journal."
                }
            };
            return Ok(RunOutcome::Failed(detail.into()));
        }
        if self.session.is_none() {
            return Err("Claude Code finished without identifying its native session".into());
        }
        let answer = if self.answer.is_empty() {
            self.redact(result["result"].as_str().unwrap_or(""))
        } else {
            self.answer.clone()
        };
        self.run.append(
            "assistant_message",
            json!({"id":"claude-answer","text":answer,
            "backend":"claude","private":self.private}),
        )?;
        Ok(RunOutcome::Completed(
            "Claude Code completed the prompt and its reported work".into(),
        ))
    }
}

fn interrupted(result: &Value) -> bool {
    matches!(
        result["terminal_reason"].as_str(),
        Some("interrupted" | "aborted_streaming" | "aborted")
    )
}

fn bounded_id(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .ok_or_else(|| "Claude Code returned an invalid native ID".into())
}

pub fn execute(
    binary: &str,
    resume: Option<String>,
    run: Arc<Run>,
    controls: mpsc::Receiver<Control>,
    jobs: Arc<Jobs>,
    history: History,
) -> Result<RunOutcome> {
    let record = run.snapshot().record;
    let spec = &record.spec;
    let bridge = match &record.resident {
        Some(resident) => Bridge::start(jobs.clone(), run.clone(), resident.clone())?,
        None => Bridge::start_work(jobs.clone(), run.clone())?,
    };
    let launch = |session: Option<&str>| {
        let mut command = record.execution.as_ref().map_or_else(
            || Command::new(binary),
            |execution| execution.command(binary),
        );
        command.args([
            "--permission-mode",
            "bypassPermissions",
            "--allow-dangerously-skip-permissions",
        ]);
        let model = if spec.model.is_empty() {
            "default"
        } else {
            &spec.model
        };
        command.arg("--model").arg(model);
        if let Some(session) = session {
            command.arg("--resume").arg(session);
        }
        Stream::launch(command, Path::new(&spec.cwd), Scope::Run(&run))
    };
    let mut rebuild = history.rebuild_reason.clone();
    let native = resume.as_deref().filter(|_| rebuild.is_none());
    let mut stream = launch(native)?;
    let init = stream.setup("initialize", json!({"subtype":"initialize"}), native);
    if run.cancellation_requested() {
        return Ok(RunOutcome::Cancelled(
            "Cancelled before the Claude Code prompt".into(),
        ));
    }
    if matches!(init?, Setup::MissingSession) {
        rebuild = Some("Claude Code reported that its native session is unavailable.".into());
        drop(stream);
        stream = launch(None)?;
        stream.setup("initialize", json!({"subtype":"initialize"}), None)?;
    }
    let configured = stream.setup(
        "bridge",
        json!({"subtype":"mcp_set_servers",
        "servers":{"boxen":bridge.claude_config()}}),
        None,
    );
    if run.cancellation_requested() {
        return Ok(RunOutcome::Cancelled(
            "Cancelled before the Claude Code prompt".into(),
        ));
    }
    let Setup::Ready(bridge_state) = configured? else {
        return Err("Claude Code did not acknowledge the Boxen connection".into());
    };
    if !bridge_state["errors"]["boxen"].is_null()
        || !bridge_state["added"]
            .as_array()
            .is_some_and(|added| added.iter().any(|name| name == "boxen"))
    {
        return Err("Claude Code could not connect to the Boxen tools".into());
    }
    Scope::Run(&run).diagnostic(
        Level::Info,
        "claude.bridge_ready",
        || json!({"connected":true}),
    );

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
            json!({"backend":"claude","previous_backend_thread":resume,"reason":reason}),
        )?;
        context.push_str(&format!("\n{reason} Boxen supplies retained conversation evidence. Hidden state and omitted payloads remain unavailable. Historical effects are retained as evidence and are not replayed.\n{}", history.reconstruction()?));
    }
    let private = record.resident.is_some();
    if let Some(resident) = &record.resident {
        context.push_str(&jobs.residents.instructions(resident));
        context.push_str("\nBoxen resident operations are available through the boxen MCP connection. Assistant narration remains private. Explicit messaging tool calls deliver correspondence.");
        run.append("resident_context", json!({"resident_id":resident.id,"revision":resident.revision,
            "machine_id":resident.machine_id,"used_bytes":resident.used_bytes(),"quota_bytes":resident.quota_bytes}))?;
    }
    run.append(
        if private {
            "activation_input"
        } else {
            "user_message"
        },
        json!({"text":spec.prompt,"attachments":spec.attachments}),
    )?;
    if run.cancellation_requested() {
        return Ok(RunOutcome::Cancelled(
            "Cancelled before the Claude Code prompt".into(),
        ));
    }
    run.status("running", "Claude Code is executing on this machine")?;
    run.append(
        "backend_prompt_started",
        json!({"backend":"claude","source":"request"}),
    )?;
    stream.send(json!({"type":"user","message":{"role":"user","content":[
        {"type":"text","text":spec.prompt},
        {"type":"text","text":format!("Boxen-authored runtime context, separate from the request author:\n{context}")}
    ]},"parent_tool_use_id":null,"session_id":""}))?;

    let mut turn = Turn {
        run: &run,
        private,
        secret: &bridge.token,
        session: None,
        answer: String::new(),
        join_text: false,
        tools: OutstandingWork::default(),
        tasks: OutstandingWork::default(),
        stopped_task: false,
    };
    let mut result = None;
    let mut stop = None;
    let mut stop_acknowledged = false;
    let mut task_stop_requests = HashSet::new();
    loop {
        if stop.is_none()
            && (run.cancellation_requested()
                || controls
                    .try_iter()
                    .any(|control| matches!(control, Control::Cancel)))
        {
            run.append("control", json!({"type":"cancel_requested"}))?;
            if stream.input.is_some() {
                stream.request("interrupt", json!({"subtype":"interrupt"}))?;
            }
            stop = Some(Instant::now() + STOP);
        }
        if stop.is_some() && stream.input.is_some() {
            for task in turn.tasks.ids() {
                if task_stop_requests.insert(task.to_owned()) {
                    stream.request(
                        &format!("stop-task:{task}"),
                        json!({"subtype":"stop_task","task_id":task}),
                    )?;
                }
            }
        }
        if stop.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(RunOutcome::Interrupted("Claude Code did not confirm that all work stopped. External effects may have occurred.".into()));
        }

        let frame = match stream.receive()? {
            Frame::Data(frame) => frame,
            Frame::Idle => continue,
            Frame::Closed => {
                if let Some(result) = &result
                    && stop.is_none()
                    && stream.input.is_none()
                    && turn.tasks.is_empty()
                    && turn.tools.is_empty()
                {
                    return turn.finish(result);
                }
                return Ok(RunOutcome::Interrupted(
                    "Claude Code closed its stream before confirming that all work ended. External effects may have occurred.".into(),
                ));
            }
        };
        if frame["type"] == "control_request" && stream.input.is_some() {
            stream.send(json!({"type":"control_response","response":{"subtype":"error",
                "request_id":frame["request_id"],"error":"This Boxen connection does not provide this Claude Code control."}}))?;
        }
        if frame["type"] == "control_response" {
            let reply = &frame["response"];
            let accepted = reply["subtype"] == "success";
            if reply["request_id"] == "interrupt" {
                stop_acknowledged = accepted;
                Scope::Run(&run).diagnostic(
                    Level::Info,
                    "claude.interrupt_reply",
                    || json!({"accepted":accepted}),
                );
            } else if let Some(task) = reply["request_id"]
                .as_str()
                .and_then(|id| id.strip_prefix("stop-task:"))
                && task_stop_requests.contains(task)
            {
                Scope::Run(&run).diagnostic(
                    Level::Info,
                    "claude.stop_task_reply",
                    || json!({"task_id":task,"accepted":accepted}),
                );
            }
        }
        // A native task notification can cause another prompt even after stdin
        // closes. Claude drains those prompts before closing its output.
        if frame["type"] == "system" && frame["subtype"] == "init" && result.take().is_some() {
            turn.join_text = !turn.answer.is_empty();
            run.append(
                "backend_prompt_started",
                json!({"backend":"claude","source":"native_activity"}),
            )?;
        }
        if frame["type"] == "result" {
            run.append(
                "backend_prompt_ended",
                json!({"backend":"claude",
                "stop_reason":frame["stop_reason"],"outstanding_tasks":turn.tasks.len()}),
            )?;
            Scope::Run(&run).diagnostic(Level::Info, "claude.prompt_ended", || {
                json!({"subtype":frame["subtype"],
                "outstanding_tasks":turn.tasks.len(),"outstanding_tools":turn.tools.len()})
            });
            result = Some(frame.clone());
        }
        turn.project(&frame)?;
        if let Some(result) = &result
            && turn.tasks.is_empty()
            && turn.tools.is_empty()
        {
            if stop.is_none() {
                stream.input.take();
            } else if stop_acknowledged {
                return if turn.stopped_task || interrupted(result) {
                    Ok(RunOutcome::Cancelled(
                        "Claude Code confirmed interruption and its reported work ended".into(),
                    ))
                } else {
                    turn.finish(result)
                };
            }
        }
    }
}
