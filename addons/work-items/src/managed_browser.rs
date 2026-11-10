use crate::{
    Config,
    browser_assignment::{self, Backend, Grant, Settings},
    id, key,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{ErrorKind, Write},
    path::PathBuf,
};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Engine {
    #[default]
    Chrome,
    Edge,
    Safari,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    executable: PathBuf,
    profile: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    server: PathBuf,
    agent_browser: Option<PathBuf>,
    #[serde(default)]
    default: Engine,
    #[serde(default)]
    headless: bool,
    #[serde(default)]
    items: Vec<i64>,
    chrome: Option<Slot>,
    edge: Option<Slot>,
    safari: Option<Slot>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    engine: Engine,
    server: PathBuf,
    settings: Settings,
}

pub fn configure(assignment: &mut Value, project: &Value) -> Result<bool> {
    let Some(config) = project
        .get("managed_browser")
        .filter(|value| !value.is_null())
    else {
        return Ok(false);
    };
    let policy: Policy = serde_json::from_value(config.clone())?;
    ensure!(
        policy.items.iter().all(|item| *item > 0),
        "Managed browser pilot item IDs must be positive"
    );
    if !policy.items.is_empty()
        && !assignment["work_item"]["id"]
            .as_i64()
            .is_some_and(|id| policy.items.contains(&id))
    {
        return Ok(false);
    }
    let slot = match policy.default {
        Engine::Chrome => policy
            .chrome
            .context("The configured Chrome slot is missing")?,
        Engine::Edge => policy.edge.context("The configured Edge slot is missing")?,
        Engine::Safari => policy
            .safari
            .context("The configured Safari slot is missing")?,
    };
    let backend = match policy.default {
        Engine::Chrome | Engine::Edge => Backend::Chromium,
        Engine::Safari => Backend::Safari,
    };
    let settings = Settings {
        executable: slot.executable,
        agent_browser: policy
            .agent_browser
            .filter(|_| backend == Backend::Chromium),
        backend,
        profile: slot.profile,
        evidence: PathBuf::from(
            assignment["evidence_dir"]
                .as_str()
                .context("Assignment evidence directory missing")?,
        ),
        headless: policy.headless,
    };
    let frozen = Frozen {
        engine: policy.default,
        server: policy.server,
        settings,
    };
    validate(&frozen)?;
    assignment["runtime"]["browser_context"] = json!(frozen.settings.profile);
    assignment["runtime"]["managed_browser"] = serde_json::to_value(frozen)?;
    assignment["task"]["browser_workflow"] = json!(match backend {
        Backend::Safari =>
            "The assignment activation exposes Safari tools through boxen_browser MCP. Navigation, page inspection, interactions, console and network inspection run in an isolated Safari automation window. Screenshot PNGs are retained in the assigned evidence directory. Safari automation sessions start without saved sign-in state.",
        Backend::Chromium =>
            "The assignment activation exposes boxen_browser MCP. Its browser_action tool opens the local app for the allocated frontend port and synthetic tenant, inspects the page, operates controls, and retains PNG evidence in the assigned directory. Browser_cdp and browser_events expose native DevTools commands and asynchronous events. The browser starts on first use and Boxen owns its process. The assigned profile persists after the activation. A missing tool or browser sign-in is a runtime blocker rather than observed application acceptance.",
    });
    if backend == Backend::Safari {
        assignment["task"]["cleanup"] = json!(
            "Owned application processes, the allocated synthetic database and temporary source configuration are cleaned after verified local work. The isolated Safari automation session ends with the activation. A candidate report distinguishes source checks, real observations, review-pending state and remaining required implementation work."
        );
    }
    Ok(true)
}

fn validate(frozen: &Frozen) -> Result<()> {
    ensure!(
        frozen.server.is_absolute()
            && frozen.settings.valid_configuration()
            && frozen.server.is_file()
            && frozen.settings.executable.is_file()
            && frozen
                .settings
                .agent_browser
                .as_ref()
                .is_none_or(|path| path.is_file())
            && (frozen.settings.backend != Backend::Safari || cfg!(target_os = "macos")),
        "Managed browser requires absolute paths and installed executables"
    );
    Ok(())
}

pub fn publish(
    cfg: &Config,
    assignment: &Value,
    message: &Value,
    job: &str,
    version: i64,
) -> Result<()> {
    let Some(frozen) = frozen_assignment(assignment)? else {
        return Ok(());
    };
    validate(&frozen)?;
    let message_id = message["id"]
        .as_str()
        .context("Assignment message ID missing")?;
    let resident = message["to"]["id"]
        .as_str()
        .context("Assignment recipient missing")?;
    key(message_id)?;
    key(resident)?;
    key(job)?;
    let grant = Grant {
        message: message_id.into(),
        resident: resident.into(),
        job: job.into(),
        sender_machine: cfg.owner.clone(),
        sender_id: cfg.client_id.clone(),
        version: u64::try_from(version)?,
        server: frozen.server,
        settings: frozen.settings,
    };
    let bytes = serde_json::to_vec(&grant)?;
    ensure!(
        grant.version > 0 && bytes.len() as u64 <= browser_assignment::MAX_BYTES,
        "Managed browser assignment has an invalid version or size"
    );
    let path = browser_assignment::grant_path(&cfg.boxen_home, message_id);
    if path.exists() {
        ensure!(
            fs::read(path)? == bytes,
            "Browser grant differs from the frozen assignment"
        );
        return Ok(());
    }
    fs::create_dir_all(path.parent().unwrap())?;
    let temp = path.with_extension(format!("{}.tmp", id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temp, &path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                ensure!(
                    fs::read(&path)? == bytes,
                    "Concurrent browser grant differs from the assignment"
                );
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    })();
    let cleanup = fs::remove_file(temp);
    result?;
    cleanup?;
    Ok(())
}

pub fn idle_profile(assignment: &Value) -> Result<Option<fs::File>> {
    let Some(frozen) = frozen_assignment(assignment)? else {
        return Ok(None);
    };
    let file = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(frozen.settings.profile.join(".boxen-browser.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    fs2::FileExt::try_lock_exclusive(&file)
        .context("The assigned browser is still shutting down")?;
    Ok(Some(file))
}

pub fn validate_assignment(assignment: &Value) -> Result<()> {
    if let Some(frozen) = frozen_assignment(assignment)? {
        validate(&frozen)?;
    }
    Ok(())
}

fn frozen_assignment(assignment: &Value) -> Result<Option<Frozen>> {
    let Some(raw) = assignment["runtime"]
        .get("managed_browser")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let frozen: Frozen = serde_json::from_value(raw.clone())?;
    ensure!(
        assignment["runtime"]["required"] == true
            && frozen.settings.profile.is_absolute()
            && assignment["runtime"]["browser_context"] == json!(frozen.settings.profile)
            && assignment["evidence_dir"] == json!(frozen.settings.evidence),
        "Managed browser settings differ from the frozen profile or evidence allocation"
    );
    Ok(Some(frozen))
}
