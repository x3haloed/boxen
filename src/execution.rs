//! Host execution account selection. Accepted runs pin the selection while the
//! existing run journal owns observed session availability and launch outcomes.
use crate::{Result, jobs::Run, platform};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    mode: Mode,
    account: Option<String>,
}

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Auto,
    Inherit,
    UserSession,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Execution {
    pub platform: String,
    pub mode: Mode,
    pub account: String,
    pub uid: Option<u32>,
    pub elevated: Option<bool>,
    pub home: String,
    pub session: String,
    pub desktop_available: Option<bool>,
}

fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(format!(
            "execution context probe failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )
        .into());
    }
    if result.stdout.len() > 4096 {
        return Err("execution context probe exceeded 4 KiB".into());
    }
    Ok(String::from_utf8(result.stdout)?.trim().into())
}

impl Execution {
    pub fn host_info(home: &Path) -> Value {
        json!({"daemon":Self::inherited().map(|context| json!(context)).unwrap_or_else(|error| json!({"error":error.to_string()})),
            "workload":Self::capture(home).map(|context| json!(context)).unwrap_or_else(|error| json!({"error":error.to_string()}))})
    }
    pub fn capture(home: &Path) -> Result<Self> {
        let policy = match fs::File::open(home.join("execution.json")) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(4097).read_to_end(&mut bytes)?;
                if bytes.len() > 4096 {
                    return Err("execution policy exceeds 4 KiB".into());
                }
                serde_json::from_slice::<Policy>(&bytes)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Policy::default(),
            Err(error) => return Err(error.into()),
        };
        let mut execution = Self::inherited()?;
        if execution.platform != "macos" {
            if policy.mode == Mode::UserSession || policy.account.is_some() {
                return Err("user_session account selection currently requires macOS".into());
            }
            return Ok(execution);
        }
        if policy.mode == Mode::Inherit {
            if policy.account.is_some() {
                return Err(
                    "inherit execution uses the daemon account and rejects an account override"
                        .into(),
                );
            }
            return Ok(execution);
        }
        if policy.mode == Mode::Auto
            && policy.account.is_none()
            && execution.uid != Some(0)
            && execution.session != "Aqua"
        {
            return Ok(execution);
        }
        #[cfg(unix)]
        let owner = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(home)?.uid()
        };
        #[cfg(not(unix))]
        let owner = 0;
        let account = match policy.account {
            Some(account) => account,
            None if execution.uid == Some(0) && owner != 0 => {
                output(Command::new("/usr/bin/id").args(["-un", &owner.to_string()]))?
            }
            None => execution.account.clone(),
        };
        let uid: u32 = output(Command::new("/usr/bin/id").args(["-u", &account]))?.parse()?;
        if uid == 0 && policy.mode == Mode::Auto {
            return Err("automatic macOS workload execution requires a non-root data-directory owner or a configured account in execution.json; mode inherit explicitly selects the daemon context".into());
        }
        if uid == 0 {
            return Err("user_session execution requires a non-root account".into());
        }
        if owner != 0 && owner != uid {
            return Err("the configured execution account differs from this host's data-directory owner; host enrollment pins the workload account".into());
        }
        if execution.uid != Some(0) && execution.uid != Some(uid) {
            return Err("the daemon can launch its own account; selecting another account requires a privileged launcher".into());
        }
        if execution.uid != Some(0) && execution.session != "Aqua" {
            return Err("macOS user-session execution requires the daemon in that login session or a privileged launcher".into());
        }
        execution.mode = Mode::UserSession;
        execution.account = account;
        execution.uid = Some(uid);
        execution.elevated = Some(false);
        execution.home = output(Command::new("/usr/bin/dscl").args([
            ".",
            "-read",
            &format!("/Users/{}", execution.account),
            "NFSHomeDirectory",
        ]))?
        .strip_prefix("NFSHomeDirectory:")
        .ok_or("account home directory is unavailable")?
        .trim()
        .into();
        execution.session = "macOS login session".into();
        execution.desktop_available = Some(execution.available());
        Ok(execution)
    }

    fn inherited() -> Result<Self> {
        let platform = platform::host_os().to_string();
        let (account, uid, elevated, session) = if platform == "macos" || platform == "linux" {
            let uid: u32 = output(Command::new("/usr/bin/id").arg("-u"))?.parse()?;
            let account = output(Command::new("/usr/bin/id").arg("-un"))?;
            let session = if platform == "macos" {
                output(Command::new("/bin/launchctl").arg("managername"))?
            } else {
                "inherited service/session".into()
            };
            (account, Some(uid), Some(uid == 0), session)
        } else {
            (
                output(&mut Command::new("whoami"))?,
                None,
                None,
                "inherited service/session".into(),
            )
        };
        let desktop_available = (platform == "macos").then_some(session == "Aqua");
        Ok(Self {
            platform,
            mode: Mode::Inherit,
            account,
            uid,
            elevated,
            home: std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default(),
            session,
            desktop_available,
        })
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        if self.mode != Mode::UserSession || self.platform != "macos" {
            return Command::new(program);
        }
        #[cfg(unix)]
        let root = unsafe { libc::geteuid() } == 0;
        #[cfg(not(unix))]
        let root = false;
        let mut command;
        if root {
            command = Command::new("/bin/launchctl");
            command.args([
                "asuser",
                &self.uid.expect("validated account").to_string(),
                "/usr/bin/sudo",
                "-n",
                "-H",
                "-E",
                "-u",
                &self.account,
                "--",
            ]);
        } else {
            command = Command::new("/usr/bin/env");
        }
        // sudo preserves workload-scoped bridge capabilities and explicit runtime
        // configuration. Account identity variables describe the selected user.
        if root {
            command.arg("/usr/bin/env");
        }
        command
            .arg(format!("HOME={}", self.home))
            .arg(format!("USER={}", self.account))
            .arg(format!("LOGNAME={}", self.account))
            .arg(program);
        command
    }

    fn available(&self) -> bool {
        self.mode != Mode::UserSession
            || Command::new("/bin/launchctl")
                .args([
                    "print",
                    &format!("gui/{}", self.uid.expect("validated account")),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    }

    pub fn prepare(&self, run: &Run) -> Result<bool> {
        if !self.available() {
            let detail = format!("Waiting for {}'s macOS login session", self.account);
            run.status("waiting", &detail)?;
            run.diagnostic(
                crate::logging::Level::Info,
                "execution.session_waiting",
                || json!({"account":self.account,"uid":self.uid}),
                || Value::Null,
            );
            while !self.available() {
                if run.cancellation_requested() {
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        let mut observed = self.clone();
        if self.platform == "macos" {
            let uid: u32 = output(self.command("/usr/bin/id").arg("-u"))?.parse()?;
            if Some(uid) != self.uid {
                return Err("workload launcher returned a different execution account".into());
            }
            observed.session = output(self.command("/bin/launchctl").arg("managername"))?;
            observed.desktop_available = Some(observed.session == "Aqua");
            if self.mode == Mode::UserSession && observed.desktop_available != Some(true) {
                return Err(
                    "the configured macOS login session became unavailable before workload launch"
                        .into(),
                );
            }
        }
        let mut event = serde_json::to_value(&observed)?;
        event["cwd"] = json!(run.snapshot().record.spec.cwd);
        run.append("execution_context", event)?;
        run.diagnostic(crate::logging::Level::Info, "execution.context_verified", || json!({"account":observed.account,"uid":observed.uid,"mode":observed.mode,"session":observed.session}), || Value::Null);
        Ok(true)
    }

    /// Only daemon-managed workspace/attachment paths reach this function.
    /// Existing caller-selected directories retain their existing ownership.
    pub fn own_managed_path(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        if self.mode == Mode::UserSession && unsafe { libc::geteuid() } == 0 {
            use std::{os::fd::AsRawFd, os::unix::fs::OpenOptionsExt};
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            if unsafe {
                libc::fchown(
                    file.as_raw_fd(),
                    self.uid.expect("validated account"),
                    u32::MAX,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let _ = path;
        Ok(())
    }

    pub fn own_run_history(&self, directory: &Path) -> Result<()> {
        if self.mode != Mode::UserSession {
            return Ok(());
        }
        self.own_managed_path(directory)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                self.own_managed_path(&entry.path())?;
            }
        }
        Ok(())
    }

    pub fn own_native_session(&self, path: &Path, id: &str) -> Result<()> {
        if self.mode != Mode::UserSession {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if unsafe { libc::geteuid() } != 0 {
                return Ok(());
            }
            let root = std::env::var_os("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| Path::new(&self.home).join(".codex"))
                .canonicalize()?;
            let path = match path.canonicalize() {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let stored_session = path.starts_with(root.join("sessions"))
                || path.starts_with(root.join("archived_sessions"));
            if !stored_session
                || !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(&format!("-{id}.jsonl")))
            {
                return Err("recorded native session path differs from the selected account's Codex session storage".into());
            }
            let metadata = fs::metadata(&path)?;
            if metadata.uid() == 0 {
                self.own_managed_path(&path)?;
            } else if Some(metadata.uid()) != self.uid {
                return Err(
                    "recorded native session belongs to another operating-system account".into(),
                );
            }
        }
        let _ = (path, id);
        Ok(())
    }
}
