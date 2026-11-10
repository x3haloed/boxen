//! The native owner runs Chromium in a dedicated process group.

use crate::Result;
use std::{
    ffi::OsString,
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Process {
    child: Child,
    stopped: bool,
}

impl Process {
    pub(super) fn spawn(executable: &Path, args: &[OsString]) -> Result<Self> {
        let child = Command::new(executable)
            .args(args)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            child,
            stopped: false,
        })
    }

    pub(super) fn owns_listener(&self, port: u16) -> Result<bool> {
        let listener = format!("127.0.0.1:{port}");
        let output = Command::new("/usr/sbin/lsof")
            .args(["-nP", "-a", "-p"])
            .arg(self.child.id().to_string())
            .arg(format!("-iTCP@{listener}"))
            .args(["-sTCP:LISTEN", "-Fn"])
            .output()?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err("macOS could not inspect the browser's debugger listener".into());
        }
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.strip_prefix('n') == Some(&listener)))
    }

    pub(super) fn exited(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    pub(super) fn wait_for_tree(&mut self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            self.child.try_wait()?;
            if !self.signal_group(0)? {
                self.stopped = true;
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn signal_group(&self, signal: i32) -> Result<bool> {
        // The child starts in a new group with its own PID as the group ID.
        if unsafe { libc::kill(-(self.child.id() as i32), signal) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            // Darwin can report EPERM while the group exists but only has
            // exited helpers awaiting reaping. Wait for the group to disappear.
            Some(libc::EPERM) if signal == 0 => Ok(true),
            _ => Err(error.into()),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.signal_group(libc::SIGKILL);
            let _ = self.wait_for_tree(Duration::from_secs(5));
        }
    }
}
