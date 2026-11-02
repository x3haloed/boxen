//! Read-only history import; future execution resumes the original native thread.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub backend_thread_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub backend_thread_id: String,
    pub metadata: Value,
    pub imported_at: u64,
    pub history_sha256: String,
    pub turn_count: usize,
}

fn native_id(id: &str) -> Result<()> {
    if id.len() != 36
        || !id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return Err("Codex thread import requires a native UUID thread ID".into());
    }
    Ok(())
}

impl Jobs {
    fn prior_import(&self, owner: &str, spec: &Request) -> Result<Option<Snapshot>> {
        valid_id(&spec.id)?;
        native_id(&spec.backend_thread_id)?;
        let runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get(&spec.id) {
            let old = run.snapshot();
            if old.record.owner == owner
                && old
                    .record
                    .imported
                    .as_ref()
                    .is_some_and(|p| p.backend_thread_id == spec.backend_thread_id)
            {
                return Ok(Some(old));
            }
            return Err("import ID already belongs to a different request or owner".into());
        }
        if runs
            .values()
            .any(|r| r.snapshot().backend_thread.as_deref() == Some(&spec.backend_thread_id))
        {
            return Err("native thread is already registered on this daemon; its existing Boxen conversation retains ownership".into());
        }
        if self.residents.all().iter().any(|r| r.id == spec.id) {
            return Err("import ID already belongs to a resident".into());
        }
        Ok(None)
    }

