mod automation;
#[path = "../../../shared/browser_assignment.rs"]
mod browser_assignment;
#[cfg(target_os = "macos")]
mod chrome_owner;
mod mcp;
#[cfg(target_os = "macos")]
mod safari;

use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
    process::ExitCode,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("boxen-browser: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next();
    #[cfg(target_os = "macos")]
    if mode.as_deref() == Some(std::ffi::OsStr::new("chrome-owner")) {
        let executable = PathBuf::from(args.next().ok_or("Chrome executable is missing")?);
        let profile = PathBuf::from(args.next().ok_or("Chrome profile is missing")?);
        let headless = match args.next().as_deref().and_then(|value| value.to_str()) {
            Some("headless") => true,
            Some("headed") => false,
            _ => return Err("Chrome owner requires headed or headless mode".into()),
        };
        return chrome_owner::run(&executable, &profile, headless);
    }
    let config = PathBuf::from(args.next().ok_or("browser MCP config path is missing")?);
    if !config.is_absolute() || args.next().is_some() {
        return Err("browser MCP requires one absolute config path".into());
    }
    let mut bytes = Vec::new();
    File::open(&config)?
        .take(browser_assignment::MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > browser_assignment::MAX_BYTES {
        return Err("browser MCP config exceeds its size bound".into());
    }
    let settings: browser_assignment::Settings = match mode
        .as_deref()
        .and_then(|value| value.to_str())
    {
        Some("mcp") => serde_json::from_slice(&bytes)?,
        Some("mcp-assignment") => {
            let grant: browser_assignment::Grant = serde_json::from_slice(&bytes)?;
            if grant.message.len() != 64
                || !grant.message.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("browser assignment message ID is invalid".into());
            }
            let home = config
                .parent()
                .and_then(|parent| parent.parent())
                .ok_or("browser assignment config has no home")?;
            if config != browser_assignment::grant_path(home, &grant.message) {
                return Err("browser assignment config is outside the assignment registry".into());
            }
            grant.settings
        }
        _ => return Err("usage: boxen-browser mcp ABSOLUTE_CONFIG_PATH".into()),
    };
    if !settings.valid_configuration() || !settings.executable.is_file() {
        return Err("browser MCP requires absolute paths and an installed executable; Safari requires headed mode and no agent-browser helper".into());
    }
    let input = io::stdin().lock();
    let output = io::stdout().lock();
    match settings.backend {
        browser_assignment::Backend::Chromium => mcp::serve(settings, input, output),
        #[cfg(target_os = "macos")]
        browser_assignment::Backend::Safari => safari::serve(settings, input, output),
        #[cfg(not(target_os = "macos"))]
        browser_assignment::Backend::Safari => Err("Safari MCP requires macOS".into()),
    }
}
