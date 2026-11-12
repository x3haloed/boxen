//! Anonymous conversation handoff over the authenticated machine transport.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const CHUNK: usize = 256 * 1024;
const MAX_FILES: usize = 16_384;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Move {
    id: String,
    pub thread: String,
    pub source: String,
    pub target: String,
    address: String,
    head: String,
    generation: u64,
    requested_cwd: Option<String>,
    requested_model: Option<String>,
    pub cwd: String,
    pub model: String,
    backend: String,
    files: Vec<RetainedFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RetainedFile {
    run: String,
    name: String,
    size: u64,
    digest: String,
}

impl RetainedFile {
    fn path(&self, runs: &Path) -> PathBuf {
        runs.join(&self.run).join(&self.name)
    }
    fn measure(root: &Path, run: &str, name: String) -> Result<Self> {
        let mut file = Self {
            run: run.into(),
            name,
            size: 0,
            digest: String::new(),
        };
        let path = file.path(root);
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return Err("retained run file is not transferable".into());
        }
        file.size = meta.len();
        file.digest = digest(&path, file.size)?;
        Ok(file)
    }
    fn validate(&self) -> Result<()> {
        valid_id(&self.run)?;
        valid_id(&self.digest)?;
        if let Some(name) = self.name.strip_prefix("artifacts/") {
            valid_id(name)?;
        } else if !matches!(
            self.name.as_str(),
            "record.json" | "events.jsonl" | "origin"
        ) {
            return Err("invalid retained run filename".into());
        }
        Ok(())
    }
}

