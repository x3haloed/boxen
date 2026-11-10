//! File contract between a local assignment authority, Boxen, and the browser addon.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DIRECTORY: &str = "browser-assignments";
pub const MAX_BYTES: u64 = 32 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Chromium,
    Safari,
}

impl Backend {
    fn is_chromium(&self) -> bool {
        *self == Self::Chromium
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub executable: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_browser: Option<PathBuf>,
    pub profile: PathBuf,
    pub evidence: PathBuf,
    #[serde(default)]
    pub headless: bool,
    #[serde(default, skip_serializing_if = "Backend::is_chromium")]
    pub backend: Backend,
}

impl Settings {
    pub fn valid_configuration(&self) -> bool {
        [&self.executable, &self.profile, &self.evidence]
            .into_iter()
            .all(|path| path.is_absolute())
            && match self.backend {
                Backend::Chromium => self
                    .agent_browser
                    .as_ref()
                    .is_some_and(|path| path.is_absolute()),
                Backend::Safari => self.agent_browser.is_none() && !self.headless,
            }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub message: String,
    pub resident: String,
    pub sender_machine: String,
    pub sender_id: String,
    pub job: String,
    pub version: u64,
    pub server: PathBuf,
    pub settings: Settings,
}

pub fn grant_path(home: &Path, message: &str) -> PathBuf {
    home.join(DIRECTORY).join(format!("{message}.json"))
}