    pub fn import_codex(&self, owner: &str, spec: Request) -> Result<Snapshot> {
        logging::record(
            Level::Info,
            Target::Workload,
            "thread_import.requested",
            Context::run(&spec.id, &spec.id, owner),
            || json!({"backend":"codex","backend_thread_id":spec.backend_thread_id}),
            || Value::Null,
        );
        if let Some(old) = self.prior_import(owner, &spec)? {
            logging::record(
                Level::Debug,
                Target::Workload,
                "thread_import.reused",
                Context::run(&spec.id, &spec.id, owner),
                || json!({"backend_thread_id":spec.backend_thread_id}),
                || Value::Null,
            );
            return Ok(old);
        }
        // Native I/O never holds the workload admission/transition mutex.
        let mut reader = NativeReader::open(&self.codex_path, owner)?;
        let mut metadata = reader.request(
            "thread/read",
            json!({"threadId":spec.backend_thread_id,"includeTurns":false}),
        )?["thread"]
            .clone();
        if metadata["id"] != spec.backend_thread_id {
            return Err("native thread read returned a different identity".into());
        }
        let cwd = metadata["cwd"]
            .as_str()
            .ok_or("native thread has no working directory")?
            .to_string();
        if !std::path::Path::new(&cwd).is_absolute() {
            return Err("native working directory is not absolute on this host".into());
        }
        // Import is possible before workspace repair; execution still checks existence.
        let model = metadata["model"].as_str().unwrap_or("").to_string();
        metadata
            .as_object_mut()
            .ok_or("invalid native thread metadata")?
            .remove("turns");
        if serde_json::to_vec(&metadata)?.len() > 256 * 1024 {
            return Err("native thread metadata exceeds 256 KiB; no import was accepted".into());
        }
        let turns = read_history(&mut reader, &spec.backend_thread_id)?;
        let after = reader.request(
            "thread/read",
            json!({"threadId":spec.backend_thread_id,"includeTurns":false}),
        )?["thread"]
            .clone();
        for key in ["id", "updatedAt", "cwd", "model", "modelProvider"] {
            if metadata[key] != after[key] {
                return Err("native thread changed during import; no snapshot was accepted and a retry can read its current history".into());
            }
        }
        let digest = state::hex(&Sha256::digest(serde_json::to_vec(&turns)?));
        drop(reader);
        let _transition = self.transitions.lock().unwrap();
        if let Some(old) = self.prior_import(owner, &spec)? {
            logging::record(
                Level::Debug,
                Target::Workload,
                "thread_import.reused",
                Context::run(&spec.id, &spec.id, owner),
                || json!({"backend_thread_id":spec.backend_thread_id}),
                || Value::Null,
            );
            return Ok(old);
        }
        let mut runs = self.runs.lock().unwrap();
        let at = now().max(
            runs.values()
                .map(|r| r.snapshot().record.created_at)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        let record = Record {
            imported: Some(Provenance {
                backend_thread_id: spec.backend_thread_id.clone(),
                metadata,
                imported_at: at,
                history_sha256: digest,
                turn_count: turns.len(),
            }),
            observations: Vec::new(),
            scheduled: None,
            resident: None,
            recovery: None,
            mailbox: None,
            spec: Spec {
                context: None,
                resident_id: None,
                id: spec.id.clone(),
                thread_id: spec.id.clone(),
                backend: "codex".into(),
                cwd,
                model,
                prompt: String::new(),
            },
            owner: owner.into(),
            created_at: at,
        };
        let mut events = vec![
            Event {
                seq: 1,
                at,
                kind: "thread_imported".into(),
                data: json!({"backend_thread_id":spec.backend_thread_id,"history":"Read-only snapshot of Codex's public turn/item projection; native session remains the execution continuity authority","turn_count":turns.len()}),
            },
            Event {
                seq: 2,
                at,
                kind: "backend_thread".into(),
                data: json!({"id":spec.backend_thread_id,"imported":true}),
            },
        ];
        let mut append = |kind: &str, data: Value| -> Result<()> {
            if serde_json::to_vec(&data)?.len() > 4 * 1024 * 1024 - 1024 {
                return Err("native history item exceeds the retained event bound; no snapshot was accepted".into());
            }
            events.push(Event {
                seq: events.len() as u64 + 1,
                at,
                kind: kind.into(),
                data,
            });
            Ok(())
        };
        for mut turn in turns {
            let items = turn
                .as_object_mut()
                .ok_or("native turn is not an object")?
                .remove("items")
                .ok_or("native turn has no items")?;
            let turn_id = turn["id"]
                .as_str()
                .ok_or("native turn has no ID")?
                .to_string();
            append("codex_import_turn", json!({"turn":turn,"historical":true}))?;
            for item in items
                .as_array()
                .ok_or("native turn items are not an array")?
            {
                if item["type"] == "userMessage" {
                    let text = item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|c| c["type"] == "text")
                        .filter_map(|c| c["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    append(
                        "user_message",
                        json!({"text":text,"historical":true,"native_turn_id":turn_id,"native_item_id":item["id"]}),
                    )?;
                }
                append(
                    "backend",
                    json!({"method":"item/completed","params":{"threadId":spec.backend_thread_id,"turnId":turn_id,"item":item},"historical":true}),
                )?;
            }
        }
        append(
            "status",
            json!({"status":"completed","detail":"History snapshot imported; no model turn was executed. Original turn statuses are retained separately."}),
        )?;
        let staging = self
            .directory
            .parent()
            .ok_or("runs parent unavailable")?
            .join("import-staging")
            .join(state::random_hex()?);
        state::private_dir(&staging)?;
        state::write_new(&staging.join("record.json"), &serde_json::to_vec(&record)?)?;
        let mut bytes = Vec::new();
        for event in &events {
            serde_json::to_writer(&mut bytes, event)?;
            bytes.push(b'\n');
        }
        state::write_new(&staging.join("events.jsonl"), &bytes)?;
        let mut snapshot = Snapshot {
            record,
            status: "starting".into(),
            backend_thread: None,
            last_seq: 0,
        };
        for event in &events {
            project(&mut snapshot, event);
        }
        // Complete files are published together. Interrupted staging is never a run.
        fs::rename(&staging, self.directory.join(&spec.id))?;
        let file = OpenOptions::new()
            .append(true)
            .open(self.directory.join(&spec.id).join("events.jsonl"))?;
        runs.insert(
            spec.id.clone(),
            Arc::new(Run {
                inner: Mutex::new(Inner {
                    cancel_requested: false,
                    snapshot: snapshot.clone(),
                    events,
                    file,
                    control: None,
                }),
                changed: Condvar::new(),
            }),
        );
        logging::record(
            Level::Info,
            Target::Workload,
            "thread_import.accepted",
            Context::run(&spec.id, &spec.id, owner),
            || json!({"backend_thread_id":spec.backend_thread_id,"turn_count":snapshot.record.imported.as_ref().map(|p|p.turn_count)}),
            || Value::Null,
        );
        Ok(snapshot)
    }
}

fn read_history(reader: &mut NativeReader, id: &str) -> Result<Vec<Value>> {
    let mut turns = Vec::new();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut ids = HashSet::new();
    let mut bytes = 0usize;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if Instant::now() >= deadline {
            return Err(
                "native history exceeded the import deadline; no partial snapshot was accepted"
                    .into(),
            );
        }
        let page=reader.request("thread/turns/list",json!({"threadId":id,"cursor":cursor,"limit":10,"sortDirection":"asc","itemsView":"full"}))?;
        let rows = page["data"]
            .as_array()
            .ok_or("native history returned no turn page")?;
        logging::record(
            Level::Debug,
            Target::Runtime,
            "thread_import.history_page",
            Context::peer(&reader.owner),
            || json!({"backend_thread_id":id,"turns":rows.len(),"has_more":!page["nextCursor"].is_null()}),
            || Value::Null,
        );
        for turn in rows {
            let tid = turn["id"].as_str().ok_or("native history turn has no ID")?;
            if !ids.insert(tid.to_string()) {
                return Err("native history repeated a turn; no snapshot was accepted".into());
            }
            if !turn["items"].is_array() || turn["itemsView"].as_str().is_some_and(|v| v != "full")
            {
                return Err(
                    "native history did not return full turn items; no snapshot was accepted"
                        .into(),
                );
            }
            if turn["status"] == "inProgress" {
                return Err("native thread has an in-progress turn; import can capture its completed history after execution stops".into());
            }
            bytes += serde_json::to_vec(turn)?.len();
            if bytes > 64 * 1024 * 1024 || turns.len() >= 10000 {
                return Err("native history exceeds the 64 MiB/10000-turn import bound; no partial snapshot was accepted".into());
            }
            turns.push(turn.clone());
        }
        match page["nextCursor"].as_str() {
            Some(next) if next.len() <= 4096 && cursors.insert(next.to_string()) => {
                cursor = Some(next.to_string())
            }
            Some(_) => return Err(
                "native history cursor repeated or exceeded its bound; no snapshot was accepted"
                    .into(),
            ),
            None if page["nextCursor"].is_null() => break,
            None => return Err("native history cursor is invalid".into()),
        }
    }
    Ok(turns)
}

