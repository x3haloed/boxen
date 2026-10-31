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

fn default_quota() -> usize {
    64 * 1024
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub memory: String,
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_quota")]
    pub quota_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resident {
    pub id: String,
    pub owner: String,
    pub machine_id: String,
    pub name: String,
    pub identity: String,
    pub memory: String,
    pub cwd: String,
    pub model: String,
    pub quota_bytes: usize,
    pub revision: u64,
    pub created_at: u64,
}
impl Resident {
    fn validate(&self) -> Result<()> {
        jobs::valid_id(&self.id)?;
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
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub expected_revision: u64,
    pub name: Option<String>,
    pub identity: Option<String>,
    pub memory: Option<String>,
    pub quota_bytes: Option<usize>,
}
pub struct Residents {
    directory: PathBuf,
    machine_id: String,
    entries: Mutex<HashMap<String, Stored>>,
    issues: Vec<Value>,
}
impl Residents {
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
                    let mut issue = json!({"at":jobs::now(),"kind":"resident_unavailable","id":path.file_stem().and_then(|v|v.to_str()),"file":path,"detail":error.to_string(),"recovery":"Original state preserved. Other residents and anonymous work remain available. Restore this file from a known copy to recover this resident."});
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
        let mut entries = self.entries.lock().unwrap();
        if let Some(prior) = entries.get(&initial.id) {
            if prior.resident.owner == owner && prior.initial == initial {
                return Ok(prior.resident.clone());
            }
            return Err("resident id already exists with different input or owner".into());
        }
        let resident = Resident {
            id: initial.id.clone(),
            owner: owner.into(),
            machine_id: self.machine_id.clone(),
            name: initial.name.clone(),
            identity: initial.identity.clone(),
            memory: initial.memory.clone(),
            cwd: initial.cwd.clone(),
            model: initial.model.clone(),
            quota_bytes: initial.quota_bytes,
            revision: 1,
            created_at: jobs::now(),
        };
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
        let path = self.directory.join(format!("{id}.json"));
        let temporary = self
            .directory
            .join(format!(".pending-{}", state::random_hex()?));
        state::write_new(&temporary, &serde_json::to_vec(&stored)?)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(temporary);
            return Err(error.into());
        }
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
            "This execution is attached to a Boxen resident with canonical Boxen thread {}. Backend processes and turns are temporary; the resident identity, memory, and retained conversation persist across them.\nCurrent host: {} (OS {}, architecture {}). Working directory: {}. Files and execution environment belong to this host; their availability can differ between hosts.\nThe durable identity/memory store is separate from the working directory and is managed by the daemon at {}. The boxen_continuity tool reads and updates this store. A read returns current continuity, including changes made after turn acceptance. A write succeeds when expected_revision matches the current revision and the resulting stored contents fit the byte quota. Rejected writes leave continuity unchanged. Direct edits to backing files bypass daemon revision tracking and quota accounting and are unsupported. Conversation history is outside this quota. Automatic movement, replication, and heartbeats are not enabled in this version.\nSaved continuity snapshot at turn acceptance (JSON):\n{}",
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
                    expected_revision: input
                        .expected_revision
                        .ok_or("expected_revision required for writes")?,
                    identity: input.identity,
                    memory: input.memory,
                    name: None,
                    quota_bytes: None,
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
