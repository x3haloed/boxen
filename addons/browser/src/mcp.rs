//! Stdio MCP session. Its trusted launch config identifies one browser profile.

#[cfg(target_os = "macos")]
use crate::chrome_owner::Browser;
use crate::{Result, automation::Automation, browser_assignment::Settings};
#[cfg(not(target_os = "macos"))]
use boxen_browser::browser::Browser;
use boxen_browser::cdp::Connection;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Read, Write},
    time::Duration,
};

pub(crate) const MAX_REQUEST: u64 = 1024 * 1024;
const INLINE_RESULT: usize = 192 * 1024;
const DEFAULT_VERSION: &str = "2025-11-25";

pub fn serve(settings: Settings, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    if !settings
        .agent_browser
        .as_ref()
        .is_some_and(|path| path.is_file())
    {
        return Err("the configured Chromium agent-browser executable must exist".into());
    }
    let mut server = Server {
        settings,
        session: None,
        initialized: false,
    };
    let result = (|| -> Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            let size = (&mut input)
                .take(MAX_REQUEST + 1)
                .read_until(b'\n', &mut line)?;
            if size == 0 {
                return Ok(());
            }
            if size as u64 > MAX_REQUEST {
                return Err("browser MCP request exceeded its frame bound".into());
            }
            let response = match serde_json::from_slice::<Value>(&line) {
                Ok(request) => server.request(&request),
                Err(_) => Some(error(Value::Null, -32700, "MCP request is not valid JSON")),
            };
            if let Some(response) = response {
                serde_json::to_writer(&mut output, &response)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
    })();
    let cleanup = server.close();
    result.and(cleanup)
}

struct Server {
    settings: Settings,
    session: Option<Session>,
    initialized: bool,
}

struct Session {
    automation: Automation,
    cdp: Connection,
    browser: Browser,
}

impl Server {
    fn request(&mut self, request: &Value) -> Option<Value> {
        let method = request["method"].as_str();
        if request["jsonrpc"] != "2.0" || method.is_none() {
            return Some(error(request["id"].clone(), -32600, "invalid MCP request"));
        }
        let id = request.get("id")?.clone();
        if !id.is_string() && !id.is_i64() && !id.is_u64() {
            return Some(error(
                Value::Null,
                -32600,
                "MCP request ID must be an integer or string",
            ));
        }
        let result = match method.unwrap() {
            "initialize" if !self.initialized => {
                self.initialized = true;
                let version = match request["params"]["protocolVersion"].as_str() {
                    Some(v @ ("2024-11-05" | "2025-03-26" | "2025-06-18" | DEFAULT_VERSION)) => v,
                    _ => DEFAULT_VERSION,
                };
                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{
                    "name":"boxen-browser","version":env!("CARGO_PKG_VERSION")
                }})
            }
            "ping" => json!({}),
            "tools/list" if self.initialized => tools(),
            "tools/call" if self.initialized => {
                let params = &request["params"];
                match self.tool(params["name"].as_str(), &params["arguments"]) {
                    Ok(value) => tool_text(value.to_string(), false),
                    Err(error) => tool_text(error.to_string(), true),
                }
            }
            _ => {
                return Some(error(
                    id,
                    -32601,
                    "MCP method is unavailable in this session",
                ));
            }
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    fn tool(&mut self, name: Option<&str>, args: &Value) -> Result<Value> {
        let tool = name.ok_or("browser tool name is missing")?;
        if !matches!(tool, "browser_action" | "browser_cdp" | "browser_events") {
            return Err("browser tool is unavailable".into());
        }
        let evidence = self.settings.evidence.clone();
        let session = self.session()?;
        let value = match tool {
            "browser_action" => session.automation.perform(args, &evidence)?,
            "browser_cdp" => {
                let method = args["method"]
                    .as_str()
                    .filter(|method| !method.is_empty())
                    .ok_or("CDP command requires a method")?;
                let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
                if !params.is_object() {
                    return Err("CDP params must be an object".into());
                }
                let target = args
                    .get("session_id")
                    .map(|session| session.as_str().ok_or("CDP session_id must be a string"))
                    .transpose()?;
                session
                    .cdp
                    .command(method, params, target, wait(args, 10_000, 1)?)?
            }
            "browser_events" => {
                let after = args
                    .get("after")
                    .map(|value| value.as_u64().ok_or("event cursor must be nonnegative"))
                    .transpose()?
                    .unwrap_or(0);
                session.cdp.events(after, wait(args, 1_000, 0)?)?
            }
            _ => unreachable!(),
        };
        let bytes = serde_json::to_vec(&value)?;
        if bytes.len() <= INLINE_RESULT {
            return Ok(value);
        }
        let mut file = tempfile::Builder::new()
            .prefix("browser-result-")
            .suffix(".json")
            .tempfile_in(&evidence)?;
        file.write_all(&bytes)?;
        let (_, path) = file.keep()?;
        Ok(json!({"path":path,"format":"json","bytes":bytes.len(),
            "cursor":value.get("cursor"),"dropped_through":value.get("dropped_through")}))
    }

