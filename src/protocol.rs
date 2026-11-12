use crate::{
    Result,
    infrastructure::{self, Attempt},
    jobs::{Control, Jobs, Spec},
    logging::{self, Context, Delivery, Filter, Level, Target},
    state::{self, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use snow::{HandshakeState, TransportState, params::NoiseParams};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TEXT: usize = 4096;
const PROLOGUE: &[u8] = b"boxen-lan-v2";

pub fn params() -> NoiseParams {
    "Noise_XX_25519_ChaChaPoly_BLAKE2s"
        .parse()
        .expect("constant Noise parameters")
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Reply {
    pub from: String,
    pub message: String,
}

fn send(stream: &mut TcpStream, message: &[u8]) -> Result<()> {
    let length = u16::try_from(message.len())?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(message)?;
    Ok(())
}
fn receive(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut length = [0; 2];
    stream.read_exact(&mut length)?;
    let mut message = vec![0; u16::from_be_bytes(length) as usize];
    stream.read_exact(&mut message)?;
    Ok(message)
}
fn handshake_send(stream: &mut TcpStream, noise: &mut HandshakeState) -> Result<()> {
    let mut buffer = [0; 65535];
    let length = noise.write_message(&[], &mut buffer)?;
    send(stream, &buffer[..length])
}
fn handshake_receive(stream: &mut TcpStream, noise: &mut HandshakeState) -> Result<()> {
    noise.read_message(&receive(stream)?, &mut [0; 65535])?;
    Ok(())
}
fn encrypt(stream: &mut TcpStream, transport: &mut TransportState, plain: &[u8]) -> Result<()> {
    let mut buffer = [0; 65535];
    let length = transport.write_message(plain, &mut buffer)?;
    send(stream, &buffer[..length])
}
fn decrypt(stream: &mut TcpStream, transport: &mut TransportState) -> Result<Vec<u8>> {
    let mut buffer = vec![0; 65535];
    let length = transport.read_message(&receive(stream)?, &mut buffer)?;
    buffer.truncate(length);
    Ok(buffer)
}
fn configure(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.set_nodelay(true)?;
    Ok(())
}
pub struct Session {
    stream: TcpStream,
    transport: TransportState,
    connection_id: u64,
    peer: String,
    diagnostics: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.diagnostics {
            logging::record(
                Level::Debug,
                Target::Peer,
                "session.closed",
                Context {
                    peer_id: Some(&self.peer),
                    connection_id: Some(self.connection_id),
                    ..Context::default()
                },
                || json!({}),
                || Value::Null,
            );
        }
    }
}
impl Session {
    pub fn diagnostics(&mut self, enabled: bool) {
        self.diagnostics = enabled;
    }
    pub fn send(&mut self, value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("peer message exceeds 8 MiB".into());
        }
        let chunks = bytes.chunks(60000);
        let count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let mut frame = Vec::with_capacity(chunk.len() + 1);
            frame.push(u8::from(index + 1 < count));
            frame.extend_from_slice(chunk);
            encrypt(&mut self.stream, &mut self.transport, &frame)?;
        }
        if self.diagnostics {
            logging::record(
                Level::Trace,
                Target::Peer,
                "message.sent",
                Context {
                    peer_id: Some(&self.peer),
                    connection_id: Some(self.connection_id),
                    run_id: value["id"]
                        .as_str()
                        .or_else(|| value["spec"]["id"].as_str())
                        .or_else(|| value["snapshot"]["record"]["spec"]["id"].as_str())
                        .or_else(|| value["record"]["spec"]["id"].as_str()),
                    thread_id: value["spec"]["thread_id"]
                        .as_str()
                        .or_else(|| value["snapshot"]["record"]["spec"]["thread_id"].as_str())
                        .or_else(|| value["record"]["spec"]["thread_id"].as_str()),
                },
                || json!({"bytes":bytes.len(),"op":value["op"],"done":value["done"]}),
                || value.clone(),
            );
        }
        Ok(())
    }
    pub fn receive(&mut self) -> Result<Value> {
        let mut bytes = Vec::new();
        loop {
            let frame = decrypt(&mut self.stream, &mut self.transport)?;
            let (&more, payload) = frame.split_first().ok_or("empty peer fragment")?;
            if more > 1 || bytes.len() + payload.len() > 8 * 1024 * 1024 {
                return Err("invalid peer fragments".into());
            }
            bytes.extend_from_slice(payload);
            if more == 0 {
                let value: Value = serde_json::from_slice(&bytes)?;
                if value["op"] == "logs" {
                    self.diagnostics = false;
                }
                if self.diagnostics {
                    logging::record(
                        Level::Trace,
                        Target::Peer,
                        "message.received",
                        Context {
                            peer_id: Some(&self.peer),
                            connection_id: Some(self.connection_id),
                            run_id: value["id"]
                                .as_str()
                                .or_else(|| value["spec"]["id"].as_str())
                                .or_else(|| value["snapshot"]["record"]["spec"]["id"].as_str())
                                .or_else(|| value["record"]["spec"]["id"].as_str()),
                            thread_id: value["spec"]["thread_id"]
                                .as_str()
                                .or_else(|| {
                                    value["snapshot"]["record"]["spec"]["thread_id"].as_str()
                                })
                                .or_else(|| value["record"]["spec"]["thread_id"].as_str()),
                        },
                        || json!({"bytes":bytes.len(),"op":value["op"]}),
                        || value.clone(),
                    );
                }
                return Ok(value);
            }
        }
    }
}
pub fn connect(state: &State, address: &str, expected: &str) -> Result<Session> {
    connect_for(state, address, expected, None)
}

