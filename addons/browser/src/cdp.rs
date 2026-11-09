//! Stateful Chrome DevTools Protocol transport for an already-owned browser.

use crate::Result;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, client::client_with_config, protocol::WebSocketConfig};

const MAX_MESSAGE: usize = 32 * 1024 * 1024;
const MAX_EVENTS: usize = 1024;
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A browser-level CDP connection retains target sessions and queued events
/// across calls. It is scoped to the loopback endpoint supplied by the owner.
pub struct Connection {
    socket: WebSocket<TcpStream>,
    last_id: u64,
    events: VecDeque<(u64, usize, Value)>,
    event_bytes: usize,
    event_cursor: u64,
    dropped_through: u64,
}

impl Connection {
    pub fn connect(port: u16, browser_path: &str) -> Result<Self> {
        let target = browser_path
            .strip_prefix("/devtools/browser/")
            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
            .ok_or("invalid browser debugger path")?;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let url = format!("ws://{address}/devtools/browser/{target}");
        let (socket, _) = client_with_config(url, stream, Some(config))?;
        Ok(Self {
            socket,
            last_id: 0,
            events: VecDeque::new(),
            event_bytes: 0,
            event_cursor: 0,
            dropped_through: 0,
        })
    }

    /// Send a command once. A timeout after sending leaves its effect unknown.
    /// The response contains the original protocol result or error.
    pub fn command(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        self.last_id += 1;
        let id = self.last_id;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        self.socket
            .get_mut()
            .set_write_timeout(Some(timeout.min(CONNECT_TIMEOUT)))?;
        self.socket
            .send(Message::Text(serde_json::to_string(&request)?.into()))
            .map_err(|error| format!("CDP send failed; command outcome is uncertain: {error}"))?;
        loop {
            match self.receive(deadline) {
                Ok(Some(response)) if response["id"].as_u64() == Some(id) => return Ok(response),
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err("CDP response timed out; command outcome is uncertain".into());
                }
                Err(error) => {
                    return Err(format!(
                        "CDP response failed; command outcome is uncertain: {error}"
                    )
                    .into());
                }
            }
        }
    }

    /// Read events after a cursor. A loss watermark makes buffer overflow visible.
    /// Waiting also reads events that arrived when no command was in flight.
    pub fn events(&mut self, after: u64, timeout: Duration) -> Result<Value> {
        if after > self.event_cursor {
            return Err("CDP event cursor is ahead of the connection".into());
        }
        if self.event_cursor <= after && !timeout.is_zero() {
            let deadline = Instant::now() + timeout;
            while self.event_cursor <= after && self.receive(deadline)?.is_some() {}
        }
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|(seq, _, _)| *seq > after)
            .map(|(seq, _, event)| json!({"cursor":seq,"event":event}))
            .collect();
        Ok(json!({
            "cursor": self.event_cursor,
            "dropped_through": self.dropped_through,
            "events": events,
        }))
    }

    fn receive(&mut self, deadline: Instant) -> Result<Option<Value>> {
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            self.socket.get_mut().set_read_timeout(Some(remaining))?;
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let value: Value = serde_json::from_str(&text)?;
                    if value.get("id").is_some() {
                        return Ok(Some(value));
                    }
                    if value.get("method").and_then(Value::as_str).is_some() {
                        self.retain_event(text.len(), value);
                        return Ok(Some(Value::Null));
                    }
                    return Err("invalid CDP message".into());
                }
                Ok(Message::Close(_)) => return Err("CDP connection closed".into()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }

    fn retain_event(&mut self, size: usize, value: Value) {
        self.event_cursor += 1;
        self.event_bytes += size;
        self.events.push_back((self.event_cursor, size, value));
        while self.event_bytes > MAX_EVENT_BYTES || self.events.len() > MAX_EVENTS {
            if let Some((cursor, bytes, _)) = self.events.pop_front() {
                self.event_bytes -= bytes;
                self.dropped_through = cursor;
            }
        }
    }
}
