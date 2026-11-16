//! Outbound-only runner for a single Boxen guest correspondence binding.
use crate::{Result, guest_chat, messaging::Instance, state};
use boxen_guest_transport::{Connection, Message, lock_channel};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
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
    save_checkpoint(&cursor_path, &checkpoint)?;
    event("runner_started", &instance, None);
    let mut previous_error = None;
    let mut backoff = 1;
    loop {
        let outbound = publish_replies(&mut cloud, &control, &mut checkpoint, &cursor_path);
        let inbound = receive_messages(&mut cloud, &control, &instance);
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
    messages: Vec<Mail>,
    next_after: u64,
}

#[derive(Deserialize)]
struct Mail {
    id: String,
    sender: String,
    body: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
struct Attachment {
    name: String,
}

fn publish_replies(
    cloud: &mut Connection,
    control: &guest_chat::Control,
    checkpoint: &mut Checkpoint,
    path: &Path,
) -> Result<()> {
    let instance = &checkpoint.instance;
    let page: Page = serde_json::from_value(control.call(
        Some(instance),
        json!({"action":"guest_chat_read","instance":instance,"after":checkpoint.after}),
    )?)?;
    if page.next_after < checkpoint.after {
        return Err("Boxen returned a regressed guest cursor".into());
    }
    for mail in page
        .messages
        .into_iter()
        .filter(|mail| mail.sender == "resident")
    {
        let packet = Message::new(
            mail.id,
            mail.body,
            mail.attachments.into_iter().map(|file| file.name).collect(),
        );
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
) -> Result<()> {
    for delivery in cloud.pull()? {
        match delivery.decode().and_then(|packet| {
            packet.validate_guest_submission()?;
            Ok(packet)
        }) {
            Ok(packet) => {
                let accepted = control.call(Some(instance), json!({
                    "action":"guest_chat_send","instance":instance,"id":packet.id,"body":packet.body
                })).map_err(|_| "Boxen could not accept guest text")?;
                if accepted["accepted"] != true {
                    return Err("Boxen did not confirm guest mailbox acceptance".into());
                }
            }
            Err(_) => event("invalid_delivery_discarded", instance, None),
        }
        cloud.ack(&[delivery.ack_id])?;
    }
    Ok(())
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