fn connect_for(
    state: &State,
    address: &str,
    expected: &str,
    attempt: Option<&Attempt>,
) -> Result<Session> {
    let connection_id = next_connection();
    logging::record(
        Level::Debug,
        Target::Peer,
        "connect.requested",
        Context {
            peer_id: Some(expected),
            connection_id: Some(connection_id),
            ..Context::default()
        },
        || json!({"address":address}),
        || Value::Null,
    );
    let started = std::time::Instant::now();
    let outcome = connect_inner(state, address, expected, connection_id);
    if let Err(error) = &outcome {
        if let Some(attempt) = attempt {
            attempt.record(
                "connect",
                "failed",
                Some(infrastructure::reason(error.as_ref())),
            );
        } else {
            Attempt::new("peer_out", "connect", Some(expected), None, None, None)
                .since(started)
                .record(
                    "connect",
                    "failed",
                    Some(infrastructure::reason(error.as_ref())),
                );
        }
        logging::record(
            Level::Warn,
            Target::Peer,
            "connect.failed",
            Context {
                peer_id: Some(expected),
                connection_id: Some(connection_id),
                ..Context::default()
            },
            || json!({"address":address,"error":error.to_string()}),
            || Value::Null,
        );
    }
    outcome
}
fn connect_inner(
    state: &State,
    address: &str,
    expected: &str,
    connection_id: u64,
) -> Result<Session> {
    state.require_trust(expected)?;
    let mut connected = None;
    for address in address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(error) => logging::record(
                Level::Debug,
                Target::Peer,
                "connect.address_failed",
                Context::connection(connection_id),
                || json!({"address":address.to_string(),"error":error.to_string()}),
                || Value::Null,
            ),
        }
    }
    let mut stream = connected.ok_or("peer unreachable; delivery is not confirmed")?;
    configure(&stream)?;
    let private = state.private_key()?;
    let mut noise = snow::Builder::new(params())
        .prologue(PROLOGUE)?
        .local_private_key(&private)?
        .build_initiator()?;
    handshake_send(&mut stream, &mut noise)?;
    handshake_receive(&mut stream, &mut noise)?;
    if noise.get_remote_static() != Some(state::key(expected)?.as_slice()) {
        return Err("remote identity differs from the selected peer; no message sent".into());
    }
    handshake_send(&mut stream, &mut noise)?;
    logging::record(
        Level::Info,
        Target::Peer,
        "connect.authenticated",
        Context {
            peer_id: Some(expected),
            connection_id: Some(connection_id),
            ..Context::default()
        },
        || json!({"address":address}),
        || Value::Null,
    );
    crate::network::remember(state, expected, address)?;
    Ok(Session {
        stream,
        transport: noise.into_transport_mode()?,
        connection_id,
        peer: expected.into(),
        diagnostics: true,
    })
}
pub fn request(state: &State, address: &str, expected: &str, mut request: Value) -> Result<Value> {
    let attempt = Attempt::request("peer_out", expected, &request, None);
    if let Some(id) = attempt.as_ref().and_then(Attempt::id) {
        request["_boxen_attempt"] = json!(id);
    }
    let mut session = connect_for(state, address, expected, attempt.as_ref())?;
    session.send(&request).inspect_err(|error| {
        if let Some(attempt) = &attempt {
            attempt.record(
                "send",
                "unknown",
                Some(infrastructure::reason(error.as_ref())),
            );
        }
    })?;
    let reply = session.receive().inspect_err(|error| {
        if let Some(attempt) = &attempt {
            attempt.record(
                "reply",
                "unknown",
                Some(infrastructure::reason(error.as_ref())),
            );
        }
    })?;
    let outcome = result(reply);
    if let Some(attempt) = &attempt {
        attempt.record(
            "reply",
            if outcome.is_ok() {
                "completed"
            } else {
                "rejected"
            },
            outcome.as_ref().err().map(|_| "peer_rejected"),
        );
    }
    outcome
}

