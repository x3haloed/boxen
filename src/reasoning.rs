//! Requested reasoning effort. The runtimes remain the authorities on model support.
use crate::Result;

pub fn validate(backend: &str, effort: &str) -> Result<()> {
    if effort == "default" {
        return Ok(());
    }
    match backend {
        "codex"
            if !effort.is_empty()
                && effort.len() <= 64
                && effort.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') =>
        {
            Ok(())
        }
        "openrouter" if matches!(effort, "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") => Ok(()),
        "codex" => Err("Codex reasoning effort requires a model-advertised name of 1–64 lowercase letters, digits, underscores or hyphens, or default".into()),
        "openrouter" => Err("OpenRouter reasoning effort accepts default, none, minimal, low, medium, high, xhigh or max; model and provider support vary".into()),
        _ => Err("reasoning effort requires a supported backend".into()),
    }
}

pub fn requested(effort: Option<&str>) -> Option<&str> {
    effort.filter(|value| *value != "default")
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub expected_version: String,
    pub reasoning_effort: String,
}
