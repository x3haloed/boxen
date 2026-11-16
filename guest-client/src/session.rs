use boxen_guest_transport::{
    Attachment, BlobStore, Connection, Control, Message, Packet, PendingMessages, Presence, Result,
    fresh_presence, hash_id, lock_channel, new_id, now_seconds,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const PENDING_LIMIT: usize = 256;

pub struct Session {
    state: Arc<Mutex<Store>>,
    files: Arc<Mutex<BlobStore>>,
}

#[derive(Clone)]
pub struct Entry {
    pub sent: bool,
    pub message: Message,
}

pub struct View {
    pub revision: u64,
    pub status: &'static str,
    pub pending: usize,
    pub history: Arc<Vec<Entry>>,
    pub resident: Option<String>,
    pub presence: Presence,
}

impl Session {
    pub fn open(config: &Path, data: &Path) -> Result<Self> {
        let (session, sync) = Self::prepare(Connection::open(config)?, data, false)?;
        // The macOS GUI backend takes over the original main thread. Start the
        // network thread before handing control to AppKit.
        thread::Builder::new()
            .name("guest-pubsub".into())
            .spawn(sync)?;
        Ok(session)
    }

    pub fn prepare(
        connection: Connection,
        data: &Path,
        verified: bool,
    ) -> Result<(Self, impl FnOnce() + Send + use<>)> {
        let channel = data.join(connection.scope_id());
        let mut opened = Store::open(&channel)?;
        if verified {
            opened.set_status("Connected");
        }
        let state = Arc::new(Mutex::new(opened));
        let files = Arc::new(Mutex::new(BlobStore::new(channel.join("files"))?));
        let worker_files = files.clone();
        let weak = Arc::downgrade(&state);
        Ok((Self { state, files }, move || {
            let mut connection = connection;
            let mut permission_check = verified.then(Instant::now);
            let mut last_probe: Option<Instant> = None;
            let mut pending = PendingMessages::default();
            let mut retry = Duration::from_secs(1);
            while let Some(state) = weak.upgrade() {
                let result = synchronize(
                    &mut connection,
                    &state,
                    &worker_files,
                    &mut permission_check,
                    &mut last_probe,
                    &mut pending,
                );
                state.lock().unwrap().set_status(if result.is_ok() {
                    "Connected"
                } else {
                    "Sync paused; retrying"
                });
                drop(state);
                if result.is_ok() {
                    retry = Duration::from_secs(1);
                    thread::sleep(Duration::from_millis(250));
                } else {
                    permission_check = None;
                    thread::sleep(retry);
                    retry = (retry * 2).min(Duration::from_secs(15));
                }
            }
        }))
    }

    pub fn send(
        &self,
        body: &str,
        attachments: impl IntoIterator<Item = (String, impl AsRef<[u8]>)>,
    ) -> Result<()> {
        let mut packet = Message::new(new_id()?, body.to_owned(), Vec::new());
        {
            let files = self.files.lock().unwrap();
            for (name, bytes) in attachments {
                packet.files.push(files.retain(name, bytes.as_ref())?);
            }
        }
        packet.validate_guest_submission()?;
        let mut state = self.state.lock().unwrap();
        if state.pending.len() >= PENDING_LIMIT {
            return Err("The outgoing queue is full".into());
        }
        state.append(Record::Queued(packet))?;
        Ok(())
    }

    pub fn read_file(&self, file: &Attachment) -> Result<Vec<u8>> {
        self.files.lock().unwrap().read(file)
    }

    pub fn file_prefix(&self, file: &Attachment) -> Result<Vec<u8>> {
        self.files.lock().unwrap().prefix(file)
    }

    pub fn view_since(&self, revision: u64) -> Option<View> {
        let mut state = self.state.lock().unwrap();
        if state.presence != Presence::Offline && !fresh_presence(state.presence_at) {
            state.presence = Presence::Offline;
            state.revision += 1;
        }
        (revision != state.revision).then(|| View {
            revision: state.revision,
            status: state.status,
            pending: state.pending.len(),
            history: state.history.clone(),
            resident: state.resident.clone(),
            presence: state.presence,
        })
    }
}

fn synchronize(
    connection: &mut Connection,
    state: &Mutex<Store>,
    files: &Mutex<BlobStore>,
    checked: &mut Option<Instant>,
    last_probe: &mut Option<Instant>,
    pending: &mut PendingMessages,
) -> Result<()> {
    if checked.is_none_or(|time| time.elapsed() >= Duration::from_secs(60)) {
        connection.check_permissions()?;
        *checked = Some(Instant::now());
    }
    let outgoing = state.lock().unwrap().pending.front().cloned();
    if let Some(packet) = outgoing {
        for file in &packet.files {
            let bytes = files.lock().unwrap().read(file)?;
            connection.publish_file(file, &bytes)?;
        }
        connection.publish(&packet)?;
        state
            .lock()
            .unwrap()
            .append(Record::Published { id: packet.id })?;
    }
    let probe_delay = if state.lock().unwrap().presence == Presence::Offline {
        30
    } else {
        12
    };
    if last_probe.is_none_or(|time| time.elapsed() >= Duration::from_secs(probe_delay)) {
        connection.control(&Control::PresenceProbe {
            sent_at: now_seconds(),
        })?;
        *last_probe = Some(Instant::now());
    }
    let mut acknowledged = Vec::new();
    for delivery in connection.pull()? {
        match delivery.decode() {
            Ok(Packet::Message(message)) => {
                pending.insert(delivery.ack_id, message)?;
                continue;
            }
            Ok(Packet::Control(Control::File(chunk))) => match chunk.bytes() {
                Ok(bytes) => files.lock().unwrap().insert(&chunk, &bytes)?,
                Err(_) => eprintln!("Guest Pub/Sub discarded an invalid attachment chunk."),
            },
            Ok(Packet::Control(Control::Presence {
                sent_at,
                name,
                state: presence,
            })) => {
                let mut store = state.lock().unwrap();
                if fresh_presence(sent_at)
                    && sent_at >= store.presence_at
                    && !name.is_empty()
                    && name.len() <= 1024
                {
                    store.presence_at = sent_at;
                    if store.presence != presence || store.resident.as_ref() != Some(&name) {
                        store.presence = presence;
                        store.resident = Some(name);
                        store.revision += 1;
                    }
                }
            }
            _ => eprintln!("Guest Pub/Sub discarded an invalid message envelope."),
        }
        acknowledged.push(delivery.ack_id);
    }
    connection.ack(&acknowledged)?;
    pending.deliver(connection, |message| {
        {
            let store = files.lock().unwrap();
            for file in &message.files {
                if store.complete(file)?.is_none() {
                    return Ok(false);
                }
            }
        }
        state.lock().unwrap().receive(message.clone())?;
        Ok(true)
    })
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "event", content = "value", rename_all = "snake_case")]
enum Record {
    Queued(Message),
    Published { id: String },
    Received(Message),
}

