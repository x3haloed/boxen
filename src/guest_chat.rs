//! Loopback browser surface for one bundled messaging connector instance.
use crate::{
    Result,
    attachments::{Attachment, FILE_LIMIT, MESSAGE_LIMIT, Store},
    jobs,
    messaging::{Binding, Definition, Instance},
    state,
};
use hmac::{Hmac, Mac};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    fs::{self, File},
    io::{BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub const PROVIDER: &str = "guest_chat";
pub const MAX_MESSAGE: usize = 8 * 1024;
const PAGE: &str = include_str!("../web/guest-chat.html");

pub fn binding(definition: &Definition) -> Result<&Binding> {
    if definition.provider != PROVIDER
        || !definition.tools.is_empty()
        || definition.credential_file.is_some()
        || !definition.capabilities.iter().any(|c| c == "receive")
        || !definition.capabilities.iter().any(|c| c == "send")
    {
        return Err("guest chat instances support correspondence with a single binding".into());
    }
    match definition.bindings.as_slice() {
        [binding] if binding.receive && binding.send && !binding.accept_bots => Ok(binding),
        _ => Err("guest chat requires one two-way binding for human messages".into()),
    }
}

struct Control {
    agent: ureq::Agent,
    url: String,
    peer: String,
    token: String,
}

impl Control {
    fn open(home: &Path, args: &mut Vec<String>) -> Result<Self> {
        let url =
            crate::take_option(args, "--url")?.unwrap_or_else(|| "http://127.0.0.1:42425".into());
        let origin = url
            .strip_prefix("http://")
            .ok_or("control URL uses loopback HTTP")?;
        let url = format!("http://{}", loopback(origin)?);
        let token = fs::read_to_string(home.join("control-token"))?;
        let token = token.trim();
        state::key(token)?;
        let token = format!("Bearer {token}");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let status: Value = agent
            .get(format!("{url}/api/status"))
            .header("Authorization", &token)
            .call()?
            .body_mut()
            .with_config()
            .limit(512 * 1024)
            .read_json()?;
        let peer = status["identity"]
            .as_str()
            .ok_or("local daemon identity unavailable")?
            .to_owned();
        state::key(&peer)?;
        Ok(Self {
            agent,
            url,
            peer,
            token,
        })
    }

    fn call(&self, instance: Option<&str>, args: Value) -> Result<Value> {
        let mut response = self
            .agent
            .post(format!("{}/api/operation", self.url))
            .header("Authorization", &self.token)
            .send_json(json!({
                "peer":self.peer, "connector_instance":instance,
                "request":{"op":"messaging","args":args},
            }))?;
        Ok(response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_json()?)
    }
}

fn loopback(address: &str) -> Result<SocketAddr> {
    let address: SocketAddr = address.parse()?;
    if !address.ip().is_loopback() {
        return Err("guest chat uses a loopback address".into());
    }
    Ok(address)
}

pub fn create(home: &Path, mut args: Vec<String>) -> Result<()> {
    let name = crate::take_option(&mut args, "--name")?.unwrap_or_else(|| "Guest chat".into());
    let control = Control::open(home, &mut args)?;
    let [resident] = args.as_slice() else {
        return Err("guest-chat-create takes one resident ID".into());
    };
    jobs::valid_id(resident)?;
    let instance = state::random_hex()?;
    let value = control.call(
        None,
        json!({
            "action":"create", "definition":{
                "id":instance, "name":name, "provider":PROVIDER, "account":"guest",
                "capabilities":["receive","send","replies"],
                "bindings":[{"conversation":"chat","resident":resident}],
            },
        }),
    )?;
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

pub fn run(home: &Path, mut args: Vec<String>) -> Result<()> {
    let instance = crate::take_option(&mut args, "--instance")?
        .ok_or("--instance is a configured guest chat connector ID")?;
    jobs::valid_id(&instance)?;
    let listen = loopback(
        &crate::take_option(&mut args, "--listen")?.unwrap_or_else(|| "127.0.0.1:42427".into()),
    )?;
    let control = Control::open(home, &mut args)?;
    if !args.is_empty() {
        return Err("unexpected guest-chat arguments".into());
    }
    let configured: Instance = serde_json::from_value(
        control.call(Some(&instance), json!({"action":"get","instance":instance}))?,
    )?;
    binding(&configured.definition)?;

    let listener = TcpListener::bind(listen)?;
    let host = listener.local_addr()?.to_string();
    println!("Guest chat origin: http://{host}");
    let server = Arc::new(Server {
        control,
        instance,
        host,
        attachments: Store::new(home.canonicalize()?.join("attachments")),
        active: AtomicUsize::new(0),
        uploads: AtomicUsize::new(0),
    });
    for stream in listener.incoming() {
        let mut stream = stream?;
        if server.active.fetch_add(1, Ordering::Relaxed) >= 16 {
            server.active.fetch_sub(1, Ordering::Relaxed);
            continue;
        }
        let connection = server.clone();
        let worker = std::thread::Builder::new().spawn(move || {
            if connection.handle(&mut stream).is_err() {
                let _ = respond(&mut stream, 400, "text/plain", b"Invalid chat request");
            }
            connection.active.fetch_sub(1, Ordering::Relaxed);
        });
        if worker.is_err() {
            server.active.fetch_sub(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

struct Server {
    control: Control,
    instance: String,
    host: String,
    attachments: Store,
    active: AtomicUsize,
    uploads: AtomicUsize,
}

struct UploadSlot<'a>(&'a AtomicUsize);

impl Drop for UploadSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    id: String,
    body: String,
    #[serde(default)]
    attachments: Vec<Upload>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    file: Attachment,
    proof: String,
}

impl Server {
    fn handle(&self, stream: &mut TcpStream) -> Result<()> {
        let timeout = Duration::from_secs(2);
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let deadline = Instant::now() + timeout;
        // Read the request head separately so early rejection can still consume
        // a bounded body and avoid resetting the response on socket close.
        let mut reader = BufReader::new(&mut *stream);
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            if bytes.len() >= 16 * 1024 || Instant::now() >= deadline {
                return Err("request headers exceeded their bound".into());
            }
            let mut byte = [0];
            reader.read_exact(&mut byte)?;
            bytes.push(byte[0]);
        }
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut request = httparse::Request::new(&mut headers);
        if !request.parse(&bytes)?.is_complete() || request.version != Some(1) {
            return Err("expected a complete HTTP/1.1 request".into());
        }
        let header = |name: &str| -> Result<Option<&str>> {
            let mut values = request
                .headers
                .iter()
                .filter(|h| h.name.eq_ignore_ascii_case(name));
            let value = values
                .next()
                .map(|h| std::str::from_utf8(h.value))
                .transpose()?;
            if values.next().is_some() {
                return Err("duplicate HTTP header".into());
            }
            Ok(value)
        };
        if header("host")? != Some(self.host.as_str()) {
            return respond(stream, 403, "text/plain", b"Chat origin does not match");
        }
        if header("transfer-encoding")?.is_some() {
            return Err("chat requires a fixed-length request".into());
        }
        let length = header("content-length")?
            .map(str::parse::<usize>)
            .transpose()?
            .unwrap_or(0);
        let raw_path = request.path.unwrap_or("");
        let (path, query) = raw_path.split_once('?').unwrap_or((raw_path, ""));
        let upload = request.method == Some("POST") && path == "/uploads";
        let limit = if upload {
            FILE_LIMIT as usize
        } else {
            MAX_MESSAGE * 6 + 8192
        };
        if length > limit {
            return respond(stream, 413, "text/plain", b"Chat request is too large");
        }
        let same_origin = header("sec-fetch-site")?.is_none_or(|value| value == "same-origin");
        let file_read = request.method == Some("GET") && path.starts_with("/attachments/");
        let allowed = same_origin
            && (header("x-boxen-chat")? == Some("1")
                || (file_read && header("sec-fetch-site")? == Some("same-origin")));
        if upload && !allowed && length > MAX_MESSAGE * 6 {
            return respond(
                stream,
                403,
                "text/plain",
                b"Chat requests come from the chat page",
            );
        }
        let _slot = if upload && allowed {
            if self.uploads.fetch_add(1, Ordering::Relaxed) >= 4 {
                self.uploads.fetch_sub(1, Ordering::Relaxed);
                return respond(stream, 503, "text/plain", b"Chat uploads are busy");
            }
            Some(UploadSlot(&self.uploads))
        } else {
            None
        };
        let deadline = if upload {
            Instant::now() + Duration::from_secs(120)
        } else {
            deadline
        };
        let mut body = vec![0; length];
        let mut read = 0;
        while read < body.len() {
            if Instant::now() >= deadline {
                return Err("chat body read timed out".into());
            }
            let count = reader.read(&mut body[read..])?;
            if count == 0 {
                return Err("chat body ended early".into());
            }
            read += count;
        }
        if request.method == Some("GET") && raw_path == "/" {
            return respond(stream, 200, "text/html; charset=utf-8", PAGE.as_bytes());
        }
        if path != "/messages" && !upload && !file_read {
            return respond(stream, 404, "text/plain", b"Page not found");
        }
        if !allowed {
            return respond(
                stream,
                403,
                "text/plain",
                b"Chat requests come from the chat page",
            );
        }
        if upload && header("content-type")? == Some("application/octet-stream") {
            let name = percent_decode_str(
                query
                    .strip_prefix("name=")
                    .ok_or("attachment name required")?,
            )
            .decode_utf8()?;
            return match self.upload(&name, &body) {
                Ok(value) => respond(
                    stream,
                    200,
                    "application/json",
                    &serde_json::to_vec(&value)?,
                ),
                Err(_) => respond(
                    stream,
                    422,
                    "text/plain",
                    b"Boxen could not retain this attachment",
                ),
            };
        }
        if file_read {
            return self.attachment(stream, &path["/attachments/".len()..], query);
        }
        let args = match request.method {
            Some("GET") => {
                let after = if query.is_empty() {
                    0
                } else {
                    query
                        .strip_prefix("after=")
                        .ok_or("invalid chat cursor")?
                        .parse::<u64>()?
                };
                json!({"action":"guest_chat_read","instance":self.instance,"after":after})
            }
            Some("POST")
                if query.is_empty() && header("content-type")? == Some("application/json") =>
            {
                let submission: Submission = serde_json::from_slice(&body)?;
                jobs::valid_id(&submission.id)?;
                if (submission.body.trim().is_empty() && submission.attachments.is_empty())
                    || submission.body.len() > MAX_MESSAGE
                    || submission.attachments.len() > MESSAGE_LIMIT
                {
                    return respond(
                        stream,
                        422,
                        "text/plain",
                        b"Enter a message of up to 8192 bytes or attach up to eight files",
                    );
                }
                let mut attachments = Vec::with_capacity(submission.attachments.len());
                for upload in submission.attachments {
                    let proof = state::key(&upload.proof)?;
                    if self
                        .upload_proof(&upload.file)?
                        .verify_slice(&proof)
                        .is_err()
                    {
                        return respond(
                            stream,
                            422,
                            "text/plain",
                            b"Attachment was not uploaded to this chat",
                        );
                    }
                    attachments.push(upload.file);
                }
                json!({"action":"guest_chat_send","instance":self.instance,"id":submission.id,"body":submission.body,"attachments":attachments})
            }
            _ => return respond(stream, 405, "text/plain", b"Unsupported chat request"),
        };
        match self.control.call(Some(&self.instance), args) {
            Ok(value) => respond(
                stream,
                200,
                "application/json",
                &serde_json::to_vec(&value)?,
            ),
            Err(error)
                if matches!(
                    error.downcast_ref::<ureq::Error>(),
                    Some(ureq::Error::StatusCode(422))
                ) =>
            {
                respond(
                    stream,
                    422,
                    "text/plain",
                    b"Boxen declined this chat request",
                )
            }
            Err(_) => respond(
                stream,
                503,
                "text/plain",
                b"Chat is temporarily unavailable",
            ),
        }
    }

    fn upload_proof(&self, file: &Attachment) -> Result<Hmac<Sha256>> {
        let mut proof = Hmac::<Sha256>::new_from_slice(self.control.token.as_bytes())?;
        proof.update(&serde_json::to_vec(&json!([
            "boxen-guest-chat-upload-v1",
            self.instance,
            file
        ]))?);
        Ok(proof)
    }

    fn upload(&self, name: &str, bytes: &[u8]) -> Result<Value> {
        let file =
            crate::attachments::capture(name, bytes, |transfer| {
                self.control.call(Some(&self.instance),
                json!({"action":"guest_chat_upload","instance":self.instance,"transfer":transfer}))
            })?;
        let proof = self.upload_proof(&file)?.finalize().into_bytes();
        Ok(json!({"file":file,"proof":state::hex(&proof)}))
    }

    fn attachment(&self, stream: &mut TcpStream, path: &str, query: &str) -> Result<()> {
        let (id, index) = path
            .split_once('/')
            .ok_or("attachment path requires a message and index")?;
        jobs::valid_id(id)?;
        let index = index.parse::<usize>()?;
        let value = self.control.call(Some(&self.instance), json!({
            "action":"guest_chat_attachment","instance":self.instance,"message_id":id,"index":index
        }));
        let file: Attachment = match value {
            Ok(value) => serde_json::from_value(value)?,
            Err(_) => return respond(stream, 404, "text/plain", b"Attachment unavailable"),
        };
        let path = match self.attachments.verify(&file) {
            Ok(path) => path,
            Err(_) => return respond(stream, 404, "text/plain", b"Attachment unavailable"),
        };
        let mime = if query.is_empty() {
            self.attachments.preview_mime(&file)
        } else {
            None
        };
        let disposition = if mime.is_some() {
            "inline"
        } else {
            "attachment"
        };
        let name = utf8_percent_encode(&file.name, NON_ALPHANUMERIC);
        response_head(
            stream,
            200,
            mime.unwrap_or("application/octet-stream"),
            file.size,
            &format!("Content-Disposition: {disposition}; filename*=UTF-8''{name}\r\n"),
        )?;
        std::io::copy(&mut File::open(path)?.take(file.size), stream)?;
        Ok(())
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    response_head(stream, status, content_type, body.len() as u64, "")?;
    stream.write_all(body)?;
    Ok(())
}

fn response_head(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    length: u64,
    extra: &str,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; img-src 'self' blob:; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n{extra}\r\n",
    )?;
    Ok(())
}
