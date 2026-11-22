use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadKey {
    pub peer: String,
    pub id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    Work,
    LivePulse,
    Agents,
    Activity(ThreadKey),
    Chat(ThreadKey),
    Threads,
    Correspondence,
    Message(ThreadKey),
    Compose(String),
    Welcome,
    #[serde(rename = "NewConversation", skip_serializing)]
    LegacyNewConversation,
    Thread(ThreadKey),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Destination {
    pub key: String,
    pub label: String,
    pub address: String,
}
#[derive(Clone, Debug)]
pub struct Conversation {
    pub key: ThreadKey,
    pub title: String,
    pub runs: Vec<Value>,
}
pub fn catalog(peer: &str, runs: &[Value]) -> Vec<Conversation> {
    // Any identity attachment excludes the whole conversation, including older runs.
    let identities: HashSet<&str> = runs
        .iter()
        .filter(|r| {
            !r["record"]["resident"].is_null() || !r["record"]["spec"]["resident_id"].is_null()
        })
        .filter_map(|r| r["record"]["spec"]["thread_id"].as_str())
        .collect();
    let mut out: Vec<Conversation> = vec![];
    for run in runs {
        let Some(id) = run["record"]["spec"]["thread_id"].as_str() else {
            continue;
        };
        if identities.contains(id) {
            continue;
        }
        if let Some(thread) = out.iter_mut().find(|t| t.key.id == id) {
            thread.runs.push(run.clone());
        } else {
            let prompt = run["title"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    run["record"]["spec"]["prompt"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                })
                .or_else(|| run["record"]["spec"]["attachments"][0]["name"].as_str())
                .unwrap_or("Untitled thread");
            out.push(Conversation {
                key: ThreadKey {
                    peer: peer.into(),
                    id: id.into(),
                },
                title: prompt
                    .lines()
                    .next()
                    .unwrap_or("Untitled thread")
                    .chars()
                    .take(72)
                    .collect(),
                runs: vec![run.clone()],
            });
        }
    }
    out.reverse();
    out
}
pub fn terminal(snapshot: &Value) -> bool {
    matches!(
        snapshot["status"].as_str(),
        Some("completed" | "failed" | "cancelled" | "interrupted")
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerState {
    Sending,
    Queued,
    Writing,
    Written,
    Refused,
    Unconfirmed,
}

#[derive(Clone, Debug)]
pub struct AnswerReceipt {
    pub attempt_id: String,
    pub machine: String,
    pub decision: String,
    pub state: AnswerState,
    pub yours: bool,
    pub machine_label: String,
}
impl AnswerReceipt {
    pub fn waiting(&self) -> bool {
        matches!(
            self.state,
            AnswerState::Sending
                | AnswerState::Queued
                | AnswerState::Writing
                | AnswerState::Unconfirmed
        )
    }

    pub fn label(&self) -> String {
        match self.state {
            AnswerState::Sending => "Sending answer…".into(),
            AnswerState::Queued | AnswerState::Writing if self.yours => {
                "◌ your answer queued".into()
            }
            AnswerState::Queued | AnswerState::Writing => "◌ answer queued".into(),
            AnswerState::Written if self.yours => "✓ your answer written".into(),
            AnswerState::Written => {
                format!(
                    "Answered by another client · {} · {}",
                    self.decision, self.machine_label
                )
            }
            AnswerState::Refused if self.yours => "Your answer was refused before writing".into(),
            AnswerState::Refused => "Answer was refused before writing".into(),
            AnswerState::Unconfirmed => "? write uncertain".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub at: Option<u64>,
    pub id: String,
    pub label: String,
    pub text: String,
    pub attachments: Vec<crate::attachments::FileRef>,
    pub request: Option<Value>,
    pub answer: Option<AnswerReceipt>,
    pub update_id: Option<String>,
    pub tool: Option<ToolDetail>,
}
#[derive(Clone, Debug, Default)]
pub struct ToolDetail {
    pub output: String,
    pub result: Option<String>,
    pub failed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextMeasure {
    Codex { tokens: u64, window: u64 },
    Claude { input_tokens: u64 },
    Antigravity { tokens: u64, window: u64 },
    Selection { selected: u64, available: u64 },
}
impl ContextMeasure {
    pub fn label(self) -> String {
        match self {
            Self::Codex { tokens, window } => {
                format!("ctx {:.0}%", tokens as f64 * 100.0 / window as f64)
            }
            Self::Claude { input_tokens } => format!("{input_tokens} tok in, last turn"),
            Self::Antigravity { tokens, window } => format!("ctx {tokens} / {window} tok"),
            Self::Selection {
                selected,
                available,
            } => {
                format!(
                    "ctx {:.0}/{:.0} KiB selectable",
                    selected as f64 / 1024.0,
                    available as f64 / 1024.0
                )
            }
        }
    }

    pub fn past_warning_line(self) -> bool {
        matches!(self, Self::Codex { tokens, window } if u128::from(tokens) * 100 >= u128::from(window) * 85)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Codex { .. } => {
                "Codex last reported exchange tokens against its model window. Boxen marks 85% as a warning. Codex controls its own compaction."
            }
            Self::Claude { .. } => {
                "Input tokens from Claude Code’s latest main-session model exchange, including cached input."
            }
            Self::Antigravity { .. } => {
                "Antigravity reported current session tokens and window size."
            }
            Self::Selection { .. } => {
                "Selected history bytes against the space available for selection after reservations."
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextChange {
    Compaction,
    Projection,
    Shortage,
}

impl ContextChange {
    pub fn label(self) -> &'static str {
        match self {
            Self::Compaction => "compacted",
            Self::Projection => "history projected",
            Self::Shortage => "context shortage",
        }
    }
}

fn context_amounts(used: &Value, limit: &Value) -> Option<(u64, u64)> {
    Some((used.as_u64()?, limit.as_u64().filter(|&n| n != 0)?))
}

#[derive(Default)]
pub struct Transcript {
    pub seq: u64,
    pub execution: Option<Value>,
    pub status: Option<Value>,
    pub context: Option<ContextMeasure>,
    pub context_at: Option<u64>,
    pub context_change: Option<(ContextChange, u64)>,
    pub backend_model: Option<String>,
    pub backend_session: Option<String>,
    pub hold: Option<Value>,
    pub latest_visible: Option<(u64, crate::pulse::Visible)>,
    pub rows: Vec<Row>,
    pub rejected_updates: BTreeMap<String, (String, bool)>,
    pub queued_updates: HashSet<String>,
    answer_attempts: BTreeMap<String, AnswerReceipt>,
    positions: BTreeMap<String, usize>,
    imported: bool,
    event_at: Option<u64>,
}
impl Transcript {
    fn measure_context(&mut self, measure: Option<ContextMeasure>) {
        self.context = measure;
        self.context_at = measure.and(self.event_at);
    }

    fn context_changed(&mut self, seq: u64, change: ContextChange, description: String) {
        if let Some(at) = self.event_at {
            self.context_change = Some((change, at));
        }
        self.put(format!("event-{seq}"), "Context", description, false);
    }

    fn put(&mut self, id: String, label: &str, text: String, append: bool) {
        let at = self.event_at;
        let index = *self.positions.entry(id.clone()).or_insert_with(|| {
            self.rows.push(Row {
                at,
                id,
                label: label.into(),
                text: String::new(),
                attachments: vec![],
                request: None,
                answer: None,
                update_id: None,
                tool: None,
            });
            self.rows.len() - 1
        });
        let row = &mut self.rows[index];
        row.label = label.into();
        if append {
            row.text.push_str(&text)
        } else {
            row.text = text
        }
    }
    pub fn see_hold(&mut self, value: &Value) {
        let Some(revision) = value["revision"].as_u64() else {
            return;
        };
        if self
            .hold
            .as_ref()
            .and_then(|hold| hold["revision"].as_u64())
            .is_none_or(|previous| previous < revision)
        {
            self.hold = Some(value.clone());
        }
    }

    pub fn decorate_answers(&mut self, mut label: impl FnMut(&mut AnswerReceipt)) {
        for row in &mut self.rows {
            if let Some(answer) = &mut row.answer {
                label(answer);
            }
        }
    }

    pub fn answer_locally(&mut self, request: &Value, attempt: AnswerReceipt) {
        if let Some(row) = self.pending_request_mut(request)
            && row.answer.as_ref().is_none_or(|known| {
                known.attempt_id == attempt.attempt_id
                    && matches!(known.state, AnswerState::Sending | AnswerState::Unconfirmed)
            })
        {
            row.answer = Some(attempt);
        }
    }

    fn pending_request_mut(&mut self, request: &Value) -> Option<&mut Row> {
        self.rows.iter_mut().rev().find(|row| {
            row.request
                .as_ref()
                .is_some_and(|value| value["id"] == *request)
        })
    }

    fn answer_event(&mut self, kind: &str, data: &Value) {
        let Some(id) = data["attempt_id"].as_str() else {
            return;
        };
        if kind == "response_attempt" {
            self.answer_attempts.insert(
                id.into(),
                AnswerReceipt {
                    attempt_id: id.into(),
                    machine: data["machine_id"].as_str().unwrap_or_default().into(),
                    decision: data["decision"]
                        .as_str()
                        .unwrap_or("submit")
                        .replace('_', " "),
                    state: AnswerState::Queued,
                    yours: false,
                    machine_label: String::new(),
                },
            );
        }
        let Some(attempt) = self.answer_attempts.get_mut(id) else {
            return;
        };
        attempt.state = match kind {
            "response_write_started" => AnswerState::Writing,
            "response_refused" => AnswerState::Refused,
            "request_answered" => AnswerState::Written,
            _ => AnswerState::Queued,
        };
        let attempt = attempt.clone();
        if let Some(row) = self.pending_request_mut(&data["request_id"]) {
            let replace = row.answer.as_ref().is_none_or(|known| {
                known.attempt_id == id
                    || matches!(
                        known.state,
                        AnswerState::Sending | AnswerState::Refused | AnswerState::Unconfirmed
                    )
                    || matches!(attempt.state, AnswerState::Writing | AnswerState::Written)
            });
            if replace {
                row.answer = Some(attempt);
            }
        } else if kind == "response_refused" {
            let row = format!("answer-{id}");
            self.put(row.clone(), "Response attempt", String::new(), false);
            self.rows[self.positions[&row]].answer = Some(attempt);
        }
    }

    pub fn apply(&mut self, event: &Value) -> Result<(), String> {
        let seq = event["seq"].as_u64().ok_or("Event has no sequence")?;
        if seq <= self.seq {
            return Ok(());
        }
        if seq != self.seq + 1 {
            return Err(format!(
                "History gap: expected {}, received {seq}",
                self.seq + 1
            ));
        }
        self.event_at = event["at"].as_u64();
        let data = &event["data"];
        let kind = event["kind"].as_str().unwrap_or("");
        if kind == "execution_context" {
            self.execution = Some(data.clone());
        }
        if kind == "status" {
            self.status = Some(data.clone());
        }
        let text = |key: &str| data[key].as_str().unwrap_or("").to_owned();
        if matches!(
            kind,
            "response_attempt" | "response_write_started" | "response_refused" | "request_answered"
        ) {
            self.answer_event(kind, data);
        }
        match kind {
            "backend_thread" => {
                self.measure_context(None);
                self.context_change = None;
                self.backend_model = data["model"].as_str().map(str::to_owned);
                self.backend_session = data["id"].as_str().map(str::to_owned);
            }
            "held_followup" => self.see_hold(data),
            "backend_usage" if data["backend"] == "antigravity" => {
                self.measure_context(context_amounts(&data["used"], &data["size"])
                    .map(|(tokens, window)| ContextMeasure::Antigravity { tokens, window }));
            }
            "backend_usage" if data["backend"] == "claude" => {
                self.measure_context(data["input_tokens"].as_u64()
                    .map(|input_tokens| ContextMeasure::Claude { input_tokens }));
            }
            "backend_compaction" if data["backend"] == "claude" => {
                self.measure_context(None);
                let trigger = data["trigger"].as_str().map_or_else(String::new, |value| format!(" · {value}"));
                let size = data["pre_tokens"].as_u64().map_or_else(String::new, |value| format!(" · {value} tok before"));
                self.context_changed(seq, ContextChange::Compaction, format!("Claude Code compacted{trigger}{size}"));
            }
            "context_budget" => {
                self.measure_context(context_amounts(&data["selected_bytes"], &data["selection_available_bytes"])
                    .map(|(selected, available)| ContextMeasure::Selection { selected, available }));
            }
            "context_shortage" => {
                self.measure_context(None);
                let description = match (data["protected_bytes"].as_u64(), data["available_bytes"].as_u64()) {
                    (Some(protected), Some(available)) => format!("OpenRouter context shortage · {protected} B required · {available} B available"),
                    _ => "OpenRouter context shortage".into(),
                };
                self.context_changed(seq, ContextChange::Shortage, description);
            }
            "activation_input" => self.put(format!("event-{seq}"),"Activation input",text("text"),false),
            "transcript_imported" => {self.imported=true;self.put(format!("event-{seq}"),"Imported history",display(data),false);}
            "inference_message" if self.imported => self.put(format!("event-{seq}"),if data["role"]=="user" {"Imported user"} else {"Imported assistant"},display(&data["content"]),false),
            "scheduled_event" => self.put(format!("event-{seq}"),"Schedule",display(data),false),
            "context_boundary" => {
                if let (Some(selected), Some(count), Some(masked)) =
                    (data["selected"].as_array(), data["source_count"].as_u64(), data["masked"].as_array())
                {
                    let omitted = count.saturating_sub(selected.len() as u64);
                    if !masked.is_empty() || omitted != 0 {
                        self.context_changed(seq, ContextChange::Projection, format!(
                            "OpenRouter history selection · {} projected · {omitted} omitted", masked.len()
                        ));
                    }
                }
            }

            "inference_frame" if data["event"]["type"]=="response_headers" && data["event"]["status"].as_u64().is_some_and(|s|s>=400) => self.put(format!("event-{seq}"),"Incident",format!("Inference provider response: HTTP {}\nGeneration: {}",data["event"]["status"],data["event"]["generation_id"].as_str().unwrap_or("not supplied")),false),
            "user_message" => {
                let id = format!("event-{seq}");
                self.put(id.clone(), "You", text("text"), false);
                let row = &mut self.rows[self.positions[&id]];
                row.attachments = crate::attachments::FileRef::parse(&data["attachments"]);
                row.update_id = data["update_id"].as_str().map(str::to_owned);
            },
            "assistant_delta" | "reasoning_delta" | "assistant_message" => self.put(
                text("id"),
                if kind == "reasoning_delta" {
                    "Thinking"
                } else {
                    "Agent"
                },
                text("text"),
                kind != "assistant_message",
            ),
            "tool_started" | "tool_output" | "tool_result" | "resident_tool_result" => {
                let call = text("call_id");
                let id = if call.is_empty() {format!("event-{seq}")} else {format!("tool-{call}")};
                if kind == "tool_started" || !self.positions.contains_key(&id) {
                    let name = text("tool");
                    self.put(id.clone(),"Tool",format!("{name}\n{}",display(&data["arguments"])),false);
                }
                let details = self.rows[self.positions[&id]].tool.get_or_insert_with(ToolDetail::default);
                if kind == "tool_output" {
                    details.output.push_str(&text("text"));
                } else if matches!(kind,"tool_result" | "resident_tool_result") {
                    details.result = Some(display(&data["result"]));
                    details.failed = data["success"] == false || data["result"]["exit_code"].as_i64().is_some_and(|n|n!=0);
                }
            }
            "control" if data["type"] == "steer_queued" => {
                if let Some(id) = data["message"]["update_id"].as_str() {
                    self.queued_updates.insert(id.to_owned());
                }
            }
            "control" if data["type"] == "steer" => self.put(
                format!("event-{seq}"),
                "You · steering",
                text("text"),
                false,
            ),
            "backend" => {
                let method = data["method"].as_str().unwrap_or("");
                let p = &data["params"];
                if method == "thread/tokenUsage/updated" {
                    let usage = &p["tokenUsage"];
                    self.measure_context(context_amounts(&usage["last"]["totalTokens"], &usage["modelContextWindow"])
                        .map(|(tokens, window)| ContextMeasure::Codex { tokens, window }));
                } else if matches!(method, "item/started" | "item/completed")
                    && p["item"]["type"] == "contextCompaction"
                {
                    if method == "item/completed" {
                        self.measure_context(None);
                        self.context_changed(seq, ContextChange::Compaction, "Codex reported compaction".into());
                    }
                } else if method == "item/tool/call" && let Some(call) = p["callId"].as_str() {
                    self.put(format!("tool-{call}"),"Tool",format!("{}\n{}",display(&p["tool"]),display(&p["arguments"])),false);
                } else if method == "item/started" || method == "item/completed" {
                    let item = &p["item"];
                    let ty = item["type"].as_str().unwrap_or("Activity");
                    if ty != "userMessage" {
                        let label = match ty {
                            "agentMessage" => "Agent",
                            "reasoning" => "Thinking",
                            "commandExecution" => "Command",
                            _ => ty,
                        };
                        let body = match ty {
                            "agentMessage" => item["text"].as_str().unwrap_or("").into(),
                            "reasoning" => item["summary"]
                                .as_array()
                                .map(|a| a.iter().map(display).collect::<Vec<_>>().join("\n"))
                                .unwrap_or_default(),
                            "commandExecution" => [
                                item["command"].as_str().unwrap_or(""),
                                item["aggregatedOutput"].as_str().unwrap_or(""),
                            ]
                            .join("\n"),
                            _ => display(item),
                        };
                        if !body.is_empty() || method == "item/completed" {
                            let id = item["id"].as_str().unwrap_or(ty).to_owned();
                            self.put(id.clone(),label,body,false);
                            if ty == "commandExecution" && method == "item/completed" {
                                self.rows[self.positions[&id]].tool = Some(ToolDetail {
                                    result:item["exitCode"].as_i64().map(|n| format!("Exit code {n}")),
                                    failed:matches!(item["status"].as_str(),Some("failed"|"declined")) || item["exitCode"].as_i64().is_some_and(|n|n!=0),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                } else if method.ends_with("Delta") || method.ends_with("/delta") {
                    let label = if method.contains("reasoning") {
                        "Thinking"
                    } else if method.contains("agentMessage") {
                        "Agent"
                    } else {
                        "Command"
                    };
                    self.put(
                        p["itemId"].as_str().unwrap_or(method).into(),
                        label,
                        p["delta"].as_str().unwrap_or("").into(),
                        true,
                    );
                } else if !data["id"].is_null() {
                    self.put(
                        format!("event-{seq}"),
                        "Backend input requested",
                        display(data),
                        false,
                    );
                    self.rows[self.positions[&format!("event-{seq}")]].request = Some(data.clone());
                } else if method == "serverRequest/resolved" {
                    for row in &mut self.rows {
                        if row.request.as_ref().is_some_and(|r| r["id"] == p["requestId"]) {
                            row.request = None;
                            row.label = "Backend input resolved".into();
                        }
                    }
                } else if matches!(method, "turn/plan/updated" | "turn/diff/updated" | "error") {
                    self.put(format!("event-{seq}"), method, display(p), false);
                }
            }
            "input_requested" if data["backend"] == "antigravity" => {
                let id = format!("event-{seq}");
                self.put(id.clone(), "Backend input requested", text("question"), false);
                self.rows[self.positions[&id]].request = Some(serde_json::json!({
                    "id":data["request_id"],"method":"antigravity/input","params":data
                }));
            }
            "request_cancelled" => {
                if let Some(row) = self.pending_request_mut(&data["request_id"]) {
                    row.request = None;
                    row.label = "Backend input cancelled".into();
                }
            }
            "request_answered" => {
                for row in &mut self.rows {
                    if row.request.as_ref().is_some_and(|r|r["id"] == data["request_id"]) {
                        row.request = None;
                        row.label = "Backend input answered".into();
                    }
                }
            }
            "control" => self.put(format!("event-{seq}"),"Control",display(data),false),
            "status" if terminal(data) => {
                for row in self.rows.iter_mut().filter(|row| row.request.is_some()) {
                    row.request = None;
                    row.label = "Closed · no answer recorded".into();
                    if let Some(answer) = &mut row.answer {
                        if matches!(answer.state, AnswerState::Writing | AnswerState::Unconfirmed) {
                            answer.state = AnswerState::Unconfirmed;
                        } else {
                            row.answer = None;
                        }
                    }
                }
                if data["status"] != "completed" {
                    self.put(format!("event-{seq}"), "Status",
                        format!("{} · {}", text("status"), text("detail")), false);
                }
            }
            "status" if data["status"] != "completed" => self.put(
                format!("event-{seq}"),
                "Status",
                if text("detail").is_empty() || text("detail") == "null" {
                    text("status")
                } else {
                    format!("{} · {}", text("status"), text("detail"))
                },
                false,
            ),
            "control_error" => {
                if let Some(id) = data["update"]["update_id"].as_str() {
                    self.rejected_updates.insert(id.into(), (text("message"), data["dispatch"] == "not_sent"));
                }
                self.put(format!("event-{seq}"), "Incident", display(data), false);
            }
            "execution_settings_changed" => {
                self.put(format!("event-{seq}"), "Run settings changed", display(data), false);
            }
            "tool_outcome_unknown"
            | "recovery_notice"
            | "backend_session_rebuilt"
            | "context_summary_failed"
            | "observation_incident" => {
                if kind == "backend_session_rebuilt" {
                    self.measure_context(None);
                    self.context_change = None;
                }
                self.put(format!("event-{seq}"), "Incident", display(data), false)
            }
            _ => {}
        }
        if let Some(observed) = crate::pulse::visible(event) {
            self.latest_visible = Some((seq, observed));
        }
        self.seq = seq;
        Ok(())
    }
}
pub fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn replay_attributes_the_only_written_answer_and_preserves_an_interrupted_write() {
        let mut transcript = Transcript::default();
        let events = [
            (
                "backend",
                json!({"id":0,"method":"item/commandExecution/requestApproval"}),
            ),
            (
                "response_attempt",
                json!({"request_id":0,"attempt_id":"winner","machine_id":"other","decision":"decline"}),
            ),
            (
                "response_attempt",
                json!({"request_id":0,"attempt_id":"local","machine_id":"here","decision":"accept"}),
            ),
            (
                "response_write_started",
                json!({"request_id":0,"attempt_id":"winner"}),
            ),
            (
                "request_answered",
                json!({"request_id":0,"attempt_id":"winner"}),
            ),
            (
                "response_refused",
                json!({"request_id":0,"attempt_id":"local"}),
            ),
            (
                "input_requested",
                json!({"backend":"antigravity","request_id":1,"question":"Choose a branch"}),
            ),
            (
                "response_attempt",
                json!({"request_id":1,"attempt_id":"later","machine_id":"here","decision":"submit"}),
            ),
            (
                "response_write_started",
                json!({"request_id":1,"attempt_id":"later"}),
            ),
            (
                "status",
                json!({"status":"interrupted","detail":"runtime stopped during control"}),
            ),
        ];
        for (index, (kind, data)) in events.into_iter().enumerate() {
            transcript
                .apply(&json!({"seq":index+1,"kind":kind,"data":data}))
                .unwrap();
        }
        let written = transcript.rows[0].answer.as_ref().unwrap();
        assert_eq!(written.attempt_id, "winner");
        assert_eq!(written.state, AnswerState::Written);
        assert!(transcript.rows[0].request.is_none());
        let refused = transcript.rows[1].answer.as_ref().unwrap();
        assert_eq!(refused.attempt_id, "local");
        assert_eq!(refused.state, AnswerState::Refused);
        let later = transcript.rows[2].answer.as_ref().unwrap();
        assert_eq!(later.attempt_id, "later");
        assert_eq!(later.state, AnswerState::Unconfirmed);
        assert!(transcript.rows[2].request.is_none());
    }

    #[test]
    fn context_measure_preserves_acp_and_selection_units_and_missing_samples() {
        let mut transcript = Transcript::default();
        transcript.apply(&json!({
            "seq":1,"kind":"backend_usage","data":{"backend":"antigravity","used":38,"size":1000}
        })).unwrap();
        assert_eq!(
            transcript.context,
            Some(ContextMeasure::Antigravity {
                tokens: 38,
                window: 1000
            })
        );
        transcript.apply(&json!({
            "seq":2,"kind":"backend_usage","data":{"backend":"antigravity","used":null,"size":1000}
        })).unwrap();
        assert!(transcript.context.is_none());
        transcript.apply(&json!({
            "seq":3,"kind":"context_budget","data":{
                "configured_bytes":196608,"selected_bytes":75776,"selection_available_bytes":163840
            }
        })).unwrap();
        assert_eq!(
            transcript.context.unwrap().label(),
            "ctx 74/160 KiB selectable"
        );
        assert!(!transcript.context.unwrap().past_warning_line());
    }

    #[test]
    fn context_projection_keeps_reported_compaction_separate_from_the_next_reading() {
        let mut transcript = Transcript::default();
        let apply = |transcript: &mut Transcript, seq, kind, data| {
            transcript.apply(&json!({"seq":seq,"at":seq*1000,"kind":kind,"data":data})).unwrap();
        };
        apply(&mut transcript, 1, "backend_usage", json!({"backend":"claude","input_tokens":162000}));
        apply(&mut transcript, 2, "backend_compaction", json!({"backend":"claude","trigger":"auto","pre_tokens":162000}));
        assert!(transcript.context.is_none());
        assert_eq!(transcript.context_change, Some((ContextChange::Compaction, 2000)));
        assert_eq!(transcript.rows[0].text, "Claude Code compacted · auto · 162000 tok before");
        apply(&mut transcript, 3, "backend_usage", json!({"backend":"claude","input_tokens":41000}));
        assert_eq!(transcript.context, Some(ContextMeasure::Claude { input_tokens: 41000 }));
        assert_eq!(transcript.context_at, Some(3000));
        assert_eq!(transcript.context_change, Some((ContextChange::Compaction, 2000)));
        apply(&mut transcript, 4, "backend_thread", json!({"id":"replacement","backend":"claude"}));
        assert!(transcript.context.is_none());
        assert!(transcript.context_change.is_none());
        assert_eq!(transcript.rows.len(), 1);
    }

    #[test]
    fn execution_context_replay_preserves_the_observed_account_and_session() {
        let mut transcript = Transcript::default();
        assert!(transcript.execution.is_none());
        let observed = json!({"account":"owner","uid":1001,"elevated":false,"session":"Aqua","desktop_available":true,"cwd":"/fixture"});
        let event = json!({"seq":1,"kind":"execution_context","data":observed});
        transcript.apply(&event).unwrap();
        transcript.apply(&event).unwrap();
        assert_eq!(transcript.execution, Some(observed));
        assert_eq!(transcript.seq, 1);
        assert!(transcript.rows.is_empty());
    }
    #[test]
    fn backend_requests_survive_replay_and_clear_on_answer_or_resolution() {
        for method in [
            "mcpServer/elicitation/request",
            "item/commandExecution/requestApproval",
            "item/tool/requestUserInput",
            "future/input/request",
        ] {
            let mut transcript = Transcript::default();
            let request = json!({"seq":1,"kind":"backend","data":{"id":0,"method":method,"params":{"message":"Allow access to Example App?"}}});
            transcript.apply(&request).unwrap();
            transcript.apply(&request).unwrap();
            assert_eq!(transcript.rows.len(), 1);
            assert_eq!(transcript.rows[0].request.as_ref().unwrap()["id"], 0);
            transcript.apply(&json!({"seq":2,"kind":"backend","data":{"method":"serverRequest/resolved","params":{"requestId":"another-request"}}})).unwrap();
            assert!(transcript.rows[0].request.is_some());
            transcript.apply(&json!({"seq":3,"kind":"backend","data":{"method":"serverRequest/resolved","params":{"requestId":0}}})).unwrap();
            assert!(transcript.rows[0].request.is_none());
            assert_eq!(transcript.rows[0].label, "Backend input resolved");
            let mut answered = Transcript::default();
            answered.apply(&request).unwrap();
            answered
                .apply(&json!({"seq":2,"kind":"request_answered","data":{"request_id":0}}))
                .unwrap();
            assert!(answered.rows[0].request.is_none());
        }
    }
    #[test]
    fn activation_and_import_roles_remain_distinct_from_delivered_mail() {
        let mut t = Transcript::default();
        t.apply(
            &json!({"seq":1,"kind":"activation_input","data":{"text":"Authored incoming batch"}}),
        )
        .unwrap();
        t.apply(&json!({"seq":2,"kind":"inference_message","data":{"role":"assistant","content":"Not a second rendered copy"}})).unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].label, "Activation input");
        let mut imported = Transcript::default();
        imported.apply(&json!({"seq":1,"kind":"transcript_imported","data":{"source":{"provider":"example"}}})).unwrap();
        imported.apply(&json!({"seq":2,"kind":"inference_message","data":{"role":"assistant","content":"Original source"}})).unwrap();
        assert_eq!(imported.rows[1].label, "Imported assistant");
        assert_eq!(imported.rows[1].text, "Original source");
    }
    #[test]
    fn identity_excludes_whole_conversation() {
        let runs = vec![
            json!({"record":{"spec":{"thread_id":"a","prompt":"one"}}}),
            json!({"record":{"spec":{"thread_id":"b","prompt":"two"}}}),
            json!({"record":{"spec":{"thread_id":"a"},"resident":{"id":"resident"}}}),
        ];
        let c = catalog("peer", &runs);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].key.id, "b");
    }
    #[test]
    fn replay_deduplicates_and_completion_replaces_deltas() {
        let mut t = Transcript::default();
        let e = json!({"seq":1,"kind":"assistant_delta","data":{"id":"m","text":"part"}});
        t.apply(&e).unwrap();
        t.apply(&e).unwrap();
        t.apply(&json!({"seq":2,"kind":"assistant_message","data":{"id":"m","text":"whole"}}))
            .unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].text, "whole");
        assert!(t.apply(&json!({"seq":4})).is_err());
        assert_eq!(t.seq, 2);
    }
}