struct Store {
    journal: File,
    _lock: File,
    pending: VecDeque<Message>,
    received: HashMap<String, String>,
    history: Arc<Vec<Entry>>,
    resident: Option<String>,
    presence: Presence,
    presence_at: u64,
    revision: u64,
    status: &'static str,
    healthy: bool,
}

impl Store {
    fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = lock_channel(&path.join("session.lock"))
            .map_err(|_| "could not lock guest history for exclusive use")?;
        let journal = options.open(path.join("messages.jsonl"))?;
        #[cfg(all(unix, not(cosmo)))]
        File::open(path)?.sync_all()?;
        let mut reader = BufReader::new(journal.try_clone()?);
        let mut store = Self {
            journal,
            _lock: lock,
            pending: VecDeque::new(),
            received: HashMap::new(),
            history: Arc::default(),
            resident: None,
            presence: Presence::Offline,
            presence_at: 0,
            revision: 1,
            status: "Connecting",
            healthy: true,
        };
        let mut end = 0;
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 {
            if line.last() != Some(&b'\n') {
                // No receipt can have been sent for an incomplete record.
                store.journal.set_len(end)?;
                store.journal.sync_all()?;
                break;
            }
            store.apply(serde_json::from_slice(&line)?)?;
            end += line.len() as u64;
            line.clear();
        }
        store.journal.seek(SeekFrom::Start(end))?;
        Ok(store)
    }

    fn receive(&mut self, message: Message) -> Result<()> {
        let digest = hash_id(&serde_json::to_vec(&message)?);
        if let Some(prior) = self.received.get(&message.id) {
            if prior != &digest {
                return Err("conflicting guest reply under an existing message ID".into());
            }
            return Ok(());
        }
        self.append(Record::Received(message))
    }

    fn append(&mut self, event: Record) -> Result<()> {
        if !self.healthy {
            return Err("guest history is unavailable until restart".into());
        }
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        let offset = self.journal.stream_position()?;
        if let Err(error) = self
            .journal
            .write_all(&line)
            .and_then(|()| self.journal.sync_all())
        {
            self.healthy = self
                .journal
                .set_len(offset)
                .and_then(|()| self.journal.seek(SeekFrom::Start(offset)))
                .and_then(|_| self.journal.sync_all())
                .is_ok();
            return Err(error.into());
        }
        self.apply(event)?;
        self.revision += 1;
        Ok(())
    }

    fn apply(&mut self, event: Record) -> Result<()> {
        match event {
            Record::Queued(message) => {
                Arc::make_mut(&mut self.history).push(Entry {
                    sent: true,
                    message: message.clone(),
                });
                self.pending.push_back(message);
            }
            Record::Published { id } => {
                if self.pending.front().is_none_or(|packet| packet.id != id) {
                    return Err("guest publication record has no matching queued message".into());
                }
                self.pending.pop_front();
            }
            Record::Received(message) => {
                let digest = hash_id(&serde_json::to_vec(&message)?);
                self.received.insert(message.id.clone(), digest);
                Arc::make_mut(&mut self.history).push(Entry {
                    sent: false,
                    message,
                });
            }
        }
        Ok(())
    }

    fn set_status(&mut self, status: &'static str) {
        if status != self.status {
            self.status = status;
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_recovers_queued_messages_and_deduplicates_replies() -> Result<()> {
        let path = std::env::temp_dir().join(format!("boxen-guest-test-{}", new_id()?));
        let outgoing = Message::new("a".repeat(64), "Queued while offline".into(), Vec::new());
        let incoming = Message::new(
            "b".repeat(64),
            "Saved before acknowledgment".into(),
            Vec::new(),
        );
        {
            let mut store = Store::open(&path)?;
            assert!(Store::open(&path).is_err());
            store.append(Record::Queued(outgoing.clone()))?;
            store.receive(incoming.clone())?;
        }
        // An interrupted append must not prevent recovery of earlier receipts.
        OpenOptions::new()
            .append(true)
            .open(path.join("messages.jsonl"))?
            .write_all(b"{partial")?;
        {
            let mut store = Store::open(&path)?;
            assert_eq!(store.pending.front(), Some(&outgoing));
            store.receive(incoming.clone())?;
            assert_eq!(store.history.len(), 2);
            let conflict = Message::new(incoming.id, "Different reply".into(), Vec::new());
            assert!(store.receive(conflict).is_err());
            store.append(Record::Published { id: outgoing.id })?;
        }
        {
            let store = Store::open(&path)?;
            assert!(store.pending.is_empty());
            assert_eq!(store.history.len(), 2);
        }
        fs::remove_dir_all(&path)?;
        Ok(())
    }
}
