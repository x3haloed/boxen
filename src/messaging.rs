//! Connector instances own external accounts and grants; the existing message journal
//! owns intake, outgoing acceptance, dispatch uncertainty and service receipts.
use crate::{
    Result,
    attachments::{Attachment, MESSAGE_LIMIT},
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<crate::message_service::Tool>,
    pub bindings: Vec<Binding>,
    #[serde(default = "yes")]
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    pub owner: String,
    pub revision: u64,
    pub definition: Definition,
    #[serde(default)]
    pub policy: Option<crate::message_policy::Policy>,
    #[serde(default)]
    pub gateway: bool,
    #[serde(default)]
    pub attention: HashMap<String, crate::message_policy::Attention>,
}
impl Instance {
    pub fn policy(&self) -> crate::message_policy::Policy {
        self.policy
            .clone()
            .unwrap_or_else(|| crate::message_policy::Policy {
                grants: self
                    .definition
                    .bindings
                    .iter()
                    .map(|b| crate::message_policy::Grant {
                        resident: b.resident.clone(),
                        scope: crate::message_policy::Scope {
                            kind: "conversation".into(),
                            id: Some(b.conversation.clone()),
                        },
                        actions: [
                            ("receive", b.receive),
                            ("read", b.receive),
                            ("send", b.send),
                        ]
                        .into_iter()
                        .filter(|(_, enabled)| *enabled)
                        .map(|(a, _)| a.into())
                        .collect(),
                        effect: "allow".into(),
                        accept_bots: b.accept_bots,
                        except_users: Vec::new(),
                    })
                    .collect(),
            })
    }
    pub fn residents(&self) -> Vec<String> {
        let mut residents: Vec<_> = self
            .definition
            .bindings
            .iter()
            .map(|b| b.resident.clone())
            .chain(self.policy().grants.into_iter().map(|g| g.resident))
            .collect();
        residents.sort();
        residents.dedup();
        residents
    }
    pub fn attention(&self, resident: &str) -> crate::message_policy::Attention {
        self.attention
            .get(resident)
            .cloned()
            .unwrap_or_else(|| crate::message_policy::Attention {
                watched: self
                    .definition
                    .bindings
                    .iter()
                    .filter(|b| b.receive && b.resident == resident)
                    .map(|b| crate::message_policy::Scope {
                        kind: "conversation".into(),
                        id: Some(b.conversation.clone()),
                    })
                    .collect(),
                ..Default::default()
            })
    }
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
    pub fn owners(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|r| r.owner.clone())
            .collect()
    }

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
                if let Some(p) = &i.policy {
                    p.validate()?;
                }
                for (id, a) in &i.attention {
                    jobs::valid_id(id)?;
                    a.validate()?;
                }
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
        if list.len() > 64
            || list.iter().any(|c| {
                c.as_str().is_none_or(|id| {
                    opaque(id, 256).is_err()
                        || !(instance.gateway && matches!(id, "_gateway" | "_gateway_intake")
                            || instance
                                .residents()
                                .iter()
                                .any(|r| instance.policy().potential(r, "receive", id)))
                })
            })
        {
            return Err(
                "health channels address bounded permitted conversations or Gateway health".into(),
            );
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
                i.owner == owner && resident.is_none_or(|r| i.residents().iter().any(|id| id == r))
            })
            .cloned()
            .collect()
    }
    pub(crate) fn local_runner_instance(&self, id: &str) -> Result<Instance> {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| "connector instance unavailable".into())
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
            policy: None,
            gateway: false,
            attention: HashMap::new(),
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
        self.configure(owner, id, revision, Some(enabled), None, None, None)
    }
    #[allow(clippy::too_many_arguments)] // One revision-checked atomic configuration transition.
    fn configure(
        &self,
        owner: &str,
        id: &str,
        revision: u64,
        enabled: Option<bool>,
        policy: Option<crate::message_policy::Policy>,
        attention: Option<(String, crate::message_policy::Attention)>,
        gateway: Option<bool>,
    ) -> Result<Instance> {
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
        if let Some(gateway) = gateway {
            next.gateway = gateway;
        }
        if let Some(enabled) = enabled {
            next.definition.enabled = enabled;
        }
        if let Some(policy) = policy {
            next.policy = Some(policy);
        }
        if let Some((resident, attention)) = attention {
            if next.attention.len() >= 64 && !next.attention.contains_key(&resident) {
                return Err("instance attention contains at most 64 residents".into());
            }
            next.attention.insert(resident, attention);
        }
        if controls_presence(&next)
            && inner.values().any(|i| {
                i.definition.id != id
                    && i.definition.account == next.definition.account
                    && i.definition.provider == next.definition.provider
                    && controls_presence(i)
            })
        {
            return Err(
                "another enabled instance on this host controls this account's presence".into(),
            );
        }
        let temporary = self
            .directory
            .join(format!("{id}.pending-{}", state::random_hex()?));
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > 32 * 1024 {
            return Err("connector configuration exceeds 32 KiB".into());
        }
        state::write_new(&temporary, &bytes)?;
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
    if d.provider == crate::guest_chat::PROVIDER {
        crate::guest_chat::binding(d)?;
    }
    if d.tools.len() > 16 {
        return Err("instances declare at most 16 service operations".into());
    }
    for tool in &d.tools {
        opaque(&tool.name, 64)?;
        opaque(&tool.description, 4096)?;
        if !matches!(tool.kind.as_str(), "query" | "action")
            || !matches!(
                tool.permission.as_str(),
                "read" | "send" | "react" | "files" | "presence"
            )
        {
            return Err(
                "declared operations have query/action kind and an explicit permission".into(),
            );
        }
    }
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
    json!({"id":m.id,"network":m.network,"from":project_address(&m.sender),"to":project_address(&m.to),"subject":if m.subject.is_empty(){Value::Null}else{json!(m.subject)},"body":m.body,"attachments":m.attachments,"service":m.service,"reply_to":project_address(&m.reply_to),"in_reply_to":m.in_reply_to,"origin":m.origin,"accepted_at":m.accepted_at,"seq":m.seq,"delivery":jobs.mailboxes.receipt(&m.id).unwrap_or_else(||json!({"status":if matches!(m.to,Address::Connector{..}){"queued"}else{"accepted"},"accepted_on":m.accepted_on}))})
}
pub fn authorize_action(
    jobs: &Jobs,
    owner: &str,
    resident: &str,
    instance: &str,
    conversation: &str,
    action: &str,
) -> Result<Instance> {
    let i = jobs.connectors.get(owner, instance)?;
    if !i.definition.enabled {
        return Err("connector instance is disabled; pending correspondence is retained".into());
    }
    if !i.policy().potential(resident, action, conversation) {
        return Err("connector conversation has no matching resident binding or policy grant for this operation".into());
    }
    Ok(i)
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
    if !i.policy().potential(
        resident,
        if send { "send" } else { "receive" },
        conversation,
    ) {
        return Err("connector conversation has no matching resident binding or policy grant for this operation".into());
    }
    Ok(i)
}
pub fn operation(
    jobs: &std::sync::Arc<Jobs>,
    owner: &str,
    resident: Option<&str>,
    args: Value,
    source: String,
) -> Result<Value> {
    let action = args["action"].as_str().unwrap_or("list");
    let id = args["instance"].as_str().unwrap_or("");
    let result = (|| -> Result<Value> {
        match action {
            "call" => {
                let r = resident
                    .or_else(|| args["resident"].as_str())
                    .ok_or("connector calls have a resident")?;
                jobs.residents.get(owner, r)?;
                let instance = args["to"]["instance"]
                    .as_str()
                    .ok_or("destination instance required")?;
                let conversation = args["to"]["conversation"]
                    .as_str()
                    .ok_or("destination conversation required")?;
                let connector = jobs.connectors.get(owner, instance)?;
                let name = args["operation"].as_str().ok_or("operation required")?;
                let tool = crate::message_service::declared(&connector, name)?;
                authorize_action(jobs, owner, r, instance, conversation, &tool.permission)?;
                let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
                if !arguments.is_object() {
                    return Err("connector arguments are an object".into());
                }
                let message_id = args["id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| jobs.mailboxes.source_id(&source))
                    .unwrap_or(state::random_hex()?);
                let mut service = json!({"operation":name,"arguments":arguments});
                if let Some(paths) = args.get("_source_paths") {
                    service["source_paths"] = paths.clone();
                }
                let message = jobs.send_mail(
                    owner,
                    Address::Resident { id: r.into() },
                    Send {
                        attachments: vec![],
                        service: Some(service),
                        id: message_id,
                        to: address(instance, conversation)?,
                        subject: String::new(),
                        body: String::new(),
                        reply_to: None,
                        in_reply_to: None,
                    },
                    source,
                )?;
                Ok(project(jobs, &message))
            }
            "permissions" | "attention" | "watch" | "unwatch" | "mute" | "unmute"
            | "update_attention" => {
                let target = resident
                    .or_else(|| args["resident"].as_str())
                    .ok_or("resident required")?;
                jobs.residents.get(owner, target)?;
                let i = jobs.connectors.get(owner, id)?;
                if !i.residents().iter().any(|r| r == target) {
                    return Err("resident is not associated with this connector".into());
                }
                let policy = i.policy();
                if action == "permissions" {
                    return Ok(
                        json!({"revision":i.revision,"grants":policy.grants.into_iter().filter(|g|g.resident==target).collect::<Vec<_>>()}),
                    );
                }
                let mut attention = i.attention(target);
                if action == "attention" {
                    let stream = gateway_session(jobs, owner, id)?;
                    return Ok(
                        json!({"revision":i.revision,"attention":attention,"transport":if i.gateway {"gateway"}else{"polling"},"health":jobs.connectors.health(id),"native_stream":{"sequence":stream.sequence,"gaps":stream.gaps,"last_gap":stream.last_gap}}),
                    );
                }
                if action == "update_attention" {
                    attention = serde_json::from_value(args["attention"].clone())?;
                } else {
                    let scope: crate::message_policy::Scope =
                        serde_json::from_value(args["scope"].clone())?;
                    scope.validate()?;
                    let list = if matches!(action, "watch" | "unwatch") {
                        &mut attention.watched
                    } else {
                        &mut attention.muted
                    };
                    list.retain(|s| s != &scope);
                    if matches!(action, "watch" | "mute") {
                        list.push(scope);
                    }
                }
                attention.validate()?;
                let next = jobs.connectors.configure(
                    owner,
                    id,
                    args["expected_revision"]
                        .as_u64()
                        .ok_or("expected_revision required")?,
                    None,
                    None,
                    Some((target.into(), attention)),
                    None,
                )?;
                Ok(json!({"revision":next.revision,"attention":next.attention(target)}))
            }
            "list" => {
                let mut instances = vec![
                    json!({"id":"boxen","provider":"boxen","capabilities":["receive","send","replies","subjects"],"account":owner}),
                ];
                for i in jobs.connectors.list(owner, resident) {
                    instances.push(json!({"id":i.definition.id,"name":i.definition.name,"provider":i.definition.provider,"account":i.definition.account,"enabled":i.definition.enabled,"health":jobs.connectors.health(&i.definition.id),"bindings":i.definition.bindings.iter().filter(|b|resident.is_none_or(|r|b.resident==r)).collect::<Vec<_>>(),"capabilities":i.definition.capabilities,"tools":crate::message_service::tools(&i)}));
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
                    attachments: vec![],
                    service: None,
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
                let participant = resident.map(|id| Address::Resident { id: id.into() });
                let m = jobs.mailboxes.participant_message(
                    owner,
                    participant.as_ref(),
                    args["id"].as_str().ok_or("message id required")?,
                )?;
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
                    "configure_gateway" => Ok(serde_json::to_value(
                        jobs.connectors.configure(
                            owner,
                            id,
                            args["expected_revision"]
                                .as_u64()
                                .ok_or("expected_revision required")?,
                            None,
                            None,
                            None,
                            Some(args["enabled"].as_bool().ok_or("enabled required")?),
                        )?,
                    )?),
                    "gateway_state" => {
                        let i = jobs.connectors.get(owner, id)?;
                        if i.definition.provider != "discord" {
                            return Err("Gateway state belongs to Discord instances".into());
                        }
                        Ok(json!({"session":gateway_session(jobs,owner,id)?}))
                    }
                    "gateway_event" => gateway_event(jobs, owner, id, &args),
                    "native_presence" => {
                        let i = jobs.connectors.get(owner, id)?;
                        let ids: Vec<_> = i
                            .residents()
                            .into_iter()
                            .filter(|r| i.policy().permits(r, "presence", "", &Value::Null))
                            .collect();
                        if !i.definition.enabled || ids.is_empty() {
                            return Ok(
                                json!({"since":null,"activities":[],"status":"invisible","afk":false}),
                            );
                        }
                        let (active, queued) = jobs.resident_activity(owner, &ids);
                        let state = if active > 0 {
                            "busy"
                        } else if queued > 0 {
                            "queued"
                        } else {
                            "ready"
                        };
                        let label = if ids.len() == 1 {
                            format!("{}: {state}", jobs.residents.get(owner, &ids[0])?.name)
                        } else {
                            format!("{} residents, {active} active", ids.len())
                        };
                        Ok(
                            json!({"since":null,"activities":[{"name":"Resident status","type":4,"state":label}],"status":if active>0 {"dnd"}else if queued>0{"idle"}else{"online"},"afk":false}),
                        )
                    }
                    "attachment" => {
                        use base64::Engine;
                        jobs.connectors.get(owner, id)?;
                        let message = jobs
                            .mailboxes
                            .get(owner, args["id"].as_str().ok_or("message id required")?)?;
                        if !matches!(&message.to,Address::Connector{instance,..} if instance==id) {
                            return Err("attachment belongs to another connector".into());
                        }
                        let index = args["index"]
                            .as_u64()
                            .filter(|i| *i < 10)
                            .ok_or("attachment index is 0–9")?
                            as usize;
                        let artifact = message
                            .service
                            .as_ref()
                            .and_then(|s| s["arguments"]["files"].get(index))
                            .ok_or("retained attachment unavailable")?;
                        jobs.get(
                            owner,
                            artifact["run"].as_str().ok_or("artifact run required")?,
                        )?;
                        let bytes = crate::media::upload_bytes(jobs.artifact_root(), artifact)?;
                        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
                        let limit =
                            args["limit"].as_u64().unwrap_or(65536).clamp(1, 262144) as usize;
                        if offset > bytes.len() {
                            return Err("attachment offset exceeds retained file".into());
                        }
                        let end = (offset + limit).min(bytes.len());
                        Ok(
                            json!({"artifact":artifact,"offset":offset,"next_offset":end,"done":end==bytes.len(),"base64":base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end])}),
                        )
                    }
                    "configure_policy" => {
                        let policy: crate::message_policy::Policy =
                            serde_json::from_value(args["policy"].clone())?;
                        policy.validate()?;
                        for g in &policy.grants {
                            jobs.residents.get(owner, &g.resident)?;
                        }
                        Ok(serde_json::to_value(
                            jobs.connectors.configure(
                                owner,
                                id,
                                args["expected_revision"]
                                    .as_u64()
                                    .ok_or("expected_revision required")?,
                                None,
                                Some(policy),
                                None,
                                None,
                            )?,
                        )?)
                    }
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
                            .residents()
                            .iter()
                            .any(|r| i.policy().potential(r, "receive", conversation))
                        {
                            return Err("conversation is outside this connector's receive bindings and policy".into());
                        }
                        Ok(
                            json!({"checkpoint":jobs.mailboxes.cursor(&format!("{owner}:{id}:{conversation}"))}),
                        )
                    }
                    "ingest" => ingest(jobs, owner, id, &args),
                    "guest_chat_read" => {
                        let instance = jobs.connectors.get(owner, id)?;
                        let (binding, guest) = guest_chat_scope(&instance)?;
                        let resident = Address::Resident {
                            id: binding.resident.clone(),
                        };
                        let (messages, next_after) = jobs.mailboxes.guest_chat_messages(
                            owner,
                            &guest,
                            &resident,
                            args["after"].as_u64().unwrap_or(0),
                        )?;
                        let resident = jobs.residents.get(owner, &binding.resident)?;
                        let status = jobs.guest_chat_status(&resident);
                        let messages: Vec<_> = messages.iter().map(|message| {
                            let mut item = json!({
                                "id":message.id,"sender":if message.sender == guest {"guest"} else {"resident"},
                                "body":message.body,"at":message.accepted_at,
                            });
                            if !message.attachments.is_empty() {
                                item["attachments"] = json!(message.attachments.iter().map(|file|
                                    json!({"name":file.name,"size":file.size,"image":jobs.attachments.preview_mime(file).is_some()})
                                ).collect::<Vec<_>>());
                            }
                            item
                        }).collect();
                        Ok(
                            json!({"resident":resident.name,"status":status,"messages":messages,"next_after":next_after}),
                        )
                    }
                    "guest_chat_send" => guest_chat_send(jobs, owner, id, &args),
                    "guest_chat_upload" => {
                        let _transition = jobs.connectors.intake.lock().unwrap();
                        let instance = jobs.connectors.get(owner, id)?;
                        guest_chat_scope(&instance)?;
                        let transfer = &args["transfer"];
                        if !matches!(transfer["action"].as_str(), Some("status" | "write")) {
                            return Err("guest uploads support only status and write".into());
                        }
                        jobs.attachments.operation(transfer)
                    }
                    "guest_chat_attachment" => {
                        let instance = jobs.connectors.get(owner, id)?;
                        let (binding, guest) = guest_chat_scope(&instance)?;
                        let resident = Address::Resident {
                            id: binding.resident.clone(),
                        };
                        let mid = args["message_id"].as_str().ok_or("message id required")?;
                        let message = jobs.mailboxes.get(owner, mid)?;
                        let receipt = jobs.mailboxes.receipt(mid);
                        if !crate::mailbox::Mailboxes::guest_chat_message_matches(
                            &message,
                            &guest,
                            &resident,
                            receipt.as_ref(),
                        ) {
                            return Err("attachment belongs to another conversation".into());
                        }
                        let index = args["index"]
                            .as_u64()
                            .filter(|index| *index < MESSAGE_LIMIT as u64)
                            .ok_or("attachment index is outside the message")?
                            as usize;
                        let file = message
                            .attachments
                            .get(index)
                            .ok_or("message attachment unavailable")?;
                        Ok(serde_json::to_value(file)?)
                    }
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
                        let instance = jobs.connectors.get(owner, id)?;
                        if !instance.definition.enabled {
                            return Err("connector instance is disabled; pending correspondence is retained".into());
                        }
                        if !instance.policy().permits(
                            r,
                            &m.service
                                .as_ref()
                                .map(|s| {
                                    crate::message_service::declared(
                                        &instance,
                                        s["operation"].as_str().unwrap_or(""),
                                    )
                                })
                                .transpose()?
                                .map(|t| t.permission)
                                .unwrap_or_else(|| "send".into()),
                            conversation,
                            &args["native_context"],
                        ) {
                            let claim_id = args["claim_id"].as_str().ok_or("claim_id required")?;
                            let claim = jobs.mailboxes.claim(owner, id, mid, claim_id)?;
                            if claim["claimed"] != true {
                                return Ok(claim);
                            }
                            let receipt = jobs.mailboxes.complete(
                                owner,
                                id,
                                mid,
                                claim_id,
                                crate::mailbox::DeliveryOutcome {
                                    result: None,
                                    status: "rejected",
                                    external_id: None,
                                    reason: Some("policy_denied"),
                                },
                            )?;
                            event(
                                owner,
                                id,
                                "messaging.dispatch_policy_denied",
                                || json!({"message_id":mid,"resident_id":r,"conversation":conversation}),
                            );
                            return Ok(
                                json!({"claimed":false,"receipt":receipt,"policy_denied":true}),
                            );
                        }
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
                    "part" => {
                        jobs.connectors.get(owner, id)?;
                        jobs.mailboxes.part(
                            owner,
                            id,
                            args["id"].as_str().ok_or("message id required")?,
                            args["claim_id"].as_str().ok_or("claim_id required")?,
                            args["index"]
                                .as_u64()
                                .filter(|n| *n < 32)
                                .ok_or("part index is 0–31")? as usize,
                            args["total"]
                                .as_u64()
                                .filter(|n| *n <= 32)
                                .ok_or("part total is 1–32")? as usize,
                            crate::mailbox::DeliveryOutcome {
                                result: None,
                                status: args["status"].as_str().ok_or("status required")?,
                                external_id: args["external_id"].as_str(),
                                reason: args["reason"].as_str(),
                            },
                        )
                    }
                    "complete" => {
                        jobs.connectors.get(owner, id)?;
                        jobs.mailboxes.complete(
                            owner,
                            id,
                            args["id"].as_str().ok_or("message id required")?,
                            args["claim_id"].as_str().ok_or("claim_id required")?,
                            crate::mailbox::DeliveryOutcome {
                                result: args.get("result"),
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
        || json!({"action":action,"instance_id":id,"resident_id":resident,"message_id":args["id"],"conversation":args["conversation"],"part_index":args["index"],"parts_total":args["total"],"part_status":args["status"],"claimed":result.as_ref().ok().and_then(|v|v.get("claimed")),"delivery":result.as_ref().ok().and_then(|v|v.get("status")),"accepted":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string())}),
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
    ingest_locked(jobs, owner, id, args)
}
fn guest_chat_scope(instance: &Instance) -> Result<(&Binding, Address)> {
    if !instance.definition.enabled {
        return Err("guest chat is disabled".into());
    }
    let binding = crate::guest_chat::binding(&instance.definition)?;
    Ok((
        binding,
        Address::Connector {
            instance: instance.definition.id.clone(),
            conversation: binding.conversation.clone(),
        },
    ))
}

pub(crate) fn guest_chat_send_files(
    jobs: &std::sync::Arc<Jobs>,
    resident: &crate::residents::Resident,
    args: &Value,
    source: String,
) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        paths: Vec<String>,
        #[serde(default)]
        body: String,
        in_reply_to: Option<String>,
    }
    let input: Input = serde_json::from_value(args["arguments"].clone())?;
    if input.paths.is_empty() || input.paths.len() > MESSAGE_LIMIT || input.body.len() > 32 * 1024 {
        return Err("guest chat replies carry 1–8 file paths and at most 32 KiB of text".into());
    }
    let instance = args["to"]["instance"].as_str().ok_or("instance required")?;
    let conversation = args["to"]["conversation"]
        .as_str()
        .ok_or("conversation required")?;
    let connector = authorize(
        jobs,
        &resident.owner,
        &resident.id,
        instance,
        conversation,
        true,
    )?;
    let (binding, target) = guest_chat_scope(&connector)?;
    if binding.resident != resident.id || binding.conversation != conversation {
        return Err("guest chat replies belong to the bound conversation".into());
    }
    if let Some(id) = jobs.mailboxes.source_id(&source) {
        return Ok(project(jobs, &jobs.mailboxes.get(&resident.owner, &id)?));
    }
    let mut files = Vec::with_capacity(input.paths.len());
    for path in input.paths {
        if path.is_empty() || path.len() > 4096 {
            return Err("attachment paths contain 1–4096 bytes".into());
        }
        files.push(
            jobs.attachments
                .capture_local(&std::path::Path::new(&resident.cwd).join(path))?,
        );
    }
    let id = args["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or(state::random_hex()?);
    let message = jobs.send_mail(
        &resident.owner,
        Address::Resident {
            id: resident.id.clone(),
        },
        Send {
            id,
            to: target,
            body: input.body,
            attachments: files,
            service: None,
            subject: String::new(),
            reply_to: None,
            in_reply_to: input.in_reply_to,
        },
        source,
    )?;
    Ok(project(jobs, &message))
}

fn native_message_id(
    owner: &str,
    instance: &str,
    conversation: &str,
    native: &str,
    recipient: &str,
) -> Result<String> {
    Ok(state::hex(&Sha256::digest(serde_json::to_vec(&json!([
        "connector-message-v1",
        owner,
        instance,
        conversation,
        native,
        recipient
    ]))?)))
}

fn guest_chat_send(jobs: &Jobs, owner: &str, id: &str, args: &Value) -> Result<Value> {
    let _transition = jobs.connectors.intake.lock().unwrap();
    let instance = jobs.connectors.get(owner, id)?;
    let (binding, guest) = guest_chat_scope(&instance)?;
    let resident = Address::Resident {
        id: binding.resident.clone(),
    };
    let native = args["id"].as_str().ok_or("guest message id required")?;
    jobs::valid_id(native)?;
    let body = args["body"].as_str().ok_or("guest message text required")?;
    let attachments: Vec<Attachment> = args
        .get("attachments")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if (body.trim().is_empty() && attachments.is_empty())
        || body.len() > crate::guest_chat::MAX_MESSAGE
    {
        return Err("guest messages carry text up to 8 KiB or attachments".into());
    }
    let message_id =
        native_message_id(owner, id, &binding.conversation, native, &binding.resident)?;
    jobs.mailboxes.check()?;
    if let Ok(prior) = jobs.mailboxes.get(owner, &message_id) {
        if prior.body != body
            || prior.sender != guest
            || prior.to != resident
            || prior.attachments != attachments
        {
            return Err("guest message id already accepted with different input".into());
        }
        return Ok(json!({"id":message_id,"accepted":true}));
    }
    let mut message = json!({"id":native,"author":{"id":instance.definition.account,"name":"Guest"},"direct":true});
    if !instance.policy().permits(
        &binding.resident,
        "receive",
        &binding.conversation,
        &message,
    ) || !instance
        .attention(&binding.resident)
        .interested(&binding.conversation, &message)
    {
        return Err("guest message is outside the resident's current routing or attention".into());
    }
    set_native_origin(
        &mut message,
        &instance.definition,
        &binding.conversation,
        native,
    );
    jobs.residents.get(owner, &binding.resident)?;
    jobs.attachments.require(&attachments)?;
    jobs.mailboxes.accept(
        owner,
        guest.clone(),
        Send {
            attachments,
            service: None,
            id: message_id.clone(),
            to: resident,
            subject: String::new(),
            body: body.into(),
            reply_to: Some(guest),
            in_reply_to: None,
        },
        format!("connector:{message_id}"),
        Some(message),
    )?;
    jobs.messaging_notify();
    Ok(json!({"id":message_id,"accepted":true}))
}
fn ingest_locked(jobs: &Jobs, owner: &str, id: &str, args: &Value) -> Result<Value> {
    let i = jobs.connectors.get(owner, id)?;
    if i.definition.provider == crate::guest_chat::PROVIDER {
        return Err("guest chat intake uses its bound conversation and exact message IDs".into());
    }
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
    let policy = i.policy();
    let recipients: Vec<_> = i
        .residents()
        .into_iter()
        .filter(|r| policy.potential(r, "receive", conversation))
        .collect();
    if recipients.is_empty() {
        return Err("conversation is outside this connector's receive bindings and policy".into());
    }
    let messages = args["messages"]
        .as_array()
        .ok_or("messages array required")?;
    if messages.len() > 32 || serde_json::to_vec(args)?.len() > 192 * 1024 {
        return Err("intake batches contain at most 32 messages and 192 KiB".into());
    }
    let cursor = args["cursor"].as_str().ok_or("cursor required")?;
    opaque(cursor, 256)?;
    let key = cursor_key(owner, id, conversation, args["stream"].as_str());
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
        set_native_origin(&mut origin, &i.definition, conversation, native_id);
        if serde_json::to_vec(&origin)?.len() > 32 * 1024 {
            return Err("native message metadata exceeds 32 KiB".into());
        }
        for recipient in &recipients {
            if i.definition.provider == "discord" && native["author"]["id"] == i.definition.account
            {
                continue;
            }
            if !policy.permits(recipient, "receive", conversation, native)
                || !i.attention(recipient).interested(conversation, native)
            {
                continue;
            }
            jobs.residents.get(owner, recipient)?;
            let mid = native_message_id(owner, id, conversation, native_id, recipient)?;
            let source = format!("connector:{mid}");
            jobs.mailboxes.accept(
                owner,
                Address::Connector {
                    instance: id.into(),
                    conversation: conversation.into(),
                },
                Send {
                    attachments: vec![],
                    service: None,
                    id: mid,
                    to: Address::Resident {
                        id: recipient.clone(),
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

fn set_native_origin(
    origin: &mut Value,
    definition: &Definition,
    conversation: &str,
    native_id: &str,
) {
    origin.as_object_mut().unwrap().remove("body");
    origin["instance"] = json!(definition.id);
    origin["provider"] = json!(definition.provider);
    origin["account"] = json!(definition.account);
    origin["conversation"] = json!(conversation);
    origin["native_id"] = json!(native_id);
}

fn cursor_key(owner: &str, id: &str, conversation: &str, stream: Option<&str>) -> String {
    match stream {
        Some(stream) => state::hex(&Sha256::digest(
            serde_json::to_vec(&json!([
                "native-stream-v1",
                owner,
                id,
                conversation,
                stream
            ]))
            .unwrap(),
        )),
        None => format!("{owner}:{id}:{conversation}"),
    }
}
fn gateway_session(
    jobs: &Jobs,
    owner: &str,
    id: &str,
) -> Result<crate::discord_connector::gateway::Session> {
    let key = format!("gateway:{owner}:{id}");
    match jobs
        .mailboxes
        .cursor(&key)
        .and_then(|v| v["cursor"].as_str().map(str::to_owned))
    {
        Some(s) => Ok(serde_json::from_str(&s)?),
        None => Ok(Default::default()),
    }
}
fn gateway_event(
    jobs: &std::sync::Arc<Jobs>,
    owner: &str,
    id: &str,
    args: &Value,
) -> Result<Value> {
    let _guard = jobs.connectors.intake.lock().unwrap();
    let instance = jobs.connectors.get(owner, id)?;
    if instance.definition.provider != "discord"
        || !instance.definition.enabled
        || !instance.gateway
    {
        return Err(
            "native Gateway intake is enabled only for configured Discord instances".into(),
        );
    }
    let mut state = gateway_session(jobs, owner, id)?;
    let event = &args["event"];
    let kind = event["type"]
        .as_str()
        .ok_or("Gateway event type required")?;
    if serde_json::to_vec(event)?.len() > 192 * 1024 {
        return Err("Gateway event exceeds 192 KiB".into());
    }
    if kind == "INVALID_SESSION" {
        state.session_id = None;
        state.resume_url = None;
        state.sequence = 0;
        state.gaps += 1;
        state.last_gap = Some("resume_unavailable".into());
    } else {
        let seq = event["sequence"]
            .as_u64()
            .ok_or("Gateway sequence required")?;
        let session = event["session_id"]
            .as_str()
            .ok_or("Gateway session required")?;
        opaque(session, 256)?;
        if state.session_id.as_deref() == Some(session) && seq <= state.sequence {
            return Ok(json!({"session":state,"duplicate":true}));
        }
        if state.session_id.as_deref() == Some(session) && seq > state.sequence.saturating_add(1) {
            state.gaps += 1;
            state.last_gap = Some("native_sequence_gap".into());
        }
        let data = &event["data"];
        if kind == "READY" {
            if data["user"]["id"] != instance.definition.account {
                return Err("Gateway identity differs from the connector account".into());
            }
            let url = data["resume_gateway_url"]
                .as_str()
                .ok_or("resume URL required")?;
            crate::discord_connector::gateway::validate_url(url)?;
            if state.session_id.is_some() && state.session_id.as_deref() != Some(session) {
                state.gaps += 1;
            }
            state.session_id = Some(session.into());
            state.resume_url = Some(url.into());
        } else if state.session_id.as_deref() != Some(session) {
            return Err("Gateway event is outside its retained session".into());
        }
        if kind == "MESSAGE_REACTION_ADD"
            && args["native_context"]["unavailable"] != true
            && publish_reaction(
                jobs,
                owner,
                &instance,
                session,
                seq,
                data,
                &args["native_context"],
            )
            .is_err()
        {
            state.gaps += 1;
            state.last_gap = Some("reaction_retention_unavailable".into());
        }
        if args["native_context"]["unavailable"] == true {
            state.gaps += 1;
            state.last_gap = Some("native_scope_unavailable".into());
        }
        if kind == "MESSAGE_CREATE" && args["native_context"]["unavailable"] != true {
            let conversation = data["channel_id"]
                .as_str()
                .ok_or("native conversation required")?;
            let policy = instance.policy();
            if instance
                .residents()
                .iter()
                .any(|r| policy.potential(r, "receive", conversation))
            {
                let context = &args["native_context"];
                let mut native = data.clone();
                native["space"] = data
                    .get("guild_id")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| context["space"].clone());
                native["direct"] = json!(data["guild_id"].is_null() && context["space"].is_null());
                native["parent"] = context["parent"].clone();
                native["mention"] = json!(
                    data["native"]["mentions"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|u| u["id"] == instance.definition.account))
                );
                native["reply"] = json!(
                    data["native"]["referenced_message"]["author"]["id"]
                        == instance.definition.account
                );
                let key = cursor_key(owner, id, conversation, Some(session));
                let after = jobs
                    .mailboxes
                    .cursor(&key)
                    .map(|v| v["cursor"].clone())
                    .unwrap_or(Value::Null);
                ingest_locked(
                    jobs,
                    owner,
                    id,
                    &json!({"conversation":conversation,"stream":session,"after":after,"cursor":seq.to_string(),"messages":[native]}),
                )?;
            }
        }
        state.sequence = seq;
    }
    let value = serde_json::to_string(&state)?;
    jobs.mailboxes.checkpoint(
        &format!("gateway:{owner}:{id}"),
        &value,
        &state::hex(&Sha256::digest(value.as_bytes())),
    )?;
    event_gateway_log(owner, id, kind, &state);
    Ok(json!({"session":state}))
}
fn event_gateway_log(
    owner: &str,
    id: &str,
    kind: &str,
    state: &crate::discord_connector::gateway::Session,
) {
    event(
        owner,
        id,
        "discord.gateway_accepted",
        || json!({"event_type":kind,"sequence":state.sequence,"gaps":state.gaps}),
    );
}

fn publish_reaction(
    jobs: &std::sync::Arc<Jobs>,
    owner: &str,
    instance: &Instance,
    session: &str,
    seq: u64,
    data: &Value,
    context: &Value,
) -> Result<()> {
    if context["message_author_id"] != instance.definition.account
        || data["user_id"] == instance.definition.account
    {
        return Ok(());
    }
    let conversation = data["channel_id"]
        .as_str()
        .ok_or("reaction conversation required")?;
    let mut native = context.clone();
    native["author"] = json!({"id":data["user_id"],"bot":data["member"]["user"]["bot"]});
    let policy = instance.policy();
    for resident in instance.residents() {
        let attention = instance.attention(&resident);
        if !attention.reactions
            || attention
                .muted
                .iter()
                .any(|s| s.matches(conversation, &native))
            || !policy.permits(&resident, "receive", conversation, &native)
        {
            continue;
        }
        let r = jobs.residents.get(owner, &resident)?;
        let id = state::hex(&Sha256::digest(serde_json::to_vec(&json!([
            "discord-reactions-v1",
            instance.definition.id,
            resident
        ]))?));
        let source=jobs.event_operation(owner,json!({"action":"create_source","spec":{"id":id,"name":"Discord reactions","cwd":r.cwd,"audience":resident}}))?;
        jobs.event_operation(owner,json!({"action":"subscribe","spec":{"id":state::hex(&Sha256::digest(format!("reaction-sub:{id}").as_bytes())),"source":id,"target":{"type":"resident","id":resident},"interval_ms":1000,"waking":false,"health_waking":false,"lifetime":"persistent"}}))?;
        if source["cursor"]["session"] == session
            && source["cursor"]["sequence"]
                .as_u64()
                .is_some_and(|n| n >= seq)
        {
            continue;
        }
        let observation = state::hex(&Sha256::digest(serde_json::to_vec(&json!([
            session, seq, &id
        ]))?));
        jobs.observations.publish(owner,&id,source["generation"].as_u64().ok_or("reaction source generation unavailable")?,None,
            json!({"protocol":"boxen-adapter-v1","cursor":{"session":session,"sequence":seq},"observations":[{"id":observation,"observed_at_ms":jobs::now(),"payload":{"source":"discord","instance":instance.definition.id,"account":instance.definition.account,"kind":"reaction","action":"add","conversation":conversation,"native_message_id":data["message_id"],"user_id":data["user_id"],"emoji":data["emoji"],"burst":data["burst"]}}]}))?;
    }
    Ok(())
}

fn controls_presence(i: &Instance) -> bool {
    i.gateway
        && i.definition.enabled
        && i.residents()
            .iter()
            .any(|r| i.policy().permits(r, "presence", "", &Value::Null))
}

/// Only the authenticated loopback control boundary calls this entry point.
/// The stored instance selects its owner; requests cannot nominate another owner.
pub(crate) fn local_runner_operation(
    jobs: &std::sync::Arc<Jobs>,
    id: &str,
    request: &Value,
) -> Result<Value> {
    let result = (|| -> Result<Value> {
        jobs::valid_id(id)?;
        if request["op"] != "messaging"
            || request["args"]["instance"] != id
            || !matches!(
                request["args"]["action"].as_str(),
                Some(
                    "get"
                        | "cursor"
                        | "ingest"
                        | "outbox"
                        | "claim"
                        | "part"
                        | "complete"
                        | "health"
                        | "gateway_state"
                        | "gateway_event"
                        | "native_presence"
                        | "attachment"
                        | "guest_chat_read"
                        | "guest_chat_send"
                        | "guest_chat_upload"
                        | "guest_chat_attachment"
                )
            )
        {
            return Err(
                "local connector control accepts only instance-matched runner operations".into(),
            );
        }
        let i = jobs.connectors.local_runner_instance(id)?;
        let action = request["args"]["action"].as_str().unwrap_or("");
        let guest_chat = i.definition.provider == crate::guest_chat::PROVIDER;
        if action != "get" && guest_chat != action.starts_with("guest_chat_") {
            return Err("guest chat runner control accepts only its chat operations".into());
        }
        crate::protocol::operation(jobs, &i.owner, request)
    })();
    event(
        "",
        id,
        "messaging.local_runner_operation",
        || json!({"action":request["args"]["action"],"accepted":result.is_ok()}),
    );
    result
}
