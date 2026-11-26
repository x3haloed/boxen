//! Chat Completions transport and conservative native-state projection.
use crate::Result;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read},
    time::Duration,
};
const FRAME_LIMIT: usize = 1024 * 1024;
pub const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
/// Stable, credential-free failure evidence at a worker or daemon boundary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Failure {
    pub stage: &'static str,
    pub code: &'static str,
    pub reason: &'static str,
}

impl Failure {
    pub const fn new(stage: &'static str, code: &'static str, reason: &'static str) -> Self {
        Self {
            stage,
            code,
            reason,
        }
    }

    pub fn event(self) -> Value {
        json!({"type":"exchange_failed","stage":self.stage,"code":self.code,"reason":self.reason})
    }

    fn request(error: ureq::Error) -> Self {
        use ureq::{Error, Timeout};
        match error {
            Error::Timeout(timeout) => Self::new(
                "timeout",
                match timeout {
                    Timeout::Resolve => "resolve_timeout",
                    Timeout::Connect => "connect_timeout",
                    Timeout::SendRequest | Timeout::SendBody | Timeout::PerWrite => "send_timeout",
                    Timeout::RecvResponse => "response_headers_timeout",
                    _ => "request_timeout",
                },
                "Inference request timed out before response headers",
            ),
            Error::HostNotFound => Self::new(
                "connection",
                "dns_failed",
                "Inference endpoint hostname could not be resolved",
            ),
            Error::ConnectionFailed => Self::connect(),
            Error::Io(error) if error.kind() == io::ErrorKind::TimedOut => Self::new(
                "timeout",
                "request_timeout",
                "Inference request timed out before response headers",
            ),
            Error::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::NotConnected
                        | io::ErrorKind::NetworkUnreachable
                        | io::ErrorKind::HostUnreachable
                ) =>
            {
                Self::connect()
            }
            Error::Tls(_) | Error::Rustls(_) | Error::TlsRequired => Self::tls(),
            Error::Io(error)
                if error
                    .get_ref()
                    .is_some_and(|source| source.is::<rustls::Error>()) =>
            {
                Self::tls()
            }
            Error::Protocol(_) | Error::LargeResponseHeader(..) => Self::new(
                "response_headers",
                "invalid_http",
                "Inference response headers were invalid",
            ),
            _ => Self::new(
                "request",
                "request_failed",
                "Inference request failed before response headers",
            ),
        }
    }

    fn tls() -> Self {
        Self::new(
            "connection",
            "tls_failed",
            "Inference endpoint TLS setup failed",
        )
    }

    fn connect() -> Self {
        Self::new(
            "connection",
            "connect_failed",
            "Inference endpoint connection failed",
        )
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(self.reason)
    }
}
impl std::error::Error for Failure {}

