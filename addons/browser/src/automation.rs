//! Maps MCP actions to the local agent-browser CLI using a single private session.

use crate::Result;
use boxen_browser::browser::Endpoint;
use serde_json::{Value, json};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const MAX_OUTPUT: u64 = 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(25);

pub struct Automation {
    executable: PathBuf,
    endpoint: Endpoint,
    private: TempDir,
    started: bool,
}

impl Automation {
    pub fn new(executable: PathBuf, endpoint: Endpoint) -> Result<Self> {
        let private = tempfile::Builder::new()
            .prefix("boxen-browser-")
            .tempdir()?;
        std::fs::write(private.path().join("config.json"), b"{}")?;
        Ok(Self {
            executable,
            endpoint,
            private,
            started: false,
        })
    }

    pub fn perform(&mut self, args: &Value, evidence: &Path) -> Result<Value> {
        let command = required(args, "action")?;
        let mut screenshot = None;
        let parameters = match command {
            "open" => json!({"url":required(args, "url")?}),
            "snapshot" => json!({"interactive":args["interactive"].as_bool().unwrap_or_default()}),
            "click" => json!({"selector":required(args, "selector")?}),
            "fill" => json!({"selector":required(args, "selector")?,
                "text":args["text"].as_str().ok_or("fill requires text")?}),
            "press" => json!({"key":required(args, "key")?}),
            "back" | "forward" | "reload" => json!({}),
            "screenshot" => {
                let file = tempfile::Builder::new()
                    .prefix("browser-")
                    .suffix(".png")
                    .tempfile_in(evidence)?;
                let parameters = json!({"path":file.path(),"format":"png",
                    "fullPage":args["full_page"].as_bool().unwrap_or_default()});
                screenshot = Some(file);
                parameters
            }
            _ => return Err("browser action is unavailable".into()),
        };
        self.started = true;
        let result = self.run(command, parameters, COMMAND_TIMEOUT)?;
        if let Some(file) = screenshot {
            let mut signature = [0; 8];
            std::fs::File::open(file.path())?.read_exact(&mut signature)?;
            if &signature != b"\x89PNG\r\n\x1a\n" {
                return Err("browser screenshot was not a PNG".into());
            }
            let (_, path) = file.keep()?;
            return Ok(serde_json::json!({"path":path,"format":"png"}));
        }
        Ok(result)
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.started {
            self.started = false;
            self.run("close", json!({}), Duration::from_secs(5))?;
        }
        Ok(())
    }

    fn run(&self, action: &str, parameters: Value, timeout: Duration) -> Result<Value> {
        let mut command = Command::new(&self.executable);
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("AGENT_BROWSER_")
            {
                command.env_remove(key);
            }
        }
        let mut input = tempfile::tempfile_in(self.private.path())?;
        serde_json::to_writer(&mut input, &parameters)?;
        input.seek(SeekFrom::Start(0))?;
        let mut output = tempfile::tempfile_in(self.private.path())?;
        command
            .current_dir(self.private.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.private.path())
            .env("AGENT_BROWSER_CDP", self.endpoint.websocket_url())
            .env("AGENT_BROWSER_MAX_OUTPUT", "262144")
            .env("AGENT_BROWSER_IDLE_TIMEOUT_MS", "120000")
            .args([
                "mcp",
                "call",
                &format!("agent_browser_{action}"),
                "-",
                "--config",
            ])
            .arg(self.private.path().join("config.json"))
            .stdin(input)
            .stdout(output.try_clone()?)
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn()?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    result?;
                    return Err("browser action timed out; its outcome is uncertain".into());
                }
            }
        };
        output.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        output.take(MAX_OUTPUT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_OUTPUT {
            return Err("browser action output exceeded its bound".into());
        }
        let result: Value = serde_json::from_slice(&bytes)
            .map_err(|_| format!("agent-browser exited with {status} without a JSON result"))?;
        if !status.success() || result["isError"].as_bool() == Some(true) {
            return Err(result["content"][0]["text"]
                .as_str()
                .unwrap_or("agent-browser rejected the action")
                .to_owned()
                .into());
        }
        result["structuredContent"]
            .get("response")
            .cloned()
            .ok_or_else(|| "agent-browser did not return a structured action result".into())
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("browser action requires {key}").into())
}
