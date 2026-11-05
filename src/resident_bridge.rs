//! Per-activation MCP projection of the existing daemon-owned agent operations.
//! No global Codex config, new mail store, or independent execution authority.
use crate::{
    Result, agent_tools,
    jobs::{Jobs, Run, terminal},
    logging::Level,
    residents::Resident,
    state,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const TOKEN_ENV: &str = "BOXEN_RESIDENT_BRIDGE_TOKEN";
pub struct Bridge {
    pub token: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Bridge {
    pub fn start(jobs: Arc<Jobs>, run: Arc<Run>, resident: Resident) -> Result<Self> {
        let record = run.snapshot().record;
        if record.resident.as_ref().map(|r| r.id.as_str()) != Some(resident.id.as_str())
            || record.owner != resident.owner
        {
            return Err("resident bridge scope does not match the accepted activation".into());
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let token = state::random_hex()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let secret = token.clone();
        run.diagnostic(
            Level::Info,
            "resident_bridge.started",
            || json!({"address":address.to_string()}),
            || Value::Null,
        );
        let thread = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let result = serve(&mut stream, &secret, &jobs, &run, &resident);
                if let Err(error) = result {
                    run.diagnostic(
                        Level::Warn,
                        "resident_bridge.request_failed",
                        || json!({"error":error.to_string()}),
                        || Value::Null,
                    );
                }
            }
            run.diagnostic(
                Level::Info,
                "resident_bridge.stopped",
                || json!({}),
                || Value::Null,
            );
        });
        Ok(Self {
            token,
            address,
            stop,
            thread: Some(thread),
        })
    }
    pub fn config(&self) -> Value {
        json!({"url":format!("http://{}/mcp",self.address),"bearer_token_env_var":TOKEN_ENV,"enabled":true,"required":true,"startup_timeout_sec":15,"tool_timeout_sec":60,
            "tools":{"boxen_send_message":{"approval_mode":"approve"},"boxen_mailbox":{"approval_mode":"approve"}}})
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn reply(stream: &mut TcpStream, status: &str, value: Option<Value>) -> Result<()> {
    let body = value
        .map(|v| serde_json::to_vec(&v))
        .transpose()?
        .unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}
fn reject(
    reader: &mut BufReader<&mut TcpStream>,
    status: &str,
    length: Option<usize>,
    deadline: Instant,
) -> Result<()> {
    reply(reader.get_mut(), status, None)?;
    // Deliver the response before waiting for a body (including clients waiting
    // for a response before sending it). Closing with unread socket data can
    // otherwise replace the HTTP rejection with a TCP reset on some platforms.
    reader.get_mut().shutdown(Shutdown::Write)?;
    let mut left = length.unwrap_or(1024 * 1024).min(1024 * 1024);
    let mut buffer = [0; 8192];
    while left > 0 {
        reader
            .get_mut()
            .set_read_timeout(Some(remaining(deadline)?))?;
        let size = left.min(buffer.len());
        let n = reader.read(&mut buffer[..size])?;
        if n == 0 {
            break;
        }
        left -= n;
    }
    Ok(())
}
fn serve(
    stream: &mut TcpStream,
    token: &str,
    jobs: &Arc<Jobs>,
    run: &Arc<Run>,
    resident: &Resident,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(&mut *stream);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        reader
            .get_mut()
            .set_read_timeout(Some(remaining(deadline)?))?;
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err("bridge request headers ended early".into());
        }
        let mut consumed = 0;
        for byte in available {
            header.push(*byte);
            consumed += 1;
            if header.len() > 16384 {
                return Err("bridge headers exceed their bound".into());
            }
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        reader.consume(consumed);
    }
    let header = String::from_utf8(header)?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().ok_or("bridge request line unavailable")?;
    let mut authorized = false;
    let mut length = None;
    let mut host_ok = false;
    for line in lines.filter(|line| !line.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "authorization" => authorized = value.trim() == format!("Bearer {token}"),
                "host" => host_ok = value.trim() == stream_address(&reader)?,
                "content-length" => {
                    if length.is_some() {
                        return Err("bridge content length repeated".into());
                    }
                    length = Some(value.trim().parse::<usize>()?);
                }
                "transfer-encoding" => {
                    return Err("bridge accepts bounded content-length requests".into());
                }
                "origin" => {
                    return Err(
                        "bridge is an authenticated runtime endpoint, not a browser origin".into(),
                    );
                }
                _ => {}
            }
        }
    }
    let rejection = if !authorized || !host_ok {
        Some("401 Unauthorized")
    } else if request_line != "POST /mcp HTTP/1.1" {
        Some("405 Method Not Allowed")
    } else {
        None
    };
    if let Some(status) = rejection {
        run.diagnostic(
            Level::Warn,
            "resident_bridge.request_rejected",
            || json!({"status":status}),
            || Value::Null,
        );
        return reject(&mut reader, status, length, deadline);
    }
    let size = length.ok_or("bridge content length unavailable")?;
    if size > 1024 * 1024 {
        return Err("bridge request exceeds 1 MiB".into());
    }
    let mut bytes = vec![0; size];
    let mut offset = 0;
    while offset < size {
        reader
            .get_mut()
            .set_read_timeout(Some(remaining(deadline)?))?;
        let n = reader.read(&mut bytes[offset..])?;
        if n == 0 {
            return Err("bridge request body ended early".into());
        }
        offset += n;
    }
    drop(reader);
    let request: Value = serde_json::from_slice(&bytes)?;
    let Some(id) = request.get("id") else {
        return reply(stream, "202 Accepted", None);
    };
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => Ok(
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"boxen-resident","version":env!("CARGO_PKG_VERSION")}}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let mut tools = agent_tools::resident_tools();
            tools.extend(agent_tools::observation_tools());
            for tool in &mut tools {
                tool.as_object_mut().unwrap().remove("type");
            }
            Ok(json!({"tools":tools}))
        }
        "tools/call" => call(jobs, run, resident, id, &request["params"]),
        _ => Err("bridge method is unavailable".into()),
    };
    let response = match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":error.to_string()}})
        }
    };
    reply(stream, "200 OK", Some(response))
}
fn stream_address(reader: &BufReader<&mut TcpStream>) -> Result<String> {
    Ok(reader.get_ref().local_addr()?.to_string())
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|v| !v.is_zero())
        .ok_or_else(|| "bridge request deadline elapsed".into())
}

