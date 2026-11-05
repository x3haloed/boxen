//! Immutable class revisions supply initial resident values. Resident files remain
//! the authority for individual continuity and explicitly applied field provenance.
use crate::{
    Result, jobs,
    logging::{self, Context, Level, Target},
    residents::{Create, Resident},
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
    sync::Mutex,
};

pub const FIELDS: &[&str] = &[
    "name",
    "identity",
    "memory",
    "backend",
    "cwd",
    "model",
    "context",
    "quota_bytes",
    "auto_activate",
];
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Instantiate {
    pub id: String,
    pub definition_id: String,
    pub definition_revision: u64,
    #[serde(default)]
    pub overrides: serde_json::Map<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Origin {
    pub id: String,
    pub instantiated_revision: u64,
    pub initial_overrides: Vec<String>,
    pub last_applied_fields: BTreeMap<String, u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    pub owner: String,
    pub machine_id: String,
    pub revision: u64,
    pub created_at: u64,
    pub spec: Create,
}
pub struct Definitions {
    directory: PathBuf,
    machine: String,
    latest: Mutex<HashMap<String, Revision>>,
    issues: Vec<Value>,
    unavailable: bool,
}
fn validate(spec: &Create, owner: &str, machine: &str) -> Result<()> {
    let r = Resident::prototype(owner, machine, spec);
    r.validate()?;
    if !std::path::Path::new(&spec.cwd).is_absolute() {
        return Err("resident definition working directory is an absolute default; instantiation checks availability".into());
    }
    Ok(())
}
pub fn patched(spec: &Create, patch: &serde_json::Map<String, Value>) -> Result<Create> {
    let mut value = serde_json::to_value(spec)?;
    for (key, v) in patch {
        if !FIELDS.contains(&key.as_str()) {
            return Err("resident template fields are name, identity, memory, backend, cwd, model, context, quota_bytes and auto_activate".into());
        }
        value[key] = v.clone();
    }
    Ok(serde_json::from_value(value)?)
}
impl Definitions {
    pub fn open(directory: PathBuf, machine: String) -> Self {
        match Self::load(directory.clone(), machine.clone()) {
            Ok(store) => store,
            Err(error) => {
                let issue = json!({"kind":"definition_registry_unavailable","detail":error.to_string(),"recovery":"Original state is preserved. Existing residents and anonymous workloads remain available; repair and restart reload definitions."});
                logging::record(
                    Level::Error,
                    Target::State,
                    "resident_definition.unavailable",
                    Context::default(),
                    || issue.clone(),
                    || Value::Null,
                );
                Self {
                    directory,
                    machine,
                    latest: Mutex::new(HashMap::new()),
                    issues: vec![issue],
                    unavailable: true,
                }
            }
        }
    }
    fn load(directory: PathBuf, machine: String) -> Result<Self> {
        state::private_dir(&directory)?;
        let mut latest = HashMap::new();
        let mut issues = vec![];
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if !path.is_dir()
                || path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_none_or(|id| jobs::valid_id(id).is_err())
            {
                continue;
            }
            let id = path.file_name().unwrap().to_str().unwrap();
            let result = (|| -> Result<Revision> {
                let mut revisions = vec![];
                for e in fs::read_dir(&path)? {
                    let p = e?.path();
                    if p.extension().is_some_and(|s| s == "json") {
                        let n = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .and_then(|s| s.parse::<u64>().ok())
                            .ok_or("invalid definition revision filename")?;
                        revisions.push(n);
                    }
                }
                revisions.sort_unstable();
                if revisions.is_empty() {
                    return Err("definition has no committed revisions".into());
                }
                if revisions.len() > 256
                    || revisions
                        .iter()
                        .enumerate()
                        .any(|(i, n)| *n != i as u64 + 1)
                {
                    return Err(
                        "definition revision sequence has a gap or exceeds 256 revisions".into(),
                    );
                }
                let version =
                    Self::read(&path.join(format!("{}.json", revisions.last().unwrap())))?;
                if version.spec.id != id
                    || version.revision != *revisions.last().unwrap()
                    || version.machine_id != machine
                {
                    return Err("definition identity, placement or revision mismatch".into());
                }
                validate(&version.spec, &version.owner, &machine)?;
                Ok(version)
            })();
            match result {
                Ok(revision) => {
                    if latest.len() >= 64 {
                        return Err("definition registry exceeds its 64-definition bound".into());
                    }
                    latest.insert(id.into(), revision);
                }
                Err(error) => {
                    let issue = json!({"kind":"definition_unavailable","id":id,"detail":error.to_string(),"recovery":"The original definition is preserved; existing instances keep independent continuity."});
                    logging::record(
                        Level::Error,
                        Target::State,
                        "resident_definition.unavailable",
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
            "resident_definitions.opened",
            Context::default(),
            || json!({"definitions":latest.len(),"issues":issues.len()}),
            || Value::Null,
        );
        Ok(Self {
            directory,
            machine,
            latest: Mutex::new(latest),
            issues,
            unavailable: false,
        })
    }
    fn read(path: &std::path::Path) -> Result<Revision> {
        if fs::metadata(path)?.len() > 2 * 1024 * 1024 {
            return Err("definition revision exceeds 2 MiB".into());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    pub fn get(&self, owner: &str, id: &str, revision: Option<u64>) -> Result<Revision> {
        jobs::valid_id(id)?;
        let latest = self.latest.lock().unwrap();
        let head = latest
            .get(id)
            .filter(|r| r.owner == owner)
            .ok_or("resident definition unavailable in authenticated scope")?;
        let n = revision.unwrap_or(head.revision);
        if n == 0 || n > head.revision {
            return Err("resident definition revision is unavailable".into());
        }
        let r = if n == head.revision {
            head.clone()
        } else {
            Self::read(&self.directory.join(id).join(format!("{n}.json")))?
        };
        if r.owner != owner || r.machine_id != self.machine || r.spec.id != id || r.revision != n {
            return Err("resident definition revision provenance mismatch".into());
        }
        validate(&r.spec, owner, &self.machine)?;
        Ok(r)
    }
    pub fn owners(&self) -> Vec<String> {
        self.latest
            .lock()
            .unwrap()
            .values()
            .map(|r| r.owner.clone())
            .collect()
    }
    pub fn list(&self, owner: &str) -> Value {
        let latest = self.latest.lock().unwrap();
        let mut items:Vec<_>=latest.values().filter(|r|r.owner==owner).map(|r|json!({"id":r.spec.id,"name":r.spec.name,"revision":r.revision,"created_at":r.created_at,"machine_id":r.machine_id})).collect();
        items.sort_by_key(|v| v["id"].as_str().unwrap().to_owned());
        json!({"definitions":items,"issues":self.issues})
    }
    pub fn create(&self, owner: &str, spec: Create) -> Result<Revision> {
        if self.unavailable {
            return Err(
                "resident definition registry is unavailable; ordinary residents remain available"
                    .into(),
            );
        }
        validate(&spec, owner, &self.machine)?;
        let mut latest = self.latest.lock().unwrap();
        if let Some(head) = latest.get(&spec.id) {
            if head.owner != owner {
                return Err("definition unavailable in authenticated scope".into());
            }
            let first = if head.revision == 1 {
                head.clone()
            } else {
                Self::read(&self.directory.join(&spec.id).join("1.json"))?
            };
            return if first.owner == owner
                && first.machine_id == self.machine
                && first.revision == 1
                && first.spec == spec
            {
                Ok(first)
            } else {
                Err("definition id already exists with another creation input".into())
            };
        }
        if latest.len() >= 64 {
            return Err("this host retains at most 64 resident definitions".into());
        }
        let directory = self.directory.join(&spec.id);
        state::private_dir(&directory)?;
        let revision = Revision {
            owner: owner.into(),
            machine_id: self.machine.clone(),
            revision: 1,
            created_at: jobs::now(),
            spec,
        };
        state::publish_new(&directory.join("1.json"), &serde_json::to_vec(&revision)?)?;
        latest.insert(revision.spec.id.clone(), revision.clone());
        drop(latest);
        record(owner, &revision, "resident_definition.created");
        Ok(revision)
    }
    pub fn update(
        &self,
        owner: &str,
        id: &str,
        expected: u64,
        patch: &serde_json::Map<String, Value>,
    ) -> Result<Revision> {
        if patch.is_empty() {
            return Err("definition update contains at least one template field".into());
        }
        let mut latest = self.latest.lock().unwrap();
        let head = latest
            .get(id)
            .filter(|r| r.owner == owner)
            .ok_or("resident definition unavailable in authenticated scope")?;
        if head.revision != expected {
            return Err("definition revision changed; get returns the current revision".into());
        }
        if head.revision >= 256 {
            return Err("resident definition reached its 256-revision bound".into());
        }
        let spec = patched(&head.spec, patch)?;
        validate(&spec, owner, &self.machine)?;
        let revision = Revision {
            owner: owner.into(),
            machine_id: self.machine.clone(),
            revision: head.revision + 1,
            created_at: jobs::now(),
            spec,
        };
        state::publish_new(
            &self
                .directory
                .join(id)
                .join(format!("{}.json", revision.revision)),
            &serde_json::to_vec(&revision)?,
        )?;
        latest.insert(id.into(), revision.clone());
        drop(latest);
        record(owner, &revision, "resident_definition.updated");
        Ok(revision)
    }
    pub fn history(&self, owner: &str, id: &str, after: u64, limit: usize) -> Result<Value> {
        let head = self.get(owner, id, None)?;
        let mut versions = vec![];
        for n in after.saturating_add(1)..=head.revision {
            if versions.len() >= limit.clamp(1, 50) {
                break;
            }
            let r = self.get(owner, id, Some(n))?;
            versions.push(json!({"revision":n,"created_at":r.created_at,"name":r.spec.name}));
        }
        let next = versions
            .last()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(after);
        Ok(json!({"revisions":versions,"next_after":next,"latest_revision":head.revision}))
    }
}
fn record(owner: &str, r: &Revision, event: &'static str) {
    logging::record(
        Level::Info,
        Target::State,
        event,
        Context::peer(owner),
        || json!({"definition_id":r.spec.id,"revision":r.revision}),
        || Value::Null,
    );
}
