//! Client projections only: the daemon's correspondence journal owns delivery.
use crate::{
    Action, Data, Desktop,
    client::{Client, Publisher, Update},
    model::{Destination, Pane, ThreadKey},
};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedMail {
    pub drafts: Vec<Draft>,
    pub read: HashSet<ThreadKey>,
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub id: String,
    pub parent: Option<ThreadKey>,
    pub recipient: Option<ThreadKey>,
    pub subject: String,
    pub body: String,
    pub pending: Option<PendingMail>,
    pub accepted: Option<String>,
    pub last_receipt: Option<Value>,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingMail {
    pub destination: Destination,
    pub request: Value,
}
#[derive(Default)]
pub struct Mailbox {
    pub messages: HashMap<ThreadKey, Value>,
    pub residents: HashMap<String, Vec<Value>>,
    pub errors: HashMap<String, String>,
    pub sending: HashSet<String>,
    pub filter: String,
    pub selected: Option<ThreadKey>,
    pub outgoing: HashMap<ThreadKey, Value>,
    pub watching: Arc<Mutex<HashSet<ThreadKey>>>,
    pub history_errors: HashMap<ThreadKey, String>,
}
pub enum MailUpdate {
    Outgoing {
        peer: String,
        messages: Vec<Value>,
        agent: String,
        error: Option<String>,
    },
    Receipt {
        draft: String,
        id: String,
        result: Result<Value, String>,
    },
    Page {
        destination: Destination,
        client_id: String,
        residents: Result<Vec<Value>, String>,
        inbox: Result<Value, String>,
        definitions: Result<Value, String>,
        health: Result<Value, String>,
    },
    Sent {
        draft: String,
        id: String,
        result: Result<Value, String>,
    },
}
pub enum MailAction {
    Open(ThreadKey, bool),
    Pin(Pane),
    New,
    Resume(String),
    Send(String),
    Retry(String),
}
impl Client {
    pub fn mail_poll(
        &self,
        destination: Destination,
        client_id: String,
        publisher: Publisher,
        stop: Arc<AtomicBool>,
        watching: Arc<Mutex<HashSet<ThreadKey>>>,
    ) {
        let client = self.clone();
        std::thread::spawn(move || {
            let mut after = 0;
            let mut sent_cursors = HashMap::new();
            let mut resident_refresh = std::time::Instant::now() - Duration::from_secs(10);
            let mut resident_cache: Result<Vec<Value>, String> = Ok(vec![]);
            let mut definitions = Ok(json!({"definitions":[]}));
            let mut health = Ok(json!({"incidents":[]}));
            while !stop.load(Ordering::Relaxed) {
                if resident_refresh.elapsed() >= Duration::from_secs(10) {
                    resident_cache = client
                        .operation(&destination, json!({"op":"resident_list"}))
                        .and_then(|v| {
                            v.as_array()
                                .map(|rows| {
                                    rows.iter()
                                        .map(|r| json!({"id":r["id"],"name":r["name"],"backend":r["backend"],"model":r["model"],"definition":r["definition"],"thread_id":r["thread_id"]}))
                                        .collect()
                                })
                                .ok_or("Invalid resident list".into())
                        });
                    if resident_cache.is_ok() {
                        let has_instances = resident_cache.as_ref().is_ok_and(|rows| {
                            rows.iter().any(|r| r["definition"]["id"].is_string())
                        });
                        definitions = if has_instances {
                            client.operation(
                                &destination,
                                json!({"op":"resident_definition","args":{"action":"list"}}),
                            )
                        } else {
                            Ok(json!({"definitions":[]}))
                        };
                        health = client.operation(&destination, json!({"op":"inference_health"}));
                    } else {
                        definitions = Err("Machine unavailable".into());
                        health = Err("Machine unavailable".into());
                    }
                    resident_refresh = std::time::Instant::now();
                }
                if let Ok(rows) = &resident_cache {
                    for resident in rows {
                        let Some(id) = resident["id"].as_str() else {
                            continue;
                        };
                        if !watching.lock().unwrap().contains(&ThreadKey {
                            peer: destination.key.clone(),
                            id: id.into(),
                        }) {
                            continue;
                        }
                        let cursor = sent_cursors.entry(id.to_owned()).or_insert(0);
                        match client.operation(
                            &destination,
                            json!({"op":"resident_inbox","id":id,"after":*cursor}),
                        ) {
                            Ok(page) => {
                                *cursor =
                                    page["next_after"].as_u64().unwrap_or(*cursor).max(*cursor);
                                let messages = page["messages"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter(|m| {
                                        m["sender"]["type"] == "client"
                                            && m["sender"]["machine"] == client_id
                                            && m["sender"]["id"] == client_id
                                    })
                                    .cloned()
                                    .collect();
                                if !publisher.send(Update::Mail(MailUpdate::Outgoing {
                                    peer: destination.key.clone(),
                                    messages,
                                    agent: id.into(),
                                    error: None,
                                })) {
                                    return;
                                }
                            }
                            Err(error) => {
                                if !publisher.send(Update::Mail(MailUpdate::Outgoing {
                                    peer: destination.key.clone(),
                                    agent: id.into(),
                                    messages: vec![],
                                    error: Some(error),
                                })) {
                                    return;
                                }
                            }
                        }
                    }
                }
                let residents = resident_cache.clone();
                let previous = after;
                let inbox = client.operation(
                    &destination,
                    json!({"op":"client_inbox","client_id":client_id,"after":after}),
                );
                if let Ok(v) = &inbox {
                    after = v["next_after"].as_u64().unwrap_or(after).max(after);
                }
                let more = inbox
                    .as_ref()
                    .is_ok_and(|v| v["messages"].as_array().is_some_and(|m| !m.is_empty()));
                if !publisher.send(Update::Mail(MailUpdate::Page {
                    destination: destination.clone(),
                    client_id: client_id.clone(),
                    residents,
                    inbox,
                    definitions: definitions.clone(),
                    health: health.clone(),
                })) {
                    return;
                }
                // Drain paginated input without losing older pages. Empty pages wait for new arrivals.
                if more && after > previous {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }
    pub(crate) fn mail_receipts(
        &self,
        destination: Destination,
        checks: Vec<(String, String)>,
        publisher: Publisher,
    ) {
        let client = self.clone();
        std::thread::spawn(move || {
            for (draft, id) in checks {
                let result = client.operation(
                    &destination,
                    json!({"op":"messaging","args":{"action":"status","id":id}}),
                );
                if !publisher.send(Update::Mail(MailUpdate::Receipt { draft, id, result })) {
                    break;
                }
            }
        });
    }
    fn mail_submit(&self, pending: PendingMail, draft: String, publisher: Publisher, lookup: bool) {
        let client = self.clone();
        std::thread::spawn(move || {
            let id = pending.request["message"]["id"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            let request = if lookup {
                json!({"op":"messaging","args":{"action":"status","id":id}})
            } else {
                pending.request
            };
            let result = client.operation(&pending.destination, request);
            publisher.send(Update::Mail(MailUpdate::Sent { draft, id, result }));
        });
    }
}
impl Desktop {
    pub(crate) fn mail_update(&mut self, update: MailUpdate) {
        match update {
            MailUpdate::Outgoing {
                peer,
                messages,
                agent,
                error,
            } => {
                let key = ThreadKey {
                    peer: peer.clone(),
                    id: agent,
                };
                if let Some(error) = error {
                    self.data.mail.history_errors.insert(key, error);
                } else {
                    self.data.mail.history_errors.remove(&key);
                }
                for message in messages {
                    if let Some(id) = message["id"].as_str() {
                        self.data.mail.outgoing.insert(
                            ThreadKey {
                                peer: peer.clone(),
                                id: id.into(),
                            },
                            message,
                        );
                    }
                }
            }
            MailUpdate::Page {
                destination,
                client_id,
                residents,
                inbox,
                definitions,
                health,
            } => {
                if client_id != self.data.local_key {
                    return;
                }
                let peer = destination.key;
                self.data
                    .agents
                    .metadata(&peer, &residents, definitions, health);
                self.data.mail.errors.remove(&peer);
                match residents {
                    Ok(v) => {
                        self.data.mail.residents.insert(peer.clone(), v);
                    }
                    Err(e) => {
                        self.data.mail.errors.insert(peer.clone(), e);
                    }
                }
                match inbox {
                    Ok(v) => {
                        for message in v["messages"].as_array().into_iter().flatten() {
                            if let Some(id) = message["id"].as_str() {
                                self.data.mail.messages.insert(
                                    ThreadKey {
                                        peer: peer.clone(),
                                        id: id.into(),
                                    },
                                    message.clone(),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        self.data.mail.errors.insert(peer.clone(), e);
                    }
                }
                // Reconcile exact pending IDs, never resubmit on polling or restart.
                if let Some(client) = &self.client {
                    for draft in &self.data.saved.mail.drafts {
                        if let Some(p) = &draft.pending
                            && p.destination.key == peer
                            && self.data.mail.sending.insert(draft.id.clone())
                        {
                            client.mail_submit(
                                p.clone(),
                                draft.id.clone(),
                                self.bus.publisher(),
                                true,
                            );
                        }
                    }
                }
            }
            MailUpdate::Receipt { draft, id, result } => {
                if let Ok(value) = result
                    && value["id"] == id
                    && let Some(d) = self.data.saved.mail.drafts.iter_mut().find(|d| {
                        d.id == draft && d.last_receipt.as_ref().is_some_and(|v| v["id"] == id)
                    })
                {
                    // A status observation never clears a newer draft or submits mail.
                    d.last_receipt = Some(value);
                }
            }
            MailUpdate::Sent { draft, id, result } => {
                self.data.mail.sending.remove(&draft);
                if let Some(d) = self
                    .data
                    .saved
                    .mail
                    .drafts
                    .iter_mut()
                    .find(|d| d.id == draft)
                    && d.pending
                        .as_ref()
                        .is_some_and(|p| p.request["message"]["id"] == id)
                {
                    match result {
                        Ok(v) if v["id"] == id => {
                            if let Some(target) = &d.recipient {
                                self.data.mail.outgoing.insert(ThreadKey { peer: target.peer.clone(), id: id.clone() }, v.clone());
                            }
                            d.pending = None;
                            d.last_receipt = Some(v);
                            d.accepted = Some(id);
                            d.error = None;
                            d.body.clear();
                        }
                        Ok(_) => d.error = Some("The daemon returned an unexpected receipt. Delivery remains unconfirmed.".into()),
                        Err(e) => d.error = Some(format!("Delivery unconfirmed: {e}")),
                    }
                }
                self.persist();
            }
        }
    }
    pub(crate) fn mail_action(&mut self, action: MailAction) {
        match action {
            MailAction::Open(key, pin) => {
                self.data.saved.mail.read.insert(key.clone());
                self.data.mail.selected = Some(key.clone());
                self.data.saved.workspace.open_message(key, pin);
            }
            MailAction::Pin(pane) => self.data.saved.workspace.pin_pane(&pane),
            MailAction::Resume(id) => {
                if self
                    .data
                    .saved
                    .mail
                    .drafts
                    .iter()
                    .any(|d| d.id == id && d.parent.is_none())
                {
                    self.data.saved.workspace.open_compose(id);
                }
            }
            MailAction::New => {
                let id = crate::new_id();
                self.data.saved.mail.drafts.push(Draft {
                    id: id.clone(),
                    ..Default::default()
                });
                self.data.saved.workspace.open_compose(id);
            }
            MailAction::Send(id) => {
                let Some(draft) = self
                    .data
                    .saved
                    .mail
                    .drafts
                    .iter()
                    .find(|d| d.id == id)
                    .cloned()
                else {
                    return;
                };
                if draft.pending.is_some() {
                    return;
                }
                let Some(target) = draft.recipient else {
                    return;
                };
                if !self
                    .data
                    .saved
                    .destinations
                    .iter()
                    .any(|d| d.key == target.peer)
                {
                    return;
                }
                if draft.subject.trim().is_empty()
                    || draft.body.trim().is_empty()
                    || draft.subject.len() > 256
                    || draft.body.len() > 32768
                {
                    return;
                }
                let pending = PendingMail {
                    destination: Destination {
                        key: self.data.local_key.clone(),
                        label: "This machine".into(),
                        address: String::new(),
                    },
                    request: json!({"op":"mailbox_send","client_id":self.data.local_key,"message":{"id":crate::new_id(),"to":{"type":"resident","id":target.id},"subject":draft.subject,"body":draft.body,"in_reply_to":draft.parent.map(|p|p.id)}}),
                };
                let d = self
                    .data
                    .saved
                    .mail
                    .drafts
                    .iter_mut()
                    .find(|d| d.id == id)
                    .unwrap();
                d.pending = Some(pending);
                d.error = None;
                d.accepted = None;
                if self.persist() {
                    self.mail_action(MailAction::Retry(id));
                }
            }
            MailAction::Retry(id) => {
                if self.data.mail.sending.contains(&id) {
                    return;
                }
                let Some(p) = self
                    .data
                    .saved
                    .mail
                    .drafts
                    .iter()
                    .find(|d| d.id == id)
                    .and_then(|d| d.pending.clone())
                else {
                    return;
                };
                // A saved exact request is the only request that an explicit retry can transmit.
                if !self.persist() {
                    return;
                }
                if let Some(client) = &self.client {
                    self.data.mail.sending.insert(id.clone());
                    client.mail_submit(p, id, self.bus.publisher(), false);
                }
            }
        }
    }
}
fn agent_name(data: &Data, key: &ThreadKey) -> String {
    let name = data
        .mail
        .residents
        .get(&key.peer)
        .into_iter()
        .flatten()
        .find(|r| r["id"] == key.id)
        .and_then(|r| r["name"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| key.id.chars().take(12).collect());
    let machine = data
        .saved
        .destinations
        .iter()
        .find(|d| d.key == key.peer)
        .map(|d| d.label.as_str())
        .unwrap_or("Machine");
    format!("{name} · {machine}")
}
pub fn message_title(data: &Data, key: &ThreadKey) -> String {
    data.mail
        .messages
        .get(key)
        .and_then(|m| m["subject"].as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Message")
        .chars()
        .take(48)
        .collect()
}
pub fn inbox(ui: &mut egui::Ui, data: &mut Data, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.heading("Mail");
        if ui.small_button("New message").clicked() {
            actions.push(Action::Mail(MailAction::New));
        }
    });
    ui.add(
        egui::TextEdit::singleline(&mut data.mail.filter)
            .hint_text("Filter messages…")
            .desired_width(f32::INFINITY),
    );
    ui.weak("Your inbox · replies are optional");
    if !data.local_key.is_empty() {
        ui.small_button("Copy mailbox address")
            .on_hover_text("Other Boxen clients can use this same address.")
            .clicked()
            .then(|| {
                ui.ctx()
                    .copy_text(format!("client:{}:{}", data.local_key, data.local_key))
            });
    }
    for dest in &data.saved.destinations {
        if let Some(error) = data.mail.errors.get(&dest.key) {
            ui.colored_label(egui::Color32::YELLOW, format!("{} · {error}", dest.label));
        }
    }
    let drafts: Vec<_> = data
        .saved
        .mail
        .drafts
        .iter()
        .filter(|d| {
            !d.id.starts_with("chat:")
                && d.parent.is_none()
                && (d.pending.is_some()
                    || (d.accepted.is_none() && (!d.subject.is_empty() || !d.body.is_empty())))
        })
        .collect();
    if !drafts.is_empty() {
        egui::CollapsingHeader::new(format!("Drafts ({})", drafts.len())).show(ui, |ui| {
            for draft in drafts {
                if ui
                    .button(if draft.subject.is_empty() {
                        "Untitled message"
                    } else {
                        &draft.subject
                    })
                    .clicked()
                {
                    actions.push(Action::Mail(MailAction::Resume(draft.id.clone())));
                }
            }
        });
    }
    ui.separator();
    let filter = data.mail.filter.to_lowercase();
    let mut messages: Vec<_> = data.mail.messages.iter().collect();
    messages.sort_by(|(ka, a), (kb, b)| {
        b["accepted_at"]
            .as_u64()
            .cmp(&a["accepted_at"].as_u64())
            .then_with(|| ka.peer.cmp(&kb.peer))
            .then_with(|| kb.id.cmp(&ka.id))
    });
    egui::ScrollArea::vertical()
        .id_salt("correspondence-inbox")
        .show(ui, |ui| {
            if messages.is_empty() {
                ui.weak("No messages yet.");
            }
            for (key, m) in messages {
                let sender = agent_name(
                    data,
                    &ThreadKey {
                        peer: m["network"]["source_machine"]
                            .as_str()
                            .unwrap_or(&key.peer)
                            .into(),
                        id: m["sender"]["id"].as_str().unwrap_or("").into(),
                    },
                );
                let subject = m["subject"].as_str().unwrap_or("(No subject)");
                let body = m["body"].as_str().unwrap_or("");
                if !filter.is_empty()
                    && !format!("{sender} {subject} {body}")
                        .to_lowercase()
                        .contains(&filter)
                {
                    continue;
                }
                let text = RichText::new(format!("{subject}\n{sender}"));
                let text = if data.saved.mail.read.contains(key) {
                    text
                } else {
                    text.strong()
                };
                let response = ui.add(
                    egui::Button::selectable(data.mail.selected.as_ref() == Some(key), text).wrap(),
                );
                if response.clicked() || response.double_clicked() {
                    actions.push(Action::Mail(MailAction::Open(
                        key.clone(),
                        response.double_clicked(),
                    )));
                }
            }
        });
}
pub fn details(ui: &mut egui::Ui, pane: &Pane, data: &mut Data, actions: &mut Vec<Action>) {
    let (draft_id, parent) = match pane {
        Pane::Message(key) => {
            let Some(message) = data.mail.messages.get(key).cloned() else {
                ui.weak("Loading this message from its mailbox…");
                return;
            };
            ui.heading(message["subject"].as_str().unwrap_or("(No subject)"));
            let target = message["reply_to"]
                .as_object()
                .filter(|a| a.get("type").and_then(Value::as_str) == Some("resident"))
                .map(|a| a["id"].as_str().unwrap_or(""))
                .or_else(|| {
                    (message["sender"]["type"] == "resident")
                        .then(|| message["sender"]["id"].as_str().unwrap_or(""))
                });
            let recipient = target.map(|id| ThreadKey {
                peer: message["network"]["source_machine"]
                    .as_str()
                    .unwrap_or(&key.peer)
                    .into(),
                id: id.into(),
            });
            ui.label(format!(
                "From: {}",
                agent_name(
                    data,
                    &ThreadKey {
                        peer: message["network"]["source_machine"]
                            .as_str()
                            .unwrap_or(&key.peer)
                            .into(),
                        id: message["sender"]["id"].as_str().unwrap_or("").into()
                    }
                )
            ));
            ui.weak("To: you · accepted into your mailbox");
            if !message["in_reply_to"].is_null() {
                ui.small(format!(
                    "In reply to {}",
                    message["in_reply_to"].as_str().unwrap_or("")
                ));
            }
            let height = (ui.available_height() - 220.)
                .min(ui.clip_rect().height() * 0.4)
                .max(40.);
            egui::ScrollArea::vertical()
                .id_salt(("message", &key.peer, &key.id))
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(message["body"].as_str().unwrap_or(""))
                            .wrap()
                            .selectable(true),
                    );
                });
            let id = format!("reply:{}:{}", key.peer, key.id);
            if !data.saved.mail.drafts.iter().any(|d| d.id == id) {
                data.saved.mail.drafts.push(Draft {
                    id: id.clone(),
                    parent: Some(key.clone()),
                    recipient,
                    subject: format!("Re: {}", message["subject"].as_str().unwrap_or("Message")),
                    ..Default::default()
                });
            }
            (id, Some(key.clone()))
        }
        Pane::Compose(id) => {
            ui.heading("New message");
            (id.clone(), None)
        }
        _ => return,
    };
    ui.separator();
    let targets: Vec<_> = data
        .saved
        .destinations
        .iter()
        .flat_map(|dest| {
            data.mail
                .residents
                .get(&dest.key)
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    r["id"].as_str().map(|id| ThreadKey {
                        peer: dest.key.clone(),
                        id: id.into(),
                    })
                })
        })
        .map(|key| {
            let label = format!(
                "{} · {}",
                agent_name(data, &key),
                key.id.chars().take(8).collect::<String>()
            );
            (key, label)
        })
        .collect();
    let Some(draft) = data.saved.mail.drafts.iter_mut().find(|d| d.id == draft_id) else {
        return;
    };
    let pending = draft.pending.is_some();
    let sending = data.mail.sending.contains(&draft_id);
    let mut changed = false;
    ui.add_enabled_ui(!pending, |ui| {
        if parent.is_none() {
            let selected = targets
                .iter()
                .find(|(k, _)| Some(k) == draft.recipient.as_ref())
                .map(|(_, label)| label.as_str())
                .unwrap_or("Choose an agent instance…");
            egui::ComboBox::from_id_salt(("recipient", &draft_id))
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for (key, label) in &targets {
                        changed |= ui
                            .selectable_value(&mut draft.recipient, Some(key.clone()), label)
                            .changed();
                    }
                });
            if targets.is_empty() {
                ui.weak("No reachable agent instances have been discovered.");
            }
        } else {
            ui.label(format!(
                "Reply to {}",
                targets
                    .iter()
                    .find(|(k, _)| Some(k) == draft.recipient.as_ref())
                    .map(|(_, s)| s.as_str())
                    .unwrap_or("unavailable agent")
            ));
        }
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut draft.subject)
                    .hint_text("Subject")
                    .desired_width(f32::INFINITY),
            )
            .changed();
        changed |= ui
            .add(
                egui::TextEdit::multiline(&mut draft.body)
                    .hint_text("Message…")
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            )
            .changed();
    });
    if changed {
        draft.accepted = None;
        draft.error = None;
        actions.push(Action::Mail(MailAction::Pin(pane.clone())));
    }
    if let Some(receipt) = &draft.last_receipt {
        egui::CollapsingHeader::new("Your last accepted message")
            .default_open(false)
            .show(ui, |ui| {
                let target = targets
                    .iter()
                    .find(|(k, _)| receipt["to"]["id"] == k.id)
                    .map(|(_, s)| s.as_str())
                    .unwrap_or("original recipient");
                ui.weak(format!("To: {target}"));
                ui.label(receipt["subject"].as_str().unwrap_or(""));
                ui.add(
                    egui::Label::new(receipt["body"].as_str().unwrap_or(""))
                        .wrap()
                        .selectable(true),
                );
            });
    }
    if draft.subject.len() > 256 || draft.body.len() > 32768 {
        ui.colored_label(
            egui::Color32::YELLOW,
            "Messages allow 256 subject bytes and 32 KiB of body text.",
        );
    }
    if let Some(id) = &draft.accepted {
        ui.weak(format!(
            "{} · {} · replies are optional",
            if draft
                .last_receipt
                .as_ref()
                .is_some_and(|v| v["delivery"]["status"] == "queued")
            {
                "Saved on this machine; awaiting destination"
            } else {
                "Accepted into mailbox"
            },
            &id[..id.len().min(12)]
        ));
    }
    if let Some(error) = &draft.error {
        ui.colored_label(egui::Color32::YELLOW, error);
    }
    if pending {
        ui.weak(if sending {
            "Checking delivery…"
        } else {
            "Delivery remains unconfirmed. The saved message is unchanged."
        });
        if ui
            .add_enabled(!sending, egui::Button::new("Retry same message"))
            .clicked()
        {
            actions.push(Action::Mail(MailAction::Retry(draft_id)));
        }
    } else {
        let valid = targets
            .iter()
            .any(|(k, _)| Some(k) == draft.recipient.as_ref())
            && !draft.subject.trim().is_empty()
            && !draft.body.trim().is_empty()
            && draft.subject.len() <= 256
            && draft.body.len() <= 32768;
        let shortcut = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if ui
            .add_enabled(valid, egui::Button::new("Send message"))
            .clicked()
            || (valid && shortcut)
        {
            actions.push(Action::Mail(MailAction::Send(draft_id)));
        }
    }
}

/// A read-only timeline over accepted correspondence, with an ordinary mailbox composer.
pub fn chat(ui: &mut egui::Ui, data: &mut Data, key: &ThreadKey, actions: &mut Vec<Action>) {
    let name = crate::activity::title(data, key);
    ui.heading(&name);
    if let Some(error) = data.mail.errors.get(&key.peer) {
        ui.colored_label(egui::Color32::YELLOW, error);
    }
    if let Some(error) = data.mail.history_errors.get(key) {
        ui.colored_label(
            egui::Color32::YELLOW,
            format!("Sent history unavailable: {error}"),
        );
    }
    let draft_id = format!("chat:{}:{}", key.peer, key.id);
    if !data.saved.mail.drafts.iter().any(|d| d.id == draft_id) {
        data.saved.mail.drafts.push(Draft {
            id: draft_id.clone(),
            recipient: Some(key.clone()),
            subject: "Conversation".into(),
            ..Default::default()
        });
    }
    let mut messages: HashMap<String, (bool, Value)> = HashMap::new();
    for (k, m) in &data.mail.messages {
        if incoming_for(m, k, key) {
            messages.insert(m["id"].as_str().unwrap_or("").into(), (false, m.clone()));
        }
    }
    for (k, m) in &data.mail.outgoing {
        if k.peer == key.peer && m["to"]["type"] == "resident" && m["to"]["id"] == key.id {
            messages.insert(m["id"].as_str().unwrap_or("").into(), (true, m.clone()));
        }
    }
    for draft in &data.saved.mail.drafts {
        if draft.recipient.as_ref() == Some(key)
            && let Some(m) = &draft.last_receipt
            && m["to"]["type"] == "resident"
            && m["to"]["id"] == key.id
        {
            messages
                .entry(m["id"].as_str().unwrap_or("").into())
                .or_insert((true, m.clone()));
        }
    }
    let mut messages: Vec<_> = messages.into_iter().collect();
    messages.sort_by(|(ia, (_, a)), (ib, (_, b))| {
        a["accepted_at"]
            .as_u64()
            .cmp(&b["accepted_at"].as_u64())
            .then_with(|| ia.cmp(ib))
    });
    let parent = messages
        .iter()
        .rev()
        .find(|(_, (outgoing, _))| !outgoing)
        .map(|(id, _)| ThreadKey {
            peer: key.peer.clone(),
            id: id.clone(),
        });
    egui::ScrollArea::vertical()
        .id_salt(("chat-history", key))
        .stick_to_bottom(true)
        .max_height((ui.available_height() - 150.).max(40.))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if messages.is_empty() {
                ui.weak("No messages yet.");
            }
            for (_, (outgoing, m)) in messages {
                ui.with_layout(
                    if outgoing {
                        egui::Layout::top_down(egui::Align::Max)
                    } else {
                        egui::Layout::top_down(egui::Align::Min)
                    },
                    |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.strong(if outgoing { "You" } else { &name });
                            let subject = m["subject"].as_str().unwrap_or("");
                            if !subject.is_empty() && subject != "Conversation" {
                                ui.weak(subject);
                            }
                            ui.add(
                                egui::Label::new(m["body"].as_str().unwrap_or(""))
                                    .wrap()
                                    .selectable(true),
                            );
                            if outgoing && m["delivery"]["status"] == "queued" {
                                ui.small("Queued for delivery");
                            }
                        });
                    },
                );
                ui.add_space(6.);
            }
        });
    ui.separator();
    let draft = data
        .saved
        .mail
        .drafts
        .iter_mut()
        .find(|d| d.id == draft_id)
        .unwrap();
    let pending = draft.pending.is_some();
    let edited = ui
        .add_enabled(
            !pending,
            egui::TextEdit::multiline(&mut draft.body)
                .hint_text("Message…")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        )
        .changed();
    if edited {
        draft.error = None;
        actions.push(Action::Mail(MailAction::Pin(Pane::Chat(key.clone()))));
    }
    if let Some(error) = &draft.error {
        ui.colored_label(egui::Color32::YELLOW, error);
    }
    let sending = data.mail.sending.contains(&draft_id);
    if pending {
        ui.weak("Delivery remains unconfirmed.");
        if ui
            .add_enabled(!sending, egui::Button::new("Retry same message"))
            .clicked()
        {
            actions.push(Action::Mail(MailAction::Retry(draft_id)));
        }
    } else {
        let valid = !draft.body.trim().is_empty() && draft.body.len() <= 32768;
        let shortcut = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if ui.add_enabled(valid, egui::Button::new("Send")).clicked() || (valid && shortcut) {
            draft.parent = parent;
            actions.push(Action::Mail(MailAction::Send(draft_id)));
        }
        if draft.body.len() > 32768 {
            ui.weak("Messages allow 32 KiB of text.");
        }
    }
}
fn incoming_for(message: &Value, stored: &ThreadKey, agent: &ThreadKey) -> bool {
    message["sender"]["type"] == "resident"
        && message["sender"]["id"] == agent.id
        && message["network"]["source_machine"]
            .as_str()
            .unwrap_or(&stored.peer)
            == agent.peer
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draft_and_uncertain_exact_request_survive_workspace_serialization() {
        let d = Draft {
            id: "draft".into(),
            subject: "Subject".into(),
            body: "Unsent or uncertain text".into(),
            pending: Some(PendingMail {
                destination: Destination {
                    key: "peer".into(),
                    label: "Host".into(),
                    address: "127.0.0.1:1234".into(),
                },
                request: json!({"op":"mailbox_send","message":{"id":"exact","body":"Original"}}),
            }),
            ..Default::default()
        };
        let state = SavedMail {
            drafts: vec![d],
            ..Default::default()
        };
        let restored: SavedMail =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(restored.drafts[0].body, "Unsent or uncertain text");
        assert_eq!(
            restored.drafts[0].pending.as_ref().unwrap().request["message"]["id"],
            "exact"
        );
    }
    #[test]
    #[ignore = "scripts/desktop-smoke.py supplies private paired daemons"]
    fn real_daemon_boundary_correspondence() {
        let home = std::path::PathBuf::from(std::env::var_os("BOXEN_GUI_TEST_HOME").unwrap());
        let client = Client::new(std::env::var("BOXEN_GUI_TEST_URL").unwrap(), &home).unwrap();
        let dest: Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        let client_id = client.status().unwrap()["identity"]
            .as_str()
            .unwrap()
            .to_owned();
        let resident = crate::new_id();
        client.operation(&dest,json!({"op":"resident_create","spec":{"id":resident,"name":"Correspondence test instance","cwd":home,"auto_activate":false}})).unwrap();
        let pending = PendingMail {
            destination: dest.clone(),
            request: json!({"op":"mailbox_send","client_id":client_id,"message":{"id":crate::new_id(),"to":{"type":"resident","id":resident},"subject":"Desktop message","body":"REPLY client message"}}),
        };
        let bus = crate::client::Bus::new(egui::Context::default());
        let roundtrip: PendingMail =
            serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap();
        client.mail_submit(roundtrip.clone(), "draft".into(), bus.publisher(), false);
        let first = match bus.rx.recv_timeout(Duration::from_secs(12)).unwrap() {
            Update::Mail(MailUpdate::Sent { result: Ok(v), .. }) => v,
            _ => panic!("send missing"),
        };
        client.mail_submit(roundtrip.clone(), "draft".into(), bus.publisher(), false);
        let retry = match bus.rx.recv_timeout(Duration::from_secs(12)).unwrap() {
            Update::Mail(MailUpdate::Sent { result: Ok(v), .. }) => v,
            _ => panic!("retry missing"),
        };
        assert_eq!(first, retry);
        client.mail_submit(roundtrip.clone(), "draft".into(), bus.publisher(), true);
        match bus.rx.recv_timeout(Duration::from_secs(12)).unwrap() {
            Update::Mail(MailUpdate::Sent { result: Ok(v), .. }) => {
                assert_eq!(v["id"], first["id"])
            }
            _ => panic!("reconciliation missing"),
        };
        assert_eq!(
            client
                .operation(&dest, json!({"op":"resident_inbox","id":resident}))
                .unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "retry or reconciliation duplicated mail"
        );

        let activation = client
            .operation(
                &dest,
                json!({"op":"resident_activate","id":resident,"activation_id":crate::new_id()}),
            )
            .unwrap();
        let run = activation["record"]["spec"]["id"].as_str().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let v = client
                .operation(&dest, json!({"op":"get","id":run}))
                .unwrap();
            if crate::model::terminal(&v) {
                assert_eq!(v["status"], "completed");
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
        let local = Destination {
            key: client_id.clone(),
            label: "This machine".into(),
            address: String::new(),
        };
        assert!(
            client
                .operation(&dest, json!({"op":"resident_list"}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == resident)
        );
        let stop = Arc::new(AtomicBool::new(false));
        client.mail_poll(
            local.clone(),
            client_id.clone(),
            bus.publisher(),
            stop.clone(),
            Default::default(),
        );
        let inbox_deadline = std::time::Instant::now() + Duration::from_secs(15);
        let incoming = loop {
            assert!(
                std::time::Instant::now() < inbox_deadline,
                "incoming reply deadline"
            );
            if let Update::Mail(MailUpdate::Page {
                residents: Ok(residents),
                inbox: Ok(v),
                ..
            }) = bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
            {
                let _ = residents;
                if let Some(m) = v["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["in_reply_to"] == first["id"])
                {
                    break m.clone();
                }
            }
        };
        stop.store(true, Ordering::Relaxed);
        let watching = Arc::new(Mutex::new(HashSet::from([ThreadKey {
            peer: dest.key.clone(),
            id: resident.clone(),
        }])));
        let sent_stop = Arc::new(AtomicBool::new(false));
        client.mail_poll(
            dest.clone(),
            client_id.clone(),
            bus.publisher(),
            sent_stop.clone(),
            watching,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "Sent history deadline"
            );
            if let Update::Mail(MailUpdate::Outgoing {
                messages, error, ..
            }) = bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
            {
                assert!(error.is_none(), "{error:?}");
                if messages
                    .iter()
                    .any(|m| m["id"] == first["id"] && m["body"] == "REPLY client message")
                {
                    break;
                }
            }
        }
        sent_stop.store(true, Ordering::Relaxed);
        assert_eq!(incoming["body"], "DELIVERED_EXPLICIT_REPLY");
        assert_eq!(incoming["sender"]["id"], resident);
        let silent = PendingMail {
            destination: dest.clone(),
            request: json!({"op":"mailbox_send","client_id":client_id,"message":{"id":crate::new_id(),"to":incoming["reply_to"],"in_reply_to":incoming["id"],"subject":"Re: Desktop message","body":"SILENT no reply required"}}),
        };
        let accepted = client.operation(&dest, silent.request).unwrap();
        assert_eq!(accepted["in_reply_to"], incoming["id"]);
        let activation = client
            .operation(
                &dest,
                json!({"op":"resident_activate","id":resident,"activation_id":crate::new_id()}),
            )
            .unwrap();
        let run = activation["record"]["spec"]["id"].as_str().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let v = client
                .operation(&dest, json!({"op":"get","id":run}))
                .unwrap();
            if crate::model::terminal(&v) {
                assert_eq!(v["status"], "completed");
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
        let page = client
            .operation(&local, json!({"op":"client_inbox","client_id":client_id}))
            .unwrap();
        assert_eq!(
            page["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["sender"]["id"] == resident)
                .count(),
            1,
            "private narration was delivered as mail"
        );
        println!(
            "Desktop correspondence: remote resident discovery, exact-ID send/retry, explicit reply, reply provenance, paginated inbox and no-reply completion passed"
        );
    }
}
