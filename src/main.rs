#[cfg(cosmo)]
extern crate cosmo_compat as _;

mod codex;
mod jobs;
mod logging;
mod platform;
mod protocol;
mod state;
mod web;

use logging::{Context, Level, Target};
use state::State;
use std::{env, net::TcpListener, path::PathBuf, sync::Arc};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("boxen: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let directory = take_option(&mut args, "--data-dir")?
        .map(PathBuf::from)
        .or_else(|| env::var_os("BOXEN_HOME").map(PathBuf::from))
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".boxen")
        });
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        println!(
            "Boxen — LAN pairing prototype\n\n\
boxen [--data-dir PATH] identity\n\
boxen [--data-dir PATH] trust PUBLIC_KEY LABEL\n\
boxen [--data-dir PATH] untrust PUBLIC_KEY\n\
boxen [--data-dir PATH] peers\n\
boxen [--data-dir PATH] serve [--listen IP:PORT] [--ui IP:PORT] [--codex PATH]\n\
boxen [--data-dir PATH] ping HOST:PORT PUBLIC_KEY [MESSAGE]\n\n\
boxen [--data-dir PATH] start HOST:PORT PUBLIC_KEY CWD MODEL PROMPT [--thread ID] [--id ID]\n\
boxen [--data-dir PATH] runs HOST:PORT PUBLIC_KEY\n\
boxen [--data-dir PATH] watch HOST:PORT PUBLIC_KEY RUN_ID [--after SEQUENCE] [--once]\n\
boxen [--data-dir PATH] cancel HOST:PORT PUBLIC_KEY RUN_ID\n\n\
boxen [--data-dir PATH] steer HOST:PORT PUBLIC_KEY RUN_ID MESSAGE\n\
boxen [--data-dir PATH] respond HOST:PORT PUBLIC_KEY RUN_ID REQUEST_ID_JSON RESULT_JSON\n\n\
boxen [--data-dir PATH] logs HOST:PORT PUBLIC_KEY [--level trace|debug|info|warn|error] [--targets peer,runtime] [--events PREFIX,...] [--run ID] [--thread ID] [--peer ID] [--payloads]\n\
boxen [--data-dir PATH] log-file HOST:PORT PUBLIC_KEY /absolute/daemon/path.jsonl [FILTER OPTIONS]\n\
boxen [--data-dir PATH] log-status HOST:PORT PUBLIC_KEY\n\
boxen [--data-dir PATH] log-off HOST:PORT PUBLIC_KEY\n\n\
boxen platform-check\n\n\
Pair by exchanging identity public keys through a trusted channel and\n\
running trust on BOTH machines. --ui must bind to IPv4 loopback.\n\
Default data: ~/.boxen (or BOXEN_HOME). Default peer port: 42424.\n\
Use MODEL '-' for the executing machine's configured default.\n\
Workloads run through Codex app-server with Full access on the destination."
        );
        return Ok(());
    }
    let command = args.remove(0);
    if command == "platform-check" && args.is_empty() {
        println!("{}", platform::check()?);
        return Ok(());
    }
    let state = Arc::new(State::open(directory)?);
    match command.as_str() {
        "identity" if args.is_empty() => println!("{}", state.public_key()),
        "trust" if args.len() == 2 => {
            state.trust(&args[0], &args[1])?;
            println!("Trusted {} ({})", args[1], args[0]);
        }
        "untrust" if args.len() == 1 => {
            state.untrust(&args[0])?;
            println!("Removed trust for {}", args[0]);
        }
        "peers" if args.is_empty() => {
            println!("{}", serde_json::to_string_pretty(&state.peers()?)?)
        }
        "ping" if (2..=3).contains(&args.len()) => {
            let reply = protocol::ping(
                &state,
                &args[0],
                &args[1],
                args.get(2)
                    .map(String::as_str)
                    .unwrap_or("hello from Boxen"),
            )?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
        }
        "serve" => {
            let codex = take_option(&mut args, "--codex")?
                .or_else(|| env::var("BOXEN_CODEX").ok())
                .unwrap_or_else(|| "codex".into());
            let listen =
                take_option(&mut args, "--listen")?.unwrap_or_else(|| "0.0.0.0:42424".into());
            let ui = take_option(&mut args, "--ui")?.unwrap_or_else(|| "127.0.0.1:42425".into());
            if !args.is_empty() {
                return Err("unexpected serve arguments".into());
            }
            let ui_addr: std::net::SocketAddrV4 = ui.parse()?;
            if !ui_addr.ip().is_loopback() {
                return Err("UI must bind to IPv4 loopback".into());
            }
            let peer_listener = TcpListener::bind(&listen)?;
            let ui_listener = TcpListener::bind(ui_addr)?;
            let token = state.control_token()?;
            logging::install(state.public_key())?;
            let jobs = jobs::Jobs::open(state.directory().join("runs"), codex)?;
            logging::record(
                Level::Info,
                Target::Daemon,
                "started",
                Context::default(),
                || serde_json::json!({"listen":listen,"ui":ui}),
                || serde_json::Value::Null,
            );
            println!("Boxen identity: {}", state.public_key());
            println!("Peers listening: {}", peer_listener.local_addr()?);
            println!(
                "Local controls: http://{}/#{token}",
                ui_listener.local_addr()?
            );
            let web_state = state.clone();
            let web_jobs = jobs.clone();
            std::thread::spawn(move || web::serve(ui_listener, web_state, web_jobs, token));
            protocol::serve(peer_listener, state, jobs);
        }
        "start" => {
            let thread = take_option(&mut args, "--thread")?;
            let id = take_option(&mut args, "--id")?.unwrap_or(state::random_hex()?);
            if args.len() != 5 {
                return Err("start requires HOST:PORT PUBLIC_KEY CWD MODEL PROMPT".into());
            }
            let spec = jobs::Spec {
                id: id.clone(),
                thread_id: thread.unwrap_or(id),
                backend: "codex".into(),
                cwd: args[2].clone(),
                model: if args[3] == "-" {
                    String::new()
                } else {
                    args[3].clone()
                },
                prompt: args[4].clone(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&protocol::request(
                    &state,
                    &args[0],
                    &args[1],
                    serde_json::json!({"op":"start","spec":spec})
                )?)?
            );
        }
        "runs" if args.len() == 2 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&protocol::request(
                    &state,
                    &args[0],
                    &args[1],
                    serde_json::json!({"op":"list"})
                )?)?
            );
        }
        "cancel" if args.len() == 3 => {
            println!(
                "{}",
                protocol::request(
                    &state,
                    &args[0],
                    &args[1],
                    serde_json::json!({"op":"control","id":args[2],"action":{"type":"cancel"}})
                )?
            );
        }
        "steer" if args.len() == 4 => {
            println!(
                "{}",
                protocol::request(
                    &state,
                    &args[0],
                    &args[1],
                    serde_json::json!({"op":"control","id":args[2],"action":{"type":"steer","text":args[3]}})
                )?
            );
        }
        "respond" if args.len() == 5 => {
            let request_id: serde_json::Value = serde_json::from_str(&args[3])?;
            let result: serde_json::Value = serde_json::from_str(&args[4])?;
            println!(
                "{}",
                protocol::request(
                    &state,
                    &args[0],
                    &args[1],
                    serde_json::json!({"op":"control","id":args[2],"action":{"type":"respond","request_id":request_id,"result":result}})
                )?
            );
        }
        "watch" => {
            use std::io::Write;
            let mut after = take_option(&mut args, "--after")?
                .unwrap_or_else(|| "0".into())
                .parse::<u64>()?;
            let once = if let Some(index) = args.iter().position(|arg| arg == "--once") {
                args.remove(index);
                true
            } else {
                false
            };
            if args.len() != 3 {
                return Err("watch requires HOST:PORT PUBLIC_KEY RUN_ID".into());
            }
            jobs::valid_id(&args[2])?;
            state.require_trust(&args[1])?;
            let mut delay = 500;
            loop {
                let outcome = (|| -> Result<()> {
                    let mut session = protocol::connect(&state, &args[0], &args[1])?;
                    session.send(&serde_json::json!({"op":"watch","id":args[2],"after":after}))?;
                    loop {
                        let batch = protocol::result(session.receive()?)?;
                        delay = 500;
                        for event in batch["events"].as_array().ok_or("missing event batch")? {
                            let seq = event["seq"].as_u64().ok_or("missing event sequence")?;
                            if seq <= after {
                                continue;
                            }
                            if seq != after + 1 {
                                return Err("event sequence gap".into());
                            }
                            // Advance only after successful publication to the consumer.
                            writeln!(std::io::stdout(), "{event}")?;
                            after = seq;
                        }
                        if batch["done"] == true {
                            return Ok(());
                        }
                    }
                })();
                match outcome {
                    Ok(()) => break,
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
                    {
                        break;
                    }
                    Err(error) if once || error.downcast_ref::<protocol::Rejected>().is_some() => {
                        return Err(error);
                    }
                    Err(error) => {
                        eprintln!(
                            "Machine unreachable; execution outcome unknown. Reconnecting after event {after}: {error}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(delay));
                        delay = (delay * 2).min(5000);
                    }
                }
            }
        }
        "log-status" | "log-off" if args.len() == 2 => {
            let op = if command == "log-status" {
                "log_status"
            } else {
                "log_off"
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&protocol::request(
                    &state,
                    &args[0],
                    &args[1],
                    serde_json::json!({"op":op})
                )?)?
            );
        }
        "logs" | "log-file" => {
            let filter = log_filter(&mut args)?;
            let expected = if command == "logs" { 2 } else { 3 };
            if args.len() != expected {
                return Err("logs requires HOST:PORT PUBLIC_KEY; log-file additionally requires an absolute destination path".into());
            }
            if command == "log-file" {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&protocol::request(
                        &state,
                        &args[0],
                        &args[1],
                        serde_json::json!({"op":"log_file","path":args[2],"filter":filter})
                    )?)?
                );
            } else {
                use std::io::Write;
                let mut session = protocol::connect(&state, &args[0], &args[1])?;
                session.diagnostics(false);
                session.send(&serde_json::json!({"op":"logs","filter":filter}))?;
                let mut output = std::io::stdout().lock();
                loop {
                    let batch = protocol::result(session.receive()?)?;
                    if let Some(session) = batch.get("session") {
                        eprintln!(
                            "Attached to daemon diagnostics (session {session}); Ctrl-C detaches."
                        );
                    }
                    for record in batch["records"].as_array().ok_or("missing log records")? {
                        if let Err(error) =
                            writeln!(output, "{record}").and_then(|_| output.flush())
                        {
                            if error.kind() == std::io::ErrorKind::BrokenPipe {
                                return Ok(());
                            }
                            return Err(error.into());
                        }
                    }
                    if batch["done"] == true {
                        break;
                    }
                }
            }
        }
        _ => return Err("invalid command or arguments; use boxen --help".into()),
    }
    Ok(())
}

fn log_filter(args: &mut Vec<String>) -> Result<logging::Filter> {
    let mut value = serde_json::json!({});
    for (option, key) in [
        ("--level", "level"),
        ("--run", "run_id"),
        ("--thread", "thread_id"),
        ("--peer", "peer_id"),
    ] {
        if let Some(text) = take_option(args, option)? {
            value[key] = serde_json::json!(text);
        }
    }
    for (option, key) in [("--targets", "targets"), ("--events", "events")] {
        if let Some(text) = take_option(args, option)? {
            value[key] = serde_json::json!(text.split(',').collect::<Vec<_>>());
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--payloads") {
        args.remove(index);
        value["payloads"] = serde_json::json!(true);
    }
    Ok(serde_json::from_value(value)?)
}

fn take_option(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(None);
    };
    if index + 1 == args.len() {
        return Err(format!("{name} requires a value").into());
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Ok(Some(value))
}
