use anyhow::{Context, Result, bail, ensure};
use boxen_codex_desktop::{Config, Store, hash, rpc, run};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn option(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    if let Some(i) = args.iter().position(|a| a == name) {
        ensure!(i + 1 < args.len(), "{name} value required");
        args.remove(i);
        return Ok(Some(args.remove(i)));
    }
    Ok(None)
}
fn home() -> Result<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .context("home directory unavailable")
}
fn codex() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_BINARY") {
        return Ok(path.into());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let p = path.join(if cfg!(windows) { "codex.exe" } else { "codex" });
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    bail!("Codex executable unavailable; --codex ABSOLUTE_PATH selects it explicitly")
}
fn execute() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|v| v == "--help") {
        println!(
            "boxen-support --config ABSOLUTE_PATH init CODEX_THREAD_UUID [--codex PATH] [--boxen-home PATH] [--url http://127.0.0.1:42425] [--label support] [--peer KEY --address HOST:PORT]\nboxen-support --config PATH run [--once]\nboxen-support --config PATH status\nboxen-support --config PATH --rpc OP JSON_ARGS\nboxen-support --config PATH --request-file UTF8_JSON_FILE\n\nScoped operations: snapshot, inspect, reply, send, disposition, reply_status, redeliver.\nTheir CODEX_THREAD_ID matches the configured desktop thread. Local replies work offline.\nThe config contains paths/public routing only; Boxen's capability is read from its private control-token file."
        );
        return Ok(());
    }
    let config =
        PathBuf::from(option(&mut args, "--config")?.context("--config ABSOLUTE_PATH required")?);
    ensure!(config.is_absolute(), "absolute config path required");
    if args.first().is_some_and(|v| v == "init") {
        args.remove(0);
        let executable = option(&mut args, "--codex")?
            .map(PathBuf::from)
            .map(Ok)
            .unwrap_or_else(codex)?;
        let boxen_home = option(&mut args, "--boxen-home")?
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".boxen"));
        let url = option(&mut args, "--url")?.unwrap_or_else(|| "http://127.0.0.1:42425".into());
        let label = option(&mut args, "--label")?.unwrap_or_else(|| "support".into());
        let peer = option(&mut args, "--peer")?;
        let address = option(&mut args, "--address")?.unwrap_or_default();
        ensure!(
            args.len() == 1,
            "init accepts one native desktop thread UUID"
        );
        let thread_id = args.remove(0);
        let cfg = Config {
            version: 1,
            label,
            client_id: hash(&format!("boxen-codex-desktop:{thread_id}")),
            thread_id,
            codex_binary: executable,
            boxen_home,
            url,
            peer,
            address,
        };
        cfg.save(&config)?;
        let store = Store::open(&config, &cfg)?;
        println!(
            "{}",
            json!({"initialized":true,"config":config,"participant":cfg.label,"client_id":cfg.client_id,"status":store.status(&cfg)?})
        );
        return Ok(());
    }
    let cfg = Config::read(&config)?;
    if args.first().is_some_and(|v| v == "run") {
        args.remove(0);
        let once = args == ["--once"];
        ensure!(args.is_empty() || once, "run accepts only --once");
        return run(&cfg, &config, once);
    }
    if args == ["status"] {
        let store = Store::open(&config, &cfg)?;
        println!("{}", serde_json::to_string_pretty(&store.status(&cfg)?)?);
        return Ok(());
    }
    cfg.scope()?;
    let request = if let Some(file) = option(&mut args, "--request-file")? {
        ensure!(
            args.is_empty(),
            "request-file cannot be combined with other operations"
        );
        let path = PathBuf::from(file);
        ensure!(path.is_absolute(), "absolute request-file path required");
        ensure!(
            fs::metadata(&path)?.len() <= 65536,
            "request-file exceeds 64 KiB"
        );
        let text = fs::read_to_string(path)?;
        serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}'))?
    } else {
        ensure!(
            args.len() == 3 && args[0] == "--rpc",
            "--rpc OP JSON_ARGS required"
        );
        json!({"op":args[1],"args":serde_json::from_str::<Value>(&args[2])?})
    };
    let store = Store::open(&config, &cfg)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&rpc(
            &cfg,
            &store,
            request["op"].as_str().context("operation required")?,
            request.get("args").unwrap_or(&json!({}))
        )?)?
    );
    Ok(())
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("boxen-support: {error}");
        std::process::exit(1);
    }
}
