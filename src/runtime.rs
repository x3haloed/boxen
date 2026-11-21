//! Capabilities that Boxen can resolve at admission without starting inference.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Codex,
    OpenRouter,
    Antigravity,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    LiveSteering,
    QueuedSteering,
    InputResponses,
    NativeSessionResume,
    LateActivity,
}

impl Capability {
    pub const ALL: [Self; 5] = [
        Self::LiveSteering,
        Self::QueuedSteering,
        Self::InputResponses,
        Self::NativeSessionResume,
        Self::LateActivity,
    ];
}

pub type Requirements = BTreeSet<Capability>;

impl Backend {
    pub const ALL: [Self; 3] = [Self::Codex, Self::OpenRouter, Self::Antigravity];

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenRouter => "openrouter",
            Self::Antigravity => "antigravity",
        }
    }

    pub fn parse(id: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|backend| backend.id() == id)
            .ok_or_else(|| "unsupported backend".into())
    }

    fn capabilities(self, resident: bool) -> impl Iterator<Item = Capability> {
        use Capability::*;
        let capabilities: &[Capability] = match self {
            Self::Codex => &[LiveSteering, InputResponses, NativeSessionResume],
            Self::OpenRouter => &[QueuedSteering],
            Self::Antigravity => &[
                LiveSteering,
                InputResponses,
                NativeSessionResume,
                LateActivity,
            ],
        };
        capabilities.iter().copied().filter(move |capability| {
            !resident || !matches!(capability, LiveSteering | QueuedSteering)
        })
    }

    pub fn require(self, requested: &Requirements, resident: bool) -> Result<()> {
        for capability in requested {
            if !self
                .capabilities(resident)
                .any(|supported| supported == *capability)
            {
                let name = serde_json::to_value(capability)?;
                let scope = if resident {
                    "resident activation"
                } else {
                    "anonymous request"
                };
                return Err(
                    format!("{} cannot accept a {scope} requiring {name}", self.id()).into(),
                );
            }
        }
        Ok(())
    }

    pub fn describe(self, configured: bool) -> Value {
        let mut info = match self {
            Self::Codex => json!({"kind":"managed","protocol":"app_server","input":["text"],
                "attachment_transport":"retained_file_path", "tools":true, "full_access":true,"resident":true}),
            Self::OpenRouter => {
                json!({"kind":"built_in","default_model":crate::inference::DEFAULT_MODEL,
                "input":["text","image","audio","video","file"],"native_read":true,
                "context_policies":["summary","verbatim"],"default_ephemeral_context":"summary",
                "default_resident_context":"verbatim","history_tool":true,"model_provider_support_required":true,
                "tools":true,"full_access":true,"resident":true})
            }
            Self::Antigravity => json!({"kind":"managed","protocol":"acp","input":["text"],
                "attachment_transport":"retained_file_path","tools":true,"full_access":true,"resident":true,
                "model_selection":"native_session","controls":["cancel","steer_during_prompt","respond"]}),
        };
        info["id"] = json!(self.id());
        info["configured"] = json!(configured);
        info["request_capabilities"] = json!(self.capabilities(false).collect::<Vec<_>>());
        info["activation_capabilities"] = json!(self.capabilities(true).collect::<Vec<_>>());
        info
    }
}

pub fn requirements(value: Option<&Value>) -> Result<Requirements> {
    value
        .map(|value| serde_json::from_value(value.clone()).map_err(Into::into))
        .unwrap_or_else(|| Ok(Requirements::new()))
}
