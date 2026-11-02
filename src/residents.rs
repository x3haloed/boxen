//! One host-authoritative identity/memory store per resident, independent of runtime sessions.
use crate::{
    Result, jobs,
    logging::{self, Context, Level, Target},
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::{collections::HashMap, fs, path::PathBuf, sync::Mutex};

pub fn default_backend() -> String {
    "codex".into()
}
fn default_auto() -> bool {
    true
}
fn default_quota() -> usize {
    64 * 1024
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Create {
    #[serde(default = "crate::context::Settings::resident")]
    pub context: crate::context::Settings,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub memory: String,
    #[serde(default = "default_backend")]
    pub backend: String,
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_quota")]
    pub quota_bytes: usize,
    #[serde(default = "default_auto")]
    pub auto_activate: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resident {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<Promotion>,
    #[serde(default = "crate::context::Settings::resident")]
    pub context: crate::context::Settings,
    pub id: String,
    pub owner: String,
    pub machine_id: String,
    pub name: String,
    pub identity: String,
    pub memory: String,
    #[serde(default = "default_backend")]
    pub backend: String,
    pub cwd: String,
    pub model: String,
    pub quota_bytes: usize,
    #[serde(default = "default_auto")]
    pub auto_activate: bool,
    pub revision: u64,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<crate::resident_definitions::Origin>,
}
impl Resident {
    pub(crate) fn prototype(owner: &str, machine: &str, initial: &Create) -> Self {
        Resident {
            context: initial.context.clone(),
            id: initial.id.clone(),
            owner: owner.into(),
            machine_id: machine.into(),
            name: initial.name.clone(),
            identity: initial.identity.clone(),
            memory: initial.memory.clone(),
            backend: initial.backend.clone(),
            cwd: initial.cwd.clone(),
            model: initial.model.clone(),
            quota_bytes: initial.quota_bytes,
            auto_activate: initial.auto_activate,
            revision: 1,
            created_at: jobs::now(),
            definition: None,
            promotion: None,
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.context.validate()?;
        jobs::valid_id(&self.id)?;
        if let Some(p) = &self.promotion {
            jobs::valid_id(&p.source_run_id)?;
            if self.backend != "codex"
                || p.request.thread_id != self.id
                || p.backend_thread_id.is_empty()
                || p.backend_thread_id.len() > 256
            {
                return Err("resident promotion provenance does not match its canonical native conversation".into());
            }
        }
        if !matches!(self.backend.as_str(), "codex" | "openrouter") {
            return Err("unsupported resident backend".into());
        }
        state::key(&self.owner)?;
        state::key(&self.machine_id)?;
        if self.name.trim().is_empty() || self.name.len() > 100 {
            return Err("resident name must contain 1–100 bytes".into());
        }
        if !(1..=256 * 1024).contains(&self.quota_bytes) {
            return Err("continuity quota must be 1–262144 bytes".into());
        }
        if self.used_bytes() > self.quota_bytes {
            return Err(format!(
                "resident continuity quota exceeded: {} of {} bytes; the write leaves stored continuity unchanged",
                self.used_bytes(),
                self.quota_bytes
            )
            .into());
        }
        if self.revision == 0 {
            return Err("invalid resident revision".into());
        }
        Ok(())
    }
    pub fn used_bytes(&self) -> usize {
        self.name.len() + self.identity.len() + self.memory.len()
    }
    pub fn value(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("resident serializes");
        value["used_bytes"] = json!(self.used_bytes());
        value["thread_id"] = json!(self.id);
        value["communication"] = json!("mailbox");
        value["mailbox"] = json!({"type":"resident","id":self.id});
        value["environment"] =
            json!({"os":crate::platform::host_os(),"architecture":std::env::consts::ARCH});
        value
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    version: u32,
    initial: Create,
    resident: Resident,
    #[serde(default)]
    instantiation: Option<crate::resident_definitions::Instantiate>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Promote {
    pub thread_id: String,
    pub name: String,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub memory: String,
    #[serde(default = "default_quota")]
    pub quota_bytes: usize,
    #[serde(default = "default_auto")]
    pub auto_activate: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Promotion {
    pub request: Promote,
    pub backend_thread_id: String,
    pub source_run_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub context: Option<crate::context::Settings>,
    pub expected_revision: u64,
    pub name: Option<String>,
    pub identity: Option<String>,
    pub memory: Option<String>,
    pub quota_bytes: Option<usize>,
    pub auto_activate: Option<bool>,
}
pub struct Residents {
    directory: PathBuf,
    machine_id: String,
    entries: Mutex<HashMap<String, Stored>>,
    issues: Vec<Value>,
}
impl Residents {
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
    // Jobs' authority lock is held for this store's entire lifetime. All mutations
    // serialize here; no resident lock is held across calls into Jobs.
    pub fn open(directory: PathBuf, machine_id: String) -> Result<Self> {
        state::private_dir(&directory)?;
        let mut entries = HashMap::new();
        let mut issues = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let result = (|| -> Result<Stored> {
                let stored: Stored = serde_json::from_slice(&fs::read(&path)?)?;
                let r = &stored.resident;
                r.validate()?;
                if stored.version != 1
                    || r.machine_id != machine_id
                    || stored.initial.id != r.id
                    || path.file_stem().and_then(|v| v.to_str()) != Some(r.id.as_str())
                {
                    return Err("resident identity, placement, or format mismatch; copying state does not authorize activation on another machine".into());
                }
                Ok(stored)
            })();
            match result {
                Ok(stored) => {
                    entries.insert(stored.resident.id.clone(), stored);
                }
                Err(error) => {
                    let mut issue = json!({"at":jobs::now(),"kind":"resident_unavailable","id":path.file_stem().and_then(|v|v.to_str()),"file":path,"detail":error.to_string(),"recovery":"Original state preserved. Other residents and anonymous work remain available. Restoring this file from a known copy makes this resident available again on restart."});
                    let retained = (|| -> Result<()> {
                        let mut options = fs::OpenOptions::new();
                        options.create(true).append(true);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::OpenOptionsExt;
                            options.mode(0o600);
                        }
                        let mut file = options.open(directory.join("incidents.jsonl"))?;
                        writeln!(file, "{issue}")?;
                        file.sync_data()?;
                        Ok(())
                    })();
                    if let Err(error) = retained {
                        issue["incident_record_error"] = json!(error.to_string());
                    }
                    logging::record(
                        Level::Error,
                        Target::State,
                        "resident.unavailable",
                        Context::default(),
                        || issue.clone(),
                        || Value::Null,
                    );
                    issues.push(issue);
                }
            }
        }
        logging::record(
            Level::Info,
            Target::State,
            "residents.opened",
            Context::default(),
            || json!({"count":entries.len()}),
            || Value::Null,
        );
        Ok(Self {
            directory,
            machine_id,
            entries: Mutex::new(entries),
            issues,
        })
    }
    pub fn issues(&self) -> &[Value] {
        &self.issues
    }
    fn authorized<'a>(
        entries: &'a HashMap<String, Stored>,
        owner: &str,
        id: &str,
    ) -> Result<&'a Stored> {
        jobs::valid_id(id)?;
        let stored = entries.get(id).ok_or("resident not found")?;
        if stored.resident.owner != owner {
            return Err("resident belongs to a different peer".into());
        }
        Ok(stored)
    }
    pub fn get(&self, owner: &str, id: &str) -> Result<Resident> {
        let entries = self.entries.lock().unwrap();
        let resident = Self::authorized(&entries, owner, id)?.resident.clone();
        logging::record(
            Level::Trace,
            Target::State,
            "resident.read",
            Context {
                thread_id: Some(id),
                peer_id: Some(owner),
                ..Context::default()
            },
            || json!({"revision":resident.revision,"used_bytes":resident.used_bytes()}),
            || Value::Null,
        );
        Ok(resident)
    }
    pub fn all(&self) -> Vec<Resident> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|s| s.resident.clone())
            .collect()
    }
    pub fn list(&self, owner: &str) -> Vec<Resident> {
        let entries = self.entries.lock().unwrap();
        let mut list: Vec<_> = entries
            .values()
            .filter(|s| s.resident.owner == owner)
            .map(|s| s.resident.clone())
            .collect();
        list.sort_by_key(|r| (r.created_at, r.id.clone()));
        list
    }
    pub fn create(&self, owner: &str, initial: Create) -> Result<Resident> {
        self.create_with_origin(owner, initial, None, None, None)
    }
    pub fn create_with_origin(
        &self,
        owner: &str,
        initial: Create,
        instantiation: Option<crate::resident_definitions::Instantiate>,
        origin: Option<crate::resident_definitions::Origin>,
        promotion: Option<Promotion>,
    ) -> Result<Resident> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(prior) = entries.get(&initial.id) {
            if prior.resident.owner == owner
                && prior.initial == initial
                && prior.instantiation == instantiation
                && prior.resident.promotion.as_ref().map(|p| &p.request)
                    == promotion.as_ref().map(|p| &p.request)
            {
                return Ok(prior.resident.clone());
            }
            return Err("resident id already exists with different input or owner".into());
        }
        let mut resident = Resident::prototype(owner, &self.machine_id, &initial);
        resident.definition = origin;
        resident.promotion = promotion;
        resident.validate()?;
        if !std::path::Path::new(&resident.cwd).is_absolute()
            || !std::path::Path::new(&resident.cwd).is_dir()
        {
            return Err(
                "resident working directory must exist on the executing machine and be absolute"
                    .into(),
            );
        }
        let stored = Stored {
            version: 1,
            instantiation,
            initial,
            resident: resident.clone(),
        };
        state::publish_new(
            &self.directory.join(format!("{}.json", resident.id)),
            &serde_json::to_vec(&stored)?,
        )?;
        entries.insert(resident.id.clone(), stored);
        logging::record(
            Level::Info,
            Target::State,
            "resident.created",
            Context {
                thread_id: Some(&resident.id),
                peer_id: Some(owner),
                ..Context::default()
            },
            || json!({"revision":1,"used_bytes":resident.used_bytes(),"quota_bytes":resident.quota_bytes}),
            || Value::Null,
        );
        Ok(resident)
    }
    pub fn existing_instance(
        &self,
        owner: &str,
        spec: &crate::resident_definitions::Instantiate,
    ) -> Result<Option<Resident>> {
        let entries = self.entries.lock().unwrap();
        if let Some(stored) = entries.get(&spec.id) {
            return if stored.resident.owner == owner && stored.instantiation.as_ref() == Some(spec)
            {
                Ok(Some(stored.resident.clone()))
            } else {
                Err(
                    "resident instance id already exists with another creation input or owner"
                        .into(),
                )
            };
        }
        Ok(None)
    }

    pub fn existing_promotion(&self, owner: &str, request: &Promote) -> Result<Option<Resident>> {
        let entries = self.entries.lock().unwrap();
        if let Some(stored) = entries.get(&request.thread_id) {
            if stored.resident.owner == owner
                && stored
                    .resident
                    .promotion
                    .as_ref()
                    .is_some_and(|p| p.request == *request)
            {
                return Ok(Some(stored.resident.clone()));
            }
            return Err(
                "thread already has a resident with another promotion input or owner".into(),
            );
        }
        Ok(None)
    }
    pub fn apply_definition(
        &self,
        owner: &str,
        id: &str,
        expected: u64,
        definition: &crate::resident_definitions::Revision,
        fields: &[String],
        has_history: bool,
    ) -> Result<Resident> {
        if fields.is_empty() || fields.len() > crate::resident_definitions::FIELDS.len() {
            return Err(
                "manual definition application names a nonempty bounded set of fields".into(),
            );
        }
        let mut entries = self.entries.lock().unwrap();
        let prior = Self::authorized(&entries, owner, id)?;
        if prior.resident.revision != expected {
            return Err("resident revision changed; a read returns current continuity before manual replacement".into());
        }
        let source = prior
            .resident
            .definition
            .as_ref()
            .ok_or("standalone resident has no definition to apply")?;
        if source.id != definition.spec.id {
            return Err("manual application addresses the instance's original definition".into());
        }
        let mut stored = prior.clone();
        let mut value = serde_json::to_value(&stored.resident)?;
        let template = serde_json::to_value(&definition.spec)?;
        let mut unique = std::collections::HashSet::new();
        for field in fields {
            if !crate::resident_definitions::FIELDS.contains(&field.as_str())
                || !unique.insert(field)
            {
                return Err("manual application fields are unique resident template fields".into());
            }
            if has_history
                && matches!(field.as_str(), "backend" | "cwd" | "model")
                && value[field] != template[field]
            {
                return Err("backend, model and working directory remain pinned after the first accepted run; no backend conversation is implicitly reconstructed".into());
            }
            value[field] = template[field].clone();
        }
        let mut resident: Resident = serde_json::from_value(value)?;
        resident.revision = resident
            .revision
            .checked_add(1)
            .ok_or("resident revision exhausted")?;
        let origin = resident.definition.as_mut().unwrap();
        for field in fields {
            origin
                .last_applied_fields
                .insert(field.clone(), definition.revision);
        }
        resident.validate()?;
        if !std::path::Path::new(&resident.cwd).is_absolute()
            || !std::path::Path::new(&resident.cwd).is_dir()
        {
            return Err("resident working directory must exist on the executing machine".into());
        }
        stored.resident = resident.clone();
        self.save(id, &stored)?;
        entries.insert(id.into(), stored);
        logging::record(
            Level::Info,
            Target::State,
            "resident.definition_applied",
            Context {
                thread_id: Some(id),
                peer_id: Some(owner),
                ..Context::default()
            },
            || json!({"definition_id":definition.spec.id,"definition_revision":definition.revision,"resident_revision":resident.revision,"fields":fields}),
            || Value::Null,
        );
        Ok(resident)
    }
    fn save(&self, id: &str, stored: &Stored) -> Result<()> {
        let temporary = self
            .directory
            .join(format!(".pending-{}", state::random_hex()?));
        state::write_new(&temporary, &serde_json::to_vec(stored)?)?;
        if let Err(error) = fs::rename(&temporary, self.directory.join(format!("{id}.json"))) {
            let _ = fs::remove_file(temporary);
            return Err(error.into());
        }
        Ok(())
    }
    pub fn update(&self, owner: &str, id: &str, patch: Patch) -> Result<Resident> {
        let mut entries = self.entries.lock().unwrap();
        let prior = Self::authorized(&entries, owner, id)?;
        if prior.resident.revision != patch.expected_revision {
            logging::record(
                Level::Warn,
                Target::State,
                "resident.revision_conflict",
                Context {
                    thread_id: Some(id),
                    peer_id: Some(owner),
                    ..Context::default()
                },
                || json!({"expected":patch.expected_revision,"current":prior.resident.revision}),
                || Value::Null,
            );
            return Err(format!(
                "resident revision conflict: expected {}, current {}; a read returns the current revision and continuity",
                patch.expected_revision, prior.resident.revision
            )
            .into());
        }
        let mut stored = prior.clone();
        let r = &mut stored.resident;
        if let Some(value) = patch.context {
            r.context = value;
        }
        if let Some(value) = patch.name {
            r.name = value;
        }
        if let Some(value) = patch.identity {
            r.identity = value;
        }
        if let Some(value) = patch.memory {
            r.memory = value;
        }
        if let Some(value) = patch.quota_bytes {
            r.quota_bytes = value;
        }
        if let Some(value) = patch.auto_activate {
            r.auto_activate = value;
        }
        r.revision = r
            .revision
            .checked_add(1)
            .ok_or("resident revision exhausted")?;
        if let Err(error) = r.validate() {
            logging::record(
                Level::Warn,
                Target::State,
                "resident.update_rejected",
                Context {
                    thread_id: Some(id),
                    peer_id: Some(owner),
                    ..Context::default()
                },
                || json!({"error":error.to_string(),"used_bytes":r.used_bytes(),"quota_bytes":r.quota_bytes}),
                || Value::Null,
            );
            return Err(error);
        }
        self.save(id, &stored)?;
        let resident = stored.resident.clone();
        entries.insert(id.into(), stored);
        logging::record(
            Level::Info,
            Target::State,
            "resident.updated",
            Context {
                thread_id: Some(id),
                peer_id: Some(owner),
                ..Context::default()
            },
            || json!({"revision":resident.revision,"used_bytes":resident.used_bytes(),"quota_bytes":resident.quota_bytes}),
            || Value::Null,
        );
        Ok(resident)
    }
    pub fn context(
        &self,
        owner: &str,
        thread_id: &str,
        resident_id: Option<&str>,
    ) -> Result<Option<Resident>> {
        let entries = self.entries.lock().unwrap();
        if let Some(id) = resident_id {
            let resident = &Self::authorized(&entries, owner, id)?.resident;
            if id != thread_id {
                return Err("resident execution accepts only its canonical thread".into());
            }
            return Ok(Some(resident.clone()));
        }
        if self
            .issues
            .iter()
            .any(|i| i["id"].as_str() == Some(thread_id))
        {
            return Err("resident state is unavailable; the retained incident describes the failure and the original file remains available for recovery".into());
        }
        if entries.contains_key(thread_id) {
            return Err("resident canonical thread requires its resident_id".into());
        }
        Ok(None)
    }
    pub fn instructions(&self, resident: &Resident) -> String {
        format!(
            "This execution is attached to a Boxen resident with canonical Boxen thread {}. Backend processes and turns are temporary; the resident identity, memory, and retained conversation persist across them.\nCurrent host: {} (OS {}, architecture {}). Working directory: {}. Files and execution environment belong to this host; their availability can differ between hosts.\nThe durable identity/memory store is separate from the working directory and is managed by the daemon at {}. The boxen_continuity tool reads and updates this store. A read returns current continuity, including changes made after turn acceptance. A write succeeds when expected_revision matches the current revision and the resulting stored contents fit the byte quota. Rejected writes leave continuity unchanged. Direct edits to backing files bypass daemon revision tracking and quota accounting and are unsupported. Conversation history is outside this quota. A definition association records initialization and explicitly applied fields; later definition edits do not automatically change this instance, and mutable continuity remains independent. Movement and replication are not enabled in this version. Heartbeats can be configured as ordinary scheduled activations. Bounded source-observation batches can accompany activations; their sources and freshness are explicit. This identity has a mailbox. Incoming messages are queued independently of activation and can be presented as a batch. Assistant text, including final text, is private execution narration retained for authorized observers; it is not delivered to correspondents. A successful boxen_send_message call durably queues a message at its destination. An activation can finish without sending any message. boxen_mailbox reads this identity’s inbox. boxen_thread_request and boxen_thread_result provide separate request/response control channels for identity-less workloads.\nSaved continuity snapshot at turn acceptance (JSON):\n{}",
            resident.id,
            resident.machine_id,
            crate::platform::host_os(),
            std::env::consts::ARCH,
            resident.cwd,
            self.directory.display(),
            resident.value()
        )
    }
    pub fn tool(&self, owner: &str, id: &str, arguments: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            action: String,
            expected_revision: Option<u64>,
            identity: Option<String>,
            memory: Option<String>,
        }
        let input: Input = serde_json::from_value(arguments)?;
        let resident = match input.action.as_str() {
            "read" => self.get(owner, id)?,
            "write" => self.update(
                owner,
                id,
                Patch {
                    context: None,
                    expected_revision: input
                        .expected_revision
                        .ok_or("expected_revision required for writes")?,
                    identity: input.identity,
                    memory: input.memory,
                    name: None,
                    quota_bytes: None,
                    auto_activate: None,
                },
            )?,
            _ => return Err("supported continuity actions are read and write".into()),
        };
        Ok(resident.value())
    }
}
pub fn tool_spec() -> Value {
    json!({"type":"function","name":"boxen_continuity","description":"This tool reads and updates the attached resident’s durable Boxen identity and memory. The read action returns current contents, revision, usage, and quota. The write action replaces supplied identity or memory fields when expected_revision matches and the result fits the quota; omitted fields remain unchanged. Revision conflicts and quota failures leave stored continuity unchanged. The tool cannot address another resident or change quota or placement.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["read","write"]},"expected_revision":{"type":"integer","minimum":1},"identity":{"type":"string"},"memory":{"type":"string"}},"required":["action"],"additionalProperties":false}})
}
