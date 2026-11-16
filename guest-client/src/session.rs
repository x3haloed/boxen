use boxen_guest_transport::{Connection, Message, Result, hash_id, lock_channel, new_id};
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

const HISTORY_ROWS: usize = 64;
const PENDING_LIMIT: usize = 256;

pub struct Session {
    state: Arc<Mutex<Store>>,
}

pub struct View {
    pub revision: u64,
    pub status: &'static str,
    pub pending: usize,
    pub history: VecDeque<(bool, String)>,
}

impl Session {
    pub fn open(config: &Path, data: &Path) -> Result<Self> {
        let connection = Connection::open(config)?;
        let state = Arc::new(Mutex::new(Store::open(&data.join(connection.scope_id()))?));
        // The macOS GUI backend takes over the original main thread. Start the
        // network thread before handing control to AppKit.
        let weak = Arc::downgrade(&state);
        thread::Builder::new()
            .name("guest-pubsub".into())
            .spawn(move || {
                let mut connection = connection;
                let mut permission_check = None;
                let mut retry = Duration::from_secs(1);
                while let Some(state) = weak.upgrade() {
                    let result = synchronize(&mut connection, &state, &mut permission_check);
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
            })?;
        Ok(Self { state })
    }

    pub fn send(&self, body: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.pending.len() >= PENDING_LIMIT {
            return Err("The outgoing queue is full".into());
        }
        let packet = Message::new(new_id()?, body.to_owned(), Vec::new());
        packet.validate_guest_submission()?;
        state.append(Record::Queued(packet))?;
        Ok(())
    }

    pub fn view_since(&self, revision: u64) -> Option<View> {
        let state = self.state.lock().unwrap();
        (revision != state.revision).then(|| View {
            revision: state.revision,
            status: state.status,
            pending: state.pending.len(),
            history: state.history.clone(),
        })
    }
}

fn synchronize(
    connection: &mut Connection,
    state: &Mutex<Store>,
    checked: &mut Option<Instant>,
) -> Result<()> {
    if checked.is_none_or(|time| time.elapsed() >= Duration::from_secs(60)) {
        connection.check_permissions()?;
        *checked = Some(Instant::now());
    }
    let outgoing = state.lock().unwrap().pending.front().cloned();
    if let Some(packet) = outgoing {
        connection.publish(&packet)?;
        state
            .lock()
            .unwrap()
            .append(Record::Published { id: packet.id })?;
    }
    for delivery in connection.pull()? {
        match delivery.decode() {
            Ok(message) => state.lock().unwrap().receive(message)?,
            Err(_) => eprintln!("Guest Pub/Sub discarded an invalid message envelope."),
        }
        connection.ack(&[delivery.ack_id])?;
    }
    Ok(())
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
    history: VecDeque<(bool, String)>,
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
            history: VecDeque::new(),
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
                self.add_history(true, message.body.clone());
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
                self.received.insert(message.id, digest);
                let mut body = message.body;
                for name in message.attachment_names {
                    body.push_str(&format!("\nAttachment unavailable: {name}"));
                }
                self.add_history(false, body);
            }
        }
        Ok(())
    }

    fn add_history(&mut self, sent: bool, body: String) {
        self.history.push_back((sent, body));
        if self.history.len() > HISTORY_ROWS {
            self.history.pop_front();
        }
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
