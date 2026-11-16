//! Outbound-only runner for a single Boxen guest correspondence binding.
use crate::{Result, guest_chat, messaging::Instance, state};
use boxen_guest_transport::{
    Attachment, BlobStore, Connection, Control, Message, Packet, PendingMessages, Presence,
    fresh_presence, lock_channel, now_seconds,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub fn run(home: &Path, mut args: Vec<String>) -> Result<()> {
    let config = crate::take_option(&mut args, "--config")?
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("guest-pubsub.json"));
    let mut cloud = Connection::open(&config)?;
    if args.as_slice() == ["--check"] {
        cloud.check_permissions()?;
        println!("Guest Pub/Sub authentication and permissions verified");
        return Ok(());
    }
    let instance = crate::take_option(&mut args, "--instance")?
        .ok_or("--instance is a configured guest chat connector ID")?;
    crate::jobs::valid_id(&instance)?;
    let control = guest_chat::Control::open(home, &mut args)?;
    if !args.is_empty() {
        return Err("unexpected guest-pubsub arguments".into());
    }
    let configured: Instance = serde_json::from_value(
        control.call(Some(&instance), json!({"action":"get","instance":instance}))?,
    )?;
    guest_chat::binding(&configured.definition)?;

    let directory = home.join("guest-pubsub").join(cloud.scope_id());
    state::private_dir(&directory)?;
    let _lock = lock_channel(&directory.join("runner.lock"))
        .map_err(|_| "could not lock the guest Pub/Sub runner for exclusive use")?;
    let cursor_path = directory.join("cursor.json");
    let mut checkpoint = match fs::read(&cursor_path) {
        Ok(bytes) => serde_json::from_slice::<Checkpoint>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Checkpoint {
            instance: instance.clone(),
            after: 0,
        },
        Err(error) => return Err(error.into()),
    };
    if checkpoint.instance != instance {
        return Err("guest Pub/Sub history belongs to a different Boxen connector".into());
    }
    let files = Files {
        incoming: BlobStore::new(directory.join("incoming"))?,
        originals: crate::attachments::Store::new(home.canonicalize()?.join("attachments")),
    };
    save_checkpoint(&cursor_path, &checkpoint)?;
    event("runner_started", &instance, None);
    let mut previous_error = None;
    let mut backoff = 1;
    let mut pending = PendingMessages::default();
    let mut presence = PresenceWatch::default();
    loop {
        let outbound = publish_replies(
            &mut cloud,
            &control,
            &mut checkpoint,
            &cursor_path,
            &files,
            &mut presence,
        );
        let inbound = receive_messages(
            &mut cloud,
            &control,
            &instance,
            &files,
            &mut pending,
            &mut presence,
        );
        let issue = outbound
            .err()
            .or_else(|| inbound.err())
            .map(|error| error.to_string());
        if issue != previous_error {
            event(
                if issue.is_some() {
                    "runner_retrying"
                } else {
                    "runner_recovered"
                },
                &instance,
                issue.as_deref(),
            );
            previous_error = issue;
        }
        if previous_error.is_some() {
            thread::sleep(Duration::from_secs(backoff));
            backoff = (backoff * 2).min(15);
        } else {
            backoff = 1;
            thread::sleep(Duration::from_millis(250));
        }
    }
}

#[derive(serde::Serialize, Deserialize)]
struct Checkpoint {
    instance: String,
    after: u64,
}

fn save_checkpoint(path: &Path, checkpoint: &Checkpoint) -> Result<()> {
    let temp = path.with_extension("new");
    // This runner holds the channel lock, so any previous temporary file is
    // the incomplete result of an interrupted checkpoint.
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    state::write_new(&temp, &serde_json::to_vec(checkpoint)?)?;
    fs::rename(temp, path)?;
    #[cfg(all(unix, not(cosmo)))]
    File::open(
        path.parent()
            .ok_or("guest checkpoint directory unavailable")?,
    )?
    .sync_all()?;
    Ok(())
}

#[cfg(all(unix, not(cosmo)))]
use std::fs::File;

#[derive(Deserialize)]
struct Page {
    resident: String,
    status: Presence,
    messages: Vec<Mail>,
    next_after: u64,
}

#[derive(Deserialize)]
struct Mail {
    id: String,
    sender: String,
    body: String,
    #[serde(default)]
    attachments: Vec<AttachmentSummary>,
}

#[derive(Deserialize)]
struct AttachmentSummary {
    size: u64,
}

struct Files {
    incoming: BlobStore,
    originals: crate::attachments::Store,
}

#[derive(Default)]
struct PresenceWatch {
    requested: Option<Instant>,
    published: Option<(Instant, String, Presence)>,
}

