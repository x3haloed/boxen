#[cfg(not(windows))]
use anyhow::Context;
use anyhow::Result;
use std::process::{Child, Command};

/// Job ownership kills the complete app-server process tree if its hub dies.
pub struct ManagedChild {
    pub child: Child,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}
impl ManagedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
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
                    anyhow::bail!("Creating process cleanup job failed");
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as _,
                    std::mem::size_of_val(&limits) as u32,
                ) == FALSE
                    || AssignProcessToJobObject(job, child.as_raw_handle() as _) == FALSE
                {
                    CloseHandle(job);
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("Attaching process cleanup job failed");
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
                child: command.spawn().context("Starting child")?,
            })
        }
    }
}
impl Drop for ManagedChild {
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