pub fn exchange(
    endpoint: &str,
    key: Option<&str>,
    body: &Value,
    deadline_seconds: Option<u64>,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> std::result::Result<(), Failure> {
    let sink = |_| {
        Failure::new(
            "retention",
            "event_sink_failed",
            "Inference event sink could not accept a record",
        )
    };
    let mut config = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(deadline_seconds.map(Duration::from_secs))
        .timeout_resolve(Some(Duration::from_secs(20)))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_send_body(Some(Duration::from_secs(30)));
    if endpoint.starts_with("http://127.0.0.1:") {
        config = config.proxy(None);
    }
    let agent = config.build().new_agent();
    let mut request = agent.post(endpoint).header("Accept", "text/event-stream");
    if let Some(key) = key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    let mut response = request.send_json(body).map_err(Failure::request)?;
    let status = response.status().as_u16();
    let metadata = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() <= 256)
            .map(str::to_owned)
    };
    emit(
        json!({"type":"response_headers","status":status,"provider":metadata("x-provider-name"),"generation_id":metadata("x-generation-id")}),
    ).map_err(sink)?;
    if status != 200 {
        return Err(Failure::new(
            "http",
            "http_rejected",
            "Provider rejected inference with a non-200 HTTP status",
        ));
    }
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"))
    {
        return Err(Failure::new(
            "response_headers",
            "unexpected_content_type",
            "Inference response content type is not text/event-stream",
        ));
    }
    let mut reader = BufReader::new(response.body_mut().as_reader());
    let mut data = String::new();
    let mut terminal = false;
    loop {
        let mut line = Vec::new();
        let read = reader
            .by_ref()
            .take((FRAME_LIMIT + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::TimedOut
                    || matches!(
                        error
                            .get_ref()
                            .and_then(|inner| inner.downcast_ref::<ureq::Error>()),
                        Some(ureq::Error::Timeout(_))
                    )
                {
                    Failure::new(
                        "timeout",
                        "stream_timeout",
                        "Inference stream read timed out",
                    )
                } else if error.kind() == io::ErrorKind::UnexpectedEof {
                    Failure::new(
                        "stream",
                        "stream_truncated",
                        "Inference stream ended before its transport framing completed",
                    )
                } else {
                    Failure::new(
                        "stream",
                        "stream_read_failed",
                        "Inference stream read failed before completion",
                    )
                }
            })?;
        if read == 0 {
            return Err(Failure::new(
                "stream",
                "missing_done",
                "Inference stream ended before [DONE]",
            ));
        }
        if read > FRAME_LIMIT {
            return Err(Failure::new(
                "frame",
                "line_too_large",
                "Inference SSE line exceeds 1 MiB",
            ));
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| Failure::new("frame", "invalid_utf8", "Inference SSE line is not UTF-8"))?
            .trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            data.pop(); // final SSE data newline
            if data == "[DONE]" {
                if !terminal {
                    return Err(Failure::new(
                        "stream",
                        "missing_finish_reason",
                        "Inference [DONE] arrived without a finish reason",
                    ));
                }
                emit(json!({"type":"exchange_completed"})).map_err(sink)?;
                return Ok(());
            }
            let frame: Value = serde_json::from_str(&data).map_err(|_| {
                Failure::new(
                    "frame",
                    "invalid_json",
                    "Inference SSE frame is not valid JSON",
                )
            })?;
            if !frame.is_object() {
                return Err(Failure::new(
                    "frame",
                    "invalid_shape",
                    "Inference SSE frame is not a JSON object",
                ));
            }
            let error = frame.get("error").is_some();
            if frame
                .get("choices")
                .and_then(Value::as_array)
                .is_some_and(|cs| {
                    cs.iter()
                        .any(|c| c.get("finish_reason").is_some_and(|f| !f.is_null()))
                })
            {
                terminal = true;
            }
            emit(json!({"type":"provider_frame","frame":frame})).map_err(sink)?;
            data.clear();
            if error {
                return Err(Failure::new(
                    "stream",
                    "provider_stream_error",
                    "Provider reported an error inside the inference stream",
                ));
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            data.push('\n');
            if data.len() > FRAME_LIMIT {
                return Err(Failure::new(
                    "frame",
                    "frame_too_large",
                    "Inference SSE frame exceeds 1 MiB",
                ));
            }
        }
    }
}