impl PresenceWatch {
    fn publish(&mut self, cloud: &mut Connection, page: &Page) -> Result<()> {
        if self
            .requested
            .is_some_and(|last| last.elapsed() < Duration::from_secs(75))
            && self.published.as_ref().is_none_or(|(last, name, state)| {
                name != &page.resident
                    || *state != page.status
                    || last.elapsed() >= Duration::from_secs(12)
            })
        {
            cloud.control(&Control::Presence {
                sent_at: now_seconds(),
                name: page.resident.clone(),
                state: page.status,
            })?;
            self.published = Some((Instant::now(), page.resident.clone(), page.status));
        }
        Ok(())
    }
}

fn publish_replies(
    cloud: &mut Connection,
    control: &guest_chat::Control,
    checkpoint: &mut Checkpoint,
    path: &Path,
    files: &Files,
    presence: &mut PresenceWatch,
) -> Result<()> {
    let instance = &checkpoint.instance;
    let page: Page = serde_json::from_value(control.call(
        Some(instance),
        json!({"action":"guest_chat_read","instance":instance,"after":checkpoint.after}),
    )?)?;
    if page.next_after < checkpoint.after {
        return Err("Boxen returned a regressed guest cursor".into());
    }
    presence.publish(cloud, &page)?;
    for mail in page
        .messages
        .into_iter()
        .filter(|mail| mail.sender == "resident")
    {
        let mut packet = Message::new(mail.id, mail.body, Vec::new());
        for (index, summary) in mail.attachments.iter().enumerate() {
            let value = control.call(Some(instance), json!({
                "action":"guest_chat_attachment","instance":instance,"message_id":packet.id,"index":index
            }))?;
            let file: Attachment = serde_json::from_value(value.clone())?;
            if file.size != summary.size {
                return Err("guest attachment changed after the mailbox read".into());
            }
            let original: crate::attachments::Attachment = serde_json::from_value(value)?;
            let path = files.originals.verify(&original)?;
            cloud.publish_file(&file, &fs::read(path)?)?;
            packet.files.push(file);
        }
        cloud.publish(&packet)?;
    }
    if page.next_after != checkpoint.after {
        let next = Checkpoint {
            instance: instance.clone(),
            after: page.next_after,
        };
        save_checkpoint(path, &next)?;
        *checkpoint = next;
    }
    Ok(())
}

fn receive_messages(
    cloud: &mut Connection,
    control: &guest_chat::Control,
    instance: &str,
    files: &Files,
    pending: &mut PendingMessages,
    presence: &mut PresenceWatch,
) -> Result<()> {
    let mut acknowledged = Vec::new();
    for delivery in cloud.pull()? {
        match delivery.decode() {
            Ok(Packet::Message(packet)) if packet.validate_guest_submission().is_ok() => {
                pending.insert(delivery.ack_id, packet)?;
                continue;
            }
            Ok(Packet::Control(Control::File(chunk))) => match chunk.bytes() {
                Ok(bytes) => files.incoming.insert(&chunk, &bytes)?,
                Err(_) => event("invalid_delivery_discarded", instance, None),
            },
            Ok(Packet::Control(Control::PresenceProbe { sent_at })) => {
                if fresh_presence(sent_at) {
                    presence.requested = Some(Instant::now());
                }
            }
            _ => event("invalid_delivery_discarded", instance, None),
        }
        acknowledged.push(delivery.ack_id);
    }
    cloud.ack(&acknowledged)?;
    pending.deliver(cloud, |packet| accept(control, instance, files, packet))
}

fn accept(
    control: &guest_chat::Control,
    instance: &str,
    files: &Files,
    packet: &Message,
) -> Result<bool> {
    for file in &packet.files {
        if files.incoming.complete(file)?.is_none() {
            return Ok(false);
        }
    }
    let mut attachments = Vec::new();
    for file in &packet.files {
        let bytes = files.incoming.read(file)?;
        attachments.push(crate::attachments::capture(
            &file.name,
            &bytes,
            |transfer| {
                control.call(
                    Some(instance),
                    json!({
                        "action":"guest_chat_upload","instance":instance,"transfer":transfer
                    }),
                )
            },
        )?);
    }
    let accepted = control
        .call(
            Some(instance),
            json!({
                "action":"guest_chat_send","instance":instance,"id":packet.id,"body":packet.body,
                "attachments":attachments
            }),
        )
        .map_err(|_| "Boxen could not accept guest message")?;
    if accepted["accepted"] != true {
        return Err("Boxen did not confirm guest mailbox acceptance".into());
    }
    Ok(true)
}

// The runner is a separate CLI process. Its supervisor owns retention of these
// bounded lifecycle diagnostics. Daemon mail and resident events still use the
// daemon's configurable diagnostic sinks.
fn event(event: &str, instance: &str, error: Option<&str>) {
    eprintln!(
        "{}",
        json!({"event":event,"instance":instance,"error":error})
    );
}
