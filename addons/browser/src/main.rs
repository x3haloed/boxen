mod automation;
mod mcp;

use std::{fs::File, io, path::PathBuf, process::ExitCode};

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
    if args.next().as_deref() != Some("mcp".as_ref()) {
        return Err("usage: boxen-browser mcp ABSOLUTE_CONFIG_PATH".into());
    }
    let config = PathBuf::from(args.next().ok_or("browser MCP config path is missing")?);
    if !config.is_absolute() || args.next().is_some() {
        return Err("browser MCP requires one absolute config path".into());
    }
    let settings = serde_json::from_reader(File::open(config)?)?;
    mcp::serve(settings, io::stdin().lock(), io::stdout().lock())
}
