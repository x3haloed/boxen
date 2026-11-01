//! Durable host-local jobs. Run journals own execution; this journal owns timing.
use crate::{
    Result, jobs,
    logging::{self, Context, Level, Target},
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Handler {
    Resident { id: String },
    Thread { id: String },
    Source { id: String },
    Delivery { id: String },
}
impl Handler {
    pub fn id(&self) -> &str {
        match self {
            Self::Resident { id }
            | Self::Thread { id }
            | Self::Source { id }
            | Self::Delivery { id } => id,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub target: Handler,
    #[serde(default)]
    pub directions: String,
    pub at_ms: u64,
    #[serde(default)]
    pub interval_ms: Option<u64>,
}
impl Definition {
    fn validate(&self) -> Result<()> {
        jobs::valid_id(&self.id)?;
        jobs::valid_id(self.target.id())?;
        if self.name.trim().is_empty()
            || self.name.len() > 100
            || (matches!(self.target, Handler::Thread { .. }) && self.directions.trim().is_empty())
            || self.directions.len() > 32768
        {
            return Err(
                "schedule name contains 1–100 bytes; directions are at most 32768 bytes and thread requests have nonblank directions"
                    .into(),
            );
        }
        if self.at_ms == 0
            || self.at_ms > i64::MAX as u64
            || self
                .interval_ms
                .is_some_and(|i| !(1000..=31536000000).contains(&i))
        {
            return Err("at_ms is a positive Unix-millisecond time; interval_ms is absent for a one-time event or 1000–31536000000 for an anchored recurring interval".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Origin {
    pub schedule_id: String,
    pub revision: u64,
    pub due_at_ms: u64,
    pub dispatch_started_at_ms: u64,
    pub coalesced_occurrences: u64,
}
impl Origin {
    pub fn run_id(&self) -> String {
        use sha2::{Digest, Sha256};
        state::hex(&Sha256::digest(
            format!(
                "boxen-schedule-v1:{}:{}:{}",
                self.schedule_id, self.revision, self.due_at_ms
            )
            .as_bytes(),
        ))
    }
    pub fn context(&self) -> String {
        format!(
            "\nThis run comes from a host-local scheduled event. Metadata (Unix milliseconds): {}. Missed recurring ticks were coalesced into this occurrence; they are not separate requests or messages. The scheduled run's outcome is retained independently of the schedule's timing state.",
            json!(self)
        )
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub definition: Definition,
    pub owner: String,
    pub revision: u64,
    pub enabled: bool,
    pub deleted: bool,
    pub next_at_ms: Option<u64>,
    pub retry_at_ms: u64,
    pub blocked: bool,
    pub claimed: Option<Origin>,
    pub last: Option<Value>,
    pub last_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Stored {
    initial_hash: String,
    job: Job,
}
fn definition_hash(definition: &Definition) -> String {
    use sha2::{Digest, Sha256};
    state::hex(&Sha256::digest(
        serde_json::to_vec(definition).expect("definition serializes"),
    ))
}
pub struct Schedules {
    directory: PathBuf,
    entries: Mutex<HashMap<String, Stored>>,
    issues: Vec<Value>,
    failed: Mutex<HashSet<String>>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub expected_revision: u64,
    pub name: Option<String>,
    pub directions: Option<String>,
    pub at_ms: Option<u64>,
    pub interval_ms: Option<u64>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub delete: bool,
}
impl Patch {
    fn apply(self, job: &mut Job) -> Result<()> {
        if job.revision != self.expected_revision {
            return Err("schedule revision conflict; a read returns the current definition".into());
        }
        if job.deleted {
            return Err("deleted schedule ids retain history and cannot be reused".into());
        }
        if job.claimed.is_some() {
            return Err("schedule claim was not reconciled by its owning dispatcher".into());
        }
        if let Some(v) = self.name {
            job.definition.name = v;
        }
        if let Some(v) = self.directions {
            job.definition.directions = v;
        }
        if let Some(v) = self.at_ms {
            job.definition.at_ms = v;
            job.next_at_ms = Some(v);
        }
        if let Some(v) = self.interval_ms {
            job.definition.interval_ms = if v == 0 { None } else { Some(v) };
        }
        if let Some(v) = self.enabled {
            job.enabled = v;
        }
        if self.delete {
            job.deleted = true;
            job.enabled = false;
        }
        if job.enabled && job.next_at_ms.is_none() {
            return Err(
                "a completed one-time job needs a new at_ms before it can be enabled".into(),
            );
        }
        job.definition.validate()?;
        job.retry_at_ms = 0;
        job.blocked = false;
        job.revision += 1;
        Ok(())
    }
}
impl Schedules {
    pub fn open(directory: PathBuf) -> Result<Self> {
        state::private_dir(&directory)?;
        let mut entries = HashMap::new();
        let mut issues = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let loaded = (|| -> Result<Stored> {
                let mut stored = None;
                let mut sequence = 0;
                let mut complete = 0;
                let mut reader = BufReader::new(fs::File::open(&path)?);
                loop {
                    let mut bytes = Vec::new();
                    if reader.read_until(b'\n', &mut bytes)? == 0 {
                        break;
                    }
                    if !bytes.ends_with(b"\n") {
                        OpenOptions::new()
                            .write(true)
                            .open(&path)?
                            .set_len(complete)?;
                        break;
                    }
                    let e: Value = serde_json::from_slice(&bytes)?;
                    if sequence == 0 && e["kind"] == "timer_checkpoint" {
                        sequence = e["seq"]
                            .as_u64()
                            .ok_or("checkpoint sequence missing")?
                            .checked_sub(1)
                            .ok_or("checkpoint sequence invalid")?;
                    }
                    sequence += 1;
                    if e["seq"].as_u64() != Some(sequence) {
                        return Err("schedule journal sequence mismatch".into());
                    }
                    let next: Stored = serde_json::from_value(e["state"].clone())?;
                    next.job.definition.validate()?;
                    if e["kind"] == "timer_checkpoint"
                        && !matches!(
                            next.job.definition.target,
                            Handler::Source { .. } | Handler::Delivery { .. }
                        )
                    {
                        return Err("checkpoint belongs only to internal observation timers".into());
                    }
                    state::key(&next.job.owner)?;
                    if next.job.last_seq != sequence
                        || next.job.revision == 0
                        || path.file_stem().and_then(|s| s.to_str())
                            != Some(next.job.definition.id.as_str())
                        || state::key(&next.initial_hash).is_err()
                    {
                        return Err("schedule record identity or revision mismatch".into());
                    }
                    complete += bytes.len() as u64;
                    stored = Some(next);
                }
                stored.ok_or_else(|| "empty schedule journal".into())
            })();
            match loaded {
                Ok(stored) => {
                    entries.insert(stored.job.definition.id.clone(), stored);
                }
                Err(_) => {
                    let issue = json!({"at":jobs::now(),"kind":"schedule_unavailable","id":path.file_stem().and_then(|s|s.to_str()),"detail":"The original journal is preserved; this schedule is isolated while other schedules and workloads remain available."});
                    logging::record(
                        Level::Error,
                        Target::State,
                        "schedule.unavailable",
                        Context::default(),
                        || issue.clone(),
                        || Value::Null,
                    );
                    issues.push(issue);
                }
            }
        }
        Ok(Self {
            directory,
            entries: Mutex::new(entries),
            issues,
            failed: Mutex::new(HashSet::new()),
        })
    }
    fn save(&self, entry: &mut Stored, kind: &str, detail: Value) -> Result<()> {
        if self
            .failed
            .lock()
            .unwrap()
            .contains(&entry.job.definition.id)
        {
            return Err("schedule storage is unavailable; accepted workload evidence is preserved and other jobs remain available".into());
        }
        let seq = entry.job.last_seq + 1;
        let mut job = entry.job.clone();
        job.last_seq = seq;
        let event = json!({"seq":seq,"at":jobs::now(),"kind":kind,"detail":detail,"state":{"initial_hash":entry.initial_hash,"job":job}});
        let bytes = serde_json::to_vec(&event)?;
        if bytes.len() > 256 * 1024 {
            self.failed
                .lock()
                .unwrap()
                .insert(job.definition.id.clone());
            return Err("schedule event exceeds 256 KiB; this job is isolated with its original journal preserved".into());
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let saved = (|| -> Result<()> {
            let path = self.directory.join(format!("{}.jsonl", job.definition.id));
            if matches!(
                job.definition.target,
                Handler::Source { .. } | Handler::Delivery { .. }
            ) && seq.is_multiple_of(32)
            {
                let mut checkpoint = event.clone();
                checkpoint["kind"] = json!("timer_checkpoint");
                checkpoint["detail"] =
                    json!({"earlier_timer_records_coalesced":true,"latest_operation":kind});
                let tmp = path.with_extension("pending");
                if tmp.exists() {
                    fs::remove_file(&tmp)?;
                }
                let mut encoded = serde_json::to_vec(&checkpoint)?;
                encoded.push(b'\n');
                state::write_new(&tmp, &encoded)?;
                if let Err(error) = fs::rename(&tmp, &path) {
                    let _ = fs::remove_file(tmp);
                    return Err(error.into());
                }
            } else {
                let mut file = options.open(path)?;
                file.write_all(&bytes)?;
                file.write_all(b"\n")?;
                file.sync_data()?;
            }
            Ok(())
        })();
        if let Err(error) = saved {
            self.failed
                .lock()
                .unwrap()
                .insert(job.definition.id.clone());
            return Err(format!("schedule journal write failed; acceptance or timing durability may be uncertain. The same schedule id remains reserved and its original journal is preserved: {error}").into());
        }
        entry.job = job;
        Ok(())
    }
    pub fn list(&self, owner: &str) -> Vec<Job> {
        let mut result: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.job.owner == owner)
            .map(|s| {let mut j=s.job.clone();if self.failed.lock().unwrap().contains(&j.definition.id){j.unavailable=Some("Schedule storage failed; the original journal is preserved. Other schedules and workloads continue.".into());}j})
            .collect();
        result.sort_by_key(|j| (j.next_at_ms, j.definition.id.clone()));
        result
    }
    pub fn page(&self, owner: &str, resident: Option<&str>, args: &Value) -> Value {
        let after = args["after_id"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
        let mut jobs = self.list(owner);
        jobs.retain(|j| {
            j.definition.id.as_str() > after
                && resident
                    .is_none_or(|r| matches!(&j.definition.target, Handler::Resident{id} if id==r))
        });
        jobs.sort_by(|a, b| a.definition.id.cmp(&b.definition.id));
        let more = jobs.len() > limit;
        jobs.truncate(limit);
        let cursor = jobs.last().map(|j| j.definition.id.clone());
        let jobs: Vec<_> = jobs
            .into_iter()
            .map(|j| {
                let mut value = json!(j);
                value["definition"]
                    .as_object_mut()
                    .unwrap()
                    .remove("directions");
                value["directions_bytes"] = json!(j.definition.directions.len());
                value
            })
            .collect();
        json!({"jobs":jobs,"next_after_id":if more {cursor}else{None}})
    }
    pub fn all(&self) -> Vec<Job> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|s| {
                !self.failed.lock().unwrap().contains(&s.job.definition.id)
                    && (s.job.claimed.is_some()
                        || (!s.job.deleted && s.job.enabled && s.job.next_at_ms.is_some()))
            })
            .map(|s| s.job.clone())
            .collect()
    }
    pub fn issues(&self) -> Vec<Value> {
        let mut issues = self.issues.clone();
        issues.extend(self.failed.lock().unwrap().iter().map(|id|json!({"kind":"schedule_storage_unavailable","id":id,"detail":"Original evidence is preserved; this schedule is isolated until storage recovers and the daemon reloads it."})));
        issues
    }
    pub fn get(&self, owner: &str, id: &str) -> Result<Job> {
        jobs::valid_id(id)?;
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .filter(|s| s.job.owner == owner)
            .map(|s| {let mut j=s.job.clone();if self.failed.lock().unwrap().contains(&j.definition.id){j.unavailable=Some("Schedule storage failed; the original journal is preserved. Other schedules and workloads continue.".into());}j})
            .ok_or_else(|| "schedule is unavailable or belongs to a different peer".into())
    }
    pub fn create(&self, owner: &str, definition: Definition) -> Result<Job> {
        definition.validate()?;
        let mut entries = self.entries.lock().unwrap();
        if let Some(old) = entries.get(&definition.id) {
            if old.job.owner == owner && old.initial_hash == definition_hash(&definition) {
                return Ok(old.job.clone());
            }
            return Err("schedule id already exists with different input or owner".into());
        }
        if entries
            .values()
            .filter(|s| !s.job.deleted && s.job.next_at_ms.is_some())
            .count()
            >= 256
        {
            return Err(
                "this host supports 256 pending schedule definitions; completed and deleted jobs retain history without consuming a pending slot".into(),
            );
        }
        if self
            .directory
            .join(format!("{}.jsonl", definition.id))
            .exists()
        {
            return Err(
                "schedule journal already exists and remains preserved for recovery".into(),
            );
        }
        let job = Job {
            definition: definition.clone(),
            owner: owner.into(),
            revision: 1,
            enabled: true,
            deleted: false,
            next_at_ms: Some(definition.at_ms),
            retry_at_ms: 0,
            blocked: false,
            claimed: None,
            last: None,
            last_seq: 0,
            unavailable: None,
        };
        let mut stored = Stored {
            initial_hash: definition_hash(&definition),
            job,
        };
        self.save(&mut stored, "created", json!({}))?;
        let job = stored.job.clone();
        entries.insert(job.definition.id.clone(), stored);
        Ok(job)
    }
    fn change(
        &self,
        owner: &str,
        id: &str,
        kind: &str,
        detail: Value,
        f: impl FnOnce(&mut Job) -> Result<()>,
    ) -> Result<Job> {
        let mut entries = self.entries.lock().unwrap();
        let old = entries
            .get(id)
            .filter(|s| s.job.owner == owner)
            .ok_or("schedule is unavailable or belongs to a different peer")?;
        let mut next = Stored {
            initial_hash: old.initial_hash.clone(),
            job: old.job.clone(),
        };
        f(&mut next.job)?;
        if old.job.next_at_ms.is_none()
            && !next.job.deleted
            && next.job.next_at_ms.is_some()
            && entries
                .values()
                .filter(|s| !s.job.deleted && s.job.next_at_ms.is_some())
                .count()
                >= 256
        {
            return Err("this host supports 256 pending schedule definitions".into());
        }
        self.save(&mut next, kind, detail)?;
        let job = next.job.clone();
        entries.insert(id.into(), next);
        Ok(job)
    }
    pub fn validate_patch(&self, owner: &str, id: &str, patch: &Patch) -> Result<()> {
        let mut preview = self.get(owner, id)?;
        // Validation has no timing side effects. A pending claim will be reconciled
        // by the owner before committing the same definition change.
        if let Some(origin) = preview.claimed.take() {
            preview.next_at_ms = preview.definition.interval_ms.and_then(|i| {
                origin
                    .coalesced_occurrences
                    .checked_add(1)
                    .and_then(|n| i.checked_mul(n))
                    .and_then(|n| origin.due_at_ms.checked_add(n))
            });
            if preview.next_at_ms.is_none() {
                preview.enabled = false;
            }
        }
        patch.clone().apply(&mut preview)
    }
    pub fn patch(&self, owner: &str, id: &str, patch: Patch) -> Result<Job> {
        self.change(
            owner,
            id,
            "updated",
            json!({"expected_revision":patch.expected_revision}),
            |job| patch.apply(job),
        )
    }
    pub fn claim(&self, job: &Job, now: u64) -> Result<Job> {
        self.change(&job.owner, &job.definition.id, "claimed", json!({}), |j| {
            let due = j.next_at_ms.ok_or("schedule has no pending occurrence")?;
            let coalesced = j
                .definition
                .interval_ms
                .map_or(0, |i| now.saturating_sub(due) / i);
            j.claimed = Some(Origin {
                schedule_id: j.definition.id.clone(),
                revision: j.revision,
                due_at_ms: due,
                dispatch_started_at_ms: now,
                coalesced_occurrences: coalesced,
            });
            Ok(())
        })
    }
    pub fn finish(&self, job: &Job, outcome: &str, detail: Value) -> Result<Job> {
        self.change(&job.owner,&job.definition.id,outcome,detail.clone(),|j|{
            let origin=j.claimed.take().ok_or("schedule has no claimed occurrence")?;
            j.last=Some(json!({"outcome":outcome,"origin":origin,"run_id":origin.run_id(),"detail":detail,"at":jobs::now()}));
            j.next_at_ms=j.definition.interval_ms.and_then(|i|origin.coalesced_occurrences.checked_add(1).and_then(|n|i.checked_mul(n)).and_then(|n|origin.due_at_ms.checked_add(n)));
            if j.next_at_ms.is_none(){j.enabled=false;}
            j.retry_at_ms=0;j.blocked=false;Ok(())
        })
    }
    pub fn defer(&self, job: &Job, blocked: bool, reason: &str) -> Result<Job> {
        self.change(
            &job.owner,
            &job.definition.id,
            "deferred",
            json!({"blocked":blocked,"reason":reason}),
            |j| {
                j.retry_at_ms = jobs::now().saturating_add(30000);
                j.blocked = blocked;
                j.last = Some(json!({"outcome":"deferred","reason":reason,"at":jobs::now()}));
                Ok(())
            },
        )
    }
    pub fn history(&self, owner: &str, id: &str, after: u64) -> Result<Value> {
        jobs::valid_id(id)?;
        let entries = self.entries.lock().unwrap();
        let current = entries
            .get(id)
            .filter(|s| s.job.owner == owner)
            .ok_or("schedule is unavailable or belongs to a different peer")?;
        let mut reader =
            BufReader::new(fs::File::open(self.directory.join(format!("{id}.jsonl")))?);
        let mut events = Vec::new();
        let mut bytes = 0;
        let mut first_retained = None;
        loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            if !line.ends_with(b"\n") {
                break;
            }
            let e: Value = serde_json::from_slice(&line)?;
            let seq = e["seq"].as_u64().unwrap_or(0);
            first_retained.get_or_insert(seq);
            if seq > current.job.last_seq {
                break;
            }
            if seq <= after {
                continue;
            }
            if !events.is_empty() && (bytes + line.len() > 256 * 1024 || events.len() >= 20) {
                break;
            }
            bytes += line.len();
            events.push(e);
        }
        Ok(
            json!({"first_retained_sequence":first_retained,"earlier_timer_records_coalesced":first_retained.is_some_and(|s|s>after.saturating_add(1)),"next_after":events.last().map_or(after,|e|e["seq"].as_u64().unwrap_or(after)),"events":events}),
        )
    }
    pub fn delay(&self, now: u64) -> std::time::Duration {
        let next = self
            .all()
            .iter()
            .filter(|j| !j.deleted && j.enabled)
            .filter_map(|j| j.next_at_ms.map(|due| due.max(j.retry_at_ms)))
            .min();
        std::time::Duration::from_millis(
            next.map_or(60000, |n| n.saturating_sub(now).clamp(1, 60000)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition() -> Definition {
        Definition {
            id: state::random_hex().unwrap(),
            name: "Heartbeat pattern".into(),
            target: Handler::Resident {
                id: state::random_hex().unwrap(),
            },
            directions: "Saved authored directions".into(),
            at_ms: 1000,
            interval_ms: Some(1000),
        }
    }
    #[test]
    fn anchored_coalescing_and_claim_recovery_preserve_one_occurrence() {
        let root =
            std::env::temp_dir().join(format!("boxen-scheduler-{}", state::random_hex().unwrap()));
        let owner = state::random_hex().unwrap();
        let def = definition();
        let store = Schedules::open(root.clone()).unwrap();
        let job = store.create(&owner, def.clone()).unwrap();
        assert_eq!(store.create(&owner, def.clone()).unwrap().last_seq, 1);
        let claimed = store.claim(&job, 3500).unwrap();
        let origin = claimed.claimed.clone().unwrap();
        assert_eq!(origin.coalesced_occurrences, 2);
        let id = origin.run_id();
        let rejected: Patch =
            serde_json::from_value(json!({"expected_revision":1,"name":""})).unwrap();
        assert!(store.validate_patch(&owner, &def.id, &rejected).is_err());
        let unchanged = store.get(&owner, &def.id).unwrap();
        assert_eq!(unchanged.last_seq, claimed.last_seq);
        assert_eq!(unchanged.claimed, claimed.claimed);
        drop(store);
        let store = Schedules::open(root.clone()).unwrap();
        let recovered = store.get(&owner, &def.id).unwrap();
        assert_eq!(recovered.claimed.as_ref().unwrap().run_id(), id);
        let finished = store
            .finish(&recovered, "dispatched", json!({"run_id":id}))
            .unwrap();
        assert_eq!(finished.next_at_ms, Some(4000));
        assert!(finished.claimed.is_none());
        assert_eq!(store.delay(3500).as_millis(), 500);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn torn_tail_and_one_corrupt_job_do_not_take_healthy_jobs_offline() {
        let root =
            std::env::temp_dir().join(format!("boxen-scheduler-{}", state::random_hex().unwrap()));
        let owner = state::random_hex().unwrap();
        let good = definition();
        let bad = definition();
        let store = Schedules::open(root.clone()).unwrap();
        store.create(&owner, good.clone()).unwrap();
        store.create(&owner, bad.clone()).unwrap();
        drop(store);
        OpenOptions::new()
            .append(true)
            .open(root.join(format!("{}.jsonl", good.id)))
            .unwrap()
            .write_all(b"{uncommitted")
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(root.join(format!("{}.jsonl", bad.id)))
            .unwrap()
            .write_all(b"{corrupt}\n")
            .unwrap();
        let store = Schedules::open(root.clone()).unwrap();
        assert!(store.get(&owner, &good.id).is_ok());
        assert!(store.get(&owner, &bad.id).is_err());
        assert_eq!(store.issues().len(), 1);
        assert_eq!(store.issues()[0]["id"], bad.id);
        assert!(store.get(&state::random_hex().unwrap(), &good.id).is_err());
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