fn call(
    jobs: &Arc<Jobs>,
    run: &Arc<Run>,
    resident: &Resident,
    id: &Value,
    params: &Value,
) -> Result<Value> {
    let snapshot = run.snapshot();
    if terminal(&snapshot.status) {
        return Err(
            "activation has ended; its resident bridge no longer accepts tool effects".into(),
        );
    }
    let call_id = format!("mcp:{id}");
    if call_id.len() > 512 {
        return Err("bridge call ID exceeds its bound".into());
    }
    if let Some(old) = run
        .retained_events("resident_bridge_call")
        .into_iter()
        .find(|e| e.data["call_id"] == call_id)
    {
        if old.data["params"] != *params {
            return Err("bridge call ID already identifies another input".into());
        }
        return match run
            .retained_events("resident_bridge_result")
            .into_iter()
            .find(|e| e.data["call_id"] == call_id)
        {
            Some(result) => Ok(result.data["result"].clone()),
            None => Ok(
                json!({"isError":true,"content":[{"type":"text","text":"The earlier tool outcome is unknown. This retry did not repeat its effects."}]}),
            ),
        };
    }
    let tool = params["name"]
        .as_str()
        .ok_or("bridge tool name unavailable")?;
    run.append(
        "resident_bridge_call",
        json!({"call_id":call_id,"params":params}),
    )?;
    let args = params["arguments"].clone();
    let result = if matches!(tool, "boxen_observations" | "boxen_adapter") {
        agent_tools::observation_tool(jobs, run, &call_id, tool, args)
    } else {
        agent_tools::resident_tool(
            jobs,
            resident,
            &snapshot.record.spec.id,
            &call_id,
            tool,
            args,
        )
    };
    let success = result.is_ok();
    let output = match result {
        Ok(v) => v,
        Err(e) => json!({"error":e.to_string()}),
    };
    let result = json!({"isError":!success,"content":[{"type":"text","text":output.to_string()}]});
    run.append(
        "resident_bridge_result",
        json!({"call_id":call_id,"tool":tool,"result":result}),
    )?;
    run.diagnostic(
        if success { Level::Info } else { Level::Warn },
        "resident_bridge.tool_completed",
        || json!({"call_id":call_id,"tool":tool,"success":success}),
        || output,
    );
    Ok(result)
}
