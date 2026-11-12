use crate::logging::{self, Context, Delivery, Filter, Level, Target};
use crate::{Result, jobs::Jobs, protocol, state::State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const PAGE: &str = include_str!("../web/index.html");

#[derive(Deserialize)]
struct Ping {
    address: String,
    peer: String,
    message: String,
}

pub fn serve(listener: TcpListener, state: Arc<State>, jobs: Arc<Jobs>, token: String) {
    let active = Arc::new(AtomicUsize::new(0));
    let port = listener.local_addr().expect("bound listener").port();
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if active.fetch_add(1, Ordering::SeqCst) >= 64 {
                    logging::record(
                        Level::Warn,
                        Target::Http,
                        "connection.limit_reached",
                        Context::default(),
                        || json!({"limit":64}),
                        || Value::Null,
                    );
                    active.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (state, jobs, token, active) =
                    (state.clone(), jobs.clone(), token.clone(), active.clone());
                std::thread::spawn(move || {
                    if let Err(error) = handle(&mut stream, &state, &jobs, &token, port) {
                        logging::record(
                            Level::Warn,
                            Target::Http,
                            "request.failed",
                            Context::default(),
                            || json!({"error":error.to_string()}),
                            || Value::Null,
                        );
                        let _ =
                            respond(&mut stream, 400, "text/plain", error.to_string().as_bytes());
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(error) => logging::record(
                Level::Error,
                Target::Http,
                "accept.failed",
                Context::default(),
                || json!({"error":error.to_string()}),
                || Value::Null,
            ),
        }
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",
        if status == 200 { "OK" } else { "Error" },
        body.len()
    )?;
    stream.write_all(body)?;
    logging::record(
        Level::Debug,
        Target::Http,
        "response.sent",
        Context::default(),
        || json!({"status":status,"content_type":content_type,"bytes":body.len()}),
        || Value::Null,
    );
    Ok(())
}

fn handle(
    stream: &mut TcpStream,
    state: &State,
    jobs: &Arc<Jobs>,
    token: &str,
    port: u16,
) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 8192 {
            return Err("request headers too large".into());
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    let header = std::str::from_utf8(&bytes)?;
    let mut lines = header.split("\r\n");
    let request = lines.next().ok_or("missing request line")?;
    // Deliberately log only method/path; never headers, authorization, or raw URLs.
    logging::record(
        Level::Debug,
        Target::Http,
        "request.received",
        Context::default(),
        || json!({"method":request.split_whitespace().next(),"path":request.split_whitespace().nth(1).unwrap_or("").split('?').next()}),
        || Value::Null,
    );
    let mut host = None;
    let mut authorization = None;
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid header")?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "host" => {
                if host.replace(value).is_some() {
                    return Err("duplicate host".into());
                }
            }
            "authorization" => {
                if authorization.replace(value).is_some() {
                    return Err("duplicate authorization".into());
                }
            }
            "content-length" => {
                if length.replace(value.parse::<usize>()?).is_some() {
                    return Err("duplicate content length".into());
                }
            }
            "transfer-encoding" => return Err("transfer encoding is not supported".into()),
            _ => {}
        }
    }
    let Some(host) = host else {
        return Err("missing host".into());
    };
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return respond(
            stream,
            403,
            "text/plain",
            b"This endpoint accepts the localhost control URL printed by Boxen",
        );
    }
    if request == "GET / HTTP/1.1" {
        return respond(stream, 200, "text/html; charset=utf-8", PAGE.as_bytes());
    }
    if authorization != Some(format!("Bearer {token}").as_str()) {
        return respond(
            stream,
            401,
            "text/plain",
            b"Local API authorization is the bearer capability in the daemon's printed control URL fragment",
        );
    }
    if request == "GET /api/status HTTP/1.1" {
        let status = json!({"api_version":2,"communication":{"resident":"mailbox","anonymous_thread":"request_response"},"identity":state.public_key(),"peers":state.peers()?});
        return respond(
            stream,
            200,
            "application/json",
            &serde_json::to_vec(&status)?,
        );
    }
    let length = length.ok_or("content length required")?;
    if length > 512 * 1024 {
        return Err("request body too large".into());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    if request == "POST /api/ping HTTP/1.1" {
        let ping: Ping = serde_json::from_slice(&body)?;
        return match protocol::ping(state, &ping.address, &ping.peer, &ping.message) {
            Ok(reply) => respond(
                stream,
                200,
                "application/json",
                &serde_json::to_vec(&reply)?,
            ),
            Err(error) => respond(stream, 502, "text/plain", error.to_string().as_bytes()),
        };
    }
    let target: Value = serde_json::from_slice(&body)?;
    let peer = target["peer"].as_str().ok_or("peer required")?;
    let address = target["address"].as_str().unwrap_or("");
    let operation = &target["request"];
    let local = peer == state.public_key();
    if let Some(id) = target["connector_instance"].as_str() {
        let result = if local && request == "POST /api/operation HTTP/1.1" {
            crate::messaging::local_runner_operation(jobs, id, operation)
        } else {
            Err("local connector control cannot be forwarded to a peer or another endpoint".into())
        };
        logging::record(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::Http,
            "connector.local_control",
            Context::peer(peer),
            || json!({"local":local,"accepted":result.is_ok(),"action":operation["args"]["action"]}),
            || Value::Null,
        );
        return match result {
            Ok(value) => respond(
                stream,
                200,
                "application/json",
                &serde_json::to_vec(&value)?,
            ),
            Err(error) => respond(stream, 422, "text/plain", error.to_string().as_bytes()),
        };
    }

    logging::record(
        Level::Debug,
        Target::Http,
        "operation.routed",
        Context {
            peer_id: Some(peer),
            run_id: operation["id"]
                .as_str()
                .or_else(|| operation["spec"]["id"].as_str()),
            thread_id: operation["spec"]["thread_id"].as_str(),
            connection_id: None,
        },
        || json!({"local":local,"op":operation["op"]}),
        || {
            if operation["op"] == "attachment" {
                Value::Null
            } else {
                operation.clone()
            }
        },
    );
    if request == "POST /api/logs HTTP/1.1" {
        let filter = serde_json::from_value::<Filter>(
            operation.get("filter").cloned().unwrap_or(json!({})),
        )?;
        let local_subscription = if local {
            Some(logging::global()?.subscribe(filter.clone())?)
        } else {
            None
        };
        let mut remote = if local {
            None
        } else {
            let mut session = protocol::connect(state, address, peer)?;
            session.diagnostics(false);
            session.send(&json!({"op":"logs","filter":filter}))?;
            Some(session)
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n"
        )?;
        loop {
            let batch = if let Some(session) = &mut remote {
                match session.receive() {
                    Ok(batch) => batch,
                    Err(error) => json!({"error":error.to_string()}),
                }
            } else {
                let (records, done) = match local_subscription
                    .as_ref()
                    .ok_or("missing log subscription")?
                    .read(Duration::from_secs(1))
                {
                    Delivery::Record(record) => (vec![record], false),
                    Delivery::Idle => (vec![], false),
                    Delivery::End => (vec![], true),
                };
                json!({"records":records,"done":done})
            };
            let mut bytes = serde_json::to_vec(&batch)?;
            bytes.push(b'\n');
            stream.write_all(&bytes)?;
            stream.flush()?;
            if batch["done"] == true || batch.get("error").is_some() {
                break;
            }
        }
        return Ok(());
    }
    if request == "POST /api/operation HTTP/1.1" {
        let attempt = if local {
            crate::infrastructure::Attempt::request("http_local", peer, operation, None)
        } else {
            None
        };
        let result = if local && operation["op"] == "network" {
            crate::network::operation(state, &operation["args"])
        } else if local {
            crate::operator::operation(jobs, peer, operation)
        } else {
            protocol::request(state, address, peer, operation.clone())
        };
        if let Some(attempt) = &attempt {
            attempt.record(
                "dispatch",
                if result.is_ok() {
                    "completed"
                } else {
                    "rejected"
                },
                result
                    .as_ref()
                    .err()
                    .map(|error| crate::infrastructure::reason(error.as_ref())),
            );
        }
        return match result {
            Ok(reply) => respond(
                stream,
                200,
                "application/json",
                &serde_json::to_vec(&reply)?,
            ),
            Err(error) => respond(
                stream,
                if local || error.downcast_ref::<protocol::Rejected>().is_some() {
                    422
                } else {
                    502
                },
                "text/plain",
                error.to_string().as_bytes(),
            ),
        };
    }
    if request == "POST /api/watch HTTP/1.1" {
        let id = operation["id"].as_str().ok_or("run id required")?;
        let mut after = operation["after"].as_u64().unwrap_or(0);
        // Establish/authenticate upstream before committing the HTTP streaming response.
        let mut remote = if local {
            None
        } else {
            let mut session = protocol::connect(state, address, peer)?;
            session.send(&json!({"op":"watch","id":id,"after":after}))?;
            Some(session)
        };
        let local_run = if local {
            Some(crate::operator::run(jobs, id)?)
        } else {
            None
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n"
        )?;
        loop {
            let batch = if let Some(session) = &mut remote {
                match session.receive() {
                    Ok(batch) => batch,
                    Err(error) => {
                        json!({"error":format!("Remote event stream disconnected: {error}")})
                    }
                }
            } else {
                let run = local_run.as_ref().ok_or("local run not available")?;
                let (events, snapshot) = run.batch(after, true)?;
                if let Some(last) = events.last() {
                    after = last.seq;
                }
                let done = crate::jobs::terminal(&snapshot.status) && after == snapshot.last_seq;
                json!({"events":events,"snapshot":snapshot,"done":done})
            };
            let mut bytes = serde_json::to_vec(&batch)?;
            bytes.push(b'\n');
            stream.write_all(&bytes)?;
            stream.flush()?;
            if batch["done"] == true || batch.get("error").is_some() {
                break;
            }
        }
        return Ok(());
    }
    respond(stream, 404, "text/plain", b"Not found")
}
