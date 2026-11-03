//! Operator settings are captured in each accepted run; journals retain that choice.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub providers: Vec<String>,
    pub allow_fallbacks: bool,
    pub max_output_tokens: Option<u32>,
    pub max_message_sends: Option<u32>,
    pub max_exchanges: Option<usize>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            providers: vec![],
            allow_fallbacks: true,
            max_output_tokens: Some(2048),
            max_message_sends: Some(16),
            max_exchanges: Some(16),
        }
    }
}
impl Policy {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("runtime-policy.json");
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(16385).read_to_end(&mut bytes)?;
        if bytes.len() > 16384 {
            return Err("runtime policy exceeds 16 KiB".into());
        }
        let policy: Self = serde_json::from_slice(&bytes)?;
        policy.validate()?;
        Ok(policy)
    }
    pub fn validate(&self) -> Result<()> {
        if self
            .max_output_tokens
            .is_some_and(|n| !(1..=131072).contains(&n))
            || self.max_message_sends.is_some_and(|n| n > 256)
            || self
                .max_exchanges
                .is_some_and(|n| !(1..=65536).contains(&n))
        {
            return Err("runtime policy numeric ranges are output tokens 1–131072, message attempts 0–256, exchanges 1–65536; null removes the corresponding cap".into());
        }
        if self.providers.len() > 16
            || self.providers.iter().any(|p| {
                p.is_empty()
                    || p.len() > 128
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_/.:".contains(&b))
            })
        {
            return Err("runtime policy providers are at most 16 bounded provider slugs".into());
        }
        Ok(())
    }
    pub fn provider(&self) -> Value {
        let mut v = json!({"require_parameters":true,"allow_fallbacks":self.allow_fallbacks});
        if !self.providers.is_empty() {
            v["only"] = json!(self.providers);
            v["order"] = json!(self.providers);
        }
        v
    }
}
