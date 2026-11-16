//! Host-wide computer and browser permission answers, shared by every client.
use crate::{
    Result,
    logging::{self, Context, Level, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io::Read, path::PathBuf, sync::Mutex};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub computer_use: bool,
    pub browser_use: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            computer_use: true,
            browser_use: true,
        }
    }
}

pub struct Store {
    path: PathBuf,
    write: Mutex<()>,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<Settings> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err("Tool approval settings exceed 4 KiB".into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn operation(&self, args: &Value) -> Result<Value> {
        let _guard = self.write.lock().unwrap();
        match args["action"].as_str().unwrap_or("get") {
            "get" => Ok(serde_json::to_value(self.load()?)?),
            "set" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Update {
                    action: String,
                    computer_use: Option<bool>,
                    browser_use: Option<bool>,
                }
                let update: Update = serde_json::from_value(args.clone())?;
                debug_assert_eq!(update.action, "set");
                let mut settings = self.load()?;
                if let Some(enabled) = update.computer_use {
                    settings.computer_use = enabled;
                }
                if let Some(enabled) = update.browser_use {
                    settings.browser_use = enabled;
                }
                let temporary = self
                    .path
                    .with_extension(format!("pending-{}", crate::state::random_hex()?));
                crate::state::write_new(&temporary, &serde_json::to_vec(&settings)?)?;
                let result = fs::rename(&temporary, &self.path);
                if result.is_err() {
                    let _ = fs::remove_file(&temporary);
                }
                result?;
                logging::record(
                    Level::Info,
                    Target::Runtime,
                    "tool_approvals.updated",
                    Context::default(),
                    || json!({"computer_use":settings.computer_use,"browser_use":settings.browser_use}),
                    || Value::Null,
                );
                Ok(serde_json::to_value(settings)?)
            }
            _ => Err("Unknown tool approval settings action".into()),
        }
    }

    pub fn automatic(&self, message: &Value) -> Result<Option<&'static str>> {
        let params = &message["params"];
        if message["method"] != "mcpServer/elicitation/request"
            || message.get("id").is_none()
            || params["_meta"]["codex_approval_kind"] != "mcp_tool_call"
        {
            return Ok(None);
        }
        let (category, enabled) = match params["_meta"]["connector_id"].as_str() {
            Some("computer-use") => ("computer_use", self.load()?.computer_use),
            Some("browser-use") => ("browser_use", self.load()?.browser_use),
            _ => return Ok(None),
        };
        if !enabled {
            logging::record(
                Level::Debug,
                Target::Runtime,
                "tool_approval.manual",
                Context::default(),
                || json!({"request_id":message["id"],"category":category}),
                || Value::Null,
            );
        }
        Ok(enabled.then_some(category))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_identification_preserve_separate_options() {
        let store = Store::new(std::env::temp_dir().join(format!(
            "boxen-approval-{}",
            crate::state::random_hex().unwrap()
        )));
        for (connector, category) in [
            ("computer-use", "computer_use"),
            ("browser-use", "browser_use"),
        ] {
            let message = json!({"id":0,"method":"mcpServer/elicitation/request","params":{"_meta":{"codex_approval_kind":"mcp_tool_call","connector_id":connector}}});
            assert_eq!(store.automatic(&message).unwrap(), Some(category));
        }
        for kind in ["browser_auth", "browser_email_otp", "unrelated"] {
            assert_eq!(store.automatic(&json!({"id":0,"method":"mcpServer/elicitation/request","params":{"_meta":{"codex_approval_kind":kind,"connector_id":"browser-use"}}})).unwrap(), None);
        }
        assert_eq!(
            store
                .automatic(&json!({"id":0,"method":"item/commandExecution/requestApproval"}))
                .unwrap(),
            None
        );
    }
}
