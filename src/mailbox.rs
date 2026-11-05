//! The host mailbox journal owns accepted communication. Run records own batch presentation.
use crate::{
    Result, jobs,
    logging::{self, Context, Level, Target},
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Address {
    Resident {
        id: String,
    },
    Client {
        machine: String,
        id: String,
    },
    Connector {
        instance: String,
        conversation: String,
    },
}
impl Address {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Resident { id } => jobs::valid_id(id),
            Self::Connector {
                instance,
                conversation,
            } => {
                jobs::valid_id(instance)?;
                crate::messaging::opaque(conversation, 256)
            }
            Self::Client { machine, id } => {
                state::key(machine)?;
                jobs::valid_id(id)
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Send {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<Value>,
    pub id: String,
    pub to: Address,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<Address>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<Value>,
    pub id: String,
    pub owner: String,
    pub sender: Address,
    pub to: Address,
    pub subject: String,
    pub body: String,
    pub reply_to: Address,
    pub in_reply_to: Option<String>,
    pub accepted_at: u64,
    pub seq: u64,
    pub accepted_on: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Value>,
}
impl Message {
    fn matches(&self, owner: &str, sender: &Address, spec: &Send) -> bool {
        self.owner == owner
            && &self.sender == sender
            && self.id == spec.id
            && self.to == spec.to
            && self.subject == spec.subject
            && self.body == spec.body
            && self.reply_to == spec.reply_to.clone().unwrap_or_else(|| sender.clone())
            && self.in_reply_to == spec.in_reply_to
            && self.service == spec.service
    }
}
struct Inner {
    file: Option<File>,
    seq: u64,
    messages: Vec<Message>,
    ids: HashMap<String, usize>,
    sources: HashMap<String, String>,
    residents: HashSet<String>,
    receipts: HashMap<String, Value>,
    cursors: HashMap<String, Value>,
}
pub struct DeliveryOutcome<'a> {
    pub result: Option<&'a Value>,
    pub status: &'a str,
    pub external_id: Option<&'a str>,
    pub reason: Option<&'a str>,
}
pub struct Mailboxes {
    machine: String,
    inner: Mutex<Inner>,
    issue: Option<String>,
}
impl Mailboxes {
    pub fn open(directory: PathBuf, machine: String) -> Result<Self> {
        match Self::load(directory.clone(), machine.clone()) {
            Ok(store) => Ok(store),
            Err(error) => {
                let mut issue = format!(
                    "Mailbox journal unavailable: {error}. Original state is preserved; anonymous workloads and resident continuity remain available."
                );
                let retained = (|| -> Result<()> {
                    state::private_dir(&directory)?;
                    let mut options = OpenOptions::new();
                    options.create(true).append(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut file = options.open(directory.join("incidents.jsonl"))?;
                    writeln!(
                        file,
                        "{}",
                        json!({"at":jobs::now(),"kind":"mailbox_unavailable","detail":issue})
                    )?;
                    file.sync_data()?;
                    Ok(())
                })();
                if let Err(error) = retained {
                    issue.push_str(&format!(" Incident retention also failed: {error}"));
                }
                logging::record(
                    Level::Error,
                    Target::State,
                    "mailbox.unavailable",
                    Context::default(),
                    || json!({"error":issue}),
                    || Value::Null,
                );
                Ok(Self {
                    machine,
                    issue: Some(issue),
                    inner: Mutex::new(Inner {
                        file: None,
                        seq: 0,
                        messages: Vec::new(),
                        ids: HashMap::new(),
                        sources: HashMap::new(),
                        residents: HashSet::new(),
                        receipts: HashMap::new(),
                        cursors: HashMap::new(),
                    }),
                })
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        if let Some(issue) = &self.issue {
            return Err(issue.clone().into());
        }
        Ok(())
    }
    pub fn status(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({"available":self.issue.is_none(),"issue":self.issue,"last_seq":if self.issue.is_none(){Some(inner.seq)}else{None},"message_count":if self.issue.is_none(){Some(inner.messages.len())}else{None},"network_pending":inner.receipts.values().filter(|r|r["transport"]=="boxen_network" && r["status"]=="queued").count()})
    }
    fn load(directory: PathBuf, machine: String) -> Result<Self> {
        state::private_dir(&directory)?;
        let path = directory.join("events.jsonl");
        if !path.exists() {
            state::write_new(&path, b"")?;
        }
        let mut inner = Inner {
            file: Some(OpenOptions::new().append(true).open(&path)?),
            seq: 0,
            messages: Vec::new(),
            ids: HashMap::new(),
            sources: HashMap::new(),
            residents: HashSet::new(),
            receipts: HashMap::new(),
            cursors: HashMap::new(),
        };
        let mut reader = BufReader::new(File::open(&path)?);
        let mut complete = 0;
        loop {
            let mut line = Vec::new();
            if reader
                .by_ref()
                .take(1024 * 1024 + 1)
                .read_until(b'\n', &mut line)?
                == 0
            {
                break;
            }
            if line.len() > 1024 * 1024 {
                return Err("mailbox journal record exceeds 1 MiB".into());
            }
            if !line.ends_with(b"\n") {
                OpenOptions::new()
                    .write(true)
                    .open(&path)?
                    .set_len(complete)?;
                break;
            }
            let value: Value = serde_json::from_slice(&line)?;
            if value["seq"].as_u64() != Some(inner.seq + 1) {
                return Err("mailbox journal sequence mismatch".into());
            }
            Self::project(&mut inner, &value)?;
            complete += line.len() as u64;
        }
        logging::record(
            Level::Info,
            Target::State,
            "mailbox.opened",
            Context::default(),
            || json!({"messages":inner.messages.len(),"last_seq":inner.seq}),
            || Value::Null,
        );
        Ok(Self {
            machine,
            issue: None,
            inner: Mutex::new(inner),
        })
    }
    fn project(inner: &mut Inner, event: &Value) -> Result<()> {
        inner.seq = event["seq"].as_u64().ok_or("missing mailbox sequence")?;
        match event["kind"].as_str() {
            Some("message") => {
                let message: Message = serde_json::from_value(event["message"].clone())?;
                if inner.ids.contains_key(&message.id) {
                    return Err("duplicate message in mailbox journal".into());
                }
                inner
                    .sources
                    .insert(message.source.clone(), message.id.clone());
                if let Some(delivery) = event.get("network_delivery") {
                    inner.receipts.insert(message.id.clone(), delivery.clone());
                }
                inner.ids.insert(message.id.clone(), inner.messages.len());
                inner.messages.push(message);
            }
            Some("connector_receipt") => {
                inner.receipts.insert(
                    event["id"].as_str().ok_or("receipt id required")?.into(),
                    event["receipt"].clone(),
                );
            }
            Some("connector_cursor") => {
                inner.cursors.insert(
                    event["key"].as_str().ok_or("cursor key required")?.into(),
                    event["checkpoint"].clone(),
                );
            }
            Some("resident_registered") => {
                inner.residents.insert(
                    event["resident_id"]
                        .as_str()
                        .ok_or("resident id required")?
                        .into(),
                );
            }
            _ => return Err("unknown mailbox journal event".into()),
        }
        Ok(())
    }
    fn append(inner: &mut Inner, mut event: Value) -> Result<()> {
        event["seq"] = json!(inner.seq + 1);
        event["at"] = json!(jobs::now());
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        let file = inner
            .file
            .as_mut()
            .ok_or("mailbox journal is unavailable")?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        Self::project(inner, &event)
    }
    pub fn register(&self, owner: &str, id: &str) -> Result<()> {
        if self.issue.is_some() {
            return Ok(());
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.residents.contains(id) {
            return Ok(());
        }
        Self::append(
            &mut inner,
            json!({"kind":"resident_registered","resident_id":id,"owner":owner,"communication":"mailbox"}),
        )?;
        logging::record(
            Level::Info,
            Target::State,
            "mailbox.resident_registered",
            Context {
                peer_id: Some(owner),
                thread_id: Some(id),
                ..Context::default()
            },
            || json!({"resident_id":id}),
            || Value::Null,
        );
        Ok(())
    }
    pub fn source_id(&self, source: &str) -> Option<String> {
        self.inner.lock().unwrap().sources.get(source).cloned()
    }
    pub fn send(
        &self,
        owner: &str,
        sender: Address,
        spec: Send,
        source: String,
    ) -> Result<Message> {
        self.accept(owner, sender, spec, source, None)
    }
    pub fn accept(
        &self,
        owner: &str,
        sender: Address,
        spec: Send,
        source: String,
        origin: Option<Value>,
    ) -> Result<Message> {
        self.accept_routed(owner, sender, spec, source, origin, None)
    }
    pub fn accept_routed(
        &self,
        owner: &str,
        sender: Address,
        spec: Send,
        source: String,
        origin: Option<Value>,
        destination: Option<&str>,
    ) -> Result<Message> {
        self.check()?;
        jobs::valid_id(&spec.id)?;
        sender.validate()?;
        spec.to.validate()?;
        if let Some(address) = &spec.reply_to {
            address.validate()?;
        }
        if let Some(id) = &spec.in_reply_to {
            jobs::valid_id(id)?;
        }
        if spec.subject.len() > 256
            || (spec.subject.trim().is_empty()
                && origin.is_none()
                && !matches!(&spec.to, Address::Connector { .. }))
        {
            return Err(
                "message acceptance requires a nonblank subject of at most 256 bytes".into(),
            );
        }
        if let Some(service) = &spec.service
            && (!matches!(spec.to, Address::Connector { .. })
                || !service.is_object()
                || serde_json::to_vec(service)?.len() > 32 * 1024)
        {
            return Err("service requests address external connectors and fit 32 KiB".into());
        }
        if spec.body.len() > 32 * 1024
            || (spec.body.is_empty() && origin.is_none() && spec.service.is_none())
        {
            return Err("message body acceptance range is 1–32768 bytes".into());
        }
        let mut inner = self.inner.lock().unwrap();
        let prior = inner
            .sources
            .get(&source)
            .or_else(|| inner.ids.get(&spec.id).map(|i| &inner.messages[*i].id));
        if let Some(id) = prior {
            let prior = &inner.messages[inner.ids[id]];
            if prior.matches(owner, &sender, &spec)
                && prior.origin == origin
                && prior
                    .network
                    .as_ref()
                    .and_then(|n| n["destination_machine"].as_str())
                    == destination
            {
                return Ok(prior.clone());
            }
            return Err(
                "message id or source already accepted with different input or sender".into(),
            );
        }
        if let Address::Connector { instance, .. } = &spec.to
            && inner
                .messages
                .iter()
                .filter(|m| {
                    matches!(&m.to,Address::Connector{instance:i,..} if i==instance)
                        && !inner.receipts.contains_key(&m.id)
                })
                .count()
                >= 256
        {
            return Err("connector pending outbox reached its 256-message bound".into());
        }
        let reply_to = spec.reply_to.clone().unwrap_or_else(|| sender.clone());
        if destination.is_some()
            && inner
                .receipts
                .values()
                .filter(|r| r["transport"] == "boxen_network" && r["status"] == "queued")
                .count()
                >= 256
        {
            return Err("network pending outbox reached its 256-message bound".into());
        }
        let message = Message {
            network: destination
                .map(|d| json!({"source_machine":self.machine,"destination_machine":d})),
            service: spec.service,
            id: spec.id,
            owner: owner.into(),
            sender,
            to: spec.to,
            subject: spec.subject,
            body: spec.body,
            reply_to,
            in_reply_to: spec.in_reply_to,
            accepted_at: jobs::now(),
            seq: inner.seq + 1,
            accepted_on: self.machine.clone(),
            source,
            origin,
        };
        let mut event = json!({"kind":"message","message":message});
        if let Some(d) = destination {
            event["network_delivery"] = json!({"transport":"boxen_network","destination_machine":d,"status":"queued","saved_on":self.machine,"attempts":0,"next_attempt_at":0});
        }
        Self::append(&mut inner, event)?;
        logging::record(
            Level::Info,
            Target::State,
            "mailbox.message_accepted",
            Context {
                peer_id: Some(owner),
                thread_id: match &message.to {
                    Address::Resident { id } => Some(id),
                    _ => None,
                },
                ..Context::default()
            },
            || json!({"message_id":message.id,"sender":message.sender,"to":message.to,"body_bytes":message.body.len(),"sequence":message.seq}),
            || json!({"subject":message.subject,"body":message.body}),
        );
        Ok(message)
    }
    pub fn accept_network(&self, remote: &str, mut message: Message) -> Result<Message> {
        self.check()?;
        jobs::valid_id(&message.id)?;
        message.sender.validate()?;
        message.to.validate()?;
        message.reply_to.validate()?;
        if message.network.as_ref().is_none_or(|n| {
            n["source_machine"] != remote || n["destination_machine"] != self.machine
        }) || message.accepted_on != remote
            || message.subject.len() > 256
            || message.subject.trim().is_empty()
            || message.body.is_empty()
            || message.body.len() > 32768
            || message.service.is_some()
            || message.origin.is_some()
            || message.source.len() > 256
            || serde_json::to_vec(&message.network)?.len() > 512
            || crate::state::key(&message.owner).is_err()
            || matches!(message.to, Address::Connector { .. })
            || matches!(message.sender, Address::Connector { .. })
        {
            return Err("network mail envelope is invalid".into());
        }
        if let Some(id) = &message.in_reply_to {
            jobs::valid_id(id)?;
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(i) = inner.ids.get(&message.id) {
            let prior = &inner.messages[*i];
            message.seq = prior.seq;
            if &message == prior {
                return Ok(prior.clone());
            }
            return Err("network message identity conflicts with retained input".into());
        }
        if inner.sources.contains_key(&message.source) {
            return Err("network message source conflicts with retained input".into());
        }
        message.seq = inner.seq + 1;
        Self::append(
            &mut inner,
            json!({"kind":"message","message":message,"network_delivery":{"transport":"boxen_network","status":"accepted","accepted_on":self.machine,"source_machine":remote}}),
        )?;
        logging::record(
            Level::Info,
            Target::State,
            "network.mail_received",
            Context::peer(remote),
            || json!({"message_id":message.id,"sequence":message.seq}),
            || Value::Null,
        );
        Ok(message)
    }
    pub fn owners(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|m| m.owner.clone())
            .collect()
    }
    pub fn retry(&self, sender: &Address, spec: &Send, source: &str) -> Result<Option<Message>> {
        self.check()?;
        let inner = self.inner.lock().unwrap();
        let old = inner
            .sources
            .get(source)
            .and_then(|id| inner.ids.get(id))
            .or_else(|| inner.ids.get(&spec.id))
            .and_then(|i| inner.messages.get(*i));
        match old {
            Some(m) if m.matches(&m.owner, sender, spec) => Ok(Some(m.clone())),
            Some(_) => {
                Err("message id or source already accepted with different input or sender".into())
            }
            None => Ok(None),
        }
    }
    pub fn network_pending(&self, at: u64) -> Vec<Message> {
        let inner = self.inner.lock().unwrap();
        inner
            .messages
            .iter()
            .filter(|m| {
                inner.receipts.get(&m.id).is_some_and(|r| {
                    r["transport"] == "boxen_network"
                        && r["status"] == "queued"
                        && r["next_attempt_at"].as_u64().unwrap_or(0) <= at
                })
            })
            .take(8)
            .cloned()
            .collect()
    }
    pub fn network_outcome(&self, id: &str, outcome: Value) -> Result<()> {
        self.check()?;
        let mut inner = self.inner.lock().unwrap();
        if !inner
            .receipts
            .get(id)
            .is_some_and(|r| r["transport"] == "boxen_network" && r["status"] == "queued")
        {
            return Err("network delivery is not pending".into());
        }
        Self::append(
            &mut inner,
            json!({"kind":"connector_receipt","id":id,"receipt":outcome}),
        )
    }
    pub fn participant_message(
        &self,
        owner: &str,
        participant: Option<&Address>,
        id: &str,
    ) -> Result<Message> {
        self.check()?;
        let inner = self.inner.lock().unwrap();
        inner
            .ids
            .get(id)
            .and_then(|i| inner.messages.get(*i))
            .filter(|m| {
                m.owner == owner || participant.is_some_and(|p| &m.sender == p || &m.to == p)
            })
            .cloned()
            .ok_or_else(|| "message unavailable to this participant".into())
    }
    pub fn get(&self, owner: &str, id: &str) -> Result<Message> {
        self.check()?;
        let inner = self.inner.lock().unwrap();
        inner
            .ids
            .get(id)
            .and_then(|i| inner.messages.get(*i))
            .filter(|m| m.owner == owner)
            .cloned()
            .ok_or_else(|| "message unavailable in authenticated scope".into())
    }
    pub fn receipt(&self, id: &str) -> Option<Value> {
        self.inner.lock().unwrap().receipts.get(id).cloned()
    }
    pub fn external_messages(&self, owner: &str, instance: &str) -> Vec<Message> {
        let inner = self.inner.lock().unwrap();
        let mut bytes = 0;
        inner
            .messages
            .iter()
            .filter(|m| {
                m.owner == owner && matches!(&m.to,Address::Connector{instance:i,..} if i==instance)
            })
            .filter(|m| !inner.receipts.contains_key(&m.id))
            .take(128)
            .take_while(|m| {
                bytes += m.body.len() + 1024;
                bytes <= 192 * 1024
            })
            .cloned()
            .collect()
    }
    pub fn claim(&self, owner: &str, instance: &str, id: &str, claim_id: &str) -> Result<Value> {
        self.check()?;
        jobs::valid_id(claim_id)?;
        let mut inner = self.inner.lock().unwrap();
        let message = inner
            .ids
            .get(id)
            .and_then(|i| inner.messages.get(*i))
            .filter(|m| {
                m.owner == owner && matches!(&m.to,Address::Connector{instance:i,..} if i==instance)
            })
            .cloned()
            .ok_or("outgoing message unavailable in connector scope")?;
        if let Some(receipt) = inner.receipts.get(id) {
            return Ok(json!({"claimed":false,"receipt":receipt}));
        }
        let receipt = json!({"status":"uncertain","claim_id":claim_id,"at":jobs::now(),"detail":"Dispatch was claimed; an external effect may have occurred. This claim is not automatically replayed."});
        Self::append(
            &mut inner,
            json!({"kind":"connector_receipt","id":id,"receipt":receipt}),
        )?;
        Ok(json!({"claimed":true,"message":message,"receipt":receipt}))
    }
    pub fn complete(
        &self,
        owner: &str,
        instance: &str,
        id: &str,
        claim_id: &str,
        outcome: DeliveryOutcome<'_>,
    ) -> Result<Value> {
        let DeliveryOutcome {
            status,
            external_id,
            reason,
            result,
        } = outcome;
        if let Some(reason) = reason {
            crate::messaging::opaque(reason, 64)?;
        }
        self.check()?;
        let mut inner = self.inner.lock().unwrap();
        let message = inner
            .ids
            .get(id)
            .and_then(|i| inner.messages.get(*i))
            .filter(|m| {
                m.owner == owner && matches!(&m.to,Address::Connector{instance:i,..} if i==instance)
            })
            .ok_or("outgoing message unavailable in connector scope")?;
        let prior = inner
            .receipts
            .get(id)
            .ok_or("external outcome has no retained dispatch claim")?;
        if prior["claim_id"] != claim_id {
            return Err("external outcome belongs to another dispatch claim".into());
        }
        if !matches!(status, "accepted" | "rejected" | "uncertain") {
            return Err("external outcome is accepted, rejected or uncertain".into());
        }
        if let Some(id) = external_id {
            crate::messaging::opaque(id, 256)?;
        }
        if result.is_some_and(|v| serde_json::to_vec(v).map_or(true, |b| b.len() > 64 * 1024)) {
            return Err("service result exceeds 64 KiB".into());
        }
        if status == "accepted"
            && external_id.is_none()
            && !(message.service.is_some() && result.is_some())
        {
            return Err("external acceptance includes the service message id".into());
        }
        let mut receipt = json!({"status":status,"claim_id":claim_id,"external_id":external_id,"reason":reason,"at":jobs::now()});
        if let Some(result) = result {
            receipt["result"] = result.clone();
        }
        if let Some(value) = prior.get("parts") {
            let parts = value
                .as_array()
                .ok_or("retained multipart outcomes are unavailable")?;
            let total = prior["parts_total"]
                .as_u64()
                .ok_or("retained part count is unavailable")?;
            if status == "accepted"
                && (parts.len() as u64 != total || parts.iter().any(|p| p["status"] != "accepted"))
            {
                return Err("multipart acceptance requires every declared part to have a retained accepted outcome".into());
            }
            if status == "rejected" && parts.iter().any(|p| p["status"] != "rejected") {
                return Err("a multipart dispatch with accepted or unknown effects cannot be relabeled rejected".into());
            }
            receipt["parts"] = value.clone();
            receipt["parts_total"] = prior["parts_total"].clone();
        }
        if prior["status"] != "uncertain" {
            return if prior["status"] == status
                && prior["external_id"] == json!(external_id)
                && prior["reason"] == json!(reason)
                && prior.get("result") == result
            {
                Ok(prior.clone())
            } else {
                Err("an established external outcome cannot be relabeled".into())
            };
        }
        Self::append(
            &mut inner,
            json!({"kind":"connector_receipt","id":id,"receipt":receipt}),
        )?;
        Ok(receipt)
    }
    /// Progress belongs to the existing correspondence receipt, never a second
    /// adapter database. A claimed part may have taken effect after a crash.
    #[allow(clippy::too_many_arguments)] // Coordinates one part within the authoritative receipt.
    pub fn part(
        &self,
        owner: &str,
        instance: &str,
        id: &str,
        claim_id: &str,
        index: usize,
        total: usize,
        outcome: DeliveryOutcome<'_>,
    ) -> Result<Value> {
        self.check()?;
        if total == 0 || total > 32 || index >= total {
            return Err("multipart delivery contains 1–32 ordered parts".into());
        }
        if !matches!(
            outcome.status,
            "claimed" | "accepted" | "rejected" | "uncertain"
        ) || (outcome.status == "accepted" && outcome.external_id.is_none())
        {
            return Err(
                "part outcomes are claimed, accepted with a service id, rejected or uncertain"
                    .into(),
            );
        }
        if let Some(v) = outcome.external_id {
            crate::messaging::opaque(v, 256)?;
        }
        if let Some(v) = outcome.reason {
            crate::messaging::opaque(v, 64)?;
        }
        let mut inner = self.inner.lock().unwrap();
        inner
            .ids
            .get(id)
            .and_then(|i| inner.messages.get(*i))
            .filter(|m| {
                m.owner == owner && matches!(&m.to,Address::Connector{instance:i,..} if i==instance)
            })
            .ok_or("outgoing message unavailable in connector scope")?;
        let prior = inner
            .receipts
            .get(id)
            .ok_or("part has no retained dispatch claim")?;
        if prior["claim_id"] != claim_id {
            return Err("part belongs to another dispatch claim".into());
        }
        if prior
            .get("parts_total")
            .is_some_and(|n| n.as_u64() != Some(total as u64))
        {
            return Err("declared part count cannot change within a dispatch".into());
        }
        let mut parts = match prior.get("parts") {
            Some(value) => value
                .as_array()
                .ok_or("retained multipart outcomes are unavailable")?
                .clone(),
            None => Vec::new(),
        };
        let mut part = json!({"index":index,"status":outcome.status,"external_id":outcome.external_id,"reason":outcome.reason});
        if let Some(old) = parts.get(index) {
            if ["index", "status", "external_id", "reason"]
                .iter()
                .all(|k| old[*k] == part[*k])
            {
                return Ok(prior.clone());
            }
            if old["status"] != "claimed" || outcome.status == "claimed" {
                return Err("an established part outcome cannot be relabeled".into());
            }
        } else if index != parts.len()
            || outcome.status != "claimed"
            || parts.iter().any(|p| p["status"] != "accepted")
        {
            return Err(
                "a new part follows accepted predecessors and begins with a durable claim".into(),
            );
        }
        if prior["status"] != "uncertain" {
            return Err("completed dispatch cannot acquire new part outcomes".into());
        }
        part["at"] = json!(jobs::now());
        if index == parts.len() {
            parts.push(part);
        } else {
            parts[index] = part;
        }
        let mut receipt = prior.clone();
        receipt["parts"] = json!(parts);
        receipt["parts_total"] = json!(total);
        Self::append(
            &mut inner,
            json!({"kind":"connector_receipt","id":id,"receipt":receipt}),
        )?;
        Ok(receipt)
    }
    pub fn cursor(&self, key: &str) -> Option<Value> {
        self.inner.lock().unwrap().cursors.get(key).cloned()
    }
    pub fn checkpoint(&self, key: &str, cursor: &str, digest: &str) -> Result<()> {
        self.check()?;
        let mut inner = self.inner.lock().unwrap();
        if inner
            .cursors
            .get(key)
            .is_some_and(|c| c["cursor"] == cursor && c["digest"] == digest)
        {
            return Ok(());
        }
        Self::append(
            &mut inner,
            json!({"kind":"connector_cursor","key":key,"checkpoint":{"cursor":cursor,"digest":digest}}),
        )
    }
    pub fn messages(&self, owner: &str, address: &Address, after: u64) -> Vec<Message> {
        let inner = self.inner.lock().unwrap();
        let mut bytes = 0;
        inner
            .messages
            .iter()
            .filter(|m| {
                m.owner == owner
                    && &m.to == address
                    && m.seq > after
                    && m.network
                        .as_ref()
                        .is_none_or(|n| n["destination_machine"] == self.machine)
            })
            .take(128)
            .take_while(|m| {
                bytes += m.body.len()
                    + m.subject.len()
                    + m.origin.as_ref().map(|v| v.to_string().len()).unwrap_or(0)
                    + 1024;
                bytes <= 192 * 1024
            })
            .cloned()
            .collect()
    }
    pub fn has_pending(&self, owner: &str, resident: &str, presented: &HashSet<String>) -> bool {
        let target = Address::Resident {
            id: resident.into(),
        };
        self.inner
            .lock()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.owner == owner && m.to == target && !presented.contains(&m.id))
    }
    pub fn pending(
        &self,
        owner: &str,
        resident: &str,
        presented: &HashSet<String>,
    ) -> Vec<Message> {
        let inner = self.inner.lock().unwrap();
        let mut bytes = 0;
        inner
            .messages
            .iter()
            .filter(|m| {
                m.owner == owner
                    && m.to
                        == Address::Resident {
                            id: resident.into(),
                        }
                    && !presented.contains(&m.id)
            })
            .take(32)
            .take_while(|m| {
                bytes += m.body.len()
                    + m.subject.len()
                    + m.origin.as_ref().map(|v| v.to_string().len()).unwrap_or(0)
                    + 1024;
                bytes <= 128 * 1024
            })
            .cloned()
            .collect()
    }
}
