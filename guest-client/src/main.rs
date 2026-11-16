//! Portable native client for a Boxen guest conversation.
mod paint;
mod session;
mod ui;

use boxen_guest_transport::{Connection, Result, host_path};
use session::Session;
use std::{env, path::PathBuf};
fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let mut config = None;
    let mut data = None;
    let mut check = false;
    let mut loopback = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") if config.is_none() => {
                config = Some(PathBuf::from(
                    args.next().ok_or("--config requires a path")?,
                ));
            }
            Some("--data-dir") if data.is_none() => {
                data = Some(PathBuf::from(
                    args.next().ok_or("--data-dir requires a path")?,
                ));
            }
            Some("--check") => check = true,
            Some("--loopback") => loopback = true,
            Some("--help" | "-h") => {
                println!(
                    "boxen-guest-client [--config PATH] [--data-dir PATH] [--check]\nboxen-guest-client --loopback"
                );
                return Ok(());
            }
            _ => return Err("unexpected guest client argument".into()),
        }
    }
    if loopback && (config.is_some() || data.is_some() || check) {
        return Err("--loopback runs without connection settings".into());
    }
    let session = if loopback {
        None
    } else {
        let config = match config {
            Some(path) => path,
            None => PathBuf::from(
                env::var_os("HOME")
                    .or_else(|| env::var_os("USERPROFILE"))
                    .ok_or("the guest client needs a home directory or --config")?,
            )
            .join(".boxen-guest")
            .join("guest-pubsub.json"),
        };
        let config = host_path(&config);
        if check {
            Connection::open(&config)?.check_permissions()?;
            println!("Guest Pub/Sub authentication and permissions verified");
            return Ok(());
        }
        let data = data
            .map(|path| host_path(&path))
            .unwrap_or_else(|| config.with_file_name("history"));
        Some(Session::open(&config, &data)?)
    };
    ui::run(session)
}