/// Only metadata and history reads use this connection. No start/resume/turn calls.
struct NativeReader {
    child: Child,
    stdin: ChildStdin,
    incoming: mpsc::Receiver<std::result::Result<Value, String>>,
    next_id: u64,
    owner: String,
}
impl NativeReader {
    fn open(binary: &str, owner: &str) -> Result<Self> {
        let executable = codex::resolve(binary);
        logging::record(
            Level::Debug,
            Target::Runtime,
            "thread_import.reader_starting",
            Context::peer(owner),
            || json!({"executable":executable}),
            || Value::Null,
        );
        let mut child = Command::new(executable)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or("native reader stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("native reader stdout unavailable")?;
        let (send, incoming) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let result=match reader.by_ref().take(4*1024*1024+1).read_until(b'\n',&mut line){
                    Ok(0)=>break,
                    Ok(_) if line.len()>4*1024*1024=>Err("native read response exceeds 4 MiB; smaller history pages may be available".into()),
                    Ok(_)=>serde_json::from_slice(&line).map_err(|e|format!("invalid native read response: {e}")),
                    Err(e)=>Err(e.to_string()),
                };
                let failed = result.is_err();
                if send.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut this = Self {
            child,
            stdin,
            incoming,
            next_id: 1,
            owner: owner.into(),
        };
        this.request("initialize",json!({"clientInfo":{"name":"boxen","title":"Boxen","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        this.write(json!({"method":"initialized"}))?;
        Ok(this)
    }
    fn write(&mut self, v: Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, &v)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        logging::record(
            Level::Debug,
            Target::Runtime,
            "thread_import.native_request",
            Context::peer(&self.owner),
            || json!({"method":method,"backend_thread_id":params["threadId"],"request_id":id}),
            || Value::Null,
        );
        self.write(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let message = self
                .incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| format!("native {method} unavailable: {e}"))?
                .map_err(|e| format!("native {method}: {e}"))?;
            if message["id"] == id {
                logging::record(
                    Level::Debug,
                    Target::Runtime,
                    "thread_import.native_reply",
                    Context::peer(&self.owner),
                    || json!({"method":method,"request_id":id,"accepted":message["error"].is_null(),"error_code":message["error"]["code"]}),
                    || Value::Null,
                );
                if !message["error"].is_null() {
                    return Err(format!("native {method} rejected: {}", message["error"]).into());
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "native read result unavailable".into());
            }
            if message.get("id").is_some() && message.get("method").is_some() {
                return Err(
                    "native read unexpectedly requested a client action; no action was executed"
                        .into(),
                );
            }
        }
    }
}
impl Drop for NativeReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let status = self.child.wait();
        logging::record(
            Level::Debug,
            Target::Runtime,
            "thread_import.reader_stopped",
            Context::peer(&self.owner),
            || json!({"pid":self.child.id(),"status":status.map(|v|v.to_string()).unwrap_or_else(|e|e.to_string())}),
            || Value::Null,
        );
    }
}
