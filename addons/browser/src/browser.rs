//! Chromium launch contract shared by the platform process implementations.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos::Process;
#[cfg(windows)]
use windows::Process;

use crate::{Result, cdp::Connection};
#[cfg(any(windows, target_os = "macos"))]
use serde_json::json;
#[cfg(any(windows, target_os = "macos"))]
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    thread,
    time::{Duration, Instant},
};
use std::{fs::File, path::Path};

#[cfg(any(windows, target_os = "macos"))]
const START_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(any(windows, target_os = "macos"))]
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Browser {
    // The process must stop before the profile lock is released.
    #[cfg(any(windows, target_os = "macos"))]
    process: Process,
    _profile_lock: File,
    endpoint: Endpoint,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Endpoint {
    port: u16,
    path: String,
}

impl Endpoint {
    pub fn connect(&self) -> Result<Connection> {
        Connection::connect(self.port, &self.path)
    }

    pub fn websocket_url(&self) -> String {
        format!("ws://127.0.0.1:{}{}", self.port, self.path)
    }
}

impl Browser {
    /// Launches a browser using an absolute executable and a dedicated profile.
    /// The endpoint is published only after the platform proves listener ownership.
    pub fn launch(executable: &Path, profile: &Path, headless: bool) -> Result<Self> {
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (executable, profile, headless);
            Err("managed browser launch is not implemented on this platform".into())
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            if !executable.is_absolute() || !profile.is_absolute() || !executable.is_file() {
                return Err("browser launch requires an existing absolute executable and an absolute profile directory".into());
            }
            fs::create_dir_all(profile)?;
            // Lock the actual profile path while passing Chromium a normal DOS path
            // where possible. Its profile storage can reject verbatim Windows paths.
            #[cfg(windows)]
            let profile = dunce::canonicalize(profile)?;
            #[cfg(target_os = "macos")]
            let profile = fs::canonicalize(profile)?;
            let profile_lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(profile.join(".boxen-browser.lock"))?;
            fs2::FileExt::try_lock_exclusive(&profile_lock).map_err(|error| {
                format!("browser profile reservation could not be acquired: {error}")
            })?;
            #[allow(unused_mut)]
            let mut process = Process::spawn(executable, &arguments(&profile, headless))?;
            let deadline = Instant::now() + START_TIMEOUT;
            loop {
                if process.exited()? {
                    return Err("browser exited before its owned debugger endpoint became ready; the profile may already be in use".into());
                }
                if let Some(endpoint) = read_endpoint(&profile)
                    && process.owns_listener(endpoint.port)?
                {
                    endpoint.connect()?;
                    return Ok(Self {
                        process,
                        _profile_lock: profile_lock,
                        endpoint,
                    });
                }
                if Instant::now() >= deadline {
                    return Err("browser did not expose an owned loopback debugger endpoint before the startup deadline".into());
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Closes through Chromium before releasing the profile so storage can flush.
    /// The platform process guard still terminates the process tree on failure.
    pub fn close(#[allow(unused_mut)] mut self) -> Result<()> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            if self.process.exited()? {
                return Err("browser exited before graceful profile shutdown was requested".into());
            }
            // Chromium can close the connection before replying to Browser.close.
            let response =
                self.endpoint
                    .connect()?
                    .command("Browser.close", json!({}), None, STOP_TIMEOUT);
            if self.process.wait_for_tree(STOP_TIMEOUT)? {
                return Ok(());
            }
            response?;
            Err("browser did not exit after Browser.close; the process guard attempted forced shutdown".into())
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        Err("managed browser shutdown is not implemented on this platform".into())
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn arguments(profile: &Path, headless: bool) -> Vec<OsString> {
    let mut profile_arg = OsString::from("--user-data-dir=");
    profile_arg.push(profile);
    let mut args = vec![
        profile_arg,
        "--remote-debugging-address=127.0.0.1".into(),
        "--remote-debugging-port=0".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-mode".into(),
    ];
    #[cfg(target_os = "macos")]
    args.push("--disable-breakpad".into());
    if headless {
        args.push("--headless=new".into());
    }
    args.push("about:blank".into());
    args
}

#[cfg(any(windows, target_os = "macos"))]
fn read_endpoint(profile: &Path) -> Option<Endpoint> {
    use std::io::Read;
    let mut input = String::new();
    File::open(profile.join("DevToolsActivePort"))
        .ok()?
        .take(512)
        .read_to_string(&mut input)
        .ok()?;
    let mut lines = input.lines();
    let port = lines.next()?.parse().ok()?;
    let path = lines.next()?.to_owned();
    Some(Endpoint { port, path })
}
