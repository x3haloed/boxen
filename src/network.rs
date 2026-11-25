//! Bounded membership anti-entropy. Noise authenticates the member introducing updates.
//! Workload journals remain host-local; this store carries no conversation history.
use crate::{
    Result,
    logging::{self, Context, Level, Target},
    state::{self, Peer, State},
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
const MAX_MEMBERS: usize = 128;
fn lock(state: &State) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.directory().join("network.lock"))?;
    f.lock_exclusive()?;
    Ok(f)
}
pub fn member(state: &State, key: &str) -> Result<Peer> {
    state::key(key)?;
    let path = state.directory().join("peers").join(format!("{key}.json"));
    if fs::metadata(&path)?.len() > 2048 {
        return Err("network member record exceeds its bound".into());
    }
    let peer: Peer = serde_json::from_slice(&fs::read(path)?)?;
    validate(&peer)?;
    if peer.key != key {
        return Err("network member filename differs from identity".into());
    }
    Ok(peer)
}
pub fn members(state: &State) -> Result<Vec<Peer>> {
    let mut out = Vec::new();
    let mut count = 0;
    for file in fs::read_dir(state.directory().join("peers"))? {
        let path = file?.path();
        if path.extension().is_some_and(|v| v == "json") {
            count += 1;
            if count > MAX_MEMBERS {
                return Err("network membership exceeds its 128-record bound".into());
            }
            // An invalid peer is unavailable; its original file stays untouched.
            // It cannot deny unrelated identities or become implicit admission.
            if let Some(key) = path.file_stem().and_then(|s| s.to_str())
                && let Ok(p) = member(state, key)
            {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}
pub fn issues(state: &State) -> Result<Vec<Value>> {
    let mut issues = Vec::new();
    for entry in fs::read_dir(state.directory().join("peers"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|v| v == "json")
            && let Some(key) = path.file_stem().and_then(|s| s.to_str())
            && let Err(e) = member(state, key)
        {
            issues.push(json!({"member":key,"error":e.to_string(),"state":"unavailable","original_preserved":true}));
        }
        if issues.len() >= MAX_MEMBERS {
            break;
        }
    }
    Ok(issues)
}
fn validate(p: &Peer) -> Result<()> {
    state::key(&p.key)?;
    if p.label.is_empty()
        || p.label.len() > 100
        || p.address.len() > 256
        || p.address
            .chars()
            .any(|c| matches!(c, '/' | '\\' | '@' | '?' | '#'))
        || (!p.address.is_empty()
            && (p
                .address
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                || p.address.rsplit_once(':').is_none_or(|(host, port)| {
                    host.is_empty() || !port.parse::<u16>().is_ok_and(|p| p > 0)
                })))
    {
        return Err("network member label/address is invalid".into());
    }
    Ok(())
}
fn write(state: &State, p: &Peer) -> Result<()> {
    validate(p)?;
    let path = state
        .directory()
        .join("peers")
        .join(format!("{}.json", p.key));
    if path.exists() {
        member(state, &p.key)?;
    } else if fs::read_dir(state.directory().join("peers"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|v| v == "json"))
        .count()
        >= MAX_MEMBERS
    {
        return Err("network membership storage is full".into());
    }
    let tmp = PathBuf::from(format!(
        "{}.pending-{}",
        path.display(),
        state::random_hex()?
    ));
    state::write_new(&tmp, &serde_json::to_vec(p)?)?;
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(tmp);
        return Err(e.into());
    }
    logging::record(
        Level::Info,
        Target::Peer,
        "network.member_changed",
        Context::peer(&p.key),
        || json!({"generation":p.generation,"removed":p.removed,"address":p.address}),
        || Value::Null,
    );
    Ok(())
}
pub fn admit(state: &State, key: &str, label: &str) -> Result<()> {
    state::key(key)?;
    if key == state.public_key() {
        return Err("cannot pair this machine with itself".into());
    }
    let _guard = lock(state)?;
    let all = members(state)?;
    // Explicit local pairing can re-enable a removed installation for joining.
    // Keeping the old generation cannot override a network removal; another
    // current member still has to explicitly readmit this identity.
    if let Some(own) = all
        .iter()
        .find(|p| p.key == state.public_key() && p.removed)
    {
        let mut joining = own.clone();
        joining.removed = false;
        write(state, &joining)?;
    }
    let old = all.iter().find(|p| p.key == key);
    if old.is_some_and(|p| !p.removed) {
        return Ok(());
    }
    if old.is_none() && all.len() >= MAX_MEMBERS {
        return Err("network membership is full".into());
    }
    write(
        state,
        &Peer {
            key: key.into(),
            label: label.into(),
            address: old.map(|p| p.address.clone()).unwrap_or_default(),
            generation: all
                .iter()
                .map(|p| p.generation)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("membership generation exhausted")?,
            removed: false,
            endpoint_revision: 0,
        },
    )
}
pub fn remove(state: &State, key: &str) -> Result<()> {
    state::key(key)?;
    let _guard = lock(state)?;
    let all = members(state)?;
    let mut p = all
        .iter()
        .find(|p| p.key == key)
        .ok_or("network member not found")?
        .clone();
    if p.removed {
        return Ok(());
    }
    p.generation = all
        .iter()
        .map(|p| p.generation)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("membership generation exhausted")?;
    p.removed = true;
    write(state, &p)
}
pub fn configure(state: &State, label: Option<&str>, address: &str) -> Result<()> {
    let _guard = lock(state)?;
    let all = members(state)?;
    let old = all.iter().find(|p| p.key == state.public_key());
    if old.is_some_and(|p| p.removed) {
        return Ok(());
    }
    let default_label = format!("Machine {}", &state.public_key()[..8]);
    let label = label
        .or_else(|| old.map(|peer| peer.label.as_str()))
        .unwrap_or(&default_label);
    let mut p = old.cloned().unwrap_or(Peer {
        key: state.public_key().into(),
        label: label.into(),
        address: address.into(),
        generation: 0,
        removed: false,
        endpoint_revision: 0,
    });
    if old.is_none() || (!address.is_empty() && p.address != address) || p.label != label {
        p.label = label.into();
        if !address.is_empty() {
            p.address = address.into();
        }
        p.endpoint_revision = next_endpoint_revision(&all)?;
        write(state, &p)?;
    }
    Ok(())
}
pub fn remember(state: &State, key: &str, address: &str) -> Result<()> {
    let _guard = lock(state)?;
    let all = members(state)?;
    if let Some(old) = all.iter().find(|p| p.key == key && !p.removed)
        && old.address.is_empty()
    {
        let mut p = old.clone();
        p.address = address.into();
        write(state, &p)?;
    }
    Ok(())
}
pub fn merge(state: &State, remote: &str, input: Value) -> Result<Value> {
    state.require_trust(remote)?;
    let incoming: Vec<Peer> = serde_json::from_value(input)?;
    if incoming.len() > MAX_MEMBERS {
        return Err("network snapshot exceeds its bound".into());
    }
    let mut seen = std::collections::HashSet::new();
    for p in &incoming {
        validate(p)?;
        if !seen.insert(&p.key) {
            return Err("network snapshot repeats a member".into());
        }
    }
    let _guard = lock(state)?;
    state.require_trust(remote)?;
    let mut all = members(state)?;
    let additional = incoming
        .iter()
        .filter(|p| !all.iter().any(|o| o.key == p.key))
        .count();
    if all.len() + additional > MAX_MEMBERS {
        return Err("merged network exceeds membership bound".into());
    }
    for p in incoming {
        let old = all.iter().position(|o| o.key == p.key);
        let replace = old.is_none_or(|i| {
            (
                p.generation,
                p.removed,
                p.endpoint_revision,
                &p.label,
                &p.address,
            ) > (
                all[i].generation,
                all[i].removed,
                all[i].endpoint_revision,
                &all[i].label,
                &all[i].address,
            )
        });
        if replace {
            write(state, &p)?;
            if let Some(i) = old {
                all[i] = p;
            } else {
                all.push(p);
            }
        }
    }
    Ok(json!(members(state)?))
}
const MAIL_WORKERS: usize = 4;

fn start_delivery(state: &Arc<State>, jobs: &Arc<crate::jobs::Jobs>) {
    for worker in 0..MAIL_WORKERS {
        let state = state.clone();
        let jobs = jobs.clone();
        std::thread::spawn(move || {
            logging::record(
                Level::Debug,
                Target::Peer,
                "network.outbox_worker_started",
                Context::default(),
                || json!({"worker":worker,"workers":MAIL_WORKERS}),
                || Value::Null,
            );
            loop {
                let delivery = jobs.mailboxes.wait_network_pending();
                logging::record(
                    Level::Debug,
                    Target::Peer,
                    "network.outbox_dispatch",
                    Context::peer(&delivery.destination),
                    || json!({"worker":worker,"messages":1,"message_id":delivery.message.id,"active":delivery.active}),
                    || Value::Null,
                );
                if let Err(error) = deliver(&state, &jobs, &delivery) {
                    logging::record(
                        Level::Error,
                        Target::Peer,
                        "network.outbox_failed",
                        Context::peer(&delivery.destination),
                        || json!({"worker":worker,"message_id":delivery.message.id,"error":error.to_string(),"retry_ms":5_000}),
                        || Value::Null,
                    );
                }
            }
        });
    }
}

pub fn start(state: Arc<State>, jobs: Arc<crate::jobs::Jobs>, port: u16) {
    *jobs.network.lock().unwrap() = Some(state.clone());
    start_delivery(&state, &jobs);
    std::thread::spawn(move || {
        let mut failures = std::collections::HashMap::<String, (u32, Instant)>::new();
        let mut prior_issues = Value::Null;
        loop {
            if logging::enabled(Level::Info, Target::Peer)
                || logging::enabled(Level::Warn, Target::Peer)
            {
                let health = match issues(&state) {
                    Ok(issues) => json!(issues),
                    Err(e) => json!({"error":e.to_string()}),
                };
                if health != prior_issues {
                    logging::record(
                        if health.as_array().is_some_and(Vec::is_empty) {
                            Level::Info
                        } else {
                            Level::Warn
                        },
                        Target::Peer,
                        "network.membership_health",
                        Context::default(),
                        || json!({"issues":health}),
                        || Value::Null,
                    );
                    prior_issues = health;
                }
            } else {
                prior_issues = Value::Null;
            }
            match state.peers() {
                Ok(peers) => {
                    for peer in peers.into_iter().filter(|p| !p.address.is_empty()) {
                        if failures
                            .get(&peer.key)
                            .is_some_and(|(_, due)| Instant::now() < *due)
                        {
                            continue;
                        }
                        let result = (|| -> Result<()> {
                            let snapshot = members(&state)?;
                            let reply = crate::protocol::request(
                                &state,
                                &peer.address,
                                &peer.key,
                                json!({"op":"network_sync","members":snapshot,"port":port,"residents":catalog(&jobs)}),
                            )?;
                            merge(&state, &peer.key, reply["members"].clone())?;
                            retain_catalog(&state, &peer.key, reply["residents"].clone())?;
                            Ok(())
                        })();
                        match result {
                            Ok(()) => {
                                if failures.remove(&peer.key).is_some() {
                                    logging::record(
                                        Level::Info,
                                        Target::Peer,
                                        "network.peer_recovered",
                                        Context::peer(&peer.key),
                                        || json!({}),
                                        || Value::Null,
                                    );
                                }
                            }
                            Err(e) => {
                                let (n, due) = failures
                                    .entry(peer.key.clone())
                                    .or_insert((0, Instant::now()));
                                *n = n.saturating_add(1);
                                *due = Instant::now() + Duration::from_secs(1u64 << (*n).min(6));
                                if *n == 1 || n.is_power_of_two() {
                                    logging::record(
                                        Level::Warn,
                                        Target::Peer,
                                        "network.sync_failed",
                                        Context::peer(&peer.key),
                                        || json!({"attempts":n,"error":e.to_string()}),
                                        || Value::Null,
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => logging::record(
                    Level::Error,
                    Target::Peer,
                    "network.members_unavailable",
                    Context::default(),
                    || json!({"error":e.to_string()}),
                    || Value::Null,
                ),
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

/// Each host publishes only its own compact address directory. Cached directories
/// remain useful during outages; a removed machine is never a usable route.
pub fn catalog(jobs: &crate::jobs::Jobs) -> Value {
    json!(
        jobs.residents
            .all()
            .iter()
            .map(|r| json!({"id":r.id,"owner":r.owner}))
            .collect::<Vec<_>>()
    )
}
pub fn retain_catalog(state: &State, remote: &str, rows: Value) -> Result<()> {
    state.require_trust(remote)?;
    let entries = rows.as_array().ok_or("network directory is an array")?;
    if entries.len() > 4096 {
        return Err("network directory exceeds 4096 residents".into());
    }
    let mut ids = std::collections::HashSet::new();
    for r in entries {
        state::key(r["id"].as_str().ok_or("directory resident id required")?)?;
        state::key(
            r["owner"]
                .as_str()
                .ok_or("directory resident provenance required")?,
        )?;
        if !ids.insert(r["id"].as_str()) {
            return Err("network directory repeats a resident".into());
        }
    }
    let bytes = serde_json::to_vec(&rows)?;
    if bytes.len() > 1024 * 1024 {
        return Err("network directory exceeds 1 MiB".into());
    }
    let directory = state.directory().join("network-directory");
    state::private_dir(&directory)?;
    let path = directory.join(format!("{remote}.json"));
    if fs::read(&path).ok().as_ref() == Some(&bytes) {
        return Ok(());
    }
    let _guard = lock(state)?;
    state.require_trust(remote)?;
    let tmp = directory.join(format!("{remote}.pending-{}", state::random_hex()?));
    state::write_new(&tmp, &bytes)?;
    fs::rename(tmp, path)?;
    logging::record(
        Level::Debug,
        Target::Peer,
        "network.directory_updated",
        Context::peer(remote),
        || json!({"residents":entries.len()}),
        || Value::Null,
    );
    Ok(())
}
pub fn route(
    state: &State,
    jobs: &crate::jobs::Jobs,
    to: &crate::mailbox::Address,
) -> Result<(String, Option<String>)> {
    use crate::mailbox::Address;
    match to {
        Address::Client { machine, .. } => {
            if machine == state.public_key() {
                return Ok((machine.clone(), None));
            }
            state.require_trust(machine)?;
            Ok((machine.clone(), Some(machine.clone())))
        }
        Address::Resident { id } => {
            let mut found = jobs
                .residents
                .all()
                .iter()
                .filter(|r| &r.id == id)
                .map(|r| (r.owner.clone(), None))
                .collect::<Vec<_>>();
            for peer in state.peers()? {
                let path = state
                    .directory()
                    .join("network-directory")
                    .join(format!("{}.json", peer.key));
                if !path.exists() {
                    continue;
                }
                if fs::metadata(&path)?.len() > 1024 * 1024 {
                    return Err("cached network directory exceeds bound".into());
                }
                let rows: Vec<Value> = serde_json::from_slice(&fs::read(path)?)?;
                for r in rows {
                    if r["id"] == *id {
                        found.push((
                            r["owner"].as_str().ok_or("directory owner missing")?.into(),
                            Some(peer.key.clone()),
                        ));
                    }
                }
            }
            if found.len() != 1 {
                return Err(if found.is_empty() {
                    "recipient location is not known in this network"
                } else {
                    "recipient identity is advertised by multiple machines"
                }
                .into());
            }
            Ok(found.remove(0))
        }
        Address::Connector { .. } => {
            Err("external connectors retain their instance-scoped local routing".into())
        }
    }
}
fn deliver(
    state: &State,
    jobs: &crate::jobs::Jobs,
    delivery: &crate::mailbox::NetworkDelivery,
) -> Result<()> {
    let message = &delivery.message;
    let destination = delivery.destination.as_str();
    let mut receipt = delivery.receipt.clone();
    let attempt = receipt["attempts"].as_u64().unwrap_or(0).saturating_add(1);
    let mut transport_failed = false;
    let result = (|| -> Result<Value> {
        state.require_trust(destination)?;
        let peer = state
            .peers()?
            .into_iter()
            .find(|p| p.key == destination)
            .ok_or("destination is no longer a member")?;
        if peer.address.is_empty() {
            return Err("destination endpoint is not known".into());
        }
        let mut request = |request| {
            crate::protocol::request(state, &peer.address, destination, request)
                .inspect_err(|e| transport_failed = !e.is::<crate::protocol::Rejected>())
        };
        jobs.attachments
            .transfer(&message.attachments, &mut request)?;
        let receipt = request(json!({"op":"network_mail","message":message}))?;
        if receipt["id"] != message.id
            || receipt["accepted_on"] != destination
            || receipt["status"] != "accepted"
        {
            return Err("destination mailbox acknowledgment differs from pending mail".into());
        }
        Ok(receipt)
    })();
    receipt["attempts"] = json!(attempt);
    let peer_retry_at = match result {
        Ok(accepted) => {
            receipt["status"] = json!("accepted");
            receipt["accepted_on"] = accepted["accepted_on"].clone();
            receipt["destination_sequence"] = accepted["sequence"].clone();
            receipt["last_error"] = Value::Null;
            None
        }
        Err(error) => {
            receipt["last_error"] = json!(error.to_string());
            let due = crate::jobs::now() + 1000 * (1u64 << attempt.min(6));
            receipt["next_attempt_at"] = json!(due);
            transport_failed.then_some(due)
        }
    };
    jobs.mailboxes
        .network_outcome(delivery, &receipt, peer_retry_at)?;
    if let Some(due) = peer_retry_at {
        // Queued siblings keep their own receipts. The failed message's deadline
        // governs the next connection attempt to this destination in this process.
        logging::record(
            Level::Debug,
            Target::Peer,
            "network.outbox_peer_deferred",
            Context::peer(destination),
            || json!({"message_id":message.id,"next_attempt_at":due}),
            || Value::Null,
        );
    }
    logging::record(
        if receipt["status"] == "accepted" {
            Level::Info
        } else {
            Level::Warn
        },
        Target::Peer,
        "network.mail_delivery",
        Context::peer(destination),
        || json!({"message_id":message.id,"status":receipt["status"],"attempts":attempt,"error":receipt["last_error"]}),
        || Value::Null,
    );
    Ok(())
}

/// Authenticated operator controls; runtime tools never enter this boundary.
pub fn operation(state: &State, args: &Value) -> Result<Value> {
    let action = args["action"].as_str().unwrap_or("status");
    let result = (|| -> Result<Value> {
        match action {
            "status" => Ok(
                json!({"members":members(state)?,"local_identity":state.public_key(),"issues":issues(state)?,"access":"network_members","consistency":"eventual"}),
            ),
            "admit" => {
                let key = args["key"].as_str().ok_or("member key required")?;
                admit(
                    state,
                    key,
                    args["label"].as_str().ok_or("member label required")?,
                )?;
                if let Some(address) = args["address"].as_str() {
                    set_endpoint(state, key, address)?;
                }
                Ok(json!({"accepted":true,"members":members(state)?}))
            }
            "rename" => {
                let key = args["key"].as_str().ok_or("Machine identity is missing")?;
                let label = args["label"].as_str().ok_or("Display name is missing")?;
                let expected = args["expected_label"].as_str().ok_or("Previous display name is missing")?;
                Ok(json!({"member": rename(state, key, label, expected)?}))
            }
            "remove" => {
                remove(state, args["key"].as_str().ok_or("member key required")?)?;
                Ok(json!({"accepted":true,"members":members(state)?}))
            }
            _ => Err("Network actions are status, admit, rename and remove".into()),
        }
    })();
    logging::record(
        if result.is_ok() {
            Level::Info
        } else {
            Level::Warn
        },
        Target::Peer,
        "network.operator_operation",
        Context::default(),
        || json!({"action":action,"member":args["key"],"accepted":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string())}),
        || Value::Null,
    );
    result
}
fn rename(state: &State, key: &str, label: &str, expected: &str) -> Result<Peer> {
    let label = label.trim();
    if label.is_empty() || label.len() > 100 || label.chars().any(char::is_control) {
        return Err("A display name contains 1–100 UTF-8 bytes of printable text".into());
    }
    let _guard = lock(state)?;
    let all = members(state)?;
    let mut peer = all
        .iter()
        .find(|peer| peer.key == key && !peer.removed)
        .ok_or("Machine is no longer a network member")?
        .clone();
    if peer.label == label {
        return Ok(peer);
    }
    if peer.label != expected {
        return Err("Display name changed. Reopen Machine connections to see the current name.".into());
    }
    peer.label = label.into();
    peer.endpoint_revision = next_endpoint_revision(&all)?;
    write(state, &peer)?;
    Ok(peer)
}

fn next_endpoint_revision(members: &[Peer]) -> Result<u64> {
    members
        .iter()
        .map(|p| p.endpoint_revision)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "endpoint revision exhausted".into())
}

pub fn set_endpoint(state: &State, key: &str, address: &str) -> Result<()> {
    let _guard = lock(state)?;
    let all = members(state)?;
    let mut peer = all
        .iter()
        .find(|p| p.key == key && !p.removed)
        .ok_or("active member not found")?
        .clone();
    if peer.address == address {
        return Ok(());
    }
    peer.address = address.into();
    peer.endpoint_revision = next_endpoint_revision(&all)?;
    write(state, &peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_converges_and_stale_additions_do_not_undo_removal() {
        let root =
            std::env::temp_dir().join(format!("boxen-network-{}", state::random_hex().unwrap()));
        let a = State::open(root.join("a")).unwrap();
        let b = State::open(root.join("b")).unwrap();
        let c = State::open(root.join("c")).unwrap();
        for s in [&a, &b, &c] {
            configure(s, Some("Fixture"), "127.0.0.1:42424").unwrap();
        }
        a.trust(b.public_key(), "B").unwrap();
        b.trust(a.public_key(), "A").unwrap();
        b.trust(c.public_key(), "C").unwrap();
        c.trust(b.public_key(), "B").unwrap();
        let old = json!(members(&b).unwrap());
        merge(&a, b.public_key(), old.clone()).unwrap();
        merge(&c, b.public_key(), old.clone()).unwrap();
        a.require_trust(c.public_key()).unwrap();
        c.require_trust(a.public_key()).unwrap();
        a.untrust(c.public_key()).unwrap();
        merge(&b, a.public_key(), json!(members(&a).unwrap())).unwrap();
        merge(&a, b.public_key(), old).unwrap();
        assert!(a.require_trust(c.public_key()).is_err());
        let reopened = State::open(root.join("a")).unwrap();
        assert!(reopened.require_trust(c.public_key()).is_err());
        // A normal endpoint update at an unaware removed machine is not a new admission.
        configure(&c, Some("Fixture changed"), "127.0.0.1:42426").unwrap();
        merge(&b, a.public_key(), json!(members(&c).unwrap())).unwrap();
        assert!(b.require_trust(c.public_key()).is_err());
        merge(&c, b.public_key(), json!(members(&b).unwrap())).unwrap();
        assert!(c.require_trust(b.public_key()).is_err());
        c.trust(b.public_key(), "B").unwrap(); // explicit local rejoin
        assert!(b.require_trust(c.public_key()).is_err());
        a.trust(c.public_key(), "C readmitted").unwrap();
        merge(&b, a.public_key(), json!(members(&a).unwrap())).unwrap();
        merge(&c, b.public_key(), json!(members(&b).unwrap())).unwrap();
        c.require_trust(a.public_key()).unwrap();
        b.require_trust(c.public_key()).unwrap();
        let corrupt = a
            .directory()
            .join("peers")
            .join(format!("{}.json", c.public_key()));
        fs::write(&corrupt, b"{broken").unwrap();
        a.require_trust(b.public_key()).unwrap();
        assert!(a.require_trust(c.public_key()).is_err());
        assert_eq!(issues(&a).unwrap().len(), 1);
        assert!(merge(&a, b.public_key(), json!(members(&b).unwrap())).is_err());
        assert_eq!(fs::read(&corrupt).unwrap(), b"{broken");
        fs::remove_dir_all(root).unwrap();
    }
}