fn digest(path: &Path, size: u64) -> Result<String> {
    let mut hash = Sha256::new();
    let count = std::io::copy(&mut File::open(path)?.take(size + 1), &mut hash)?;
    if count != size {
        return Err("retained run file size differs from its manifest".into());
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl Move {
    pub(super) fn head(&self) -> &str {
        &self.head
    }
    pub(super) fn is_head(&self, run: &str) -> bool {
        self.head == run
    }
    pub(super) fn summary(&self) -> Value {
        json!({"machine":self.target,"generation":self.generation})
    }

    fn validate(&self) -> Result<()> {
        for key in [
            &self.id,
            &self.thread,
            &self.source,
            &self.target,
            &self.head,
        ] {
            valid_id(key)?;
        }
        if self.source == self.target
            || self.generation == 0
            || self.address.len() > 1024
            || self.cwd.len() > 4096
            || self.model.len() > 1024
            || self.files.is_empty()
            || self.files.len() > MAX_FILES
        {
            return Err("invalid conversation move".into());
        }
        let mut paths = std::collections::HashSet::new();
        let mut bytes = 0u64;
        for file in &self.files {
            file.validate()?;
            bytes = bytes.saturating_add(file.size);
            if bytes > MAX_BYTES || !paths.insert((&file.run, &file.name)) {
                return Err("conversation move exceeds the retained file bounds".into());
            }
        }
        Ok(())
    }
}

impl Jobs {
    fn move_root(&self) -> PathBuf {
        self.directory
            .parent()
            .expect("runs have a parent")
            .join("thread-moves")
    }
    fn move_receipt(&self, m: &Move) -> PathBuf {
        self.move_root()
            .join("receipts")
            .join(&m.thread)
            .join(format!("{:020}.json", m.generation))
    }
    pub(super) fn placement(&self, thread: &str) -> Result<Option<Move>> {
        valid_id(thread)?;
        let dir = self.move_root().join("receipts").join(thread);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut latest = None;
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json")
                && latest.as_ref().is_none_or(|old| &path > old)
            {
                latest = Some(path);
            }
        }
        latest
            .map(|path| Ok(serde_json::from_slice(&fs::read(path)?)?))
            .transpose()
    }
    fn previous_move(&self, thread: &str, id: &str) -> Result<Option<Move>> {
        let dir = self.move_root().join("receipts").join(thread);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let receipt: Move = serde_json::from_slice(&fs::read(path)?)?;
                if receipt.id == id {
                    return Ok(Some(receipt));
                }
            }
        }
        Ok(None)
    }
    fn publish_move(&self, m: &Move) -> Result<()> {
        let path = self.move_receipt(m);
        state::private_dir(path.parent().expect("receipt has a parent"))?;
        state::publish_new(&path, &serde_json::to_vec(m)?)?;
        #[cfg(unix)]
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
    pub(super) fn incoming_published(&self, path: &Path) -> Result<bool> {
        let marker = path.join("incoming.json");
        if !marker.exists() {
            return Ok(true);
        }
        let (thread, generation): (String, u64) = serde_json::from_slice(&fs::read(marker)?)?;
        valid_id(&thread)?;
        Ok(self
            .move_root()
            .join("receipts")
            .join(thread)
            .join(format!("{generation:020}.json"))
            .is_file())
    }
    pub(super) fn run_machine(&self, run: &str) -> Result<String> {
        let path = self.directory.join(run).join("origin");
        match fs::read_to_string(path) {
            Ok(machine) => {
                valid_id(&machine)?;
                Ok(machine)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(self.machine_id().into()),
            Err(e) => Err(e.into()),
        }
    }
    pub(crate) fn thread_move(self: &Arc<Self>, request: &Value) -> Result<Value> {
        let thread = request["thread_id"].as_str().ok_or("thread ID required")?;
        let id = request["id"].as_str().ok_or("move ID required")?;
        let target = request["target"]
            .as_str()
            .ok_or("target machine required")?;
        let address = request["address"]
            .as_str()
            .ok_or("target address required")?;
        let head = request["after"].as_str().ok_or("latest run ID required")?;
        let cwd = request["cwd"].as_str();
        let model = request["model"].as_str();
        for key in [id, thread, target, head] {
            valid_id(key)?;
        }
        let network = self
            .network
            .lock()
            .unwrap()
            .clone()
            .ok_or("machine network unavailable")?;
        let peer_address;
        let address = if address.is_empty() {
            peer_address = network
                .peers()?
                .into_iter()
                .find(|p| p.key == target)
                .map(|p| p.address)
                .filter(|a| !a.is_empty())
                .ok_or("Move could not be prepared: destination machine address is unavailable")?;
            &peer_address
        } else {
            address
        };
        let send = |request| crate::protocol::request(&network, address, target, request);
        let candidate = {
            let _transition = self.transitions.lock().unwrap();
            if self.residents.all().iter().any(|r| r.id == thread) {
                return Err(
                    "identity-bearing conversations use resident execution authority".into(),
                );
            }
            let placement = self.placement(thread)?;
            let existing = self.previous_move(thread, id)?;
            if let Some(m) = existing
                .as_ref()
                .or_else(|| placement.as_ref().filter(|m| m.source == self.machine_id()))
            {
                if (
                    m.source.as_str(),
                    m.id.as_str(),
                    m.target.as_str(),
                    m.head.as_str(),
                    m.requested_cwd.as_deref(),
                    m.requested_model.as_deref(),
                ) != (self.machine_id(), id, target, head, cwd, model)
                {
                    return Err("conversation already has an outgoing move".into());
                }
                m.clone()
            } else {
                let mut runs: Vec<_> = self
                    .runs
                    .lock()
                    .unwrap()
                    .values()
                    .filter_map(|run| {
                        let s = run.snapshot();
                        (s.record.spec.thread_id == thread).then_some(s)
                    })
                    .collect();
                runs.sort_by_key(|s| (s.record.created_at, s.record.spec.id.clone()));
                let last = runs.last().ok_or("conversation not found")?;
                if last.record.spec.id != head
                    || runs.iter().any(|s| {
                        !terminal(&s.status)
                            || s.record.resident.is_some()
                            || s.record.spec.resident_id.is_some()
                    })
                {
                    return Err(
                        "a move requires the latest run of an idle anonymous conversation".into(),
                    );
                }
                let mut files = Vec::new();
                for run in &runs {
                    let run = &run.record.spec.id;
                    let handle = self.operator_run(run)?;
                    let _journal = handle.inner.lock().unwrap();
                    for name in ["record.json", "events.jsonl"] {
                        files.push(RetainedFile::measure(&self.directory, run, name.into())?);
                    }
                    let origin = self.directory.join(run).join("origin");
                    if origin.exists() {
                        files.push(RetainedFile::measure(
                            &self.directory,
                            run,
                            "origin".into(),
                        )?);
                    }
                    let artifacts = self.directory.join(run).join("artifacts");
                    if artifacts.exists() {
                        for item in fs::read_dir(artifacts)? {
                            let name = item?
                                .file_name()
                                .into_string()
                                .map_err(|_| "invalid artifact name")?;
                            files.push(RetainedFile::measure(
                                &self.directory,
                                run,
                                format!("artifacts/{name}"),
                            )?);
                        }
                    }
                }
                files.sort_by(|a, b| (&a.run, &a.name).cmp(&(&b.run, &b.name)));
                Move {
                    id: id.into(),
                    thread: thread.into(),
                    source: self.machine_id().into(),
                    target: target.into(),
                    address: address.into(),
                    head: head.into(),
                    generation: placement.map_or(1, |p| p.generation.saturating_add(1)),
                    requested_cwd: cwd.map(str::to_owned),
                    requested_model: model.map(str::to_owned),
                    cwd: cwd.map(str::to_owned).unwrap_or_else(|| {
                        if last.record.automatic_cwd {
                            String::new()
                        } else {
                            last.record.spec.cwd.clone()
                        }
                    }),
                    model: model.unwrap_or(&last.record.spec.model).into(),
                    backend: last.record.spec.backend.clone(),
                    files,
                }
            }
        };
        candidate.validate()?;
        let ready = send(json!({"op":"thread_move_receive","action":"prepare","move":candidate}))
            .map_err(|error| {
            if self
                .placement(thread)
                .is_ok_and(|p| p.is_none_or(|p| p.target == self.machine_id()))
            {
                format!("Move could not be prepared: {error}").into()
            } else {
                error
            }
        })?;
        if ready["complete"] == true {
            if self.previous_move(thread, id)?.as_ref() != Some(&candidate) {
                return Err(
                    "destination reported completion without a retained source handoff".into(),
                );
            }
            return Ok(ready);
        }
        for file in candidate
            .files
            .iter()
            .filter(|file| file.name == "record.json")
        {
            let run = self.operator_run(&file.run)?;
            self.attachments
                .require(&run.snapshot().record.spec.attachments)?;
        }
        {
            let _transition = self.transitions.lock().unwrap();
            if self.placement(thread)?.as_ref() != Some(&candidate) {
                let last = self
                    .runs
                    .lock()
                    .unwrap()
                    .values()
                    .map(|r| r.snapshot())
                    .filter(|s| s.record.spec.thread_id == thread)
                    .max_by_key(|s| (s.record.created_at, s.record.spec.id.clone()))
                    .ok_or("conversation not found")?;
                let generation = self
                    .placement(thread)?
                    .map_or(1, |m| m.generation.saturating_add(1));
                if self.residents.all().iter().any(|r| r.id == thread) {
                    return Err(
                        "identity-bearing conversations use resident execution authority".into(),
                    );
                }
                if last.record.spec.id != head
                    || !terminal(&last.status)
                    || generation != candidate.generation
                {
                    return Err("conversation changed before the move could begin".into());
                }
                self.publish_move(&candidate)?;
                self.move_diagnostic("thread_move.source_fenced", &candidate);
            }
        }
        for (index, file) in candidate.files.iter().enumerate() {
            let mut input = File::open(file.path(&self.directory))?.take(file.size);
            let mut buffer = vec![0; CHUNK];
            let mut offset = 0;
            while offset < file.size {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    return Err("retained file shortened during move".into());
                }
                send(
                    json!({"op":"thread_move_receive","action":"part","id":id,"index":index,
                    "offset":offset,"data":STANDARD.encode(&buffer[..count])}),
                )?;
                offset += count as u64;
            }
        }
        for file in &candidate.files {
            if file.name == "record.json" {
                let run = self.operator_run(&file.run)?;
                self.attachments
                    .transfer(&run.snapshot().record.spec.attachments, &send)?;
                for event in &run.inner.lock().unwrap().events {
                    if let Some(files) = event.data.get("attachments").and_then(Value::as_array) {
                        let files: Vec<crate::attachments::Attachment> =
                            serde_json::from_value(Value::Array(files.clone()))?;
                        self.attachments.transfer(&files, &send)?;
                    }
                }
            }
        }
        send(json!({"op":"thread_move_receive","action":"finish","id":id}))
    }

    pub(crate) fn receive_thread_move(&self, actor: &str, request: &Value) -> Result<Value> {
        let _transition = self.transitions.lock().unwrap();
        let action = request["action"].as_str().ok_or("move action required")?;
        let m = if action == "prepare" {
            let m: Move = serde_json::from_value(request["move"].clone())?;
            m.validate()?;
            m
        } else {
            let id = request["id"].as_str().ok_or("move ID required")?;
            valid_id(id)?;
            serde_json::from_slice(&fs::read(
                self.move_root().join("incoming").join(id).join("move.json"),
            )?)?
        };
        if m.source != actor || m.target != self.machine_id() {
            return Err(
                "conversation move source or destination differs from the authenticated machines"
                    .into(),
            );
        }
        let receipt = self.move_receipt(&m);
        if receipt.exists() {
            let accepted: Move = serde_json::from_slice(&fs::read(&receipt)?)?;
            if accepted != m {
                return Err("conversation move receipt differs".into());
            }
            if !self.runs.lock().unwrap().contains_key(&m.head) {
                #[cfg(unix)]
                File::open(receipt.parent().unwrap())?.sync_all()?;
                let mut recovered = HashMap::new();
                for file in m.files.iter().filter(|file| file.name == "record.json") {
                    let run = load_run(&self.directory.join(&file.run), &file.run)?;
                    recovered.insert(file.run.clone(), run);
                }
                if !recovered.contains_key(&m.head) {
                    return Err("accepted move history is unavailable".into());
                }
                self.runs.lock().unwrap().extend(recovered);
                self.move_diagnostic("thread_move.destination_recovered", &m);
            }
            return Ok(
                json!({"complete":true,"thread_id":m.thread,"machine":m.target,"generation":m.generation}),
            );
        }
        let stage = self.move_root().join("incoming").join(&m.id);
        let saved = stage.join("move.json");
        match action {
            "prepare" => {
                if self.residents.all().iter().any(|r| r.id == m.thread) {
                    return Err(
                        "destination already has a resident with this conversation ID".into(),
                    );
                }
                self.resolve_working_directory(&m.thread, &m.cwd, true)?;
                if m.backend == "openrouter" {
                    self.inference.ready()?;
                }
                if let Some(previous) = self.placement(&m.thread)? {
                    if previous.generation >= m.generation || previous.target == self.machine_id() {
                        return Err(
                            "destination already has newer or active execution authority".into(),
                        );
                    }
                } else if self.thread_owner(&m.thread)?.is_some() {
                    return Err(
                        "destination already has an independent conversation with this ID".into(),
                    );
                }
                if saved.exists() {
                    let prior: Move = serde_json::from_slice(&fs::read(saved)?)?;
                    if prior != m {
                        return Err("move ID already staged with different input".into());
                    }
                } else {
                    state::private_dir(&stage)?;
                    state::publish_new(&saved, &serde_json::to_vec(&m)?)?;
                    self.move_diagnostic("thread_move.destination_prepared", &m);
                }
                Ok(json!({"complete":false}))
            }
            "part" => {
                let index = request["index"]
                    .as_u64()
                    .ok_or("move file index required")? as usize;
                let file = m.files.get(index).ok_or("invalid move file index")?;
                let offset = request["offset"]
                    .as_u64()
                    .ok_or("move file offset required")?;
                let encoded = request["data"].as_str().ok_or("move file data required")?;
                if encoded.len() > CHUNK.div_ceil(3) * 4 {
                    return Err("move chunk exceeds bound".into());
                }
                let bytes = STANDARD.decode(encoded)?;
                if bytes.is_empty()
                    || bytes.len() > CHUNK
                    || offset.saturating_add(bytes.len() as u64) > file.size
                {
                    return Err("move chunk is outside its file bounds".into());
                }
                let mut out = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(stage.join(index.to_string()))?;
                let retained = out.metadata()?.len();
                if retained < offset {
                    return Err("move chunk skipped a retained offset".into());
                }
                out.seek(SeekFrom::Start(offset))?;
                let overlap = ((retained - offset) as usize).min(bytes.len());
                let mut existing = vec![0; overlap];
                out.read_exact(&mut existing)?;
                if existing != bytes[..overlap] {
                    return Err("move chunk changed on retry".into());
                }
                out.write_all(&bytes[overlap..])?;
                out.sync_data()?;
                Ok(json!({"next":offset+bytes.len() as u64}))
            }
            "finish" => {
                if self.residents.all().iter().any(|r| r.id == m.thread) {
                    return Err(
                        "destination already has a resident with this conversation ID".into(),
                    );
                }
                self.resolve_working_directory(&m.thread, &m.cwd, true)?;
                if self
                    .placement(&m.thread)?
                    .is_some_and(|p| p.generation >= m.generation || p.target == self.machine_id())
                {
                    return Err("destination has newer or active execution authority".into());
                }
                let mut incoming = HashMap::new();
                for (index, file) in m.files.iter().enumerate() {
                    let input = stage.join(index.to_string());
                    if file.size == 0 && !input.exists() {
                        state::write_new(&input, b"")?;
                    }
                    if digest(&input, file.size)? != file.digest {
                        return Err("retained file failed move integrity check".into());
                    }
                    let run_dir = self.directory.join(&file.run);
                    let existing = self.runs.lock().unwrap().contains_key(&file.run);
                    let path = file.path(&self.directory);
                    if existing {
                        if file.name != "origin" && digest(&path, file.size)? != file.digest {
                            return Err(
                                "destination has a different retained run with the same ID".into(),
                            );
                        }
                        continue;
                    }
                    state::private_dir(&run_dir)?;
                    let marker = run_dir.join("incoming.json");
                    let marker_bytes = serde_json::to_vec(&(&m.thread, m.generation))?;
                    if !marker.exists() {
                        state::publish_new(&marker, &marker_bytes)?;
                    }
                    if fs::read(&marker)? != marker_bytes {
                        return Err("retained run is staged for a different move".into());
                    }
                    state::private_dir(path.parent().unwrap())?;
                    if !path.exists() {
                        fs::hard_link(&input, &path)?;
                    }
                    if digest(&path, file.size)? != file.digest {
                        return Err("retained run differs from staged bytes".into());
                    }
                }
                for file in m.files.iter().filter(|file| file.name == "record.json") {
                    let existing = self.runs.lock().unwrap().get(&file.run).cloned();
                    let path = self.directory.join(&file.run);
                    let run = match existing {
                        Some(run) => run,
                        None => {
                            let origin = path.join("origin");
                            if !origin.exists() {
                                state::publish_new(&origin, m.source.as_bytes())?;
                            }
                            load_run(&path, &file.run)?
                        }
                    };
                    let snapshot = run.snapshot();
                    if snapshot.record.spec.thread_id != m.thread
                        || !terminal(&snapshot.status)
                        || snapshot.record.resident.is_some()
                        || snapshot.record.spec.resident_id.is_some()
                    {
                        return Err(
                            "staged run is not terminal anonymous history for this conversation"
                                .into(),
                        );
                    }
                    self.attachments
                        .require(&snapshot.record.spec.attachments)?;
                    incoming.insert(file.run.clone(), run);
                }
                if !incoming.contains_key(&m.head) {
                    return Err("staged conversation lacks its latest run".into());
                }
                #[cfg(unix)]
                {
                    for id in incoming.keys() {
                        let run_dir = self.directory.join(id);
                        let artifacts = run_dir.join("artifacts");
                        if artifacts.is_dir() {
                            File::open(artifacts)?.sync_all()?;
                        }
                        File::open(run_dir)?.sync_all()?;
                    }
                    File::open(&self.directory)?.sync_all()?;
                }
                self.publish_move(&m)?;
                self.runs.lock().unwrap().extend(incoming);
                for index in 0..m.files.len() {
                    if let Err(error) = fs::remove_file(stage.join(index.to_string())) {
                        logging::record(
                            Level::Warn,
                            Target::State,
                            "thread_move.stage_cleanup_failed",
                            Context {
                                thread_id: Some(&m.thread),
                                ..Context::default()
                            },
                            || json!({"move":m.id,"file_index":index,"error":error.to_string()}),
                            || Value::Null,
                        );
                    }
                }
                self.move_diagnostic("thread_move.destination_accepted", &m);
                Ok(
                    json!({"complete":true,"thread_id":m.thread,"machine":m.target,"generation":m.generation}),
                )
            }
            _ => Err("unknown conversation move action".into()),
        }
    }

    pub(crate) fn thread_placement(&self, request: &Value) -> Result<Value> {
        let thread = request["thread_id"].as_str().ok_or("thread ID required")?;
        Ok(self.placement(thread)?.map(|m| json!({
            "machine":m.target,"generation":m.generation,"head":m.head,"cwd":m.cwd,"model":m.model
        })).unwrap_or(Value::Null))
    }
    fn move_diagnostic(&self, event: &'static str, m: &Move) {
        logging::record(
            Level::Info,
            Target::Workload,
            event,
            Context {
                thread_id: Some(&m.thread),
                ..Context::default()
            },
            || json!({"move":m.id,"source":m.source,"target":m.target,"generation":m.generation}),
            || Value::Null,
        );
    }
}
