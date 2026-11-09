//! File contract between a local assignment authority, Boxen, and the browser addon.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DIRECTORY: &str = "browser-assignments";
pub const MAX_BYTES: u64 = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub executable: PathBuf,
    pub agent_browser: PathBuf,
    pub profile: PathBuf,
    pub evidence: PathBuf,
    #[serde(default)]
    pub headless: bool,
}

impl Settings {
    pub fn absolute_paths(&self) -> bool {
        [
            &self.executable,
            &self.agent_browser,
            &self.profile,
            &self.evidence,
        ]
        .into_iter()
        .all(|path| path.is_absolute())
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