    fn session(&mut self) -> Result<&mut Session> {
        if self.session.is_none() {
            std::fs::create_dir_all(&self.settings.evidence)?;
            let browser = Browser::launch(
                &self.settings.executable,
                &self.settings.profile,
                self.settings.headless,
            )?;
            let cdp = browser.endpoint().connect()?;
            let automation = Automation::new(
                self.settings.agent_browser.clone().unwrap(),
                browser.endpoint().clone(),
            )?;
            self.session = Some(Session {
                automation,
                cdp,
                browser,
            });
        }
        Ok(self.session.as_mut().unwrap())
    }

    fn close(self) -> Result<()> {
        if let Some(Session {
            mut automation,
            cdp,
            browser,
        }) = self.session
        {
            let automation_result = automation.stop();
            drop(cdp);
            let browser_result = browser.close();
            return browser_result.and(automation_result);
        }
        Ok(())
    }
}

fn wait(args: &Value, default: u64, min: u64) -> Result<Duration> {
    let ms = args
        .get("wait_ms")
        .map(|value| value.as_u64().ok_or("wait_ms must be nonnegative"))
        .transpose()?
        .unwrap_or(default);
    if !(min..=25_000).contains(&ms) {
        return Err(format!("wait_ms must be between {min} and 25000").into());
    }
    Ok(Duration::from_millis(ms))
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn tool_text(text: String, is_error: bool) -> Value {
    json!({"isError":is_error,"content":[{"type":"text","text":text}]})
}

fn tools() -> Value {
    json!({"tools":[
        {"name":"browser_action","description":"Navigates, inspects or operates the assigned browser. Snapshots return element references. Screenshots are retained as PNGs in the assignment evidence directory.",
            "inputSchema":{"type":"object","required":["action"],"properties":{
                "action":{"type":"string","enum":["open","snapshot","click","fill","press","back","forward","reload","screenshot"]},
                "url":{"type":"string"},"interactive":{"type":"boolean"},"selector":{"type":"string"},
                "text":{"type":"string"},"key":{"type":"string"},"full_page":{"type":"boolean"}
            },"additionalProperties":false}},
        {"name":"browser_cdp","description":"Sends an arbitrary native DevTools command to the assigned browser. A flattened Target.attachToTarget session ID routes later commands to that target. The result includes native errors. Large results are retained as JSON in the assignment evidence directory.",
            "inputSchema":{"type":"object","required":["method"],"properties":{
                "method":{"type":"string"},"params":{"type":"object"},"session_id":{"type":"string"},"wait_ms":{"type":"integer","minimum":1,"maximum":25000}
            },"additionalProperties":false}},
        {"name":"browser_events","description":"Reads DevTools events including debugger pauses after the supplied cursor. The returned cursor advances as events are observed. dropped_through identifies the last evicted event.",
            "inputSchema":{"type":"object","properties":{
                "after":{"type":"integer","minimum":0},"wait_ms":{"type":"integer","minimum":0,"maximum":25000}
            },"additionalProperties":false}}
    ]})
}
