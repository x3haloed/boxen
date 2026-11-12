use crate::model::Destination;
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

#[derive(Clone)]
pub struct Client {
    origin: String,
    token: String,
    agent: ureq::Agent,
    local_key: Arc<std::sync::OnceLock<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    pub destination: Destination,
    pub spec: Value,
}
pub enum Update {
    Mail(crate::correspondence::MailUpdate),
    Files(crate::attachments::FileUpdate),
    Steered(crate::model::ThreadKey, Result<Value, String>, bool),
    Discovered(Result<Value, String>),
    Catalog(Destination, Result<Vec<Value>, String>),
    Batch(String, u64, Value),
    WatchError(String, u64, String),
    Followup(String, Result<Pending, String>),
    Submitted(String, Result<Value, String>),
    Controlled(String, Destination, String, Result<Value, String>),
}
pub struct Bus {
    pub tx: SyncSender<Update>,
    pub rx: Receiver<Update>,
    pub ctx: egui::Context,
}
impl Bus {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::sync_channel(256);
        Self { tx, rx, ctx }
    }
    pub fn publisher(&self) -> Publisher {
        Publisher {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        }
    }
}
#[derive(Clone)]
pub struct Publisher {
    tx: SyncSender<Update>,
    ctx: egui::Context,
}
impl Publisher {
    pub(crate) fn send(&self, event: Update) -> bool {
        if self.tx.send(event).is_err() {
            return false;
        }
        self.ctx.request_repaint();
        true
    }
}
impl Client {
    pub fn new(origin: String, home: &Path) -> Result<Self, String> {
        // A local capability is never transmitted to a peer or an arbitrary URL.
        let authority = origin
            .strip_prefix("http://")
            .unwrap_or("")
            .trim_end_matches('/');
        let loopback = authority
            .parse::<std::net::SocketAddr>()
            .is_ok_and(|a| a.ip().is_loopback() && a.port() != 0);
        if !loopback {
            return Err("The desktop connection requires a loopback HTTP origin".into());
        }
        let token = std::fs::read_to_string(home.join("control-token"))
            .map_err(|e| format!("Cannot read daemon capability: {e}"))?;
        Ok(Self {
            origin: origin.trim_end_matches('/').into(),
            token: token.trim().into(),
            local_key: Default::default(),
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(8)))
                .build()
                .into(),
        })
    }
    pub fn discover(&self, publisher: Publisher) {
        let client = self.clone();
        std::thread::spawn(move || {
            publisher.send(Update::Discovered(client.status()));
        });
    }
    pub fn status(&self) -> Result<Value, String> {
        let mut response = self
            .agent
            .get(format!("{}/api/status", self.origin))
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .map_err(safe_error)?;
        if !response.status().is_success() {
            return Err(format!("Daemon status: {}", response.status()));
        }
        let status: Value = response.body_mut().read_json().map_err(safe_error)?;
        if let Some(id) = status["identity"].as_str() {
            let _ = self.local_key.set(id.into());
        }
        Ok(status)
    }
    pub fn local(&self) -> Destination {
        Destination {
            key: self.local_key.get().cloned().unwrap_or_default(),
            address: String::new(),
            label: "This machine".into(),
        }
    }
    pub fn operation(&self, dest: &Destination, request: Value) -> Result<Value, String> {
        let mut response = self
            .agent
            .post(format!("{}/api/operation", self.origin))
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(json!({"peer":dest.key,"address":dest.address,"request":request}))
            .map_err(safe_error)?;
        if !response.status().is_success() {
            let code = response.status().as_u16();
            let detail = response
                .body_mut()
                .with_config()
                .limit(4096)
                .read_to_string()
                .unwrap_or_default();
            return Err(format!("HTTP {code}: {detail}"));
        }
        response.body_mut().read_json().map_err(safe_error)
    }
    pub fn catalog(&self, dest: Destination, publisher: Publisher, stop: Arc<AtomicBool>) {
        let client = self.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let result = client.run_catalog(&dest, &stop);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if !publisher.send(Update::Catalog(dest.clone(), result)) {
                    break;
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }
    fn run_catalog(&self, dest: &Destination, stop: &AtomicBool) -> Result<Vec<Value>, String> {
        let mut runs = Vec::new();
        let mut after = Value::Null;
        while !stop.load(Ordering::Relaxed) {
            let page = self.operation(dest, json!({"op":"list","summary_page":{"after":after}}))?;
            // An older daemon treats this as a full list request.
            if after.is_null()
                && let Some(legacy) = page.as_array()
            {
                return Ok(legacy.clone());
            }
            runs.extend(
                page["summaries"]
                    .as_array()
                    .ok_or("Invalid run summaries")?
                    .iter()
                    .cloned(),
            );
            let next = page.get("next_after").ok_or("Missing run summary cursor")?;
            if next.is_null() {
                return Ok(runs);
            }
            let cursor =
                |v: &Value| Some((v["created_at"].as_u64()?, v["id"].as_str()?.to_owned()));
            if cursor(next).is_none_or(|next| Some(next) <= cursor(&after)) {
                return Err("Run summary cursor did not advance".into());
            }
            after = next.clone();
        }
        Err("Run catalog poll stopped".into())
    }
    pub fn followup(
        &self,
        destination: Destination,
        previous: String,
        id: String,
        text: String,
        files: Vec<crate::attachments::FileRef>,
        publisher: Publisher,
    ) {
        let client = self.clone();
        std::thread::spawn(move || {
            let tag = run_key(&destination.key, &previous);
            let result = client
                .operation(&destination, json!({"op":"get","id":previous}))
                .and_then(|snapshot| {
                    let mut spec = snapshot["record"]["spec"].clone();
                    if spec["id"] != previous || !spec["thread_id"].is_string() {
                        return Err("Invalid follow-up source run".into());
                    }
                    spec["id"] = json!(id);
                    spec["prompt"] = json!(text);
                    if files.is_empty() {
                        if let Some(spec) = spec.as_object_mut() {
                            spec.remove("attachments");
                        }
                    } else {
                        spec["attachments"] = json!(files);
                    }
                    Ok(Pending { destination, spec })
                });
            publisher.send(Update::Followup(tag, result));
        });
    }
    pub fn watch(
        &self,
        dest: Destination,
        id: String,
        after: u64,
        epoch: u64,
        publisher: Publisher,
        stop: Arc<AtomicBool>,
    ) {
        let client = self.clone();
        std::thread::spawn(move || {
            let tag = run_key(&dest.key, &id);
            let result = (|| -> Result<(), String> {
                #[cfg(not(test))]
                let lease = Duration::from_secs(8);
                #[cfg(test)]
                let lease = Duration::from_millis(500);
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .timeout_connect(Some(Duration::from_secs(3)))
                    // The loopback controller authenticates the remote peer before
                    // sending headers; address-family fallback can take more than five seconds.
                    .timeout_recv_response(Some(Duration::from_secs(12)))
                    .timeout_recv_body(Some(lease))
                    .build()
                    .into();
                let mut cursor = after;
                'connect: while !stop.load(Ordering::Relaxed) {
                    let mut response=agent.post(format!("{}/api/watch",client.origin)).header("Authorization",format!("Bearer {}",client.token)).send_json(json!({"peer":dest.key,"address":dest.address,"request":{"id":id,"after":cursor}})).map_err(safe_error)?;

                    let mut reader = BufReader::new(response.body_mut().as_reader());
                    while !stop.load(Ordering::Relaxed) {
                        let mut line = String::new();
                        let n = match std::io::Read::take(&mut reader, 8 * 1024 * 1024)
                            .read_line(&mut line)
                        {
                            Ok(n) => n,
                            // A finite observation lease bounds detached workers. Renewals resume
                            // immediately at the last losslessly published sequence, without a UI outage.
                            Err(ref e)
                                if e.get_ref()
                                    .and_then(|e| e.downcast_ref::<ureq::Error>())
                                    .is_some_and(|e| {
                                        matches!(e, ureq::Error::Timeout(ureq::Timeout::RecvBody))
                                    }) =>
                            {
                                continue 'connect;
                            }
                            Err(e) => return Err(safe_error(e)),
                        };
                        if n == 0 {
                            return Err("Observation connection ended; reconnecting".into());
                        }
                        if !line.ends_with('\n') {
                            return Err("Observation batch exceeded the client bound".into());
                        }
                        let batch: Value = serde_json::from_str(&line).map_err(safe_error)?;
                        if let Some(error) = batch["error"].as_str() {
                            return Err(error.into());
                        }
                        let done = batch["done"] == true;
                        if let Some(seq) = batch["events"]
                            .as_array()
                            .and_then(|a| a.last())
                            .and_then(|e| e["seq"].as_u64())
                        {
                            cursor = seq;
                        }
                        if !publisher.send(Update::Batch(tag.clone(), epoch, batch)) {
                            return Ok(());
                        }
                        if done {
                            return Ok(());
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                publisher.send(Update::WatchError(tag, epoch, error));
            }
        });
    }
    pub fn submit(&self, pending: Pending, publisher: Publisher) {
        let client = self.clone();
        std::thread::spawn(move || {
            let id = pending.spec["id"].as_str().unwrap_or("").to_owned();
            let result = client
                .copy_files(&pending.destination, &pending.spec["attachments"])
                .and_then(|_| {
                    client.operation(
                        &pending.destination,
                        json!({"op":"start","spec":pending.spec}),
                    )
                });
            publisher.send(Update::Submitted(id, result));
        });
    }
    pub fn control(&self, dest: Destination, id: String, action: Value, publisher: Publisher) {
        let client = self.clone();
        std::thread::spawn(move || {
            let text = action["text"].as_str().unwrap_or("").to_owned();
            let result = client.operation(&dest, json!({"op":"control","id":id,"action":action}));
            publisher.send(Update::Controlled(id, dest, text, result));
        });
    }
}
fn safe_error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn run_key(peer: &str, id: &str) -> String {
    format!("{peer}:{id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_remote_setup_still_delivers_the_retained_batch() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let home = std::env::temp_dir().join(format!("boxen-delayed-view-{}", crate::new_id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("control-token"), "fixture-only").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            std::thread::sleep(Duration::from_secs(6));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n{\"events\":[{\"seq\":1,\"kind\":\"status\",\"data\":{\"status\":\"completed\"}}],\"done\":true}\n").unwrap();
        });
        let client = Client::new(format!("http://{address}"), &home).unwrap();
        let bus = Bus::new(egui::Context::default());
        client.watch(
            Destination {
                key: "peer".into(),
                label: "Host".into(),
                address: "example.invalid:42424".into(),
            },
            "run".into(),
            0,
            42,
            bus.publisher(),
            Arc::new(AtomicBool::new(false)),
        );
        match bus.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Update::Batch(_, 42, batch) => assert_eq!(batch["events"][0]["seq"], 1),
            _ => panic!("Delayed setup did not produce a retained batch"),
        }
        server.join().unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn capability_stays_on_loopback() {
        for url in [
            "https://example.com",
            "http://127.0.0.1:80.evil.test",
            "http://127.0.0.1:80@evil.test",
            "http://127.0.0.1:80/path",
            "http://192.168.1.1:80",
        ] {
            assert!(
                Client::new(url.into(), Path::new("/does-not-exist"))
                    .unwrap_err_or_message()
                    .contains("loopback")
            );
        }
    }
    trait ErrorMessage {
        fn unwrap_err_or_message(self) -> String;
    }
    impl ErrorMessage for Result<Client, String> {
        fn unwrap_err_or_message(self) -> String {
            match self {
                Err(e) => e,
                Ok(_) => panic!("Untrusted capability destination accepted"),
            }
        }
    }
    #[test]
    #[ignore = "scripts/desktop-smoke.py supplies private paired daemons"]
    fn real_daemon_boundary() {
        let home = std::path::PathBuf::from(
            std::env::var_os("BOXEN_GUI_TEST_HOME").expect("fixture home"),
        );
        let client = Client::new(std::env::var("BOXEN_GUI_TEST_URL").unwrap(), &home).unwrap();
        let dest: Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        let id = crate::new_id();
        let spec = json!({"id":id,"thread_id":id,"backend":"codex","cwd":home,"model":"fixture","prompt":"SLOW desktop transport proof"});
        let first = client
            .operation(&dest, json!({"op":"start","spec":spec}))
            .unwrap();
        assert_eq!(first["record"]["spec"]["id"], id);
        let bus = Bus::new(egui::Context::default());
        let stop = Arc::new(AtomicBool::new(false));
        client.watch(
            dest.clone(),
            id.clone(),
            0,
            0,
            bus.publisher(),
            stop.clone(),
        );
        let mut transcript = crate::model::Transcript::default();
        let mut saw_live = false;
        while !saw_live {
            match bus.rx.recv_timeout(Duration::from_secs(12)).unwrap() {
                Update::Batch(_, _, batch) => {
                    for event in batch["events"].as_array().unwrap() {
                        transcript.apply(event).unwrap();
                        if event["data"]["method"] == "item/commandExecution/outputDelta" {
                            saw_live = true;
                            assert_ne!(batch["done"], true);
                        }
                    }
                }
                Update::WatchError(_, _, error) => panic!("{error}"),
                _ => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        let cursor = transcript.seq;
        let replay = Bus::new(egui::Context::default());
        client.watch(
            dest.clone(),
            id.clone(),
            cursor,
            0,
            replay.publisher(),
            Arc::new(AtomicBool::new(false)),
        );
        loop {
            match replay.rx.recv_timeout(Duration::from_secs(12)).unwrap() {
                Update::Batch(_, _, batch) => {
                    for event in batch["events"].as_array().unwrap() {
                        assert!(event["seq"].as_u64().unwrap() > cursor);
                        transcript.apply(event).unwrap();
                    }
                    if batch["done"] == true {
                        break;
                    }
                }
                Update::WatchError(_, _, error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(transcript.rows.iter().any(|r| r.label == "Thinking"));
        assert!(
            transcript
                .rows
                .iter()
                .any(|r| r.text.contains("Fixture result"))
        );
        // Same-ID submission is an observation-safe reconciliation, never a second accepted effect.
        client
            .operation(&dest, json!({"op":"start","spec":spec}))
            .unwrap();
        let runs = client.run_catalog(&dest, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            runs.iter()
                .filter(|r| r["record"]["spec"]["id"] == id)
                .count(),
            1
        );
        let next = crate::new_id();
        let followup = Bus::new(egui::Context::default());
        use base64::{Engine, engine::general_purpose::STANDARD};
        use sha2::{Digest, Sha256};
        let original = b"fixture follow-up attachment";
        let file = crate::attachments::FileRef {
            name: "fixture.txt".into(),
            digest: format!("{:x}", Sha256::digest(original)),
            size: original.len() as u64,
        };
        client.status().unwrap();
        client.operation(&client.local(),json!({"op":"attachment","action":"write","file":file,"offset":0,"data":STANDARD.encode(original)})).unwrap();
        client.followup(
            dest.clone(),
            id,
            next.clone(),
            "Follow-up".into(),
            vec![file.clone()],
            followup.publisher(),
        );
        let pending = match followup.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Update::Followup(_, result) => result.unwrap(),
            _ => panic!("Catalog entry could not prepare a follow-up"),
        };
        let mut expected = first["record"]["spec"].clone();
        expected["id"] = json!(next);
        expected["prompt"] = json!("Follow-up");
        expected["attachments"] = json!([file]);
        assert_eq!(pending.spec, expected);
        let submitted = Bus::new(egui::Context::default());
        client.submit(pending, submitted.publisher());
        match submitted.rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Update::Submitted(id, result) => {
                assert_eq!(id, next);
                assert_eq!(result.unwrap()["record"]["spec"], expected);
            }
            _ => panic!("Follow-up with the original was not accepted"),
        }
        let retained = client
            .operation(
                &dest,
                json!({"op":"attachment","action":"read","file":file,"offset":0}),
            )
            .unwrap();
        assert_eq!(
            STANDARD.decode(retained["data"].as_str().unwrap()).unwrap(),
            original
        );
        println!(
            "Remote native client: thinking, incremental tool output, detached replay, final response, exact-ID retry and attachment follow-up passed"
        );
    }
}
