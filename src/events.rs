//! Bounded pending observations. Accepted run journals own presentation/effects.
use crate::{
    Result,
    jobs::{self, Run},
    logging::{self, Context, Level, Target},
    scheduler::Handler,
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

const MAX_SOURCE_BYTES: usize = 65536;
const MAX_INPUT: usize = 32768;
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub command: Vec<String>,
    pub cwd: String,
    #[serde(default = "poll_default")]
    pub interval_ms: u64,
    #[serde(default = "mode_default")]
    pub mode: String,
    #[serde(default = "timeout_default")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub tools: Vec<Tool>,
}
fn poll_default() -> u64 {
    60000
}
fn mode_default() -> String {
    "ordered".into()
}
fn timeout_default() -> u64 {
    5000
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    #[serde(default)]
    pub parameters: Value,
    pub name: String,
    pub description: String,
    pub kind: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Observation {
    pub seq: u64,
    pub id: String,
    pub observed_at_ms: u64,
    pub received_at_ms: u64,
    pub payload: Value,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Health {
    pub pending_notice: bool,
    pub notice: Option<Value>,
    pub consecutive_failures: u64,
    pub condition: String,
    pub failures: u64,
    pub first_failure_ms: Option<u64>,
    pub last_failure_ms: Option<u64>,
    pub last_success_ms: Option<u64>,
    pub next_probe_ms: u64,
    pub transitions: u64,
    pub notice_seq: u64,
    pub notice_at_ms: u64,
    pub reason: String,
}
impl Default for Health {
    fn default() -> Self {
        Self {
            pending_notice: false,
            notice: None,
            consecutive_failures: 0,
            condition: "unknown".into(),
            failures: 0,
            first_failure_ms: None,
            last_failure_ms: None,
            last_success_ms: None,
            next_probe_ms: 0,
            transitions: 0,
            notice_seq: 0,
            notice_at_ms: 0,
            reason: String::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Source {
    pub created_at_ms: u64,
    pub ingest_window_ms: u64,
    pub ingests: u64,
    pub spec: SourceSpec,
    pub owner: String,
    pub generation: u64,
    pub enabled: bool,
    pub seq: u64,
    pub dropped: u64,
    pub items: Vec<Observation>,
    pub cursor: Value,
    pub producer_seq: u64,
    pub health: Health,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionSpec {
    pub id: String,
    pub source: String,
    pub target: Handler,
    #[serde(default = "batch_default")]
    pub interval_ms: u64,
    #[serde(default)]
    pub waking: bool,
    #[serde(default)]
    pub health_waking: bool,
    #[serde(default = "lifetime_default")]
    pub lifetime: String,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    #[serde(default = "wake_default")]
    pub wake_limit: u64,
    #[serde(default = "target_wake_default")]
    pub target_wake_limit: u64,
}
fn batch_default() -> u64 {
    60000
}
fn lifetime_default() -> String {
    "activity".into()
}
fn target_wake_default() -> u64 {
    120
}
fn wake_default() -> u64 {
    60
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub created_at_ms: u64,
    pub creation: String,
    pub spec: SubscriptionSpec,
    pub owner: String,
    pub revision: u64,
    pub generation: u64,
    pub enabled: bool,
    pub ended: bool,
    pub cursor: u64,
    pub health_cursor: u64,
    pub ready: bool,
    pub last_presented_ms: u64,
    pub cutoff: u64,
    pub health_cutoff: u64,
    pub wake_window_ms: u64,
    pub wakes: u64,
    pub condition: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Data {
    sources: HashMap<String, Source>,
    subscriptions: HashMap<String, Subscription>,
}
struct Inner {
    data: Data,
    busy: HashSet<String>,
    unavailable: Option<String>,
}
pub struct Events {
    path: PathBuf,
    inner: Mutex<Inner>,
}
fn size(v: &impl Serialize) -> usize {
    serde_json::to_vec(v).map_or(usize::MAX, |b| b.len())
}
pub fn timer_id(kind: &str, id: &str) -> String {
    use sha2::{Digest, Sha256};
    state::hex(&Sha256::digest(format!("boxen-events-v1:{kind}:{id}")))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: u64,
    enabled: Option<bool>,
    end: Option<bool>,
    expires_at_ms: Option<u64>,
    waking: Option<bool>,
    health_waking: Option<bool>,
    wake_limit: Option<u64>,
    target_wake_limit: Option<u64>,
}
impl Events {
    pub fn open(directory: PathBuf) -> Result<Self> {
        state::private_dir(&directory)?;
        let path = directory.join("pending.json");
        let (data, unavailable) = if path.exists() {
            match fs::read(&path).and_then(|b|{if b.len()>4*1024*1024{return Err(std::io::Error::other("oversized event store"));}serde_json::from_slice::<Data>(&b).map_err(std::io::Error::other)}){Ok(d)=>(d,None),Err(_)=>(Data::default(),Some("Observation storage is unavailable; original pending.json is preserved. Other workloads remain available.".into()))}
        } else {
            (Data::default(), None)
        };
        Ok(Self {
            path,
            inner: Mutex::new(Inner {
                data,
                busy: HashSet::new(),
                unavailable,
            }),
        })
    }
    fn save(&self, inner: &mut Inner, next: Data) -> Result<()> {
        if let Some(reason) = &inner.unavailable {
            return Err(reason.clone().into());
        }
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("observation storage exceeds its 4 MiB bound".into());
        }
        let tmp = self.path.with_extension("pending");
        let result = (|| -> Result<()> {
            if tmp.exists() {
                fs::remove_file(&tmp)?;
            }
            state::write_new(&tmp, &bytes)?;
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
            inner.unavailable=Some("Observation storage failed; original evidence is preserved and other workloads continue. Reload after storage repair reconciles accepted input.".into());
            result?;
        }
        logging::record(
            Level::Debug,
            Target::State,
            "observation.state_committed",
            Context::default(),
            || json!({"bytes":bytes.len(),"sources":next.sources.iter().filter(|(id,s)|inner.data.sources.get(*id).is_none_or(|old|old.generation!=s.generation||old.seq!=s.seq||old.health.failures!=s.health.failures||old.health.condition!=s.health.condition)).map(|(id,s)|json!({"source_id":id,"peer_id":s.owner,"generation":s.generation,"seq":s.seq,"condition":s.health.condition,"failures":s.health.failures})).collect::<Vec<_>>(),"subscriptions":next.subscriptions.iter().filter(|(id,s)|inner.data.subscriptions.get(*id).is_none_or(|old|old.revision!=s.revision||old.cursor!=s.cursor||old.health_cursor!=s.health_cursor||old.ready!=s.ready||old.cutoff!=s.cutoff)).map(|(id,s)|json!({"subscription_id":id,"thread_id":s.spec.target.id(),"peer_id":s.owner,"generation":s.generation,"revision":s.revision,"condition":s.condition,"cursor":s.cursor,"ready":s.ready,"cutoff":s.cutoff})).collect::<Vec<_>>()}),
            || Value::Null,
        );
        inner.data = next;
        Ok(())
    }
    fn copy(inner: &Inner) -> Result<Data> {
        if let Some(reason) = &inner.unavailable {
            return Err(reason.clone().into());
        }
        Ok(inner.data.clone())
    }
    pub fn available(&self) -> bool {
        self.inner.lock().unwrap().unavailable.is_none()
    }
    pub fn source(&self, owner: &str, id: &str) -> Result<Source> {
        let g = self.inner.lock().unwrap();
        g.data
            .sources
            .get(id)
            .filter(|s| s.owner == owner)
            .cloned()
            .ok_or_else(|| "source is unavailable or owned by another peer".into())
    }
    pub fn subscription(&self, owner: &str, id: &str) -> Result<Subscription> {
        let g = self.inner.lock().unwrap();
        g.data
            .subscriptions
            .get(id)
            .filter(|s| s.owner == owner)
            .cloned()
            .ok_or_else(|| "subscription is unavailable or owned by another peer".into())
    }
    pub fn create_source(&self, owner: &str, spec: SourceSpec) -> Result<Source> {
        if let Some(id) = &spec.audience {
            jobs::valid_id(id)?;
        }
        jobs::valid_id(&spec.id)?;
        if spec.name.trim().is_empty()
            || spec.name.len() > 100
            || !matches!(spec.mode.as_str(), "ordered" | "latest")
            || !(1000..=86400000).contains(&spec.interval_ms)
            || !(100..=10000).contains(&spec.timeout_ms)
            || spec.command.len() > 32
            || size(&spec) > 8192
            || !std::path::Path::new(&spec.cwd).is_absolute()
            || !std::path::Path::new(&spec.cwd).is_dir()
            || (!spec.tools.is_empty() && spec.command.is_empty())
            || spec.tools.len() > 16
            || spec.tools.iter().any(|t| {
                t.name.is_empty()
                    || t.name.len() > 64
                    || t.description.len() > 1000
                    || !matches!(t.kind.as_str(), "query" | "action")
            })
        {
            return Err("source needs a name, existing absolute cwd, ordered/latest mode, interval 1000–86400000 ms, timeout 100–10000 ms, at most 16 named query/action tools, and a definition below 8 KiB".into());
        }
        let mut g = self.inner.lock().unwrap();
        if let Some(old) = g.data.sources.get(&spec.id) {
            if old.owner == owner && old.spec == spec {
                return Ok(old.clone());
            }
            return Err("source id already exists with different input or owner".into());
        }
        if g.data.sources.len() >= 32 {
            return Err("this host supports 32 source definitions".into());
        }
        let s = Source {
            created_at_ms: jobs::now(),
            ingest_window_ms: jobs::now(),
            ingests: 0,
            spec,
            owner: owner.into(),
            generation: 1,
            enabled: true,
            seq: 0,
            dropped: 0,
            items: Vec::new(),
            cursor: Value::Null,
            producer_seq: 0,
            health: Health::default(),
        };
        let mut next = Self::copy(&g)?;
        next.sources.insert(s.spec.id.clone(), s.clone());
        self.save(&mut g, next)?;
        Ok(s)
    }
    pub fn subscribe(&self, owner: &str, mut spec: SubscriptionSpec) -> Result<Subscription> {
        jobs::valid_id(&spec.id)?;
        jobs::valid_id(spec.target.id())?;
        let source = self.source(owner, &spec.source)?;
        if source
            .spec
            .audience
            .as_ref()
            .is_some_and(|id| id != spec.target.id())
        {
            return Err("observation source is restricted to another target".into());
        }
        if !(1000..=86400000).contains(&spec.interval_ms)
            || !(1..=3600).contains(&spec.wake_limit)
            || !(1..=3600).contains(&spec.target_wake_limit)
            || !matches!(spec.lifetime.as_str(), "activity" | "persistent" | "until")
        {
            return Err("subscription interval is 1000–86400000 ms, wake_limit is 1–3600 per hour, and lifetime is activity, persistent, or until".into());
        }
        let creation = timer_id("subscription-input", &serde_json::to_string(&spec)?);
        let mut g = self.inner.lock().unwrap();
        if let Some(old) = g.data.subscriptions.get(&spec.id) {
            if old.owner == owner && old.creation == creation {
                return Ok(old.clone());
            }
            return Err("subscription id already exists with different input or owner".into());
        }
        if spec.lifetime == "activity" && spec.expires_at_ms.is_none() {
            spec.expires_at_ms = Some(jobs::now() + 3600000);
        }
        if spec.lifetime != "persistent" && spec.expires_at_ms.is_none_or(|t| t <= jobs::now()) {
            return Err(
                "activity/until subscriptions have a future expiry; activity defaults to one hour"
                    .into(),
            );
        }
        if spec.lifetime == "persistent" && spec.expires_at_ms.is_some() {
            return Err(
                "persistent subscriptions have no expiry; until expresses a fixed deadline".into(),
            );
        }
        if g.data.subscriptions.len() >= 128 {
            return Err(
                "this host supports 128 subscription definitions including stopped histories"
                    .into(),
            );
        }
        if !matches!(
            spec.target,
            Handler::Resident { .. } | Handler::Thread { .. }
        ) {
            return Err("observations target residents or ordinary threads".into());
        }
        let s = Subscription {
            created_at_ms: jobs::now(),
            creation,
            spec,
            owner: owner.into(),
            revision: 1,
            generation: 1,
            enabled: true,
            ended: false,
            cursor: 0,
            health_cursor: 0,
            ready: false,
            last_presented_ms: 0,
            cutoff: 0,
            health_cutoff: 0,
            wake_window_ms: jobs::now(),
            wakes: 0,
            condition: "active".into(),
        };
        let mut next = Self::copy(&g)?;
        next.subscriptions.insert(s.spec.id.clone(), s.clone());
        self.save(&mut g, next)?;
        Ok(s)
    }
    pub fn update(&self, owner: &str, kind: &str, id: &str, args: &Value) -> Result<Value> {
        let patch: Update = serde_json::from_value(args.clone())?;
        if patch.wake_limit.is_some_and(|v| !(1..=3600).contains(&v))
            || patch
                .target_wake_limit
                .is_some_and(|v| !(1..=3600).contains(&v))
        {
            return Err("wake limits are 1–3600 accepted presentations per hour".into());
        }
        if kind == "source"
            && (patch.end.is_some()
                || patch.expires_at_ms.is_some()
                || patch.waking.is_some()
                || patch.health_waking.is_some()
                || patch.wake_limit.is_some()
                || patch.target_wake_limit.is_some())
        {
            return Err("source updates contain expected_revision and enabled".into());
        }
        let mut g = self.inner.lock().unwrap();
        let mut next = Self::copy(&g)?;
        let value = if kind == "source" {
            let s = next
                .sources
                .get_mut(id)
                .filter(|s| s.owner == owner)
                .ok_or("source unavailable")?;
            if Some(patch.expected_revision) != Some(s.generation) {
                return Err("source generation conflict".into());
            }
            s.enabled = args["enabled"]
                .as_bool()
                .ok_or("source update has enabled")?;
            s.generation += 1;
            s.health.next_probe_ms = 0;
            json!(s)
        } else {
            let s = next
                .subscriptions
                .get_mut(id)
                .filter(|s| s.owner == owner)
                .ok_or("subscription unavailable")?;
            if Some(patch.expected_revision) != Some(s.revision) {
                return Err("subscription revision conflict".into());
            }
            if s.ended || s.spec.expires_at_ms.is_some_and(|t| t <= jobs::now()) {
                return Err(
                    "ended subscription stays ended; a new subscription expresses a new activity"
                        .into(),
                );
            }
            if let Some(t) = patch.expires_at_ms {
                if s.spec.lifetime == "persistent" || t <= jobs::now() {
                    return Err(
                        "renewal has a future expiry on an activity/until subscription".into(),
                    );
                }
                s.spec.expires_at_ms = Some(t);
            }
            if let Some(v) = patch.waking {
                s.spec.waking = v;
            }
            if let Some(v) = patch.health_waking {
                s.spec.health_waking = v;
            }
            if let Some(v) = patch.wake_limit {
                s.spec.wake_limit = v;
            }
            if let Some(v) = patch.target_wake_limit {
                s.spec.target_wake_limit = v;
            }
            let prior_enabled = s.enabled;
            if let Some(v) = patch.enabled {
                s.enabled = v;
            }
            if patch.end == Some(true) {
                s.ended = true;
                s.enabled = false;
            }
            s.ready = false;
            if prior_enabled != s.enabled || s.ended {
                s.generation += 1;
            }
            s.revision += 1;
            s.condition = if s.ended {
                "ended"
            } else if s.enabled {
                "active"
            } else {
                "paused"
            }
            .into();
            json!(s)
        };
        self.save(&mut g, next)?;
        Ok(value)
    }
    pub fn list(&self, owner: &str) -> Value {
        let g = self.inner.lock().unwrap();
        json!({"sources":g.data.sources.values().filter(|s|s.owner==owner).map(|s|json!({"id":s.spec.id,"name":s.spec.name,"audience":s.spec.audience,"generation":s.generation,"enabled":s.enabled,"mode":s.spec.mode,"seq":s.seq,"retained":s.items.len(),"dropped":s.dropped,"health":s.health,"tools":s.spec.tools})).collect::<Vec<_>>(),"subscriptions":g.data.subscriptions.values().filter(|s|s.owner==owner).collect::<Vec<_>>(),"unavailable":g.unavailable})
    }
    fn active(s: &Subscription) -> bool {
        s.enabled && !s.ended && s.spec.expires_at_ms.is_none_or(|t| t > jobs::now())
    }
    pub fn expire(&self) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        if !g
            .data
            .subscriptions
            .values()
            .any(|s| s.enabled && s.spec.expires_at_ms.is_some_and(|t| t <= jobs::now()))
        {
            return Ok(());
        }
        let mut next = Self::copy(&g)?;
        for s in next.subscriptions.values_mut() {
            if s.enabled && s.spec.expires_at_ms.is_some_and(|t| t <= jobs::now()) {
                s.enabled = false;
                s.ended = true;
                s.ready = false;
                s.generation += 1;
                s.revision += 1;
                s.condition = "expired".into();
            }
        }
        self.save(&mut g, next)
    }
    pub fn ready(&self, owner: &str, id: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        let mut next = Self::copy(&g)?;
        let source_id = next
            .subscriptions
            .get(id)
            .filter(|s| s.owner == owner)
            .ok_or("subscription unavailable")?
            .spec
            .source
            .clone();
        let mut promoted = false;
        if let Some(src) = next.sources.get_mut(&source_id) {
            let before = src.health.notice_seq;
            Self::promote_notice(&mut src.health);
            promoted = before != src.health.notice_seq;
        }
        let s = next.subscriptions.get_mut(id).expect("checked");
        if !Self::active(s) {
            return Ok(());
        }
        let source = next.sources.get(&source_id).ok_or("source unavailable")?;
        let s = next.subscriptions.get_mut(id).expect("checked");
        if s.ready
            && s.cutoff == source.seq
            && s.health_cutoff == source.health.notice_seq
            && !promoted
        {
            return Ok(());
        }
        s.ready = true;
        s.cutoff = source.seq;
        s.health_cutoff = source.health.notice_seq;
        self.save(&mut g, next)
    }
    pub fn pending_targets(&self) -> Vec<(String, Handler)> {
        let g = self.inner.lock().unwrap();
        if g.unavailable.is_some() {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        let mut v = Vec::new();
        for s in g
            .data
            .subscriptions
            .values()
            .filter(|s| Self::active(s) && s.ready)
        {
            let Some(_src) = g.data.sources.get(&s.spec.source).filter(|src| src.enabled) else {
                continue;
            };
            let data = s.spec.waking && s.cutoff > s.cursor;
            let health = s.spec.health_waking && s.health_cutoff > s.health_cursor;
            if (data || health) && seen.insert((s.owner.clone(), s.spec.target.id().to_owned())) {
                v.push((s.owner.clone(), s.spec.target.clone()));
            }
        }
        v
    }
    fn batch(data: &Data, s: &Subscription) -> Option<Value> {
        let source = data.sources.get(&s.spec.source)?;
        if !Self::active(s) || !s.ready || !source.enabled {
            return None;
        }
        if s.cutoff <= s.cursor && s.health_cutoff <= s.health_cursor {
            return None;
        }
        let first = source.items.first().map_or(source.seq + 1, |i| i.seq);
        let gap = if s.cursor + 1 < first && s.cursor < s.cutoff {
            Some(
                json!({"from_seq":s.cursor+1,"through_seq":(first-1).min(s.cutoff),"reason":if source.spec.mode=="latest"{"superseded pending state"}else{"source retention bound"}}),
            )
        } else {
            None
        };
        let mut items = Vec::new();
        for item in source
            .items
            .iter()
            .filter(|i| i.seq > s.cursor && (i.seq <= s.cutoff || source.spec.mode == "latest"))
        {
            if size(&items) + size(item) > 10000 {
                break;
            }
            items.push(item.clone());
        }
        let health = if s.health_cutoff > s.health_cursor {
            source.health.notice.clone()
        } else {
            None
        };
        if items.is_empty() && gap.is_none() && health.is_none() {
            return None;
        }
        let through = items
            .last()
            .map_or(s.cursor.max(first.saturating_sub(1).min(s.cutoff)), |i| {
                i.seq
            });
        Some(
            json!({"subscription_id":s.spec.id,"subscription_generation":s.generation,"source_id":source.spec.id,"source_generation":source.generation,"mode":source.spec.mode,"source_health":{"condition":source.health.condition,"last_success_ms":source.health.last_success_ms,"last_failure_ms":source.health.last_failure_ms},"from_seq":s.cursor+1,"through_seq":through,"health_through":if health.is_some(){source.health.notice_seq}else{s.health_cursor},"gap":gap,"health":health,"observations":items,"provenance":"Source-authored observations; presentation does not prove understanding or prescribe a reply."}),
        )
    }
    pub fn batches(&self, owner: &str, target: &str) -> Vec<Value> {
        let g = self.inner.lock().unwrap();
        if g.unavailable.is_some() {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut subs: Vec<_> = g
            .data
            .subscriptions
            .values()
            .filter(|s| s.owner == owner && s.spec.target.id() == target)
            .collect();
        subs.sort_by_key(|s| (s.last_presented_ms, s.spec.id.clone()));
        for s in subs {
            if let Some(b) = Self::batch(&g.data, s) {
                if size(&result) + size(&b) > 32768 {
                    break;
                }
                result.push(b);
            }
        }
        result
    }
    pub fn acknowledge(&self, owner: &str, batches: &[Value], waking: bool) -> Result<()> {
        if batches.is_empty() {
            return Ok(());
        }
        let mut g = self.inner.lock().unwrap();
        let advance = batches.iter().any(|b| {
            g.data
                .subscriptions
                .get(b["subscription_id"].as_str().unwrap_or(""))
                .is_some_and(|s| {
                    s.owner == owner
                        && Some(s.generation) == b["subscription_generation"].as_u64()
                        && (b["through_seq"].as_u64().is_some_and(|v| v > s.cursor)
                            || b["health_through"]
                                .as_u64()
                                .is_some_and(|v| v > s.health_cursor))
                })
        });
        if !advance {
            return Ok(());
        }
        let mut next = Self::copy(&g)?;
        for b in batches {
            if let Some(s) = next
                .subscriptions
                .get_mut(b["subscription_id"].as_str().unwrap_or(""))
                .filter(|s| {
                    s.owner == owner && Some(s.generation) == b["subscription_generation"].as_u64()
                })
            {
                let advanced = b["through_seq"].as_u64().is_some_and(|v| v > s.cursor)
                    || b["health_through"]
                        .as_u64()
                        .is_some_and(|v| v > s.health_cursor);
                s.cursor = s.cursor.max(b["through_seq"].as_u64().unwrap_or(s.cursor));
                s.health_cursor = s
                    .health_cursor
                    .max(b["health_through"].as_u64().unwrap_or(s.health_cursor));
                if advanced {
                    s.ready = false;
                    s.last_presented_ms = jobs::now();
                }
                if waking && advanced {
                    if jobs::now().saturating_sub(s.wake_window_ms) >= 3600000 {
                        s.wake_window_ms = jobs::now();
                        s.wakes = 0;
                    }
                    s.wakes += 1;
                }
                s.condition = "active".into();
            }
        }
        self.save(&mut g, next)
    }
    pub fn wake_allowed(&self, owner: &str, target: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        if g.unavailable.is_some() {
            return false;
        }
        let now = jobs::now();
        let mut total = 0;
        let mut target_limit = 3600;
        let mut eligible = false;
        let mut last_presented = 0;
        let mut interval = u64::MAX;
        let positions: HashMap<_, _> = g
            .data
            .sources
            .iter()
            .map(|(id, s)| (id.clone(), s.enabled))
            .collect();
        for s in g
            .data
            .subscriptions
            .values_mut()
            .filter(|s| s.owner == owner && s.spec.target.id() == target)
        {
            if now.saturating_sub(s.wake_window_ms) >= 3600000 {
                s.wake_window_ms = now;
                s.wakes = 0;
            }
            total += s.wakes;
            last_presented = last_presented.max(s.last_presented_ms);
            if Self::active(s) {
                interval = interval.min(s.spec.interval_ms);
            }
            if Self::active(s) {
                target_limit = target_limit.min(s.spec.target_wake_limit);
            }
            let pending = (s.spec.waking && s.cutoff > s.cursor)
                || (s.spec.health_waking && s.health_cutoff > s.health_cursor);
            if Self::active(s) && s.ready && pending && positions.get(&s.spec.source) == Some(&true)
            {
                if s.wakes < s.spec.wake_limit {
                    eligible = true;
                } else {
                    s.condition = "wake_limit".into();
                }
            }
        }
        let allowed =
            eligible && total < target_limit && now.saturating_sub(last_presented) >= interval;
        logging::record(
            Level::Debug,
            Target::Workload,
            "observation.wake_decision",
            Context {
                peer_id: Some(owner),
                thread_id: Some(target),
                ..Context::default()
            },
            || json!({"allowed":allowed,"eligible":eligible,"presentations":total,"target_limit":target_limit,"last_presented_ms":last_presented,"minimum_interval_ms":interval}),
            || Value::Null,
        );
        allowed
    }
    pub fn publish(
        &self,
        owner: &str,
        id: &str,
        generation: u64,
        producer_seq: Option<u64>,
        response: Value,
    ) -> Result<Value> {
        if response["protocol"] != "boxen-adapter-v1" {
            return Err("observation publication has protocol boxen-adapter-v1".into());
        }
        if size(&response) > MAX_SOURCE_BYTES {
            return Err("adapter response exceeds 64 KiB".into());
        }
        let mut g = self.inner.lock().unwrap();
        let mut next = Self::copy(&g)?;
        let source = next
            .sources
            .get_mut(id)
            .filter(|s| s.owner == owner)
            .ok_or("source unavailable")?;
        if !source.enabled || source.generation != generation {
            return Err(
                "source generation changed or source stopped; late observations are rejected"
                    .into(),
            );
        }
        if let Some(seq) = producer_seq {
            if seq <= source.producer_seq {
                logging::record(
                    Level::Debug,
                    Target::Workload,
                    "observation.duplicate",
                    Context::peer(owner),
                    || json!({"source_id":id,"producer_sequence":seq,"last_producer_sequence":source.producer_seq}),
                    || Value::Null,
                );
                return Ok(json!({"duplicate":true,"seq":source.seq}));
            }
            if seq != source.producer_seq + 1 {
                return Err(
                    "producer sequence has a gap; status returns the last accepted producer_seq"
                        .into(),
                );
            }
            source.producer_seq = seq;
        }
        if jobs::now().saturating_sub(source.ingest_window_ms) >= 1000 {
            source.ingest_window_ms = jobs::now();
            source.ingests = 0;
        }
        if source.ingests >= 20 {
            return Err("source ingestion is limited to 20 accepted batches per second".into());
        }
        source.ingests += 1;
        let observations = response["observations"]
            .as_array()
            .ok_or("adapter response has an observations array")?;
        if observations.len() > 64 {
            return Err("adapter response contains at most 64 observations".into());
        }
        for value in observations {
            let key = value["id"]
                .as_str()
                .filter(|k| !k.is_empty() && k.len() <= 200)
                .ok_or("observation has a bounded nonblank id")?;
            let observed = value["observed_at_ms"]
                .as_u64()
                .ok_or("observation has observed_at_ms")?;
            if size(&value["payload"]) > 8192 {
                return Err("observation payload exceeds 8 KiB".into());
            }
            if source.items.iter().any(|i| i.id == key) {
                continue;
            }
            source.seq += 1;
            source.items.push(Observation {
                seq: source.seq,
                id: key.into(),
                observed_at_ms: observed,
                received_at_ms: jobs::now(),
                payload: value["payload"].clone(),
            });
            while source.items.len() > 64
                || size(&source.items) > MAX_SOURCE_BYTES
                || (source.spec.mode == "latest" && source.items.len() > 1)
            {
                source.items.remove(0);
                source.dropped += 1;
            }
        }
        if let Some(cursor) = response.get("cursor") {
            if size(cursor) > 8192 {
                return Err("adapter cursor exceeds 8 KiB".into());
            }
            source.cursor = cursor.clone();
        }
        Self::health(source, None);
        let result = json!({"seq":source.seq,"retained":source.items.len(),"dropped":source.dropped,"producer_seq":source.producer_seq});
        self.save(&mut g, next)?;
        logging::record(
            Level::Info,
            Target::Workload,
            "observation.accepted",
            Context::peer(owner),
            || json!({"source_id":id,"generation":generation,"seq":result["seq"],"retained":result["retained"],"evicted":result["dropped"]}),
            || response.clone(),
        );
        Ok(result)
    }
    fn promote_notice(h: &mut Health) {
        let now = jobs::now();
        if h.pending_notice && (h.notice_at_ms == 0 || now.saturating_sub(h.notice_at_ms) >= 60000)
        {
            h.notice_seq += 1;
            h.notice_at_ms = now;
            h.pending_notice = false;
            h.notice = Some(
                json!({"condition":h.condition,"reason":h.reason,"notice_seq":h.notice_seq,"at_ms":now,"transitions":h.transitions}),
            );
        }
    }
    fn health(source: &mut Source, error: Option<&str>) {
        let h = &mut source.health;
        let now = jobs::now();
        let condition = if error.is_some() {
            "unavailable"
        } else {
            "healthy"
        };
        if h.condition != condition {
            h.condition = condition.into();
            h.transitions += 1;
            h.pending_notice = true;
        }
        if let Some(reason) = error {
            h.failures += 1;
            h.consecutive_failures += 1;
            h.first_failure_ms.get_or_insert(now);
            h.last_failure_ms = Some(now);
            h.reason = reason.chars().take(240).collect();
            h.next_probe_ms = now
                + 1000u64
                    .saturating_mul(1u64 << h.consecutive_failures.min(8))
                    .min(300000);
        } else {
            h.last_success_ms = Some(now);
            h.consecutive_failures = 0;
            h.next_probe_ms = 0;
            h.reason.clear();
        }
        Self::promote_notice(h);
    }
    fn collecting_allowed(&self, owner: &str, id: &str, generation: u64) -> bool {
        let g = self.inner.lock().unwrap();
        g.unavailable.is_none()
            && g.data
                .sources
                .get(id)
                .is_some_and(|s| s.owner == owner && s.enabled && s.generation == generation)
            && g.data
                .subscriptions
                .values()
                .any(|s| s.spec.source == id && Self::active(s))
    }
    pub fn collect(self: &Arc<Self>, owner: &str, id: &str) -> Result<()> {
        self.expire()?;
        let source = {
            let mut g = self.inner.lock().unwrap();
            if g.unavailable.is_some() {
                return Err("observation storage is unavailable".into());
            }
            let source = g
                .data
                .sources
                .get(id)
                .filter(|s| s.owner == owner)
                .ok_or("source unavailable")?
                .clone();
            if !source.enabled
                || source.spec.command.is_empty()
                || source.health.next_probe_ms > jobs::now()
                || g.busy.len() >= 4
                || g.busy.contains(id)
                || !g
                    .data
                    .subscriptions
                    .values()
                    .any(|s| s.spec.source == id && Self::active(s))
            {
                return Ok(());
            }
            g.busy.insert(id.into());
            source
        };
        let request = json!({"protocol":"boxen-adapter-v1","operation":"collect","source_id":id,"generation":source.generation,"cursor":source.cursor});
        logging::record(
            Level::Info,
            Target::Workload,
            "observation.collecting",
            Context::peer(owner),
            || json!({"source_id":id,"generation":source.generation}),
            || Value::Null,
        );
        let result = adapter(&source.spec, request, || {
            !self.collecting_allowed(owner, id, source.generation)
        });
        self.inner.lock().unwrap().busy.remove(id);
        if !self.collecting_allowed(owner, id, source.generation) {
            logging::record(
                Level::Info,
                Target::Workload,
                "observation.collection_stopped",
                Context::peer(owner),
                || json!({"source_id":id,"generation":source.generation}),
                || Value::Null,
            );
            return Ok(());
        }
        match result {
            Ok(response) => {
                if let Err(error) = self.publish(owner, id, source.generation, None, response) {
                    self.failure(owner, id, source.generation, &error.to_string())?;
                }
            }
            Err(error) => self.failure(owner, id, source.generation, &error.to_string())?,
        }
        Ok(())
    }
    fn failure(&self, owner: &str, id: &str, generation: u64, reason: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        let mut next = Self::copy(&g)?;
        if let Some(s) = next
            .sources
            .get_mut(id)
            .filter(|s| s.owner == owner && s.generation == generation && s.enabled)
        {
            Self::health(s, Some(reason));
            self.save(&mut g, next)?;
        }
        logging::record(
            Level::Warn,
            Target::Workload,
            "observation.source_failed",
            Context::peer(owner),
            || json!({"source_id":id,"generation":generation,"reason":reason}),
            || Value::Null,
        );
        Ok(())
    }
    pub fn tool_call(
        &self,
        owner: &str,
        target: &str,
        run: &Arc<Run>,
        call_id: &str,
        args: Value,
    ) -> Result<Value> {
        if run.cancellation_requested() {
            return Err(
                "run cancellation is already requested; no adapter call was dispatched".into(),
            );
        }
        let sub = self.subscription(
            owner,
            args["subscription_id"]
                .as_str()
                .ok_or("adapter call has subscription_id")?,
        )?;
        if sub.spec.target.id() != target || !Self::active(&sub) {
            return Err("adapter tool belongs to an active subscription on this thread".into());
        }
        let source = self.source(owner, &sub.spec.source)?;
        let tool = source
            .spec
            .tools
            .iter()
            .find(|t| Some(t.name.as_str()) == args["tool"].as_str())
            .ok_or("adapter tool is not declared by this source")?;
        if !source.enabled || source.generation != args["source_generation"].as_u64().unwrap_or(0) {
            return Err("adapter call has the current source_generation; stopped or stale sources reject actions".into());
        }
        // The run journal is the only receipt authority. An unknown historical
        // effect never becomes an automatic dispatch after restart/retry.
        {
            let mut g = self.inner.lock().unwrap();
            if g.unavailable.is_some() {
                return Err(
                    "observation storage is unavailable; no adapter call was dispatched".into(),
                );
            }
            if g.busy.len() >= 4 || g.busy.contains(&source.spec.id) {
                return Err("adapter is busy; no new call was dispatched".into());
            }
            g.busy.insert(source.spec.id.clone());
        }
        struct Release<'a>(&'a Events, String);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.inner.lock().unwrap().busy.remove(&self.1);
            }
        }
        let _release = Release(self, source.spec.id.clone());
        let mut events = run.retained_events("adapter_call_started");
        events.extend(run.retained_events("adapter_call_result"));
        if let Some(done) = events
            .iter()
            .find(|e| e.kind == "adapter_call_result" && e.data["call_id"] == call_id)
        {
            return Ok(done.data["result"].clone());
        }
        if events
            .iter()
            .any(|e| e.kind == "adapter_call_started" && e.data["call_id"] == call_id)
        {
            return Ok(
                json!({"unknown":true,"detail":"This call was already started without a retained result. It is not dispatched again."}),
            );
        }
        run.append("adapter_call_started",json!({"call_id":call_id,"source_id":source.spec.id,"subscription_id":sub.spec.id,"tool":tool.name,"kind":tool.kind,"generation":source.generation,"arguments":args.get("arguments")}))?;
        let response = adapter(
            &source.spec,
            json!({"protocol":"boxen-adapter-v1","operation":"call","source_id":source.spec.id,"generation":source.generation,"call_id":format!("{}:{}",run.snapshot().record.spec.id,call_id),"tool":tool.name,"arguments":args["arguments"],"cursor":source.cursor}),
            || {
                run.cancellation_requested()
                    || !self
                        .subscription(owner, &sub.spec.id)
                        .is_ok_and(|s| Self::active(&s) && s.generation == sub.generation)
                    || !self
                        .source(owner, &source.spec.id)
                        .is_ok_and(|s| s.enabled && s.generation == source.generation)
            },
        );
        let result = match response {
            Ok(value) => {
                let publication = if value.get("observations").is_some() {
                    Some(
                        self.publish(
                            owner,
                            &source.spec.id,
                            source.generation,
                            None,
                            value.clone(),
                        )
                        .map(|v| json!({"accepted":true,"position":v}))
                        .unwrap_or_else(|e| json!({"accepted":false,"error":e.to_string()})),
                    )
                } else {
                    None
                };
                json!({"response":value,"publication":publication})
            }
            Err(error) => {
                json!({"unknown":true,"detail":"Adapter execution ended without a supported result; effects may already have occurred. The call is not retried.","reason":error.to_string()})
            }
        };
        run.diagnostic(if result["unknown"]==true{Level::Warn}else{Level::Info},"observation.adapter_completed",||json!({"source_id":source.spec.id,"subscription_id":sub.spec.id,"tool":tool.name,"call_id":call_id,"unknown":result["unknown"]}),||result.clone());
        run.append(
            "adapter_call_result",
            json!({"call_id":call_id,"result":result}),
        )?;
        Ok(result)
    }
}

/// One bounded invocation, no daemon-side interpreter or game implementation.
fn adapter(spec: &SourceSpec, request: Value, cancel: impl Fn() -> bool) -> Result<Value> {
    if cancel() {
        return Err("adapter invocation was cancelled before process launch".into());
    }
    let bytes = serde_json::to_vec(&request)?;
    if bytes.len() > MAX_INPUT {
        return Err("adapter request exceeds 32 KiB".into());
    }
    let executable = spec
        .command
        .first()
        .ok_or("source has no adapter command")?;
    let mut command = Command::new(executable);
    command
        .args(&spec.command[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    struct Process<'a>(std::process::Child, &'a str);
    impl Drop for Process<'_> {
        fn drop(&mut self) {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let status = self.0.wait();
            logging::record(
                Level::Debug,
                Target::Workload,
                "observation.process_stopped",
                Context::default(),
                || json!({"source_id":self.1,"pid":self.0.id(),"status":status.map(|s|s.to_string()).unwrap_or_default()}),
                || Value::Null,
            );
        }
    }
    let mut process = Process(child, &spec.id);
    logging::record(
        Level::Debug,
        Target::Workload,
        "observation.process_started",
        Context::default(),
        || json!({"source_id":spec.id,"pid":process.0.id(),"timeout_ms":spec.timeout_ms,"operation":request["operation"],"call_id":request.get("call_id")}),
        || Value::Null,
    );
    let mut stdin = process.0.stdin.take().ok_or("adapter stdin missing")?;
    let stdout = process.0.stdout.take().ok_or("adapter stdout missing")?;
    std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
        let _ = stdin.write_all(b"\n");
    });
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let result = stdout
            .take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut b)
            .map(|_| b);
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + Duration::from_millis(spec.timeout_ms);
    let mut output = None;
    loop {
        if cancel() {
            return Err("adapter stopped because its source or activity changed".into());
        }
        if Instant::now() >= deadline {
            return Err("adapter deadline expired".into());
        }
        if output.is_none() {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(bytes) => {
                    let bytes = bytes?;
                    if bytes.len() > MAX_SOURCE_BYTES {
                        return Err("adapter output exceeds 64 KiB".into());
                    }
                    output = Some(bytes);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("adapter output reader disconnected".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(status) = process.0.try_wait()? {
            if !status.success() {
                return Err(
                    "adapter exited unsuccessfully; stderr is not injected into model context"
                        .into(),
                );
            }
            if let Some(bytes) = output {
                let value: Value = serde_json::from_slice(&bytes)?;
                if value["protocol"] != "boxen-adapter-v1" {
                    return Err("adapter protocol version is unsupported".into());
                }
                if let Some(error) = value.get("error") {
                    let code = error["code"]
                        .as_str()
                        .filter(|s| {
                            !s.is_empty()
                                && s.len() <= 64
                                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        })
                        .unwrap_or("unspecified");
                    logging::record(
                        Level::Warn,
                        Target::Workload,
                        "observation.adapter_error",
                        Context::default(),
                        || json!({"source_id":spec.id,"code":code}),
                        || error.clone(),
                    );
                    return Err(format!("adapter reported {code}").into());
                }
                return Ok(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(mode: &str) -> (PathBuf, Events, String, Source, Subscription) {
        let root =
            std::env::temp_dir().join(format!("boxen-events-{}", state::random_hex().unwrap()));
        let e = Events::open(root.clone()).unwrap();
        let owner = state::random_hex().unwrap();
        let source = e
            .create_source(
                &owner,
                SourceSpec {
                    audience: None,
                    id: state::random_hex().unwrap(),
                    name: "bounded test".into(),
                    command: vec![],
                    cwd: root.to_string_lossy().into(),
                    interval_ms: 1000,
                    mode: mode.into(),
                    timeout_ms: 1000,
                    tools: vec![],
                },
            )
            .unwrap();
        let sub = e
            .subscribe(
                &owner,
                SubscriptionSpec {
                    id: state::random_hex().unwrap(),
                    source: source.spec.id.clone(),
                    target: Handler::Resident {
                        id: state::random_hex().unwrap(),
                    },
                    interval_ms: 1000,
                    waking: false,
                    health_waking: false,
                    lifetime: "activity".into(),
                    expires_at_ms: None,
                    wake_limit: 60,
                    target_wake_limit: 120,
                },
            )
            .unwrap();
        (root, e, owner, source, sub)
    }
    fn push(e: &Events, owner: &str, source: &Source, n: u64) {
        e.publish(owner,&source.spec.id,source.generation,Some(n),json!({"protocol":"boxen-adapter-v1","observations":[{"id":n.to_string(),"observed_at_ms":n,"payload":{"value":n}}]})).unwrap();
    }
    #[test]
    fn independent_positions_cadence_and_latest_gap_are_explicit() {
        let (root, e, owner, source, sub) = fixture("ordered");
        push(&e, &owner, &source, 1);
        e.ready(&owner, &sub.spec.id).unwrap();
        push(&e, &owner, &source, 2);
        let batches = e.batches(&owner, sub.spec.target.id());
        assert_eq!(batches[0]["through_seq"], 1);
        assert_eq!(batches[0]["observations"].as_array().unwrap().len(), 1);
        let mut other = sub.spec.clone();
        other.id = state::random_hex().unwrap();
        other.target = Handler::Resident {
            id: state::random_hex().unwrap(),
        };
        let other = e.subscribe(&owner, other).unwrap();
        e.ready(&owner, &other.spec.id).unwrap();
        e.acknowledge(&owner, &batches, true).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let inode = fs::metadata(&e.path).unwrap().ino();
            e.acknowledge(&owner, &batches, true).unwrap();
            assert_eq!(fs::metadata(&e.path).unwrap().ino(), inode);
        }
        assert!(e.batches(&owner, sub.spec.target.id()).is_empty());
        assert_eq!(
            e.batches(&owner, other.spec.target.id())[0]["through_seq"],
            2
        );
        assert!(
            e.subscription(&state::random_hex().unwrap(), &sub.spec.id)
                .is_err()
        );
        drop(e);
        fs::remove_dir_all(root).unwrap();
        let (root, e, owner, source, sub) = fixture("latest");
        push(&e, &owner, &source, 1);
        push(&e, &owner, &source, 2);
        e.ready(&owner, &sub.spec.id).unwrap();
        let b = e.batches(&owner, sub.spec.target.id());
        assert_eq!(b[0]["gap"]["through_seq"], 1);
        assert_eq!(b[0]["observations"].as_array().unwrap().len(), 1);
        drop(e);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn health_repeats_flaps_and_recovery_stay_bounded() {
        let (root, e, owner, mut source, sub) = fixture("ordered");
        for _ in 0..50 {
            Events::health(&mut source, Some("broken"));
            Events::health(&mut source, None);
        }
        assert_eq!(source.health.failures, 50);
        assert_eq!(source.health.notice_seq, 1);
        assert_eq!(source.health.transitions, 100);
        assert!(source.items.is_empty());
        assert!(source.health.pending_notice);
        assert!(size(&source.health) < 1024);
        source.health.notice_at_ms = jobs::now() - 60001;
        Events::promote_notice(&mut source.health);
        assert_eq!(source.health.notice_seq, 2);
        assert_eq!(
            source.health.notice.as_ref().unwrap()["condition"],
            "healthy"
        );
        e.update(
            &owner,
            "subscription",
            &sub.spec.id,
            &json!({"expected_revision":1,"end":true}),
        )
        .unwrap();
        drop(e);
        let e = Events::open(root.clone()).unwrap();
        assert!(e.subscription(&owner, &sub.spec.id).unwrap().ended);
        assert!(
            e.update(
                &owner,
                "subscription",
                &sub.spec.id,
                &json!({"expected_revision":2,"enabled":true})
            )
            .is_err()
        );
        drop(e);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn storage_failure_cannot_repeat_automatic_activation() {
        let (root, e, owner, source, sub) = fixture("ordered");
        push(&e, &owner, &source, 1);
        e.ready(&owner, &sub.spec.id).unwrap();
        let batches = e.batches(&owner, sub.spec.target.id());
        fs::remove_file(&e.path).unwrap();
        fs::create_dir(&e.path).unwrap();
        assert!(e.acknowledge(&owner, &batches, true).is_err());
        assert!(e.pending_targets().is_empty());
        assert!(e.batches(&owner, sub.spec.target.id()).is_empty());
        assert!(!e.wake_allowed(&owner, sub.spec.target.id()));
        assert!(e.list(&owner)["unavailable"].is_string());
        drop(e);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn empty_subscriptions_and_replacement_cannot_bypass_wake_limits() {
        let (root, e, owner, source, sub) = fixture("ordered");
        {
            let mut g = e.inner.lock().unwrap();
            let s = g.data.subscriptions.get_mut(&sub.spec.id).unwrap();
            s.spec.waking = true;
            s.spec.wake_limit = 1;
            s.spec.target_wake_limit = 1;
        }
        push(&e, &owner, &source, 1);
        e.ready(&owner, &sub.spec.id).unwrap();
        assert!(e.wake_allowed(&owner, sub.spec.target.id()));
        let b = e.batches(&owner, sub.spec.target.id());
        e.acknowledge(&owner, &b, true).unwrap();
        push(&e, &owner, &source, 2);
        e.ready(&owner, &sub.spec.id).unwrap();
        assert!(!e.wake_allowed(&owner, sub.spec.target.id()));
        e.update(
            &owner,
            "subscription",
            &sub.spec.id,
            &json!({"expected_revision":1,"end":true}),
        )
        .unwrap();
        let mut spec = sub.spec.clone();
        spec.id = state::random_hex().unwrap();
        spec.waking = true;
        spec.target_wake_limit = 1;
        let next = e.subscribe(&owner, spec).unwrap();
        e.ready(&owner, &next.spec.id).unwrap();
        assert!(!e.wake_allowed(&owner, next.spec.target.id()));
        drop(e);
        fs::remove_dir_all(root).unwrap();
    }
}
