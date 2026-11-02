//! Connector instances own external accounts and grants; the existing message journal
//! owns intake, outgoing acceptance, dispatch uncertainty and service receipts.
use crate::{
    Result,
    jobs::{self, Jobs},
    logging::{self, Context, Level, Target},
    mailbox::{Address, Message, Send},
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs, path::PathBuf, sync::Mutex};

pub fn opaque(s: &str, max: usize) -> Result<()> {
    if s.is_empty() || s.len() > max || s.chars().any(char::is_control) {
        Err("identifier is nonempty, bounded and contains no control characters".into())
    } else {
        Ok(())
    }
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub conversation: String,
    pub resident: String,
    #[serde(default = "yes")]
    pub receive: bool,
    #[serde(default = "yes")]
    pub send: bool,
    #[serde(default)]
    pub accept_bots: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub account: String,
    #[serde(default)]
    pub credential_file: Option<PathBuf>,
    #[serde(default)]
    pub settings: Value,
    pub capabilities: Vec<String>,
    pub bindings: Vec<Binding>,
    #[serde(default = "yes")]
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    pub owner: String,
    pub revision: u64,
    pub definition: Definition,
}
pub struct Connectors {
    directory: PathBuf,
    inner: Mutex<HashMap<String, Instance>>,
    pub intake: Mutex<()>,
    issues: Vec<String>,
    health: Mutex<HashMap<String, Value>>,
    unavailable: bool,
}
impl Connectors {
    pub fn open(directory: PathBuf) -> Result<Self> {
        match Self::load(directory.clone()) {
            Ok(value) => Ok(value),
            Err(error) => {
                let issue = format!(
                    "Connector registry unavailable: {error}. Original state is preserved; ordinary workloads and resident mail remain available. Repair and restart reload connector definitions."
                );
                logging::record(
                    Level::Error,
                    Target::State,
                    "messaging.registry_unavailable",
                    Context::default(),
                    || json!({"error":issue}),
                    || Value::Null,
                );
                Ok(Self {
                    directory,
                    inner: Mutex::new(HashMap::new()),
                    intake: Mutex::new(()),
                    issues: vec![issue],
                    health: Mutex::new(HashMap::new()),
                    unavailable: true,
                })
            }
        }
    }
    fn load(directory: PathBuf) -> Result<Self> {
        state::private_dir(&directory)?;
        let mut instances = HashMap::new();
        let mut issues = vec![];
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let result = (|| -> Result<Instance> {
                if fs::metadata(&path)?.len() > 32 * 1024 {
                    return Err("connector definition exceeds 32 KiB".into());
                }
                let bytes = fs::read(&path)?;
                if bytes.len() > 32 * 1024 {
                    return Err("connector definition exceeds 32 KiB".into());
                }
                let i: Instance = serde_json::from_slice(&bytes)?;
                state::key(&i.owner)?;
                validate(&i.definition)?;
                if path.file_stem().and_then(|s| s.to_str()) != Some(&i.definition.id) {
                    return Err("connector filename and id differ".into());
                }
                Ok(i)
            })();
            match result {
                Ok(i) => {
                    instances.insert(i.definition.id.clone(), i);
                }
                Err(_) => {
                    issues.push(format!(
                        "Connector definition {} is unavailable; its file is preserved",
                        path.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
        logging::record(
            Level::Info,
            Target::State,
            "messaging.connectors_opened",
            Context::default(),
            || json!({"instances":instances.len(),"unavailable":issues.len()}),
            || Value::Null,
        );
        Ok(Self {
            directory,
            inner: Mutex::new(instances),
            intake: Mutex::new(()),
            issues,
            health: Mutex::new(HashMap::new()),
            unavailable: false,
        })
    }
    fn health(&self, id: &str) -> Value {
        let mut value = self
            .health
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_else(|| json!({"state":"unknown","last_seen":null}));
        value["stale"] = json!(
            value["last_seen"]
                .as_u64()
                .is_none_or(|seen| jobs::now().saturating_sub(seen) > 90000)
        );
        value
    }
    fn report(&self, owner: &str, id: &str, status: &str, channels: &Value) -> Result<Value> {
        let instance = self.get(owner, id)?;
        if !matches!(status, "ready" | "degraded" | "unavailable" | "disabled") {
            return Err(
                "connector health state is ready, degraded, unavailable or disabled".into(),
            );
        }
        let list = channels
            .as_array()
            .ok_or("health channels are an array of configured conversation ids")?;
        if list.len() > 32
            || list.iter().any(|c| {
                !instance
                    .definition
                    .bindings
                    .iter()
                    .any(|b| json!(b.conversation) == *c)
            })
        {
            return Err("health channels address only configured conversations".into());
        }
        let value =
            json!({"state":status,"last_seen":jobs::now(),"unavailable_conversations":channels});
        let mut health = self.health.lock().unwrap();
        let changed = health
            .get(id)
            .is_none_or(|p| p["state"] != status || p["unavailable_conversations"] != *channels);
        health.insert(id.into(), value.clone());
        drop(health);
        if changed {
            event(
                owner,
                id,
                "messaging.health_changed",
                || json!({"state":status,"unavailable_conversations":channels}),
            );
        }
        Ok(value)
    }
    pub fn list(&self, owner: &str, resident: Option<&str>) -> Vec<Instance> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|i| {
                i.owner == owner
                    && resident
                        .is_none_or(|r| i.definition.bindings.iter().any(|b| b.resident == r))
            })
            .cloned()
            .collect()
    }
    pub fn get(&self, owner: &str, id: &str) -> Result<Instance> {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .filter(|i| i.owner == owner)
            .cloned()
            .ok_or_else(|| "connector instance unavailable in authenticated scope".into())
    }
    pub fn create(&self, owner: &str, definition: Definition) -> Result<Instance> {
        if self.unavailable {
            return Err(
                "connector registry is unavailable; existing state is preserved for repair/reload"
                    .into(),
            );
        }
        validate(&definition)?;
        state::key(owner)?;
        let mut inner = self.inner.lock().unwrap();
        if let Some(prior) = inner.get(&definition.id) {
            return if prior.owner == owner && prior.definition == definition {
                Ok(prior.clone())
            } else {
                Err(
                    "connector instance id already exists with different configuration or owner"
                        .into(),
                )
            };
        }
        if inner.len() >= 16 {
            return Err("this host retains at most 16 external connector instances".into());
        }
        let instance = Instance {
            owner: owner.into(),
            revision: 1,
            definition,
        };
        state::publish_new(
            &self
                .directory
                .join(format!("{}.json", instance.definition.id)),
            &serde_json::to_vec(&instance)?,
        )?;
        inner.insert(instance.definition.id.clone(), instance.clone());
        drop(inner);
        event(
            owner,
            &instance.definition.id,
            "messaging.instance_created",
            || json!({"provider":instance.definition.provider,"bindings":instance.definition.bindings.len()}),
        );
        Ok(instance)
    }
    pub fn enabled(&self, owner: &str, id: &str, revision: u64, enabled: bool) -> Result<Instance> {
        let _transition = self.intake.lock().unwrap();
        let mut inner = self.inner.lock().unwrap();
        let prior = inner
            .get(id)
            .filter(|i| i.owner == owner)
            .ok_or("connector instance unavailable in authenticated scope")?;
        if prior.revision != revision {
            return Err(
                "connector revision changed; the current revision is available through get".into(),
            );
        }
        let mut next = prior.clone();
        next.revision += 1;
        next.definition.enabled = enabled;
        let temporary = self
            .directory
            .join(format!("{id}.pending-{}", state::random_hex()?));
        state::write_new(&temporary, &serde_json::to_vec(&next)?)?;
        fs::rename(&temporary, self.directory.join(format!("{id}.json")))?;
        inner.insert(id.into(), next.clone());
        drop(inner);
        event(
            owner,
            id,
            "messaging.instance_updated",
            || json!({"revision":next.revision,"enabled":enabled}),
        );
        Ok(next)
    }
}
fn validate(d: &Definition) -> Result<()> {
    jobs::valid_id(&d.id)?;
    opaque(&d.name, 100)?;
    opaque(&d.provider, 64)?;
    opaque(&d.account, 256)?;
    if d.credential_file.as_ref().is_some_and(|p| !p.is_absolute()) {
        return Err(
            "connector credentials use an absolute file reference, not a token value".into(),
        );
    }
    if serde_json::to_vec(&d.settings)?.len() > 8192 {
        return Err("connector public settings exceed 8 KiB".into());
    }
    if d.capabilities.len() > 8
        || d.capabilities.iter().any(|c| {
            !matches!(
                c.as_str(),
                "send" | "receive" | "replies" | "subjects" | "native_metadata"
            )
        })
    {
        return Err(
            "connector capabilities are send, receive, replies, subjects or native_metadata".into(),
        );
    }
    if d.bindings.is_empty() || d.bindings.len() > 32 {
        return Err("connector instances have 1–32 explicit conversation/resident bindings".into());
    }
    let mut unique = std::collections::HashSet::new();
    for b in &d.bindings {
        if !unique.insert((&b.conversation, &b.resident)) {
            return Err("connector conversation/resident bindings are unique".into());
        }
        if d.bindings
            .iter()
            .filter(|x| x.conversation == b.conversation && x.receive)
            .count()
            > 8
        {
            return Err("a connector conversation has at most eight receive bindings".into());
        }
        opaque(&b.conversation, 256)?;
        jobs::valid_id(&b.resident)?;
    }
    Ok(())
}
fn event(owner: &str, instance: &str, name: &'static str, fields: impl FnOnce() -> Value) {
    logging::record(
        Level::Info,
        Target::State,
        name,
        Context::peer(owner),
        || json!({"instance_id":instance,"details":fields()}),
        || Value::Null,
    );
}
fn address(instance: &str, conversation: &str) -> Result<Address> {
    if instance == "boxen" {
        if let Some(id) = conversation.strip_prefix("resident:") {
            jobs::valid_id(id)?;
            return Ok(Address::Resident { id: id.into() });
        }
        if let Some(client) = conversation.strip_prefix("client:")
            && let Some((machine, id)) = client.split_once(':')
        {
            let a = Address::Client {
                machine: machine.into(),
                id: id.into(),
            };
            a.validate()?;
            return Ok(a);
        }
        return Err("Boxen conversations are resident:ID or client:MACHINE:ID".into());
    }
    let a = Address::Connector {
        instance: instance.into(),
        conversation: conversation.into(),
    };
    a.validate()?;
    Ok(a)
}
fn project_address(a: &Address) -> Value {
    match a {
        Address::Resident { id } => {
            json!({"instance":"boxen","conversation":format!("resident:{id}")})
        }
        Address::Client { machine, id } => {
            json!({"instance":"boxen","conversation":format!("client:{machine}:{id}")})
        }
        Address::Connector {
            instance,
            conversation,
        } => json!({"instance":instance,"conversation":conversation}),
    }
}
fn project(jobs: &Jobs, m: &Message) -> Value {
    json!({"id":m.id,"from":project_address(&m.sender),"to":project_address(&m.to),"subject":if m.subject.is_empty(){Value::Null}else{json!(m.subject)},"body":m.body,"reply_to":project_address(&m.reply_to),"in_reply_to":m.in_reply_to,"origin":m.origin,"accepted_at":m.accepted_at,"seq":m.seq,"delivery":jobs.mailboxes.receipt(&m.id).unwrap_or_else(||json!({"status":if matches!(m.to,Address::Connector{..}){"queued"}else{"accepted"},"accepted_on":m.accepted_on}))})
}
pub fn authorize(
    jobs: &Jobs,
    owner: &str,
    resident: &str,
    instance: &str,
    conversation: &str,
    send: bool,
) -> Result<Instance> {
    let i = jobs.connectors.get(owner, instance)?;
    if !i
        .definition
        .capabilities
        .iter()
        .any(|c| c == if send { "send" } else { "receive" })
    {
        return Err("connector instance does not declare this operation".into());
    }
    if !i.definition.enabled {
        return Err("connector instance is disabled; pending correspondence is retained".into());
    }
    if !i.definition.bindings.iter().any(|b| {
        b.resident == resident
            && b.conversation == conversation
            && if send { b.send } else { b.receive }
    }) {
        return Err(
            "connector conversation has no matching resident binding for this operation".into(),
        );
    }
    Ok(i)
}
pub fn operation(
    jobs: &Jobs,
    owner: &str,
    resident: Option<&str>,
    args: Value,
    source: String,
) -> Result<Value> {
    let action = args["action"].as_str().unwrap_or("list");
    let id = args["instance"].as_str().unwrap_or("");
    let result = (|| -> Result<Value> {
        match action {
            "list" => {
                let mut instances = vec![
                    json!({"id":"boxen","provider":"boxen","capabilities":["receive","send","replies","subjects"],"account":owner}),
                ];
                for i in jobs.connectors.list(owner, resident) {
                    instances.push(json!({"id":i.definition.id,"name":i.definition.name,"provider":i.definition.provider,"account":i.definition.account,"enabled":i.definition.enabled,"health":jobs.connectors.health(&i.definition.id),"bindings":i.definition.bindings.iter().filter(|b|resident.is_none_or(|r|b.resident==r)).collect::<Vec<_>>(),"capabilities":i.definition.capabilities}));
                }
                Ok(
                    json!({"instances":instances,"issues":if resident.is_none(){json!(jobs.connectors.issues)}else{json!([])}}),
                )
            }
            "inbox" => {
                let a = if let Some(r) = resident {
                    Address::Resident { id: r.into() }
                } else {
                    address(
                        "boxen",
                        args["conversation"]
                            .as_str()
                            .ok_or("conversation required")?,
                    )?
                };
                let page = jobs.inbox(owner, a, args["after"].as_u64().unwrap_or(0))?;
                let messages = page["messages"]
                    .as_array()
                    .ok_or("invalid inbox projection")?
                    .iter()
                    .map(|v| {
                        let m: Message = serde_json::from_value(v.clone()).unwrap();
                        let mut p = project(jobs, &m);
                        p["presented_in"] = v["presented_in"].clone();
                        p["presentation"] = v["delivery"].clone();
                        p
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"messages":messages,"next_after":page["next_after"]}))
            }
            "send" => {
                if resident.is_none() {
                    jobs::valid_id(
                        args["id"].as_str().ok_or(
                            "client sends retain an explicit message id before submission",
                        )?,
                    )?;
                }
                let from = if let Some(r) = resident {
                    Address::Resident { id: r.into() }
                } else {
                    let client = args["client_id"].as_str().ok_or("client_id required")?;
                    jobs::valid_id(client)?;
                    Address::Client {
                        machine: owner.into(),
                        id: client.into(),
                    }
                };
                let to = &args["to"];
                let target = address(
                    to["instance"]
                        .as_str()
                        .ok_or("destination instance required")?,
                    to["conversation"]
                        .as_str()
                        .ok_or("destination conversation required")?,
                )?;
                let message_id = args["id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| jobs.mailboxes.source_id(&source))
                    .unwrap_or(state::random_hex()?);
                let spec = Send {
                    id: message_id,
                    to: target,
                    subject: args["subject"].as_str().unwrap_or("").into(),
                    body: args["body"].as_str().ok_or("message body required")?.into(),
                    reply_to: None,
                    in_reply_to: args["in_reply_to"].as_str().map(str::to_owned),
                };
                Ok(project(jobs, &jobs.send_mail(owner, from, spec, source)?))
            }
            "status" => {
                let m = jobs
                    .mailboxes
                    .get(owner, args["id"].as_str().ok_or("message id required")?)?;
                if let Some(r) = resident
                    && m.sender != (Address::Resident { id: r.into() })
                    && m.to != (Address::Resident { id: r.into() })
                {
                    return Err("message status belongs to another participant".into());
                }
                Ok(project(jobs, &m))
            }
            _ => {
                if resident.is_some() {
                    return Err("connector configuration and native intake belong to the authenticated operator or connector runner".into());
                }
                match action {
                    "create" => {
                        let definition: Definition =
                            serde_json::from_value(args["definition"].clone())?;
                        for b in &definition.bindings {
                            jobs.residents.get(owner, &b.resident)?;
                        }
                        Ok(serde_json::to_value(
                            jobs.connectors.create(owner, definition)?,
                        )?)
                    }
                    "health" => jobs.connectors.report(
                        owner,
                        id,
                        args["state"].as_str().ok_or("health state required")?,
                        args.get("channels").unwrap_or(&json!([])),
                    ),
                    "get" => Ok(serde_json::to_value(jobs.connectors.get(owner, id)?)?),
                    "update" => Ok(serde_json::to_value(
                        jobs.connectors.enabled(
                            owner,
                            id,
                            args["expected_revision"]
                                .as_u64()
                                .ok_or("expected_revision required")?,
                            args["enabled"].as_bool().ok_or("enabled required")?,
                        )?,
                    )?),
                    "cursor" => {
                        let conversation = args["conversation"]
                            .as_str()
                            .ok_or("conversation required")?;
                        let i = jobs.connectors.get(owner, id)?;
                        if !i
                            .definition
                            .bindings
                            .iter()
                            .any(|b| b.conversation == conversation && b.receive)
                        {
                            return Err(
                                "conversation is outside this connector's receive bindings".into(),
                            );
                        }
                        Ok(
                            json!({"checkpoint":jobs.mailboxes.cursor(&format!("{owner}:{id}:{conversation}"))}),
                        )
                    }
                    "ingest" => ingest(jobs, owner, id, &args),
                    "outbox" => {
                        let i = jobs.connectors.get(owner, id)?;
                        Ok(
                            json!({"enabled":i.definition.enabled,"messages":jobs.mailboxes.external_messages(owner,id).iter().map(|m|project(jobs,m)).collect::<Vec<_>>()}),
                        )
                    }
                    "claim" => {
                        let _transition = jobs.connectors.intake.lock().unwrap();
                        let mid = args["id"].as_str().ok_or("message id required")?;
                        let m = jobs.mailboxes.get(owner, mid)?;
                        let Address::Connector {
                            instance,
                            conversation,
                        } = &m.to
                        else {
                            return Err("dispatch claims address external messages".into());
                        };
                        if instance != id {
                            return Err("dispatch claim addresses another instance".into());
                        }
                        let Address::Resident { id: r } = &m.sender else {
                            return Err("external sends have a bound resident sender".into());
                        };
                        authorize(jobs, owner, r, id, conversation, true)?;
                        let native_reply = if let Some(reply) = &m.in_reply_to {
                            if !jobs
                                .connectors
                                .get(owner, id)?
                                .definition
                                .capabilities
                                .iter()
                                .any(|c| c == "replies")
                            {
                                return Err("connector instance does not declare replies".into());
                            }
                            let original = jobs.mailboxes.get(owner, reply)?;
                            let origin = original.origin.ok_or(
                                "external reply references a message with native provenance",
                            )?;
                            if origin["instance"] != id || origin["conversation"] != *conversation {
                                return Err(
                                    "reply provenance belongs to another connector conversation"
                                        .into(),
                                );
                            }
                            origin["native_id"].clone()
                        } else {
                            Value::Null
                        };
                        let mut result = jobs.mailboxes.claim(
                            owner,
                            id,
                            mid,
                            args["claim_id"].as_str().ok_or("claim_id required")?,
                        )?;
                        result["native_reply"] = native_reply;
                        Ok(result)
                    }
                    "complete" => {
                        jobs.connectors.get(owner, id)?;
                        jobs.mailboxes.complete(
                            owner,
                            id,
                            args["id"].as_str().ok_or("message id required")?,
                            args["claim_id"].as_str().ok_or("claim_id required")?,
                            crate::mailbox::DeliveryOutcome {
                                status: args["status"].as_str().ok_or("status required")?,
                                external_id: args["external_id"].as_str(),
                                reason: args["reason"].as_str(),
                            },
                        )
                    }
                    _ => Err("unknown messaging action".into()),
                }
            }
        }
    })();
    logging::record(
        if result.is_ok() {
            Level::Debug
        } else {
            Level::Warn
        },
        Target::State,
        "messaging.operation",
        Context::peer(owner),
        || json!({"action":action,"instance_id":id,"resident_id":resident,"message_id":args["id"],"conversation":args["conversation"],"claimed":result.as_ref().ok().and_then(|v|v.get("claimed")),"delivery":result.as_ref().ok().and_then(|v|v.get("status")),"accepted":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string())}),
        || Value::Null,
    );
    result
}
/// Bounds a prepared native envelope without substituting authored content.
/// Unretained portions carry byte counts and hashes; the availability record is
/// distinct from the sender's words and does not claim those bytes remain local.
pub fn bounded_native(value: &Value) -> Value {
    let mut message = value.clone();
    let body = message["body"].as_str().unwrap_or("");
    if body.len() > 32 * 1024 {
        let notice = json!({"bytes":body.len(),"sha256_utf8":state::hex(&Sha256::digest(body.as_bytes())),"retained":false});
        message["body"] = json!("");
        message["availability"]["body"] = notice;
    }
    let mut metadata = message.clone();
    metadata.as_object_mut().map(|m| m.remove("body"));
    let bytes = serde_json::to_vec(&metadata).unwrap_or_default();
    if bytes.len() > 24 * 1024 {
        message = json!({"id":message["id"],"body":message["body"],"author":{"id":message["author"]["id"],"bot":message["author"]["bot"]},"availability":{"body":message["availability"]["body"],"metadata":{"bytes":bytes.len(),"sha256_prepared_json":state::hex(&Sha256::digest(&bytes)),"retained":false}},"timestamp":message["timestamp"]});
    }
    message
}

fn ingest(jobs: &Jobs, owner: &str, id: &str, args: &Value) -> Result<Value> {
    let _transition = jobs.connectors.intake.lock().unwrap();
    let i = jobs.connectors.get(owner, id)?;
    if !i.definition.enabled {
        return Err("connector instance is disabled".into());
    }
    if !i.definition.capabilities.iter().any(|c| c == "receive") {
        return Err("connector instance does not declare receive".into());
    }
    let conversation = args["conversation"]
        .as_str()
        .ok_or("conversation required")?;
    opaque(conversation, 256)?;
    let bindings: Vec<_> = i
        .definition
        .bindings
        .iter()
        .filter(|b| b.conversation == conversation && b.receive)
        .collect();
    if bindings.is_empty() {
        return Err("conversation is outside this connector's receive bindings".into());
    }
    let messages = args["messages"]
        .as_array()
        .ok_or("messages array required")?;
    if messages.len() > 32 || serde_json::to_vec(args)?.len() > 192 * 1024 {
        return Err("intake batches contain at most 32 messages and 192 KiB".into());
    }
    let cursor = args["cursor"].as_str().ok_or("cursor required")?;
    opaque(cursor, 256)?;
    let key = format!("{owner}:{id}:{conversation}");
    let digest = state::hex(&Sha256::digest(serde_json::to_vec(args)?));
    let prior = jobs.mailboxes.cursor(&key);
    if prior.as_ref().is_some_and(|p| p["cursor"] == cursor) {
        return if prior.as_ref().unwrap()["digest"] == digest {
            Ok(json!({"accepted":true,"replayed":true,"cursor":cursor}))
        } else {
            Err("checkpoint cursor was accepted with another batch".into())
        };
    }
    if prior
        .as_ref()
        .map(|p| p["cursor"].clone())
        .unwrap_or(Value::Null)
        != args["after"]
    {
        return Err(
            "connector checkpoint changed; current cursor is available through cursor".into(),
        );
    }
    let mut accepted = 0;
    for original in messages {
        if !original.is_object() {
            return Err("native messages are objects".into());
        }
        let native = &bounded_native(original);
        let native_id = native["id"].as_str().ok_or("native message id required")?;
        opaque(native_id, 256)?;
        let author = native["author"]
            .as_object()
            .ok_or("native author object required")?;
        opaque(
            author
                .get("id")
                .and_then(Value::as_str)
                .ok_or("native author id required")?,
            256,
        )?;
        let body = native["body"].as_str().ok_or("native body required")?;
        let mut origin = native.clone();
        origin["instance"] = json!(id);
        origin["provider"] = json!(i.definition.provider);
        origin["account"] = json!(i.definition.account);
        origin["conversation"] = json!(conversation);
        origin["native_id"] = json!(native_id);
        origin.as_object_mut().unwrap().remove("body");
        if serde_json::to_vec(&origin)?.len() > 32 * 1024 {
            return Err("native message metadata exceeds 32 KiB".into());
        }
        for b in &bindings {
            if native["author"]["bot"] == true && !b.accept_bots {
                continue;
            }
            jobs.residents.get(owner, &b.resident)?;
            let mid = state::hex(&Sha256::digest(serde_json::to_vec(&json!([
                "connector-message-v1",
                owner,
                id,
                conversation,
                native_id,
                b.resident
            ]))?));
            let source = format!("connector:{mid}");
            jobs.mailboxes.accept(
                owner,
                Address::Connector {
                    instance: id.into(),
                    conversation: conversation.into(),
                },
                Send {
                    id: mid,
                    to: Address::Resident {
                        id: b.resident.clone(),
                    },
                    subject: native["subject"].as_str().unwrap_or("").into(),
                    body: body.into(),
                    reply_to: Some(Address::Connector {
                        instance: id.into(),
                        conversation: conversation.into(),
                    }),
                    in_reply_to: None,
                },
                source,
                Some(origin.clone()),
            )?;
            accepted += 1;
        }
    }
    jobs.mailboxes.checkpoint(&key, cursor, &digest)?;
    jobs.messaging_notify();
    event(
        owner,
        id,
        "messaging.intake_accepted",
        || json!({"conversation":conversation,"native_messages":messages.len(),"deliveries":accepted,"cursor":cursor}),
    );
    Ok(json!({"accepted":true,"cursor":cursor,"deliveries":accepted}))
}