pub fn project(events: impl IntoIterator<Item = Value>) -> Result<Value> {
    let mut complete = false;
    let mut failed = false;
    let mut finish = Value::Null;
    let mut assistant = json!({"role":"assistant"});
    let mut tools: BTreeMap<u64, Value> = BTreeMap::new();
    let mut details: BTreeMap<u64, Value> = BTreeMap::new();
    let mut usage = Value::Null;
    let mut uncertain_projection = false;
    for event in events {
        if complete || failed {
            return Err("journal contains records after its terminal state".into());
        }
        match event["type"].as_str() {
            Some("exchange_completed") => complete = true,
            Some("exchange_failed") => failed = true,
            Some("provider_frame") => {
                let frame = &event["frame"];
                if frame.get("error").is_some() {
                    uncertain_projection = true;
                }
                if frame.get("usage").is_some_and(|u| !u.is_null()) {
                    usage = frame["usage"].clone();
                }
                if let Some(choices) = frame["choices"].as_array() {
                    for choice in choices {
                        if choice["index"] != 0 {
                            uncertain_projection = true;
                            continue;
                        }
                        if !choice["finish_reason"].is_null() {
                            if !finish.is_null() && finish != choice["finish_reason"] {
                                uncertain_projection = true;
                            }
                            finish = choice["finish_reason"].clone();
                        }
                        if let Some(delta) = choice["delta"].as_object() {
                            for (field, value) in delta {
                                if value.is_null() {
                                    continue;
                                }
                                match field.as_str() {
                                    "content" | "reasoning" | "reasoning_content" => {
                                        append_string(&mut assistant[field], value)?
                                    }
                                    "role" => {
                                        if value != "assistant" {
                                            uncertain_projection = true;
                                        }
                                    }
                                    "tool_calls" => merge_parts(&mut tools, value, true)?,
                                    "reasoning_details" => merge_parts(&mut details, value, false)?,
                                    _ => {
                                        // scalar metadata is replayable only when it is consistent
                                        if !assistant[field].is_null() && assistant[field] != *value
                                        {
                                            uncertain_projection = true;
                                        }
                                        assistant[field] = value.clone();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some("exchange_started" | "response_headers") => {}
            _ => return Err("unrecognized journal record".into()),
        }
    }
    if !tools.is_empty() {
        assistant["tool_calls"] = tools.into_values().collect();
    }
    if !details.is_empty() {
        assistant["reasoning_details"] = details.into_values().collect();
    }
    if assistant.get("content").is_none() {
        assistant["content"] = Value::Null;
    }
    let accepted = complete && !failed && !finish.is_null() && !uncertain_projection;
    Ok(
        json!({"completed":complete,"projection_supported":!uncertain_projection,"finish_reason":finish,"usage":usage,
        "assistant":if accepted { assistant } else { Value::Null }}),
    )
}
fn append_string(target: &mut Value, value: &Value) -> Result<()> {
    let value = value.as_str().ok_or("expected string delta")?;
    if target.is_null() {
        *target = json!("");
    }
    let mut text = target
        .as_str()
        .ok_or("inconsistent string delta")?
        .to_string();
    text.push_str(value);
    *target = json!(text);
    Ok(())
}
fn merge_parts(parts: &mut BTreeMap<u64, Value>, value: &Value, tool: bool) -> Result<()> {
    for item in value.as_array().ok_or("expected delta array")? {
        let index = item["index"]
            .as_u64()
            .ok_or("delta array has no index; projection unavailable")?;
        let target = parts.entry(index).or_insert_with(|| json!({}));
        if !tool {
            target["index"] = json!(index);
        }
        for (field, value) in item.as_object().ok_or("expected delta object")? {
            if field == "index" {
                continue;
            }
            if !tool && matches!(field.as_str(), "text" | "summary" | "data" | "signature") {
                append_string(&mut target[field], value)?;
            } else if tool && field == "function" {
                if target[field].is_null() {
                    target[field] = json!({});
                }
                for (key, value) in value.as_object().ok_or("invalid function delta")? {
                    if key == "name" || key == "arguments" {
                        append_string(&mut target[field][key], value)?;
                    } else {
                        merge_constant(&mut target[field][key], value)?;
                    }
                }
            } else {
                merge_constant(&mut target[field], value)?;
            }
        }
    }
    Ok(())
}
fn merge_constant(target: &mut Value, value: &Value) -> Result<()> {
    if !target.is_null() && *target != *value {
        return Err("changing opaque field cannot be safely projected".into());
    }
    *target = value.clone();
    Ok(())
}
