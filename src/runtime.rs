//! Capabilities that Boxen can resolve at admission without starting inference.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};

/// Native work that has started and whose settlement remains outstanding.
#[derive(Default)]
pub(crate) struct OutstandingWork {
    items: HashSet<String>,
}

impl OutstandingWork {
    pub(crate) fn start(&mut self, id: &str) -> Result<()> {
        if id.is_empty() || id.len() > 1024 {
            return Err("The runtime reported an invalid work item ID".into());
        }
        if self.items.len() >= 4096 && !self.items.contains(id) {
            return Err("The runtime exceeded the outstanding work limit".into());
        }
        self.items.insert(id.to_owned());
        Ok(())
    }

    pub(crate) fn finish(&mut self, id: &str) {
        self.items.remove(id);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Codex,
    OpenRouter,
    ChatCompletions,
    Antigravity,
    Claude,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    LiveSteering,
    QueuedSteering,
    InputResponses,
    NativeSessionResume,
    LateActivity,
    ScheduledHandoff,
}

impl Capability {
    pub const ALL: [Self; 6] = [
        Self::LiveSteering,
        Self::QueuedSteering,
        Self::InputResponses,
        Self::NativeSessionResume,
        Self::LateActivity,
        Self::ScheduledHandoff,
    ];
}

pub type Requirements = BTreeSet<Capability>;

impl Backend {
    pub const ALL: [Self; 4] = [
        Self::Codex,
        Self::OpenRouter,
        Self::Antigravity,
        Self::Claude,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenRouter => "openrouter",
            Self::ChatCompletions => "chat-completions",
            Self::Antigravity => "antigravity",
            Self::Claude => "claude",
        }
    }

    pub fn parse(id: &str) -> Result<Self> {
        if let Some(name) = id.strip_prefix("chat-completions:")
            && valid_endpoint_name(name)
        {
            return Ok(Self::ChatCompletions);
        }
        Self::ALL
            .into_iter()
            .find(|backend| backend.id() == id)
            .ok_or_else(|| "unsupported backend".into())
    }

    pub fn is_inference(self) -> bool {
        matches!(self, Self::OpenRouter | Self::ChatCompletions)
    }

    fn capabilities(self, resident: bool) -> impl Iterator<Item = Capability> {
        use Capability::*;
        let capabilities: &[Capability] = match self {
            Self::Codex => &[
                LiveSteering,
                InputResponses,
                NativeSessionResume,
                LateActivity,
            ],
            Self::OpenRouter | Self::ChatCompletions => &[QueuedSteering],
            Self::Claude => &[NativeSessionResume, LateActivity],
            Self::Antigravity => &[
                LiveSteering,
                InputResponses,
                NativeSessionResume,
                LateActivity,
            ],
        };
        capabilities
            .iter()
            .copied()
            .filter(move |capability| {
                !resident || !matches!(capability, LiveSteering | QueuedSteering)
            })
            .chain(resident.then_some(ScheduledHandoff))
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
            Self::OpenRouter | Self::ChatCompletions => {
                json!({"kind":"built_in","default_model":crate::inference::DEFAULT_MODEL,
                "input":["text","image","audio","video","file"],"native_read":true,
                "context_policies":["summary","verbatim"],"default_ephemeral_context":"summary",
                "default_resident_context":"verbatim","history_tool":true,"model_provider_support_required":true,
                "tools":true,"full_access":true,"resident":true})
            }
            Self::Antigravity => json!({"kind":"managed","protocol":"acp","input":["text"],
                "attachment_transport":"retained_file_path","tools":true,"full_access":true,"resident":true,
                "model_selection":"native_session","controls":["cancel","steer_during_prompt","respond"]}),
            Self::Claude => json!({"kind":"managed","protocol":"stream_json","input":["text"],
                "attachment_transport":"retained_file_path","tools":true,"full_access":true,"resident":true,
                "model_selection":"native_session","controls":["cancel"]}),
        };
        info["id"] = json!(self.id());
        info["configured"] = json!(configured);
        info["request_capabilities"] = json!(self.capabilities(false).collect::<Vec<_>>());
        info["activation_capabilities"] = json!(self.capabilities(true).collect::<Vec<_>>());
        info
    }
}

pub fn valid_endpoint_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn is_inference(id: &str) -> bool {
    Backend::parse(id).is_ok_and(Backend::is_inference)
}

pub fn requirements(value: Option<&Value>) -> Result<Requirements> {
    value
        .map(|value| serde_json::from_value(value.clone()).map_err(Into::into))
        .unwrap_or_else(|| Ok(Requirements::new()))
}
