#[cfg(cosmo)]
extern crate cosmo_compat as _;

mod codex;
mod jobs;
mod protocol;
mod state;
mod web;

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
boxen [--data-dir PATH] watch HOST:PORT PUBLIC_KEY RUN_ID [--after SEQUENCE]\n\
boxen [--data-dir PATH] cancel HOST:PORT PUBLIC_KEY RUN_ID\n\n\
boxen [--data-dir PATH] steer HOST:PORT PUBLIC_KEY RUN_ID MESSAGE\n\
boxen [--data-dir PATH] respond HOST:PORT PUBLIC_KEY RUN_ID REQUEST_ID_JSON RESULT_JSON\n\n\
Pair by exchanging identity public keys through a trusted channel and\n\
running trust on BOTH machines. --ui must bind to IPv4 loopback.\n\
Default data: ~/.boxen (or BOXEN_HOME). Default peer port: 42424.\n\
Use MODEL '-' for the executing machine's configured default.\n\
Workloads run through Codex app-server with Full access on the destination."
        );
        return Ok(());
    }
    let command = args.remove(0);
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
            let token = state::random_hex()?;
            let jobs = jobs::Jobs::open(state.directory().join("runs"), codex)?;
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
            let after = take_option(&mut args, "--after")?
                .unwrap_or_else(|| "0".into())
                .parse::<u64>()?;
            if args.len() != 3 {
                return Err("watch requires HOST:PORT PUBLIC_KEY RUN_ID".into());
            }
            let mut session = protocol::connect(&state, &args[0], &args[1])?;
            session.send(&serde_json::json!({"op":"watch","id":args[2],"after":after}))?;
            loop {
                let batch = protocol::result(session.receive()?)?;
                for event in batch["events"].as_array().ok_or("missing event batch")? {
                    println!("{}", event);
                }
                if batch["done"] == true {
                    break;
                }
            }
        }
        _ => return Err("invalid command or arguments; use boxen --help".into()),
    }
    Ok(())
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
