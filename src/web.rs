use crate::{Result, protocol, state::State};
use serde::Deserialize;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    time::Duration,
};

const PAGE: &str = include_str!("../web/index.html");

#[derive(Deserialize)]
struct Ping {
    address: String,
    peer: String,
    message: String,
}

pub fn serve(listener: TcpListener, state: Arc<State>, token: String) {
    // A bounded, sequential local control surface; peer connections have their own loop.
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(error) = handle(
                    &mut stream,
                    &state,
                    &token,
                    listener.local_addr().unwrap().port(),
                ) {
                    let _ = respond(&mut stream, 400, "text/plain", error.to_string().as_bytes());
                }
            }
            Err(error) => eprintln!("local UI: {error}"),
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
    Ok(())
}

fn handle(stream: &mut TcpStream, state: &State, token: &str, port: u16) -> Result<()> {
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
            b"Use the localhost control URL printed by Boxen",
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
            b"Open the control URL printed by Boxen, including its fragment",
        );
    }
    match request {
        "GET /api/status HTTP/1.1" => {
            let status =
                serde_json::json!({"identity": state.public_key(), "peers": state.peers()?});
            respond(
                stream,
                200,
                "application/json",
                &serde_json::to_vec(&status)?,
            )
        }
        "POST /api/ping HTTP/1.1" => {
            let length = length.ok_or("content length required")?;
            if length > 8192 {
                return Err("request body too large".into());
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body)?;
            let ping: Ping = serde_json::from_slice(&body)?;
            match protocol::ping(state, &ping.address, &ping.peer, &ping.message) {
                Ok(reply) => respond(
                    stream,
                    200,
                    "application/json",
                    &serde_json::to_vec(&reply)?,
                ),
                Err(error) => respond(stream, 502, "text/plain", error.to_string().as_bytes()),
            }
        }
        _ => respond(stream, 404, "text/plain", b"Not found"),
    }
}
