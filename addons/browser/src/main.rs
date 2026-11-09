mod automation;
#[path = "../../../shared/browser_assignment.rs"]
mod browser_assignment;
mod mcp;

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
    let settings = match mode.as_deref().and_then(|value| value.to_str()) {
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
    mcp::serve(settings, io::stdin().lock(), io::stdout().lock())
}
