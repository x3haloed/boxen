//! Ownership applies only to a queue subprocess started by this adapter.
use anyhow::{Result, bail};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct OwnedChild {
    child: Child,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}
impl OwnedChild {
    fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::{io::AsRawHandle, process::CommandExt};
            use windows_sys::Win32::{
                Foundation::{CloseHandle, FALSE},
                System::JobObjects::*,
            };
            command.creation_flags(0x08000000);
            let mut child = command.spawn()?;
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("queue process ownership unavailable");
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as _,
                    std::mem::size_of_val(&info) as u32,
                ) == FALSE
                    || AssignProcessToJobObject(job, child.as_raw_handle() as _) == FALSE
                {
                    CloseHandle(job);
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("queue process ownership unavailable");
                }
                Ok(Self { child, job })
            }
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            Ok(Self {
                child: command.spawn()?,
            })
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}
pub fn submit(executable: &Path, thread: &str, notice: &str, timeout: Duration) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .args(["queue", "--thread", thread, "--message", notice])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut process = OwnedChild::spawn(&mut command)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = process.child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("Codex queue returned a failure; notice acceptance is uncertain");
        }
        if Instant::now() >= deadline {
            bail!("Codex queue timed out; notice acceptance is uncertain");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