pub fn result(reply: Value) -> Result<Value> {
    if let Some(error) = reply.get("error") {
        return Err(Box::new(Rejected(
            error.as_str().unwrap_or("peer error").to_string(),
        )));
    }
    Ok(reply)
}

#[derive(Debug)]
pub struct Rejected(pub String);
impl std::fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Rejected {}
pub fn ping(state: &State, address: &str, expected: &str, message: &str) -> Result<Reply> {
    if message.len() > MAX_TEXT {
        return Err("message exceeds 4096 bytes".into());
    }
    let reply: Reply = serde_json::from_value(request(
        state,
        address,
        expected,
        json!({"op":"ping","message":message}),
    )?)?;
    if reply.from != expected || reply.message != message {
        return Err("invalid peer reply".into());
    }
    Ok(reply)
}
pub fn operation(jobs: &Arc<Jobs>, owner: &str, request: &Value) -> Result<Value> {
    logging::record(
        Level::Debug,
        Target::Daemon,
        "operation.requested",
        Context::peer(owner),
        || json!({"op":request["op"]}),
        || Value::Null,
    );
    let outcome = (|| -> Result<Value> {
        match request["op"].as_str().unwrap_or("") {
            "attachment" => {
                let result = jobs.attachments.operation(request);
                logging::record(
                    if result.is_ok() {
                        Level::Debug
                    } else {
                        Level::Warn
                    },
                    Target::State,
                    "attachment.operation",
                    Context::peer(owner),
                    || json!({"action":request["action"],"digest":request["file"]["digest"],"offset":request["offset"],"complete":result.as_ref().ok().and_then(|v| v["complete"].as_bool())}),
                    || Value::Null,
                );
                result
            }
            "log_status" => Ok(logging::global()?.status()),
            "log_off" => {
                logging::global()?.off();
                Ok(logging::global()?.status())
            }
            "log_file" => {
                let logger = logging::global()?;
                logger.file(
                    request["path"].as_str().ok_or("log path required")?,
                    serde_json::from_value::<Filter>(
                        request.get("filter").cloned().unwrap_or(json!({})),
                    )?,
                )?;
                logging::record(
                    Level::Info,
                    Target::Daemon,
                    "logging.file_enabled",
                    Context::peer(owner),
                    || json!({"path":request["path"]}),
                    || Value::Null,
                );
                Ok(logger.status())
            }
            "start" => Ok(serde_json::to_value(
                jobs.start_after(
                    owner,
                    serde_json::from_value::<Spec>(request["spec"].clone())?,
                    request
                        .get("after")
                        .map(|v| v.as_str().ok_or("after must be a run ID"))
                        .transpose()?,
                )?,
            )?),
            "context" => jobs.context_operation(
                owner,
                request["thread_id"].as_str().ok_or("thread_id required")?,
                request["args"].clone(),
            ),
            "events" => jobs.event_operation(owner, request["args"].clone()),
            "schedule" => jobs.schedule_operation(owner, request["args"].clone()),
            "runtime_info" => Ok(jobs.runtime_info()),
            "messaging" => crate::messaging::operation(
                jobs,
                owner,
                None,
                request["args"].clone(),
                format!(
                    "client-messaging:{owner}:{}",
                    request["args"]["id"].as_str().unwrap_or("")
                ),
            ),
            "mailbox_status" => Ok(jobs.mailboxes.status()),
            "mailbox_send" => {
                let client = request["client_id"].as_str().ok_or("client_id required")?;
                crate::jobs::valid_id(client)?;
                let spec: crate::mailbox::Send =
                    serde_json::from_value(request["message"].clone())?;
                let source = format!("client:{owner}:{client}:{}", spec.id);
                Ok(serde_json::to_value(jobs.send_mail(
                    owner,
                    crate::mailbox::Address::Client {
                        machine: owner.into(),
                        id: client.into(),
                    },
                    spec,
                    source,
                )?)?)
            }
            "resident_inbox" => jobs.inbox(
                owner,
                crate::mailbox::Address::Resident {
                    id: request["id"].as_str().ok_or("resident id required")?.into(),
                },
                request["after"].as_u64().unwrap_or(0),
            ),
            "client_inbox" => jobs.inbox(
                owner,
                crate::mailbox::Address::Client {
                    machine: owner.into(),
                    id: request["client_id"]
                        .as_str()
                        .ok_or("client id required")?
                        .into(),
                },
                request["after"].as_u64().unwrap_or(0),
            ),
            "inference_health" => {
                Ok(jobs.inference_health(owner, request["offset"].as_u64().unwrap_or(0) as usize))
            }
            "inference_resume" => Ok(serde_json::to_value(
                jobs.resume_inference(
                    owner,
                    request["failed_run_id"]
                        .as_str()
                        .ok_or("failed_run_id required")?,
                    request["id"].as_str().ok_or("id required")?.into(),
                )?,
            )?),
            "resident_activate" => Ok(serde_json::to_value(
                jobs.activate(
                    owner,
                    request["id"].as_str().ok_or("resident id required")?,
                    request["activation_id"]
                        .as_str()
                        .ok_or("activation_id required")?
                        .into(),
                    request["directions"].as_str().unwrap_or("").into(),
                    true,
                )?,
            )?),
            "thread_result" => {
                jobs.thread_result(owner, request["id"].as_str().ok_or("run id required")?)
            }
            "resident_definition" => jobs.definition_operation(owner, request["args"].clone()),
            "resident_issues" => Ok(json!(jobs.residents.issues())),
            "resident_create" => Ok(jobs
                .create_resident(owner, serde_json::from_value(request["spec"].clone())?)?
                .value()),
            "resident_promote" => Ok(jobs
                .promote(owner, serde_json::from_value(request["spec"].clone())?)?
                .value()),
            "resident_list" => Ok(json!(
                jobs.residents
                    .list(owner)
                    .iter()
                    .map(|r| r.value())
                    .collect::<Vec<_>>()
            )),
            "resident_get" => Ok(jobs
                .residents
                .get(owner, request["id"].as_str().ok_or("resident id required")?)?
                .value()),
            "resident_update" => {
                let resident = jobs.residents.update(
                    owner,
                    request["id"].as_str().ok_or("resident id required")?,
                    serde_json::from_value(request["patch"].clone())?,
                )?;
                jobs.notify();
                Ok(resident.value())
            }
            "thread_activity" => {
                jobs.thread_activity(owner, request["id"].as_str().ok_or("thread id required")?)
            }
            "list" => Ok(serde_json::to_value(jobs.list(owner))?),
            "transcript_import" => Ok(serde_json::to_value(
                jobs.import_transcript(owner, serde_json::from_value(request["spec"].clone())?)?,
            )?),
            "thread_import" => Ok(serde_json::to_value(
                jobs.import_codex(owner, serde_json::from_value(request["spec"].clone())?)?,
            )?),
            "get" => Ok(serde_json::to_value(
                jobs.get(owner, request["id"].as_str().ok_or("run id required")?)?
                    .snapshot(),
            )?),
            "control" => {
                let run = jobs.get(owner, request["id"].as_str().ok_or("run id required")?)?;
                let mut action: Control = serde_json::from_value(request["action"].clone())?;
                if let Control::Steer {
                    attachments,
                    file_context,
                    ..
                } = &mut action
                {
                    jobs.attachments.require(attachments)?;
                    *file_context = jobs.attachments.context(attachments);
                }
                run.control(action)?;
                Ok(json!({"accepted":true}))
            }
            _ => Err("unknown peer operation".into()),
        }
    })();
    logging::record(
        if outcome.is_ok() {
            Level::Debug
        } else {
            Level::Warn
        },
        Target::Daemon,
        if outcome.is_ok() {
            "operation.completed"
        } else {
            "operation.rejected"
        },
        Context {
            peer_id: Some(owner),
            run_id: request["id"]
                .as_str()
                .or_else(|| request["spec"]["id"].as_str()),
            ..Context::default()
        },
        || json!({"op":request["op"],"error":outcome.as_ref().err().map(|e|e.to_string())}),
        || Value::Null,
    );
    outcome
}
fn handle(stream: TcpStream, state: &State, jobs: Option<&Arc<Jobs>>) -> Result<()> {
    let connection_id = next_connection();
    let outcome = handle_inner(stream, state, jobs, connection_id);
    logging::record(
        if outcome.is_ok() {
            Level::Debug
        } else {
            Level::Warn
        },
        Target::Peer,
        if outcome.is_ok() {
            "connection.closed"
        } else {
            "connection.failed"
        },
        Context::connection(connection_id),
        || json!({"error":outcome.as_ref().err().map(|e|e.to_string())}),
        || Value::Null,
    );
    outcome
}
fn handle_inner(
    mut stream: TcpStream,
    state: &State,
    jobs: Option<&Arc<Jobs>>,
    connection_id: u64,
) -> Result<()> {
    logging::record(
        Level::Debug,
        Target::Peer,
        "handshake.started",
        Context::connection(connection_id),
        || json!({}),
        || Value::Null,
    );
    configure(&stream)?;
    let private = state.private_key()?;
    let mut noise = snow::Builder::new(params())
        .prologue(PROLOGUE)?
        .local_private_key(&private)?
        .build_responder()?;
    handshake_receive(&mut stream, &mut noise)?;
    handshake_send(&mut stream, &mut noise)?;
    handshake_receive(&mut stream, &mut noise)?;
    let remote = state::hex(noise.get_remote_static().ok_or("missing peer identity")?);
    state.require_trust(&remote)?;
    logging::record(
        Level::Info,
        Target::Peer,
        "handshake.authenticated",
        Context {
            peer_id: Some(&remote),
            connection_id: Some(connection_id),
            ..Context::default()
        },
        || json!({}),
        || Value::Null,
    );
    let mut session = Session {
        stream,
        transport: noise.into_transport_mode()?,
        connection_id,
        peer: remote.clone(),
        diagnostics: true,
    };
    let mut request = session.receive()?;
    state.require_trust(&remote)?;
    let attempt_id = request
        .as_object_mut()
        .and_then(|object| object.remove("_boxen_attempt"));
    let mut attempt = Attempt::request(
        "peer_in",
        &remote,
        &request,
        attempt_id.as_ref().and_then(Value::as_str),
    );
    let outcome = (|| -> Result<()> {
        if request["op"] == "network" {
            session.send(&crate::network::operation(state, &request["args"])?)?;
        } else if request["op"] == "logs" {
            session.diagnostics(false);
            let filter = serde_json::from_value::<Filter>(
                request.get("filter").cloned().unwrap_or(json!({})),
            )?;
            let subscription = logging::global()?.subscribe(filter)?;
            session.send(&json!({"records":[],"done":false,"session":logging::global()?.status()["session_id"]}))?;
            loop {
                state.require_trust(&remote)?;
                let (records, done) = match subscription.read(Duration::from_secs(1)) {
                    Delivery::Record(record) => (vec![record], false),
                    Delivery::Idle => (vec![], false),
                    Delivery::End => (vec![], true),
                };
                session.send(&json!({"records":records,"done":done}))?;
                if done {
                    break;
                }
            }
        } else if request["op"] == "network_sync" {
            let port = request["port"]
                .as_u64()
                .and_then(|v| u16::try_from(v).ok())
                .filter(|p| *p > 0)
                .ok_or("network listen port required")?;
            let address =
                std::net::SocketAddr::new(session.stream.peer_addr()?.ip(), port).to_string();
            crate::network::merge(state, &remote, request["members"].clone())?;
            crate::network::remember(state, &remote, &address)?;
            let jobs = jobs.ok_or("workload service not available")?;
            crate::network::retain_catalog(state, &remote, request["residents"].clone())?;
            session.send(&json!({"members":crate::network::members(state)?,"residents":crate::network::catalog(jobs)}))?;
        } else if request["op"] == "network_mail" {
            let jobs = jobs.ok_or("workload service not available")?;
            let message: crate::mailbox::Message =
                serde_json::from_value(request["message"].clone())?;
            match &message.to {
                crate::mailbox::Address::Resident { id } => {
                    let resident = jobs
                        .residents
                        .all()
                        .into_iter()
                        .find(|r| &r.id == id)
                        .ok_or("network recipient is not resident on this machine")?;
                    if resident.owner != message.owner {
                        return Err("network recipient namespace changed".into());
                    }
                }
                crate::mailbox::Address::Client { machine, .. }
                    if machine == state.public_key() && message.owner == *machine => {}
                _ => return Err("network mail recipient is not local".into()),
            }
            jobs.attachments.require(&message.attachments)?;
            let accepted = jobs.mailboxes.accept_network(&remote, message)?;
            jobs.notify();
            send_operation_reply(
                &mut session,
                &mut attempt,
                &json!({"id":accepted.id,"status":"accepted","accepted_on":state.public_key(),"sequence":accepted.seq}),
                None,
            )?;
        } else if request["op"] == "ping" {
            let message = request["message"].as_str().ok_or("message required")?;
            if message.len() > MAX_TEXT {
                return Err("message exceeds 4096 bytes".into());
            }
            session.send(&json!({"from":state.public_key(),"message":message}))?;
        } else {
            let jobs = jobs.ok_or("workload service not available")?;
            if request["op"] == "watch" {
                let run =
                    crate::operator::run(jobs, request["id"].as_str().ok_or("run id required")?)?;
                let mut after = request["after"].as_u64().unwrap_or(0);
                loop {
                    state.require_trust(&remote)?;
                    let (events, snapshot) = run.batch(after, true)?;
                    if let Some(last) = events.last() {
                        after = last.seq;
                    }
                    let done =
                        crate::jobs::terminal(&snapshot.status) && after == snapshot.last_seq;
                    session.send(&json!({"events":events,"snapshot":snapshot,"done":done}))?;
                    if done {
                        break;
                    }
                }
            } else {
                send_operation_reply(
                    &mut session,
                    &mut attempt,
                    &crate::operator::operation(jobs, &remote, &request)?,
                    None,
                )?;
            }
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        let _ = send_operation_reply(
            &mut session,
            &mut attempt,
            &json!({"error":error.to_string()}),
            Some(infrastructure::reason(error.as_ref())),
        );
        return Err(error);
    }
    Ok(())
}
fn send_operation_reply(
    session: &mut Session,
    attempt: &mut Option<Attempt>,
    value: &Value,
    rejection: Option<&'static str>,
) -> Result<()> {
    let attempt = attempt.take();
    if let Some(attempt) = &attempt {
        attempt.record(
            "dispatch",
            if rejection.is_some() {
                "rejected"
            } else {
                "completed"
            },
            rejection,
        );
    }
    session.send(value).inspect_err(|error| {
        if let Some(attempt) = &attempt {
            attempt.record(
                "reply_send",
                "failed",
                Some(infrastructure::reason(error.as_ref())),
            );
        }
    })
}

pub fn serve(listener: TcpListener, state: Arc<State>, jobs: Arc<Jobs>) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                logging::record(
                    Level::Error,
                    Target::Peer,
                    "accept.failed",
                    Context::default(),
                    || json!({"error":error.to_string()}),
                    || Value::Null,
                );
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= 32 {
            logging::record(
                Level::Warn,
                Target::Peer,
                "connection.limit_reached",
                Context::default(),
                || json!({"limit":32}),
                || Value::Null,
            );
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let state = state.clone();
        let active = active.clone();
        let jobs = jobs.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, &state, Some(&jobs)) {
                logging::record(
                    Level::Warn,
                    Target::Peer,
                    "exchange.failed",
                    Context::default(),
                    || json!({"error":error.to_string()}),
                    || Value::Null,
                );
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn next_connection() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> (std::path::PathBuf, Arc<State>) {
        let directory =
            std::env::temp_dir().join(format!("boxen-test-{}", state::random_hex().unwrap()));
        let state = Arc::new(State::open(directory.clone()).unwrap());
        (directory, state)
    }
    #[test]
    fn mutual_trust_identity_pinning_restart_and_revocation() {
        let (a_dir, a) = temporary();
        let (b_dir, b) = temporary();
        let (c_dir, c) = temporary();
        a.trust(b.public_key(), "B").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let worker_b = b.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..4 {
                let (stream, _) = listener.accept().unwrap();
                let _ = handle(stream, &worker_b, None);
            }
        });
        assert!(ping(&a, &address, b.public_key(), "unpaired").is_err());
        b.trust(a.public_key(), "A").unwrap();
        let reopened = State::open(a_dir.clone()).unwrap();
        assert_eq!(a.public_key(), reopened.public_key());
        assert_eq!(
            ping(&reopened, &address, b.public_key(), "hello 🐈")
                .unwrap()
                .message,
            "hello 🐈"
        );
        a.trust(c.public_key(), "C").unwrap();
        assert!(ping(&a, &address, c.public_key(), "wrong identity").is_err());
        b.untrust(a.public_key()).unwrap();
        assert!(ping(&a, &address, b.public_key(), "revoked").is_err());
        worker.join().unwrap();
        for dir in [a_dir, b_dir, c_dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn malformed_keys_and_oversized_messages_fail_before_connecting() {
        let (dir, state) = temporary();
        assert!(state.trust("../oops", "bad").is_err());
        assert!(state.trust(state.public_key(), "self").is_err());
        let pair = snow::Builder::new(params()).generate_keypair().unwrap();
        let peer = state::hex(&pair.public);
        state.trust(&peer, "other").unwrap();
        assert!(
            ping(&state, "invalid", &peer, &"x".repeat(4097))
                .unwrap_err()
                .to_string()
                .contains("4096")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
