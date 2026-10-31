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
    Resident { id: String },
    Client { machine: String, id: String },
}
impl Address {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Resident { id } => jobs::valid_id(id),
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
    pub id: String,
    pub to: Address,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<Address>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
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
    }
}
struct Inner {
    file: Option<File>,
    seq: u64,
    messages: Vec<Message>,
    ids: HashMap<String, usize>,
    sources: HashMap<String, String>,
    residents: HashSet<String>,
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
        json!({"available":self.issue.is_none(),"issue":self.issue,"last_seq":if self.issue.is_none(){Some(inner.seq)}else{None},"message_count":if self.issue.is_none(){Some(inner.messages.len())}else{None}})
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
                inner.ids.insert(message.id.clone(), inner.messages.len());
                inner.messages.push(message);
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
        if spec.subject.trim().is_empty() || spec.subject.len() > 256 {
            return Err(
                "message acceptance requires a nonblank subject of at most 256 bytes".into(),
            );
        }
        if spec.body.is_empty() || spec.body.len() > 32 * 1024 {
            return Err("message body acceptance range is 1–32768 bytes".into());
        }
        let mut inner = self.inner.lock().unwrap();
        let prior = inner
            .sources
            .get(&source)
            .or_else(|| inner.ids.get(&spec.id).map(|i| &inner.messages[*i].id));
        if let Some(id) = prior {
            let prior = &inner.messages[inner.ids[id]];
            if prior.matches(owner, &sender, &spec) {
                return Ok(prior.clone());
            }
            return Err(
                "message id or source already accepted with different input or sender".into(),
            );
        }
        let reply_to = spec.reply_to.clone().unwrap_or_else(|| sender.clone());
        let message = Message {
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
        };
        Self::append(&mut inner, json!({"kind":"message","message":message}))?;
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
    pub fn messages(&self, owner: &str, address: &Address, after: u64) -> Vec<Message> {
        let inner = self.inner.lock().unwrap();
        let mut bytes = 0;
        inner
            .messages
            .iter()
            .filter(|m| m.owner == owner && &m.to == address && m.seq > after)
            .take(128)
            .take_while(|m| {
                bytes += m.body.len() + m.subject.len() + 1024;
                bytes <= 192 * 1024
            })
            .cloned()
            .collect()
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
                bytes += m.body.len() + m.subject.len() + 1024;
                bytes <= 128 * 1024
            })
            .cloned()
            .collect()
    }
}
