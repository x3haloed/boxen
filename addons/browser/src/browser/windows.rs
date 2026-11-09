use crate::Result;
use std::{
    ffi::{OsStr, OsString},
    io,
    mem::{size_of, size_of_val, zeroed},
    net::Ipv4Addr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, WAIT_OBJECT_0, WAIT_TIMEOUT},
    NetworkManagement::IpHelper::{GetTcpTable2, MIB_TCP_STATE_LISTEN, MIB_TCPROW2, MIB_TCPTABLE2},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_SUSPENDED, CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
            TerminateProcess, WaitForSingleObject,
        },
    },
};

pub(super) struct Process {
    job: OwnedHandle,
    process: OwnedHandle,
    pid: u32,
}

impl Process {
    pub(super) fn spawn(executable: &Path, args: &[OsString]) -> Result<Self> {
        let mut program: Vec<_> = executable.as_os_str().encode_wide().collect();
        if program.contains(&0) {
            return Err("browser executable contains a NUL".into());
        }
        program.push(0);
        let mut line = Vec::new();
        append_arg(&mut line, executable.as_os_str())?;
        for arg in args {
            line.push(b' ' as u16);
            append_arg(&mut line, arg)?;
        }
        line.push(0);
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(io::Error::last_os_error().into());
            }
            let job = OwnedHandle::from_raw_handle(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                ..zeroed()
            };
            let mut info: PROCESS_INFORMATION = zeroed();
            if CreateProcessW(
                program.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_SUSPENDED,
                null(),
                null(),
                &startup,
                &mut info,
            ) == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            let thread = OwnedHandle::from_raw_handle(info.hThread);
            let process = OwnedHandle::from_raw_handle(info.hProcess);
            // Starting suspended prevents Chromium from spawning children before job ownership.
            if AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) == 0 {
                let error = io::Error::last_os_error();
                TerminateProcess(process.as_raw_handle(), 1);
                return Err(error.into());
            }
            if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                return Err(io::Error::last_os_error().into());
            }
            Ok(Self {
                job,
                process,
                pid: info.dwProcessId,
            })
        }
    }

    pub(super) fn owns_listener(&self, port: u16) -> Result<bool> {
        let mut size = 0;
        // The table may grow between the sizing and fill calls.
        for _ in 0..4 {
            let mut buffer = vec![0u32; (size as usize).div_ceil(size_of::<u32>())];
            let table = if buffer.is_empty() {
                null_mut()
            } else {
                buffer.as_mut_ptr().cast::<MIB_TCPTABLE2>()
            };
            match unsafe { GetTcpTable2(table, &mut size, 0) } {
                ERROR_INSUFFICIENT_BUFFER => continue,
                0 if !table.is_null() => {
                    let rows = size as usize;
                    let count = buffer[0] as usize;
                    let offset = std::mem::offset_of!(MIB_TCPTABLE2, table);
                    if rows > size_of_val(buffer.as_slice())
                        || rows < offset
                        || count > (rows - offset) / size_of::<MIB_TCPROW2>()
                    {
                        return Err("Windows returned a malformed TCP listener table".into());
                    }
                    let entries = unsafe {
                        std::slice::from_raw_parts(
                            buffer
                                .as_ptr()
                                .cast::<u8>()
                                .add(offset)
                                .cast::<MIB_TCPROW2>(),
                            count,
                        )
                    };
                    return Ok(entries.iter().any(|row| {
                        row.dwOwningPid == self.pid
                            && row.dwState == MIB_TCP_STATE_LISTEN as u32
                            && row.dwLocalAddr.to_ne_bytes() == Ipv4Addr::LOCALHOST.octets()
                            && u16::from_be(row.dwLocalPort as u16) == port
                    }));
                }
                0 => return Ok(false),
                code => return Err(io::Error::from_raw_os_error(code as i32).into()),
            }
        }
        Ok(false)
    }

    pub(super) fn exited(&self) -> Result<bool> {
        self.wait(Duration::ZERO)
    }

    pub(super) fn wait_for_tree(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            if unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of_val(&info) as u32,
                    null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            if info.ActiveProcesses == 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn wait(&self, timeout: Duration) -> Result<bool> {
        match unsafe {
            WaitForSingleObject(
                self.process.as_raw_handle(),
                timeout.as_millis().min((u32::MAX - 1) as u128) as u32,
            )
        } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error().into()),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
        let _ = self.wait_for_tree(Duration::from_secs(2));
    }
}

fn append_arg(line: &mut Vec<u16>, arg: &OsStr) -> Result<()> {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    line.push(QUOTE);
    let mut slashes = 0;
    for unit in arg.encode_wide() {
        match unit {
            0 => return Err("browser argument contains a NUL".into()),
            SLASH => slashes += 1,
            _ => {
                let count = if unit == QUOTE {
                    slashes * 2 + 1
                } else {
                    slashes
                };
                line.extend(std::iter::repeat_n(SLASH, count));
                line.push(unit);
                slashes = 0;
            }
        }
    }
    line.extend(std::iter::repeat_n(SLASH, slashes * 2));
    line.push(QUOTE);
    Ok(())
}
