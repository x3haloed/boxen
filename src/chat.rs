//! OpenRouter Chat Completions transport and conservative native-state projection.
use crate::Result;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    time::Duration,
};
const FRAME_LIMIT: usize = 1024 * 1024;
pub const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
pub fn exchange(
    endpoint: &str,
    key: Option<&str>,
    body: &Value,
    seconds: u64,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(seconds)))
        .build()
        .new_agent();
    let mut request = agent.post(endpoint).header("Accept", "text/event-stream");
    if let Some(key) = key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    let mut response = request
        .send_json(body)
        .map_err(|_| "inference transport failed (details suppressed)")?;
    let status = response.status().as_u16();
    emit(json!({"type":"response_headers","status":status}))?;
    if status != 200 {
        return Err("inference HTTP status was not 200 (body suppressed)".into());
    }
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"))
    {
        return Err("response content type is not text/event-stream".into());
    }
    let mut reader = BufReader::new(response.body_mut().as_reader());
    let mut data = String::new();
    let mut terminal = false;
    loop {
        let mut line = Vec::new();
        let read = reader
            .by_ref()
            .take((FRAME_LIMIT + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err("stream ended before [DONE]".into());
        }
        if read > FRAME_LIMIT {
            return Err("SSE line exceeds 1 MiB".into());
        }
        let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            data.pop(); // final SSE data newline
            if data == "[DONE]" {
                if !terminal {
                    return Err("[DONE] arrived without a finish reason".into());
                }
                emit(json!({"type":"exchange_completed"}))?;
                return Ok(());
            }
            let frame: Value = serde_json::from_str(&data)?;
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
            emit(json!({"type":"provider_frame","frame":frame}))?;
            data.clear();
            if error {
                return Err(
                    "provider reported an in-stream error (details retained in private journal)"
                        .into(),
                );
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            data.push('\n');
            if data.len() > FRAME_LIMIT {
                return Err("SSE frame exceeds 1 MiB".into());
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
