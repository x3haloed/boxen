//! A bounded operator observation of the Git worktree recorded for a run.
use crate::{Result, jobs::Run, logging::Level};
use serde_json::{Value, json};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

const MAX_OUTPUT: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);
static PROBE: Mutex<()> = Mutex::new(());

pub fn observe(run: &Run) -> Result<Value> {
    let result = inspect(run);
    run.diagnostic(
        if result.is_ok() { Level::Debug } else { Level::Warn },
        "repo.probe",
        || json!({"accepted":result.is_ok(),"detail":result.as_ref().err().map(ToString::to_string)}),
        || Value::Null,
    );
    result
}

fn inspect(run: &Run) -> Result<Value> {
    let _probe = PROBE
        .try_lock()
        .map_err(|_| "A repository probe is already running")?;
    let record = run.snapshot().record;
    let execution = record
        .execution
        .ok_or("The recorded execution account is unavailable")?;
    let mut git = execution.command("git");
    git.current_dir(&record.spec.cwd).args([
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "status",
        "--porcelain=v2",
        "--branch",
        "--no-renames",
        "--untracked-files=normal",
        "--ignore-submodules=all",
    ]);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        git.env_remove(var);
    }
    let output = read_status(&mut git)?;
    let mut facts = parse(&String::from_utf8_lossy(&output))?;
    facts["run_id"] = json!(record.spec.id);
    facts["cwd"] = json!(record.spec.cwd);
    facts["at"] = json!(crate::jobs::now());
    Ok(facts)
}

fn read_status(command: &mut Command) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Git is unavailable in the recorded execution context")?;
    let deadline = Instant::now() + TIMEOUT;
    let stdout = child.stdout.take().expect("piped git output");
    let (sent, received) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sent.send(result);
    });
    let result = (|| -> Result<Vec<u8>> {
        let output = received
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "Repository probe timed out")??;
        if output.len() as u64 > MAX_OUTPUT {
            return Err("Repository status exceeded the observation limit".into());
        }
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(output)
                } else {
                    Err("Git could not read a worktree from the recorded directory".into())
                };
            }
            if Instant::now() >= deadline {
                return Err("Repository probe timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn parse(status: &str) -> Result<Value> {
    let mut branch = None;
    let mut head = None;
    let mut divergence = None;
    let mut dirty = false;
    for row in status.lines() {
        if let Some(value) = row.strip_prefix("# branch.oid ") {
            head = (value.len() == 40 || value.len() == 64)
                .then_some(value)
                .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()));
        } else if let Some(value) = row.strip_prefix("# branch.head ") {
            branch = Some(value);
        } else if let Some(value) = row.strip_prefix("# branch.ab ")
            && let Some((ahead, behind)) = value.split_once(' ')
        {
            divergence = ahead
                .strip_prefix('+')
                .and_then(|n| n.parse::<u64>().ok())
                .zip(behind.strip_prefix('-').and_then(|n| n.parse::<u64>().ok()));
        } else if !row.starts_with('#') && !row.is_empty() {
            dirty = true;
        }
    }
    let branch = branch.ok_or("Git did not return branch metadata")?;
    Ok(json!({
        "branch":branch, "head":head, "dirty":dirty,
        "ahead":divergence.map(|(ahead, _)| ahead),
        "behind":divergence.map(|(_, behind)| behind),
        "scope":"worktree excluding submodules",
    }))
}
