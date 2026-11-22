use crate::{
    Action, Data, Desktop,
    attachments::{FileRef, Target},
    client::{Update, run_key},
    conversation as view, find_thread,
    model::{Row, ThreadKey, terminal},
    model_catalogs::Runtime,
    new_id,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct RunSettings {
    pub target: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingMove {
    key: ThreadKey,
    request: Value,
}

fn settings_for(data: &Data, key: &ThreadKey) -> RunSettings {
    data.saved
        .thread_settings
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, settings)| settings.clone())
        .unwrap_or_default()
}

fn next_run_settings(
    ui: &mut egui::Ui,
    data: &mut Data,
    key: &ThreadKey,
    enabled: bool,
    backend: &str,
    actions: &mut Vec<Action>,
) {
    if enabled
        && ui
            .add_enabled(enabled, egui::Button::new("Run settings"))
            .clicked()
    {
        data.editing_thread_settings = Some((key.clone(), settings_for(data, key)));
    }
    if data
        .editing_thread_settings
        .as_ref()
        .is_none_or(|(target, _)| target != key)
    {
        return;
    }
    let Some((_, mut settings)) = data.editing_thread_settings.take() else {
        return;
    };
    let modal =
        egui::Modal::new(egui::Id::new(("thread-run-settings", key))).show(ui.ctx(), |ui| {
            ui.heading("Run settings");
            if let Some(issue) = data.thread_issues.get(key) {
                ui.label(issue);
            }
            let before_target = settings.target.clone();
            egui::ComboBox::from_label("Machine")
                .selected_text(settings.target.as_ref().map_or_else(
                    || view::destination(data, &key.peer).label,
                    |peer| view::destination(data, peer).label,
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut settings.target,
                        None,
                        view::destination(data, &key.peer).label,
                    );
                    for d in data.saved.destinations.iter().filter(|d| d.key != key.peer) {
                        ui.selectable_value(&mut settings.target, Some(d.key.clone()), &d.label);
                    }
                });

            let mut change_directory = settings.cwd.is_some();
            ui.checkbox(&mut change_directory, "Choose a working directory");
            if change_directory {
                ui.add(
                    egui::TextEdit::singleline(settings.cwd.get_or_insert_with(String::new))
                        .hint_text("Automatic working area"),
                );
            } else {
                settings.cwd = None;
            }
            if settings.target != before_target {
                settings.model = None;
            }
            let destination =
                view::destination(data, settings.target.as_deref().unwrap_or(&key.peer));
            let target_valid =
                settings.target.is_none() || data.runtimes.available(&destination, backend);
            if settings.target.is_some() {
                data.runtimes.render_status(ui, &destination, actions);
                if !target_valid && data.runtimes.catalog(&destination).is_some() {
                    ui.label(format!(
                        "{} is unavailable on this machine.",
                        crate::runtimes::label(backend)
                    ));
                }
            }
            let mut change_model = settings.model.is_some();
            ui.add_enabled(
                target_valid,
                egui::Checkbox::new(&mut change_model, "Choose a model"),
            );
            let model_valid = if !change_model {
                settings.model = None;
                true
            } else if let Some(runtime) = Runtime::for_backend(backend).filter(|_| target_valid) {
                ui.vertical(|ui| {
                    data.model_catalogs.picker(
                        ui,
                        210.,
                        runtime,
                        Some(destination),
                        settings.model.get_or_insert_with(String::new),
                        actions,
                    )
                })
                .inner
            } else {
                ui.add(
                    egui::TextEdit::singleline(settings.model.get_or_insert_with(String::new))
                        .hint_text("Default model"),
                );
                true
            };
            ui.horizontal(|ui| {
                (
                    ui.button("Cancel").clicked(),
                    ui.add_enabled(target_valid && model_valid, egui::Button::new("Done"))
                        .clicked(),
                )
            })
            .inner
        });
    if modal.inner.1 && settings.target.is_some() {
        actions.push(Action::MoveConversation(key.clone(), settings));
    } else if modal.inner.1 {
        data.saved
            .thread_settings
            .retain(|(target, _)| target != key);
        if settings.cwd.is_some() || settings.model.is_some() {
            data.saved.thread_settings.push((key.clone(), settings));
        }
    } else if !modal.inner.0 && !modal.should_close() {
        data.editing_thread_settings = Some((key.clone(), settings));
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Steering {
    pub key: ThreadKey,
    pub run: String,
    pub id: String,
    pub text: String,
    pub files: Vec<FileRef>,
}
#[derive(Clone)]
pub struct SendIntent {
    pub key: ThreadKey,
    pub text: String,
    pub files: Vec<FileRef>,
    pub refused_id: Option<String>,
}

pub(super) fn draft_receipt(after_refusal: bool) -> &'static str {
    if after_refusal {
        "↩ refused · back to draft"
    } else {
        "Returned to draft"
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SendRoute {
    Steer,
    Followup,
    Hold,
    Draft,
}

fn send_route(head: &Value, after_refusal: bool) -> SendRoute {
    if terminal(head) || head["native_prompt"] == "ready" {
        SendRoute::Followup
    } else if after_refusal || head["native_prompt"] == "ended" || head["status"] == "cancelling" {
        SendRoute::Draft
    } else if head["record"]["spec"]["backend"] == "claude" {
        SendRoute::Hold
    } else if matches!(
        head["record"]["spec"]["backend"].as_str(),
        Some("codex" | "antigravity" | "openrouter")
    ) {
        SendRoute::Steer
    } else {
        SendRoute::Draft
    }
}

pub fn active(last: Option<&Value>) -> Option<&str> {
    last.filter(|r| !terminal(r))
        .and_then(|r| r["record"]["spec"]["id"].as_str())
}

fn prompt_ended(run: &Value) -> bool {
    matches!(run["native_prompt"].as_str(), Some("ended" | "ready"))
}

fn start_receipt(ui: &mut egui::Ui, number: usize, refused_update: Option<&str>) {
    if let Some(update) = refused_update {
        ui.weak(format!("refused → started run {number}"))
            .on_hover_text(format!("Original steering attempt {update}"));
    } else if number > 1 {
        ui.weak(format!("✓ started run {number}"));
    }
}

impl Desktop {
    pub fn move_conversation(&mut self, key: ThreadKey, settings: RunSettings) {
        let Some(target) = settings.target.as_deref() else {
            return;
        };
        let Some(last) = find_thread(&self.data, &key).and_then(|t| t.runs.last()) else {
            return;
        };
        let Some(head) = last["record"]["spec"]["id"].as_str() else {
            return;
        };
        if !terminal(last) || self.data.saved.moves.iter().any(|m| m.key == key) {
            return;
        }
        let dest = view::destination(&self.data, target);
        let backend = last["record"]["spec"]["backend"]
            .as_str()
            .unwrap_or("codex");
        if !self.data.runtimes.available(&dest, backend) {
            return;
        }
        let mut request = json!({"op":"thread_move","id":new_id(),"thread_id":key.id,
            "after":head,"target":dest.key,"address":dest.address});
        if let Some(cwd) = settings.cwd {
            request["cwd"] = json!(cwd);
        }
        if let Some(model) = settings.model {
            request["model"] = json!(model);
        }
        self.data.saved.moves.push(PendingMove {
            key: key.clone(),
            request,
        });
        self.retry_move(&key);
    }

    pub fn retry_move(&mut self, key: &ThreadKey) {
        if self.data.moving.contains(key) {
            return;
        }
        let Some(m) = self
            .data
            .saved
            .moves
            .iter()
            .find(|m| &m.key == key)
            .cloned()
        else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.persist() {
            return;
        }
        self.data.moving.insert(key.clone());
        let destination = view::destination(&self.data, &key.peer);
        let publisher = self.bus.publisher();
        std::thread::spawn(move || {
            let result = client.operation(&destination, m.request);
            publisher.send(Update::Moved(m.key, result));
        });
    }

    pub fn moved(&mut self, key: ThreadKey, result: Result<Value, String>) {
        self.data.moving.remove(&key);
        match result {
            Ok(result) if result["complete"] == true => {
                if let Some(machine) = result["machine"].as_str() {
                    self.relocate_view(&key, machine);
                    self.persist();
                }
            }
            Ok(_) => {
                self.data
                    .thread_issues
                    .insert(key, "The move is still pending.".into());
            }
            Err(error) => {
                let preflight_rejected = error.contains("Move could not be prepared:")
                    || error.ends_with(
                        "a move requires the latest run of an idle anonymous conversation",
                    )
                    || error.ends_with("conversation changed before the move could begin");
                if preflight_rejected
                    && let Some(m) = self.data.saved.moves.iter().find(|m| m.key == key)
                {
                    let settings = RunSettings {
                        target: m.request["target"].as_str().map(str::to_owned),
                        cwd: m.request["cwd"].as_str().map(str::to_owned),
                        model: m.request["model"].as_str().map(str::to_owned),
                    };
                    self.data.saved.moves.retain(|m| m.key != key);
                    self.data
                        .saved
                        .thread_settings
                        .retain(|(target, _)| target != &key);
                    self.data.editing_thread_settings = Some((key.clone(), settings));
                    self.persist();
                }
                self.data.thread_issues.insert(key, error);
            }
        }
    }

    pub fn reconcile_placements(&mut self) {
        let mut accepted = std::collections::HashMap::<String, (u64, String)>::new();
        for (peer, threads) in &self.data.catalogs {
            for thread in threads {
                if let Some(place) = thread.runs.last().map(|run| &run["placement"])
                    && place["machine"] == *peer
                    && let Some(generation) = place["generation"].as_u64()
                    && accepted
                        .get(&thread.key.id)
                        .is_none_or(|(old, _)| generation > *old)
                {
                    accepted.insert(thread.key.id.clone(), (generation, peer.clone()));
                }
            }
        }
        let old_keys: Vec<_> = self
            .data
            .saved
            .workspace
            .dock
            .iter_all_tabs()
            .filter_map(|(_, pane)| match pane {
                crate::model::Pane::Thread(key) => Some(key.clone()),
                _ => None,
            })
            .chain(self.data.saved.drafts.iter().map(|(key, _)| key.clone()))
            .chain(self.data.saved.moves.iter().map(|m| m.key.clone()))
            .collect();
        let mut changed = false;
        for key in old_keys {
            if let Some((_, peer)) = accepted.get(&key.id)
                && peer != &key.peer
            {
                self.relocate_view(&key, peer);
                changed = true;
            }
        }
        for (peer, threads) in &mut self.data.catalogs {
            threads.retain(|thread| {
                accepted
                    .get(&thread.key.id)
                    .is_none_or(|(_, target)| target == peer)
            });
        }
        if changed {
            self.persist();
        }
    }

    fn relocate_view(&mut self, key: &ThreadKey, machine: &str) {
        let next = ThreadKey {
            peer: machine.into(),
            id: key.id.clone(),
        };
        self.data.saved.workspace.relocate_thread(key, &next);
        if let Some(view) = self.data.views.remove(key) {
            self.data.views.entry(next.clone()).or_insert(view);
        }
        if let Some(thread) = find_thread(&self.data, key) {
            let runs: Vec<_> = thread
                .runs
                .iter()
                .filter_map(|run| run["record"]["spec"]["id"].as_str().map(str::to_owned))
                .collect();
            for run in runs {
                if self
                    .data
                    .saved
                    .reply_watches
                    .get_mut(&key.peer)
                    .is_some_and(|ids| ids.remove(&run))
                {
                    self.data
                        .saved
                        .reply_watches
                        .entry(next.peer.clone())
                        .or_default()
                        .insert(run.clone());
                }
                let old_tag = run_key(&key.peer, &run);
                let new_tag = run_key(machine, &run);
                if let Some(update) = self.data.saved.refused_followups.remove(&old_tag) {
                    self.data
                        .saved
                        .refused_followups
                        .insert(new_tag.clone(), update);
                }
                if let Some(transcript) = self.data.transcripts.remove(&old_tag) {
                    self.data
                        .transcripts
                        .entry(new_tag)
                        .or_insert(transcript);
                }
            }
        }
        for reply in &mut self.data.saved.thread_replies {
            if &reply.key == key {
                reply.key = next.clone();
            }
        }
        for (draft, _) in &mut self.data.saved.drafts {
            if draft == key {
                *draft = next.clone();
            }
        }
        for (draft, _) in &mut self.data.saved.thread_files {
            if draft == key {
                *draft = next.clone();
            }
        }
        if self.data.saved.activity_views.remove(key) {
            self.data.saved.activity_views.insert(next.clone());
        }
        if self.data.selected_thread.as_ref() == Some(key) {
            self.data.selected_thread = Some(next);
        }
        self.data.saved.moves.retain(|m| &m.key != key);
        self.data
            .saved
            .thread_settings
            .retain(|(target, _)| target != key);
        self.data.thread_issues.remove(key);
        if let Some(threads) = self.data.catalogs.get_mut(&key.peer) {
            threads.retain(|thread| &thread.key != key);
        }
    }

    pub fn send(&mut self, key: ThreadKey) {
        if self.data.saved.moves.iter().any(|m| m.key == key)
            || self.data.hold_pending.contains(&key)
            || self
                .data
                .saved
                .steering
                .iter()
                .any(|message| message.key == key)
            || self
                .data
                .preparing_followups
                .values()
                .any(|intent| intent.key == key)
            || self.data.saved.pending.iter().any(|message| {
                message.destination.key == key.peer && message.spec["thread_id"] == key.id
            })
        {
            return;
        }
        let text = self
            .data
            .saved
            .drafts
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, text)| text.clone())
            .unwrap_or_default();
        let files = self
            .data
            .saved
            .thread_files
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, files)| files.clone())
            .unwrap_or_default();
        if (text.trim().is_empty() && files.is_empty()) || text.len() > 256 * 1024 {
            return;
        }
        if !self.persist() {
            return;
        }
        self.check_send(SendIntent {
            key,
            text,
            files,
            refused_id: None,
        });
    }

    fn check_send(&mut self, intent: SendIntent) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.data.send_checking.insert(intent.key.clone()) {
            return;
        }
        let destination = view::destination(&self.data, &intent.key.peer);
        let publisher = self.bus.publisher();
        std::thread::spawn(move || {
            let head = client.conversation_head(&destination, &intent.key.id);
            publisher.send(Update::SendChecked(intent, destination, head));
        });
    }

    pub fn send_checked(
        &mut self,
        intent: SendIntent,
        dest: crate::Destination,
        head: Result<Value, String>,
    ) {
        let SendIntent {
            key,
            text,
            files,
            refused_id,
        } = intent.clone();
        let after_refusal = refused_id.is_some();
        self.data.send_checking.remove(&key);
        let head = match head {
            Ok(head) => head,
            Err(error) => {
                self.restore_send_intent(&intent);
                let receipt = draft_receipt(after_refusal);
                self.data
                    .thread_issues
                    .insert(key, format!("{receipt} · {error}"));
                return;
            }
        };
        if view::destination(&self.data, &key.peer).address != dest.address
            || head["record"]["spec"]["thread_id"] != key.id
            || self.data.saved.moves.iter().any(|entry| entry.key == key)
        {
            self.restore_send_intent(&intent);
            let receipt = draft_receipt(after_refusal);
            self.data
                .thread_issues
                .insert(key, format!("{receipt} · conversation route changed"));
            return;
        }
        let (Some(run), Some(client)) =
            (head["record"]["spec"]["id"].as_str(), self.client.clone())
        else {
            self.restore_send_intent(&intent);
            return;
        };
        self.data.thread_issues.remove(&key);
        match send_route(&head, after_refusal) {
            SendRoute::Draft => {
                self.restore_send_intent(&intent);
                self.data.thread_issues.insert(
                    key,
                    if after_refusal {
                        draft_receipt(true).into()
                    } else {
                        "Returned to draft · the conversation cannot take input yet".into()
                    },
                );
            }
            SendRoute::Hold => self.hold_message(key, run.into(), text, files),
            SendRoute::Followup => {
                let settings = settings_for(&self.data, &key);
                let id = new_id();
                self.data
                    .preparing_followups
                    .insert(run_key(&dest.key, run), intent);
                client.followup(
                    dest,
                    run.into(),
                    id,
                    text,
                    files,
                    settings,
                    self.bus.publisher(),
                );
            }
            SendRoute::Steer => {
                let steer = Steering {
                    key: key.clone(),
                    run: run.into(),
                    id: new_id(),
                    text,
                    files,
                };
                self.data.saved.steering.push(steer.clone());
                if !self.persist() {
                    self.data
                        .saved
                        .steering
                        .retain(|message| message.id != steer.id);
                    return;
                }
                self.data.fresh_steering.insert(steer.id.clone());
                self.data.active_steering.insert(key);
                let publisher = self.bus.publisher();
                std::thread::spawn(move || {
                    let files = json!(steer.files);
                    let (result, attempted) = match client.copy_files(&dest, &files) {
                        Err(error) => (Err(error), false),
                        Ok(()) => (client.operation(&dest, json!({"op":"control","id":steer.run,
                            "action":{"type":"steer","text":steer.text,"attachments":files,"update_id":steer.id}})), true),
                    };
                    publisher.send(Update::Steered(steer.key, steer.id, result, attempted));
                });
            }
        }
    }

    pub fn retire_refused_steer(&mut self, id: &str) {
        self.data.saved.steering.retain(|entry| entry.id != id);
        self.data.fallback_inflight.remove(id);
    }

    pub fn restore_send_intent(&mut self, intent: &SendIntent) {
        self.restore_thread_draft(&intent.key, &intent.text, &intent.files);
        if let Some(id) = &intent.refused_id {
            self.retire_refused_steer(id);
        }
        self.persist();
    }

    pub fn return_steer(&mut self, steering: Steering, can_followup: bool, reason: String) {
        if self.data.fallback_inflight.contains(&steering.id) {
            return;
        }
        self.data.active_steering.remove(&steering.key);
        let authorizes_fallback = self.data.fresh_steering.remove(&steering.id);
        let intent = SendIntent {
            key: steering.key,
            text: steering.text,
            files: steering.files,
            refused_id: Some(steering.id.clone()),
        };
        if can_followup && authorizes_fallback && self.client.is_some() {
            self.data.fallback_inflight.insert(steering.id);
            self.check_send(intent);
        } else {
            self.restore_send_intent(&intent);
            self.data
                .thread_issues
                .insert(intent.key, format!("Returned to draft · {reason}"));
        }
    }

    pub fn steered(
        &mut self,
        key: ThreadKey,
        update: String,
        result: Result<Value, String>,
        attempted: bool,
    ) {
        let Some(steering) = self
            .data
            .saved
            .steering
            .iter()
            .find(|s| s.id == update && s.key == key)
            .cloned()
        else {
            return;
        };
        self.data.active_steering.remove(&key);
        if let Err(error) = result {
            if !attempted || error.starts_with("HTTP 422:") {
                let ended = attempted && error.trim_end() == "HTTP 422: run is no longer active";
                self.return_steer(steering, ended, error);
            } else {
                self.data.fresh_steering.remove(&update);
                self.data
                    .thread_issues
                    .insert(key, "? not confirmed · draft kept".into());
            }
        }
    }

    pub fn reconcile_steering(&mut self, tag: &str) {
        let Some(transcript) = self.data.transcripts.get(tag) else {
            return;
        };
        let frontier_reached = self
            .data
            .watches
            .get(tag)
            .and_then(|watch| watch.frontier)
            .is_some_and(|frontier| transcript.seq >= frontier);
        let run_terminal = frontier_reached && transcript.status.as_ref().is_some_and(terminal);
        let done: Vec<_> = self
            .data
            .saved
            .steering
            .iter()
            .filter(|s| run_key(&s.key.peer, &s.run) == tag)
            .filter_map(|s| {
                if transcript
                    .rows
                    .iter()
                    .any(|row| row.update_id.as_deref() == Some(&s.id))
                {
                    Some((s.clone(), Ok(())))
                } else if let Some(refusal) = transcript.rejected_updates.get(&s.id) {
                    Some((s.clone(), Err(refusal.clone())))
                } else if run_terminal && transcript.queued_updates.contains(&s.id) {
                    Some((
                        s.clone(),
                        Err((
                            "Run ended before the queued message was incorporated".into(),
                            false,
                        )),
                    ))
                } else {
                    None
                }
            })
            .collect();
        for (steering, outcome) in done {
            match outcome {
                Ok(()) => {
                    self.data
                        .saved
                        .steering
                        .retain(|entry| entry.id != steering.id);
                    self.data.active_steering.remove(&steering.key);
                    self.data.fresh_steering.remove(&steering.id);
                    self.clear_submitted_draft(&steering.key, &steering.text, &steering.files);
                    self.data.thread_issues.remove(&steering.key);
                    self.persist();
                }
                Err((reason, confirmed_not_sent)) => {
                    self.return_steer(steering, confirmed_not_sent, reason)
                }
            }
        }
    }
}

fn thread_status(
    data: &Data,
    key: &ThreadKey,
    last: Option<&Value>,
    native_ended: bool,
    accepts_followup: bool,
) -> (String, egui::Color32) {
    use crate::console_style::{ACCENT, ERROR, OK, WARN};
    if let Some(error) = data.errors.get(&key.peer) {
        return (format!("machine unavailable · {error}"), ERROR);
    }
    if native_ended {
        return (
            if accepts_followup {
                "background command open".into()
            } else {
                "waiting for outstanding work".into()
            },
            ACCENT,
        );
    }
    let Some(last) = last else {
        return ("loading thread catalog…".into(), egui::Color32::GRAY);
    };
    match last["status"].as_str() {
        Some("starting" | "running") => ("running".into(), ACCENT),
        Some("waiting" | "blocked") => {
            let tag = run_key(
                &key.peer,
                last["record"]["spec"]["id"].as_str().unwrap_or(""),
            );
            let detail = data
                .transcripts
                .get(&tag)
                .and_then(|transcript| transcript.status.as_ref())
                .filter(|status| status["status"] == last["status"])
                .and_then(|status| status["detail"].as_str())
                .filter(|detail| !detail.is_empty())
                .unwrap_or("waiting for input");
            (detail.to_owned(), WARN)
        }
        Some("failed") => ("run failed".into(), ERROR),
        Some("interrupted") => ("run interrupted".into(), ERROR),
        Some("cancelled") => ("cancelled".into(), egui::Color32::GRAY),
        Some("completed") => ("done".into(), OK),
        Some(other) => (other.to_owned(), egui::Color32::GRAY),
        None => ("unknown".into(), egui::Color32::GRAY),
    }
}

pub fn panel(ui: &mut egui::Ui, data: &mut Data, key: &ThreadKey, actions: &mut Vec<Action>) {
    // Threads are one timeline: steps collapse between messages.
    data.saved.activity_views.remove(key);
    let activity = false;
    let dest = view::destination(data, &key.peer);
    let runs = find_thread(data, key)
        .map(|t| t.runs.clone())
        .unwrap_or_default();
    let last = runs.last();
    let running = active(last).map(str::to_owned);
    let native_ended = running.is_some() && last.is_some_and(prompt_ended);
    let accepts_followup = last.is_some_and(|run| run["native_prompt"] == "ready");
    let moving = data.saved.moves.iter().any(|m| &m.key == key);
    let pending = data
        .saved
        .pending
        .iter()
        .enumerate()
        .find(|(_, p)| p.destination.key == key.peer && p.spec["thread_id"] == key.id)
        .map(|(i, p)| (i, p.spec["id"].as_str().unwrap_or("").to_owned()));
    let waiting = data.saved.steering.iter().find(|s| s.key == *key).cloned();
    let target = Target::Thread(key.clone());
    let errors = data
        .preparing
        .get(&target)
        .map_or(0, |p| usize::from(!p.errors.is_empty()));
    let mut draft = data
        .saved
        .drafts
        .iter_mut()
        .find(|(k, _)| k == key)
        .map(|(_, d)| std::mem::take(d))
        .unwrap_or_default();
    let mut files = data
        .saved
        .thread_files
        .iter_mut()
        .find(|(k, _)| k == key)
        .map(|(_, f)| std::mem::take(f))
        .unwrap_or_default();
    let settings_enabled = last.is_some()
        && running.is_none()
        && pending.is_none()
        && !moving
        && !data
            .preparing_followups
            .values()
            .any(|intent| &intent.key == key);
    let backend = last
        .and_then(|run| run["record"]["spec"]["backend"].as_str())
        .unwrap_or("codex")
        .to_owned();
    let (status, status_color) = thread_status(data, key, last, native_ended, accepts_followup);
    let live_id = running.as_deref().unwrap_or("");
    let live_tag = run_key(&key.peer, live_id);
    let execution = last.map(|last| {
        let tag = run_key(
            &key.peer,
            last["record"]["spec"]["id"].as_str().unwrap_or(""),
        );
        data.transcripts
            .get(&tag)
            .and_then(|transcript| transcript.execution.clone())
            .unwrap_or_else(|| last["record"]["execution"].clone())
    });
    let mut open_settings = false;
    ui.horizontal(|ui| {
        crate::console_style::dot(ui, status_color);
        ui.add(
            egui::Label::new(
                egui::RichText::new(&status)
                    .monospace()
                    .size(10.5)
                    .color(status_color),
            )
            .truncate(),
        );
        if let Some(created) = running
            .is_some()
            .then(|| last.and_then(|run| run["record"]["created_at"].as_u64()))
            .flatten()
        {
            ui.label(
                egui::RichText::new(format!("· {}", crate::pulse::age(created)))
                    .monospace()
                    .size(10.5)
                    .weak(),
            );
        }
        if last.is_some() {
            view::context_label(ui, data, key, &runs);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::containers::menu::MenuButton::from_button(
                egui::Button::new(egui::RichText::new("···").size(13.)).frame(false),
            )
            .ui(ui, |ui| {
                ui.set_min_width(220.);
                if ui
                    .add_enabled(settings_enabled, egui::Button::new("Run settings…"))
                    .clicked()
                {
                    open_settings = true;
                    ui.close();
                }
                if running.is_some() && ui.button("Cancel run").clicked() {
                    actions.push(Action::Cancel(key.clone()));
                    ui.close();
                }
                if let Some(execution) = execution.as_ref().filter(|e| e.is_object()) {
                    ui.separator();
                    ui.label(
                        egui::RichText::new("EXECUTION")
                            .monospace()
                            .size(10.)
                            .weak(),
                    );
                    ui.label(format!("Machine · {}", dest.label));
                    ui.label(format!(
                        "Account · {}",
                        execution["account"].as_str().unwrap_or("Unknown")
                    ));
                    ui.label(format!(
                        "Session · {}",
                        execution["session"].as_str().unwrap_or("Unknown")
                    ));
                    let privileges = match execution["elevated"].as_bool() {
                        Some(true) => "Elevated",
                        Some(false) => "Standard user",
                        None => "Unknown",
                    };
                    ui.label(format!("Privileges · {privileges}"));
                    ui.label(format!(
                        "Directory · {}",
                        execution["cwd"]
                            .as_str()
                            .or_else(|| last.and_then(|l| l["record"]["spec"]["cwd"].as_str()))
                            .unwrap_or("Unknown")
                    ));
                    if let Some(available) = execution["desktop_available"].as_bool() {
                        ui.label(if available {
                            "Desktop session available"
                        } else {
                            "Desktop session unavailable"
                        });
                    }
                }
            })
            .0
            .on_hover_text("Run settings, cancel and execution context");
        });
    });
    ui.separator();
    if open_settings {
        data.editing_thread_settings = Some((key.clone(), settings_for(data, key)));
    }
    next_run_settings(ui, data, key, false, &backend, actions);
    if let Some(error) = data.watches.get(&live_tag).and_then(|w| w.error.as_ref()) {
        ui.label(format!("Reconnecting to activity · {error}"));
    }
    let latest_reply = runs.iter().rev().find_map(|run| {
        let id = run["record"]["spec"]["id"].as_str()?;
        data.transcripts
            .get(&run_key(&key.peer, id))?
            .rows
            .iter()
            .rev()
            .find(|r| r.label == "Agent")
            .map(|r| format!("{id}:{}", r.id))
    });
    let attached: Vec<_> = runs
        .iter()
        .flat_map(|run| {
            let id = run["record"]["spec"]["id"].as_str().unwrap_or("");
            data.transcripts
                .get(&run_key(&key.peer, id))
                .into_iter()
                .flat_map(move |t| {
                    t.rows.iter().flat_map(move |r| {
                        r.attachments
                            .iter()
                            .map(move |f| (format!("{id}:{}", r.id), f.clone()))
                    })
                })
        })
        .collect();
    let mut file_anchor = None;
    if !attached.is_empty() {
        ui.menu_button(format!("Files ({})", attached.len()), |ui| {
            egui::ScrollArea::vertical()
                .max_height(260.)
                .show(ui, |ui| {
                    for (source, file) in &attached {
                        if ui.button(&file.name).on_hover_text(file.info()).clicked() {
                            file_anchor = Some(source.clone());
                            ui.close();
                        }
                    }
                });
        });
    }
    if let Some(source) = file_anchor {
        view::switch(data, key, activity, Some(source));
    }
    // A split pane may have room for a decision and the composer, with the
    // journal itself below the fold. Keep the current decision reachable.
    let compact = ui.available_height() < 220.;
    if compact
        && !data.errors.contains_key(&key.peer)
        && last.is_some_and(|run| crate::needs::replay_current(data, &live_tag, run))
        && let Some(transcript) = data.transcripts.get(&live_tag)
    {
        for row in transcript.rows.iter().filter(|row| row.request.is_some()) {
            backend_request(ui, row, key, live_id, actions);
        }
    }
    let extra = errors
        + usize::from(last.is_some())
        + usize::from(data.send_checking.contains(key))
        + usize::from(moving)
        + usize::from(waiting.is_some())
        + usize::from(pending.is_some())
        + usize::from(data.thread_issues.contains_key(key))
        + usize::from(
            data.preparing_followups
                .values()
                .any(|intent| &intent.key == key),
        );
    view::history(
        ui,
        data,
        key,
        activity,
        view::reserve(&draft, &files, extra, ui.available_width()),
        |ui, data| {
            if runs.is_empty() && pending.is_none() {
                ui.weak("No retained messages yet.");
            }
            for (index, run) in runs.iter().enumerate() {
                let number = index + 1;
                let id = run["record"]["spec"]["id"].as_str().unwrap_or("");
                let tag = run_key(&key.peer, id);
                // Completed Messages blocks are static once replay reaches the terminal
                // sequence. Activity can contain independently expanding controls.
                let layout = (!activity && terminal(run))
                    .then(|| {
                        let seq = data.transcripts.get(&tag)?.seq;
                        (seq >= run["last_seq"].as_u64()?).then(|| view::MessageLayout {
                            sequence: seq,
                            width: ui.available_width(),
                            pixels_per_point: ui.ctx().pixels_per_point(),
                            latest_reply: latest_reply
                                .as_deref()
                                .and_then(|anchor| anchor.split_once(':'))
                                .is_some_and(|(run, _)| run == id),
                        })
                    })
                    .flatten();
                if let Some(bounds) =
                    layout.and_then(|layout| data.views.get(key)?.offscreen_run(ui, id, layout))
                {
                    ui.advance_cursor_after_rect(bounds);
                    continue;
                }
                let origin = ui.next_widget_position();
                let response = ui.push_id(&tag, |ui| {
                    let live = !compact
                        && !data.errors.contains_key(&key.peer)
                        && crate::needs::replay_current(data, &tag, run);
                    let moved = run["machine"]
                        .as_str()
                        .filter(|m| *m != key.peer)
                        .map(|machine| {
                            data.saved
                                .destinations
                                .iter()
                                .find(|dest| dest.key == machine)
                                .map(|dest| dest.label.clone())
                                .unwrap_or_else(|| {
                                    format!(
                                        "Machine {}",
                                        machine.chars().take(8).collect::<String>()
                                    )
                                })
                        });
                    let end = data
                        .transcripts
                        .get(&tag)
                        .and_then(|t| t.rows.iter().filter_map(|r| r.at).max());
                    let detail = data
                        .transcripts
                        .get(&tag)
                        .and_then(|t| t.status.as_ref())
                        .filter(|status| status["status"] == run["status"])
                        .and_then(|status| status["detail"].as_str())
                        .filter(|d| !d.is_empty())
                        .map(str::to_owned);
                    view::run_divider(ui, run, number, end, detail.as_deref(), moved.as_deref());
                    let transcript = data.transcripts.get(&tag);
                    let initial_message = transcript.and_then(|t| t.initial_message.as_deref());
                    let refused_update = data.saved.refused_followups.get(&tag).map(String::as_str);
                    let spec = &run["record"]["spec"];
                    let (prompt, mut files) = transcript
                        .and_then(|transcript| transcript.accepted_input.as_ref())
                        .map(|(prompt, files)| (prompt.as_str(), files.clone()))
                        .unwrap_or_else(|| {
                            (
                                spec["prompt"].as_str().unwrap_or(""),
                                FileRef::parse(&spec["attachments"]),
                            )
                        });
                    if initial_message.is_none()
                        && run["record"]["imported"].is_null()
                        && (!prompt.is_empty() || !files.is_empty())
                    {
                        view::message_row(
                            ui,
                            run["record"]["created_at"].as_u64(),
                            view::MessageAuthor::You,
                            prompt,
                            false,
                            |ui| {
                                view::chips(
                                    ui,
                                    &mut files,
                                    &dest,
                                    false,
                                    &mut data.file_previews,
                                    actions,
                                );
                                start_receipt(ui, number, refused_update);
                            },
                        );
                        ui.add_space(2.);
                    }
                    let rows = transcript
                        .map(|t| t.rows.as_slice())
                        .unwrap_or_default();
                    let mut group_start = 0;
                    for (i, row) in rows.iter().enumerate() {
                        if view::is_message(row) {
                            render_rows(
                                ui,
                                &rows[group_start..i],
                                key,
                                id,
                                live,
                                &mut data.views,
                                actions,
                            );
                            let anchor = format!("{id}:{}", row.id);
                            let mut attached = row.attachments.clone();
                            let outgoing = row.label.starts_with("You");
                            let response = ui
                                .push_id(&anchor, |ui| {
                                    view::message_row(
                                        ui,
                                        row.at,
                                        if outgoing {
                                            view::MessageAuthor::You
                                        } else {
                                            view::MessageAuthor::Assistant
                                        },
                                        &row.text,
                                        !outgoing,
                                        |ui| {
                                            view::chips(
                                                ui,
                                                &mut attached,
                                                &dest,
                                                false,
                                                &mut data.file_previews,
                                                actions,
                                            );
                                            if let Some(update) = &row.update_id {
                                                let was_queued = transcript.is_some_and(|t| {
                                                    t.queued_updates.contains(update)
                                                });
                                                ui.weak(if was_queued {
                                                    format!("✓ incorporated into run {number}")
                                                } else {
                                                    format!("✓ steered into run {number}")
                                                });
                                            } else if initial_message == Some(row.id.as_str()) {
                                                start_receipt(ui, number, refused_update);
                                            }
                                        },
                                    )
                                })
                                .inner;
                            view::record(ui, &mut data.views, key, &anchor, response.rect);
                            if data.selected_thread.as_ref() == Some(key)
                                && ui.is_rect_visible(response.rect)
                            {
                                data.saved.thread_replies.retain(|reply| {
                                    &reply.key != key || reply.run != id || reply.row != row.id
                                });
                            }
                            ui.add_space(2.);
                            group_start = i + 1;
                        }
                    }
                    render_rows(
                        ui,
                        &rows[group_start..],
                        key,
                        id,
                        live,
                        &mut data.views,
                        actions,
                    );
                    if rows.is_empty()
                        && transcript.map_or(0, |t| t.seq) < run["last_seq"].as_u64().unwrap_or(0)
                    {
                        ui.weak("Loading retained conversation…");
                    }
                    if let Some(hold) = data
                            .transcripts
                            .get(&tag)
                            .and_then(|transcript| transcript.hold.as_ref())
                    {
                        let successor = runs.iter().position(|run| {
                            run["record"]["spec"]["id"] == hold["run_id"]
                        }).map(|index| index + 1);
                        crate::hold::panel(ui, data, key, id, (number, successor), hold, actions);
                    }
                });
                if let Some(layout) = layout
                    && let Some(view) = data.views.get_mut(key)
                {
                    view.measure_run(id, layout, origin, response.response.rect);
                }
            }
        },
    );
    if moving {
        ui.horizontal_wrapped(|ui| {
            if data.moving.contains(key) {
                ui.label("Moving conversation…");
            } else if ui.button("Retry same move").clicked() {
                actions.push(Action::RetryMove(key.clone()));
            }
        });
    }
    if let Some((index, id)) = &pending {
        ui.horizontal_wrapped(|ui| {
            let sending = data.sending.contains(id);
            ui.label(if sending {
                "Sending…"
            } else {
                "Request acceptance is unconfirmed."
            });
            if !sending && ui.small_button("Retry same request").clicked() {
                actions.push(Action::Retry(*index));
            }
        });
    }
    if data
        .preparing_followups
        .values()
        .any(|intent| &intent.key == key)
    {
        ui.weak("Preparing follow-up…");
    }
    if data.send_checking.contains(key) {
        ui.weak("Checking which run owns input…");
    }
    if let Some(issue) = data.thread_issues.get(key) {
        ui.label(issue);
    }
    if let Some(steering) = waiting {
        ui.horizontal_wrapped(|ui| {
            let tag = run_key(&key.peer, &steering.run);
            let queued = data
                .transcripts
                .get(&tag)
                .is_some_and(|t| t.queued_updates.contains(&steering.id));
            if queued {
                if let Some(index) = runs
                    .iter()
                    .position(|run| run["record"]["spec"]["id"] == steering.run)
                {
                    ui.weak(format!("○ queued for run {}", index + 1));
                } else {
                    ui.weak("○ queued");
                }
            } else if data.active_steering.contains(key) {
                ui.weak("Sending…");
            } else if running.as_deref() != Some(&steering.run)
                || data.thread_issues.contains_key(key)
            {
                ui.weak("? not confirmed · draft kept");
            } else {
                ui.weak("Awaiting runtime");
            }
            if !data.active_steering.contains(key) && ui.small_button("Keep as draft").clicked() {
                actions.push(Action::RestoreSteer(steering));
            }
        });
    }
    let hint = last.map(|run| match send_route(run, false) {
        SendRoute::Hold => format!("→ held until run {} settles", runs.len()),
        SendRoute::Followup => format!("→ starts run {}", runs.len() + 1),
        SendRoute::Steer if run["record"]["spec"]["backend"] == "openrouter" => {
            "→ queued for next step".into()
        }
        SendRoute::Steer => format!("→ steers run {}", runs.len()),
        SendRoute::Draft => "→ waiting for a sendable run".into(),
    });
    let frozen = moving
        || data.send_checking.contains(key)
        || data.hold_pending.contains(key)
        || pending.is_some()
        || data.saved.steering.iter().any(|s| s.key == *key)
        || data
            .preparing_followups
            .values()
            .any(|intent| &intent.key == key);
    let compose = view::composer(
        ui,
        data,
        view::Composer {
            text: &mut draft,
            files: &mut files,
            target,
            label: "Send",
            hint,
            placeholder: "Message…".into(),
            limit: 256 * 1024,
            blocked: frozen || last.is_none(),
        },
        actions,
    );
    if compose.changed && (!draft.is_empty() || !files.is_empty()) {
        actions.push(Action::Pin(key.clone()));
    }
    data.saved.drafts.retain(|(k, _)| k != key);
    data.saved.thread_files.retain(|(k, _)| k != key);
    if !draft.is_empty() {
        data.saved.drafts.push((key.clone(), draft));
    }
    if !files.is_empty() {
        data.saved.thread_files.push((key.clone(), files));
    }
    if compose.send {
        actions.push(Action::Send(key.clone()));
    }
}
fn render_rows(
    ui: &mut egui::Ui,
    rows: &[Row],
    key: &ThreadKey,
    run: &str,
    live: bool,
    views: &mut std::collections::HashMap<ThreadKey, view::View>,
    actions: &mut Vec<Action>,
) {
    let mut start = 0;
    for (i, row) in rows.iter().enumerate() {
        if view::incident(row) {
            view::activity_rows(ui, &rows[start..i]);
            let response = ui
                .scope(|ui| {
                    if live && row.request.is_some() {
                        backend_request(ui, row, key, run, actions);
                    } else {
                        view::activity_row(ui, row);
                    }
                })
                .response;
            view::record(ui, views, key, &format!("{run}:{}", row.id), response.rect);
            start = i + 1;
        }
    }
    view::activity_rows(ui, &rows[start..]);
}
fn antigravity_input(
    ui: &mut egui::Ui,
    request: &Value,
    key: &ThreadKey,
    run: &str,
    id: &Value,
    actions: &mut Vec<Action>,
    mut first: Option<bool>,
) {
    ui.strong(
        request["question"]
            .as_str()
            .unwrap_or("Antigravity asks for input"),
    );
    if let Some(choices) = request["choices"].as_array() {
        ui.horizontal_wrapped(|ui| {
            for choice in choices {
                if let (Some(label), Some(option)) =
                    (choice["label"].as_str(), choice["id"].as_str())
                    && decision_button(ui, label, &mut first)
                {
                    actions.push(Action::Respond(
                        key.clone(),
                        run.into(),
                        id.clone(),
                        json!({"option_id":option}),
                    ));
                }
            }
        });
        return;
    }
    let schema = &request["requested_schema"];
    let properties = schema["properties"].as_object();
    let single = properties
        .filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.iter().next())
        .and_then(|(field, spec)| Some((field, spec["enum"].as_array()?)));
    ui.horizontal_wrapped(|ui| {
        if let Some((field, choices)) = single {
            for choice in choices {
                if decision_button(ui, crate::model::display(choice), &mut first) {
                    actions.push(Action::Respond(
                        key.clone(),
                        run.into(),
                        id.clone(),
                        json!({"action":"accept","content":{(field):choice}}),
                    ));
                }
            }
        }
        for (label, action) in [("Decline", "decline"), ("Cancel", "cancel")] {
            if ui.button(label).clicked() {
                actions.push(Action::Respond(
                    key.clone(),
                    run.into(),
                    id.clone(),
                    json!({"action":action}),
                ));
            }
        }
    });
    if single.is_none() {
        ui.collapsing("Requested form schema", |ui| {
            ui.monospace(crate::model::display(schema));
        });
        if let Some(content) = backend_json_input(ui, "Form values (JSON)")
            && ui.small_button("Submit form").clicked()
        {
            actions.push(Action::Respond(
                key.clone(),
                run.into(),
                id.clone(),
                json!({"action":"accept","content":content}),
            ));
        }
    }
}

pub(crate) fn backend_request(
    ui: &mut egui::Ui,
    row: &Row,
    key: &ThreadKey,
    run: &str,
    actions: &mut Vec<Action>,
) {
    // A pending request reads as a gate in the timeline: amber rule and wash.
    let pending = row.answer.as_ref().is_none_or(|answer| !answer.waiting());
    let response = egui::Frame::new()
        .fill(if pending {
            egui::Color32::from_rgb(26, 22, 14)
        } else {
            egui::Color32::TRANSPARENT
        })
        .inner_margin(egui::Margin {
            left: 10,
            right: 6,
            top: 5,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            request_controls(ui, row, key, run, actions, None);
        })
        .response;
    if pending {
        ui.painter().vline(
            response.rect.left() + 1.5,
            response.rect.y_range(),
            egui::Stroke::new(3., crate::console_style::WARN),
        );
    }
}

pub(crate) fn request_controls(
    ui: &mut egui::Ui,
    row: &Row,
    key: &ThreadKey,
    run: &str,
    actions: &mut Vec<Action>,
    mut first: Option<bool>,
) {
    let Some(req) = &row.request else { return };
    let method = req["method"].as_str().unwrap_or("");
    let p = &req["params"];
    ui.push_id((&row.id, run), |ui| {
        if let Some(answer) = &row.answer {
            ui.weak(answer.label());
            if answer.waiting() {
                return;
            }
        }
        if method == "antigravity/input" {
            antigravity_input(ui, p, key, run, &req["id"], actions, first);
            return;
        }
        if method == COMMAND_APPROVAL {
            if let Some(decision) = command_approval(ui, p, first) {
                actions.push(Action::Respond(
                    key.clone(),
                    run.into(),
                    req["id"].clone(),
                    json!({"decision":decision}),
                ));
            }
            return;
        }
        let elicitation = method == "mcpServer/elicitation/request";
        ui.strong(if method.contains("requestApproval") || elicitation {
            "Approval requested"
        } else {
            "Input requested"
        });
        if let Some(command) = p["command"].as_str() {
            ui.monospace(command);
        }
        if let Some(reason) = p["reason"].as_str() {
            ui.label(reason);
        }
        if let Some(message) = p["message"].as_str() {
            ui.label(message);
        }
        if elicitation {
            if let Some(server) = p["serverName"].as_str() {
                ui.weak(format!("MCP server · {server}"));
            }
            for parameter in p["_meta"]["tool_params_display"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let name = parameter["display_name"].as_str().unwrap_or("Parameter");
                let value = crate::model::display(&parameter["value"]);
                ui.label(format!("{name} · {value}"));
            }
            if let Some(url) = p["url"].as_str() {
                ui.hyperlink(url);
            }
            let mut content = Some(Value::Null);
            if p["mode"] != "url" {
                if p["requestedSchema"]["type"] == "object"
                    && p["requestedSchema"]["properties"]
                        .as_object()
                        .is_some_and(|properties| properties.is_empty())
                {
                    content = Some(json!({}));
                } else {
                    ui.collapsing("Requested form schema", |ui| {
                        ui.monospace(crate::model::display(&p["requestedSchema"]));
                    });
                    content = backend_json_input(ui, "Form values (JSON)");
                }
            }
            ui.horizontal_wrapped(|ui| {
                for (label, action) in [
                    ("Approve", "accept"),
                    ("Decline", "decline"),
                    ("Cancel", "cancel"),
                ] {
                    let enabled = action != "accept" || content.is_some();
                    let chosen = if enabled && action == "accept" {
                        decision_button(ui, label, &mut first)
                    } else {
                        ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                    };
                    if chosen {
                        let values = if action == "accept" {
                            content.clone()
                        } else {
                            None
                        };
                        actions.push(Action::Respond(
                            key.clone(),
                            run.into(),
                            req["id"].clone(),
                            json!({"action":action,"content":values}),
                        ));
                    }
                }
            });
        } else if method.contains("requestApproval") {
            ui.horizontal_wrapped(|ui| {
                for (label, decision) in [
                    ("Approve", "accept"),
                    ("Decline", "decline"),
                    ("Cancel", "cancel"),
                ] {
                    if decision_button(ui, label, &mut first) {
                        actions.push(Action::Respond(
                            key.clone(),
                            run.into(),
                            req["id"].clone(),
                            json!({"decision":decision}),
                        ));
                    }
                }
            });
        } else if let Some(questions) = p["questions"].as_array() {
            let mut answers = serde_json::Map::new();
            for q in questions {
                let id = q["id"].as_str().unwrap_or("");
                let field = ui.make_persistent_id(id);
                let mut answer = ui
                    .data_mut(|d| d.get_temp::<String>(field))
                    .unwrap_or_default();
                ui.label(q["question"].as_str().unwrap_or("Question"));
                for option in q["options"].as_array().into_iter().flatten() {
                    let label = option["label"].as_str().unwrap_or("");
                    if ui.selectable_label(answer == label, label).clicked() {
                        answer = label.into();
                    }
                }
                ui.text_edit_singleline(&mut answer);
                ui.data_mut(|d| d.insert_temp(field, answer.clone()));
                answers.insert(id.into(), json!({"answers":[answer]}));
            }
            if ui.button("Reply").clicked() {
                actions.push(Action::Respond(
                    key.clone(),
                    run.into(),
                    req["id"].clone(),
                    json!({"answers":answers}),
                ));
            }
        } else {
            ui.monospace(crate::model::display(req));
            let result = backend_json_input(ui, "Response (JSON)");
            if ui
                .add_enabled(result.is_some(), egui::Button::new("Reply"))
                .clicked()
            {
                actions.push(Action::Respond(
                    key.clone(),
                    run.into(),
                    req["id"].clone(),
                    result.expect("enabled for valid JSON"),
                ));
            }
        }
    });
}

fn decision_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    first: &mut Option<bool>,
) -> bool {
    let default = first.take();
    let mut button = egui::Button::new(label);
    if default.is_some() {
        button = button.shortcut_text("↵");
    }
    ui.add(button).clicked() || default == Some(true)
}

fn command_approval(ui: &mut egui::Ui, params: &Value, mut first: Option<bool>) -> Option<Value> {
    let legacy = [json!("accept"), json!("decline"), json!("cancel")];
    let decisions = params["availableDecisions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&legacy);
    let command = params["command"].as_str();
    ui.horizontal_wrapped(|ui| {
        ui.strong("Approval requested");
        if let Some(command) = command
            && ui.small_button("Copy command").clicked()
        {
            ui.ctx().copy_text(command.to_owned());
        }
    });
    egui::ScrollArea::vertical()
        .id_salt("approval-details")
        .min_scrolled_height(0.)
        .max_height((ui.available_height() * 0.35).clamp(0., 120.))
        .show(ui, |ui| {
            if let Some(reason) = params["reason"].as_str() {
                ui.label(reason);
            }
            if let Some(cwd) = params["cwd"].as_str() {
                ui.weak(format!("Directory · {cwd}"));
            }
            if let Some(host) = params["networkApprovalContext"]["host"].as_str() {
                ui.label(format!("Network host · {host}"));
            }
            if let Some(command) = command {
                ui.add(
                    egui::Label::new(egui::RichText::new(command).monospace())
                        .wrap()
                        .selectable(true),
                );
            }
            for decision in decisions {
                let rule = &decision["acceptWithExecpolicyAmendment"]["execpolicy_amendment"];
                if rule.is_array() {
                    ui.weak("Proposed command rule");
                    ui.add(
                        egui::Label::new(crate::model::display(rule))
                            .wrap()
                            .selectable(true),
                    );
                }
            }
        });
    let mut selected = None;
    ui.horizontal_wrapped(|ui| {
        for decision in decisions {
            let label = match decision.as_str() {
                Some("accept") => "Approve once".into(),
                Some("acceptForSession") => "Approve for session".into(),
                Some("decline") => "Decline".into(),
                Some("cancel") => "Cancel turn".into(),
                _ if decision["acceptWithExecpolicyAmendment"]["execpolicy_amendment"]
                    .is_array() =>
                {
                    "Approve and save command rule".into()
                }
                _ => {
                    let rule = &decision["applyNetworkPolicyAmendment"]["network_policy_amendment"];
                    match (rule["action"].as_str(), rule["host"].as_str()) {
                        (Some("allow"), Some(host)) => format!("Always allow {host}"),
                        (Some("deny"), Some(host)) => format!("Always deny {host}"),
                        _ => continue,
                    }
                }
            };
            if decision_button(ui, label, &mut first) {
                selected = Some(decision.clone());
            }
        }
    });
    selected
}

fn backend_json_input(ui: &mut egui::Ui, label: &str) -> Option<Value> {
    let field = ui.make_persistent_id(label);
    let mut text = ui
        .data_mut(|data| data.get_temp::<String>(field))
        .unwrap_or_default();
    ui.label(label);
    ui.add(
        egui::TextEdit::multiline(&mut text)
            .code_editor()
            .desired_rows(3),
    );
    let result = serde_json::from_str(&text);
    if !text.is_empty()
        && let Err(error) = &result
    {
        ui.label(format!("Invalid JSON · {error}"));
    }
    ui.data_mut(|data| data.insert_temp(field, text));
    result.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Transcript;
    use egui::{Event, FullOutput, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

    fn click_request(transcript: &Transcript, label: &str, prompt_text: &str) -> Action {
        let ctx = egui::Context::default();
        let key = ThreadKey {
            peer: "peer".into(),
            id: "thread".into(),
        };
        let frame = |events| {
            let mut actions = vec![];
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(800., 600.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    for row in &transcript.rows {
                        if row.request.is_some() {
                            backend_request(ui, row, &key, "run", &mut actions);
                        }
                    }
                },
            );
            (output, actions)
        };
        let (output, actions) = frame(vec![]);
        assert!(actions.is_empty(), "Rendering or replay answered a request");
        assert!(visible_text(&output, prompt_text).is_some());
        let position = visible_text(&output, label).expect("visible response button");
        let (_, mut actions) = click(frame, position);
        assert_eq!(actions.len(), 1);
        actions.remove(0)
    }

    fn visible_text(output: &FullOutput, label: &str) -> Option<Pos2> {
        output.shapes.iter().find_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape
                && text.galley.job.text == label
            {
                let center = text.pos + text.galley.size() / 2.;
                return shape.clip_rect.contains(center).then_some(center);
            }
            None
        })
    }

    #[test]
    fn a_refused_followup_receipt_stays_with_the_accepted_prompt_on_replay() {
        let key = ThreadKey {
            peer: "fixture-peer".into(),
            id: "fixture-thread".into(),
        };
        let pending = crate::Pending {
            destination: crate::Destination {
                key: key.peer.clone(),
                label: "Fixture".into(),
                address: String::new(),
            },
            spec: json!({"id":"successor","thread_id":key.id,"backend":"codex","prompt":"Accepted follow-up"}),
            after: Some("predecessor".into()),
            refused_update: Some("refused-update".into()),
        };
        let mut saved = crate::Saved {
            pending: vec![pending],
            ..Default::default()
        };
        saved.workspace.enable_grid();
        let mut saved: crate::Saved =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let pending = saved.pending.pop().unwrap();
        // The run catalog can confirm this exact ID after restart with no submit reply.
        saved.remember_refused_followup(&pending);
        let mut data = Data {
            saved: serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap(),
            ..Default::default()
        };
        let runs = [
            json!({"record":{"spec":{"id":"predecessor","thread_id":key.id,"backend":"codex"}},"status":"completed"}),
            json!({"record":{"spec":{"id":"successor","thread_id":key.id,"backend":"codex"}},"status":"completed","last_seq":2}),
        ];
        data.catalogs
            .insert(key.peer.clone(), crate::model::catalog(&key.peer, &runs));
        let ctx = egui::Context::default();
        for has_initial_message in [false, true] {
            let mut transcript = Transcript::default();
            transcript.see_accepted_input(&pending.spec);
            if has_initial_message {
                transcript.apply(&json!({
                    "seq":1,"kind":"user_message","data":{"text":"Accepted follow-up"}
                })).unwrap();
            } else {
                transcript.apply(&json!({"seq":1,"kind":"status","data":{"status":"running"}})).unwrap();
            }
            transcript.apply(&json!({
                "seq":2,"kind":"assistant_message","data":{"id":"reply","text":"Reply after replay"}
            })).unwrap();
            data.transcripts
                .insert(run_key(&key.peer, "successor"), transcript);
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(800., 600.))),
                    ..Default::default()
                },
                |ui| {
                    let mut actions = vec![];
                    panel(ui, &mut data, &key, &mut actions);
                    assert!(actions.is_empty());
                },
            );
            let prompt_count = output.shapes.iter().filter(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Text(text)
                    if text.galley.job.text == "Accepted follow-up")
            }).count();
            assert_eq!(prompt_count, 1);
            assert!(visible_text(&output, "refused → started run 2").is_some());
            assert!(visible_text(&output, "Reply after replay").is_some());
        }
    }

    fn click(
        mut frame: impl FnMut(Vec<Event>) -> (FullOutput, Vec<Action>),
        position: Pos2,
    ) -> (FullOutput, Vec<Action>) {
        frame(vec![
            Event::PointerMoved(position),
            Event::PointerButton {
                pos: position,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        frame(vec![Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }])
    }

    #[test]
    fn send_follows_current_input_owner_and_cannot_steer_twice() {
        for backend in ["codex", "antigravity", "openrouter", "claude"] {
            let mut head = json!({"status":"running","record":{"spec":{"backend":backend}}});
            assert_eq!(
                send_route(&head, false),
                if backend == "claude" {
                    SendRoute::Hold
                } else {
                    SendRoute::Steer
                }
            );
            assert_eq!(send_route(&head, true), SendRoute::Draft);
            if backend == "antigravity" {
                head["native_prompt"] = json!("ended");
                assert_eq!(send_route(&head, false), SendRoute::Draft);
                head["native_prompt"] = json!("ready");
                assert_eq!(send_route(&head, false), SendRoute::Followup);
                assert_eq!(send_route(&head, true), SendRoute::Followup);
            }
            head["status"] = json!("completed");
            assert_eq!(send_route(&head, false), SendRoute::Followup);
            assert_eq!(send_route(&head, true), SendRoute::Followup);
        }
    }

    #[test]
    fn ended_antigravity_prompt_exposes_ordinary_send_for_background_work() {
        for (state, hint) in [
            ("ready", "→ starts run 2 ↵"),
            ("ended", "→ waiting for a sendable run ↵"),
        ] {
            let ctx = egui::Context::default();
            let key = ThreadKey {
                peer: "fixture-peer".into(),
                id: "fixture-thread".into(),
            };
            let mut data = Data::default();
            data.saved.drafts.push((key.clone(), "A new prompt".into()));
            data.thread_issues.insert(
                key.clone(),
                "Antigravity steering requires an active prompt. The draft is still here.".into(),
            );
            view::switch(&mut data, &key, false, None);
            let runs = [
                json!({"status":"running","native_prompt":state,"record":{"spec":{
                    "id":"fixture-run","thread_id":key.id,"backend":"antigravity","prompt":"Fixture"
                }}}),
            ];
            data.catalogs
                .insert(key.peer.clone(), crate::model::catalog(&key.peer, &runs));
            let mut frame = |events| {
                let mut actions = vec![];
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(800., 600.))),
                        events,
                        ..Default::default()
                    },
                    |ui| panel(ui, &mut data, &key, &mut actions),
                );
                (output, actions)
            };
            frame(vec![]);
            let (output, _) = frame(vec![]);
            assert!(visible_text(&output, "Running").is_none());
            assert!(visible_text(&output, "Send update").is_none());
            assert!(visible_text(&output, hint).is_some());
            assert!(visible_text(&output, "· antigravity · ctx unknown").is_some());
            let send = visible_text(&output, hint).expect("ordinary send control");
            let (_, actions) = click(&mut frame, send);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, Action::Send(target) if *target == key))
            );
        }
    }

    #[test]
    fn choosing_a_model_on_an_idle_managed_thread_loads_its_machine_catalog() {
        for (backend, runtime) in [
            ("antigravity", Runtime::Antigravity),
            ("claude", Runtime::Claude),
        ] {
            let ctx = egui::Context::default();
            let key = ThreadKey {
                peer: "fixture-peer".into(),
                id: "fixture-thread".into(),
            };
            let destination = crate::model::Destination {
                key: key.peer.clone(),
                label: "Fixture peer".into(),
                address: "127.0.0.1:42426".into(),
            };
            let mut data = Data::default();
            data.saved.destinations.push(destination.clone());
            data.editing_thread_settings = Some((key.clone(), RunSettings::default()));
            let mut frame = |events| {
                let mut actions = vec![];
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(800., 600.))),
                        events,
                        ..Default::default()
                    },
                    |ui| next_run_settings(ui, &mut data, &key, true, backend, &mut actions),
                );
                (output, actions)
            };
            frame(vec![]);
            let (output, _) = frame(vec![]);
            let choose = visible_text(&output, "Choose a model").expect("visible model checkbox");
            let (_, actions) = click(&mut frame, choose);
            assert!(actions.iter().any(|action| matches!(action,
            Action::LoadModelCatalog(actual, target)
                if *actual == runtime && target.key == destination.key && target.address == destination.address)));
        }
    }

    #[test]
    fn long_command_approval_is_clickable_in_a_short_pane() {
        let key = ThreadKey {
            peer: "peer".into(),
            id: "thread".into(),
        };
        let tag = run_key(&key.peer, "run");
        let command = "echo sample output\n".repeat(200);
        let offered_rule =
            json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["echo"]}});
        let request = json!({"seq":1,"kind":"backend","data":{
            "id":0,"method":COMMAND_APPROVAL,"params":{
                "command":command,"availableDecisions":["accept",offered_rule,"cancel"]
            }
        }});
        let screen = Rect::from_min_size(pos2(0., 0.), vec2(280., 200.));
        for (activity, label, decision) in [
            (false, "Approve once", json!("accept")),
            (true, "Approve once", json!("accept")),
            (true, "Approve and save command rule", offered_rule),
            (true, "Cancel turn", json!("cancel")),
        ] {
            let ctx = egui::Context::default();
            let mut data = Data::default();
            view::switch(&mut data, &key, activity, None);
            let runs = [json!({"status":"waiting","record":{"spec":{
                "id":"run","thread_id":key.id,"backend":"codex","prompt":"Fixture"
            }}})];
            data.catalogs
                .insert(key.peer.clone(), crate::model::catalog(&key.peer, &runs));
            data.transcripts
                .entry(tag.clone())
                .or_default()
                .apply(&request)
                .unwrap();
            data.watches.insert(
                tag.clone(),
                crate::Observing {
                    key: key.clone(),
                    epoch: 1,
                    stop: Default::default(),
                    retry: std::time::Instant::now(),
                    error: None,
                    done: false,
                    frontier: Some(1),
                },
            );
            let frame = |data: &mut Data, events| {
                let mut actions = vec![];
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        events,
                        ..Default::default()
                    },
                    |ui| panel(ui, data, &key, &mut actions),
                );
                (output, actions)
            };
            frame(&mut data, vec![]);
            let (output, actions) = frame(&mut data, vec![]);
            assert!(actions.is_empty());
            assert!(visible_text(&output, "Decline").is_none());
            let position = visible_text(&output, label).expect("visible decision in short pane");
            assert!(screen.contains(position));
            let copy = visible_text(&output, "Copy command").expect("visible copy button");
            let (copied, actions) = click(|events| frame(&mut data, events), copy);
            assert!(actions.is_empty());
            assert!(copied.platform_output.commands.iter().any(|output| {
                matches!(output, egui::OutputCommand::CopyText(value) if value == &command)
            }));
            let (_, actions) = click(|events| frame(&mut data, events), position);
            let [Action::Respond(target, run, request_id, result)] = actions.as_slice() else {
                panic!("expected exactly one response");
            };
            assert_eq!(
                (target, run.as_str(), request_id, result),
                (&key, "run", &json!(0), &json!({"decision": decision}))
            );
            data.transcripts
                .get_mut(&tag)
                .unwrap()
                .apply(&json!({"seq":2,"kind":"request_answered","data":{"request_id":0}}))
                .unwrap();
            let (output, actions) = frame(&mut data, vec![]);
            assert!(actions.is_empty());
            assert!(visible_text(&output, label).is_none());
        }
    }

    #[test]
    fn elicitation_buttons_emit_exact_protocol_responses() {
        for (label, expected) in [
            ("Approve", "accept"),
            ("Decline", "decline"),
            ("Cancel", "cancel"),
        ] {
            let mut transcript = Transcript::default();
            transcript.apply(&json!({"seq":1,"kind":"backend","data":{"id":0,"method":"mcpServer/elicitation/request","params":{"serverName":"fixture","message":"Allow Computer Use to use Example App?","mode":"form","requestedSchema":{"type":"object","properties":{}}}}})).unwrap();
            let Action::Respond(key, run, request_id, result) =
                click_request(&transcript, label, "Allow Computer Use to use Example App?")
            else {
                panic!("response action")
            };
            assert_eq!(key.peer, "peer");
            assert_eq!(run, "run");
            assert_eq!(request_id, 0);
            assert_eq!(
                result,
                json!({"action":expected,"content":if expected == "accept" {Some(json!({}))} else {None}})
            );
        }
    }

    #[test]
    #[ignore = "scripts/desktop-smoke.py supplies private paired daemons"]
    fn real_daemon_boundary_lost_answer_acknowledgment() {
        use crate::client::{Bus, Client, Update};
        use crate::model::AnswerState;
        use std::{
            sync::{Arc, atomic::AtomicBool},
            time::{Duration, Instant},
        };

        let home = std::path::PathBuf::from(std::env::var_os("BOXEN_GUI_TEST_HOME").unwrap());
        let client = Client::new(std::env::var("BOXEN_GUI_TEST_URL").unwrap(), &home).unwrap();
        let drop_ack = Client::new(
            std::env::var("BOXEN_GUI_TEST_DROP_RESPONSE_URL").unwrap(),
            &home,
        ).unwrap();
        let destination: crate::Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        let id = crate::new_id();
        client.operation(&destination, json!({
            "op":"start","spec":{"id":id,"thread_id":id,"backend":"codex","prompt":"ATTRIBUTED_GATE_FIXTURE"}
        })).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.operation(&destination, json!({"op":"get","id":id})).unwrap()["status"] != "waiting" {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        let attempt = crate::new_id();
        let bus = Bus::new(egui::Context::default());
        drop_ack.respond(destination.clone(), json!({
            "op":"respond_attempt","id":id,"request_id":0,
            "attempt_id":attempt,"result":{"decision":"decline"}
        }), bus.publisher());
        let Update::AnswerSubmitted(response_id, Err(_)) =
            bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
        else {
            panic!("The client expected an acknowledgment and saw a transport failure");
        };
        assert_eq!(response_id, attempt);
        client.watch(destination, id, 0, 1, bus.publisher(), Arc::new(AtomicBool::new(false)));
        let mut transcript = Transcript::default();
        let mut writes = 0;
        loop {
            let Update::Batch(_, 1, batch) = bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
            else {
                panic!("Replay could not reach the run");
            };
            for event in batch["events"].as_array().unwrap() {
                if event["kind"] == "response_write_started" {
                    writes += 1;
                }
                transcript.apply(event).unwrap();
            }
            if batch["done"] == true {
                break;
            }
        }
        assert_eq!(writes, 1);
        transcript.decorate_answers(|answer| answer.yours = answer.attempt_id == attempt);
        let gate = transcript.rows.iter().find(|row| row.answer.is_some()).unwrap();
        let answer = gate.answer.as_ref().unwrap();
        assert!(gate.request.is_none() && answer.yours);
        assert_eq!(answer.state, AnswerState::Written);
        assert_eq!(answer.decision, "Decline");
    }

    #[test]
    #[ignore = "scripts/desktop-smoke.py supplies private paired daemons"]
    fn real_daemon_boundary_backend_requests() {
        use crate::client::{Bus, Client, Update};
        use crate::model::Destination;
        use std::{
            sync::{Arc, atomic::AtomicBool},
            time::Duration,
        };
        let home = std::path::PathBuf::from(std::env::var_os("BOXEN_GUI_TEST_HOME").unwrap());
        let client = Client::new(std::env::var("BOXEN_GUI_TEST_URL").unwrap(), &home).unwrap();
        let destination: Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        let elicitation_text = "Allow Computer Use to use Example App?";
        for (label, prompt, prompt_text) in [
            ("Approve", "ELICITATION_ACCEPT", elicitation_text),
            ("Decline", "ELICITATION_DECLINE", elicitation_text),
            ("Cancel", "ELICITATION_CANCEL", elicitation_text),
            ("Approve once", "LONG_COMMAND_REVIEW", "Approval requested"),
        ] {
            let id = crate::new_id();
            client.operation(&destination, json!({"op":"start","spec":{"id":id,"thread_id":id,"backend":"codex","model":"fixture","prompt":prompt}})).unwrap();
            let bus = Bus::new(egui::Context::default());
            client.watch(
                destination.clone(),
                id.clone(),
                0,
                0,
                bus.publisher(),
                Arc::new(AtomicBool::new(false)),
            );
            let mut transcript = Transcript::default();
            let mut answered = false;
            let mut resolved = false;
            loop {
                match bus.rx.recv_timeout(Duration::from_secs(15)).unwrap() {
                    Update::Batch(_, _, batch) => {
                        for event in batch["events"].as_array().unwrap() {
                            transcript.apply(event).unwrap();
                            if event["data"]["method"] == "serverRequest/resolved" {
                                resolved = true;
                            }
                        }
                        if !answered && transcript.rows.iter().any(|row| row.request.is_some()) {
                            let Action::Respond(_, _, request_id, result) =
                                click_request(&transcript, label, prompt_text)
                            else {
                                panic!("response action")
                            };
                            client.control(
                                destination.clone(),
                                id.clone(),
                                json!({"type":"respond","request_id":request_id,"result":result}),
                                bus.publisher(),
                            );
                            answered = true;
                        }
                        if batch["done"] == true {
                            break;
                        }
                    }
                    Update::Controlled(_, _, _, result) => {
                        assert_eq!(result.unwrap()["accepted"], true);
                    }
                    Update::WatchError(_, _, error) => panic!("{error}"),
                    _ => {}
                }
            }
            assert!(answered && resolved);
            assert!(transcript.rows.iter().all(|row| row.request.is_none()));
            assert_eq!(
                client
                    .operation(&destination, json!({"op":"get","id":id}))
                    .unwrap()["status"],
                "completed"
            );
        }

        let race: Value =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_RESPONSE_RACE").unwrap()).unwrap();
        let ctx = egui::Context::default();
        let bus = Bus::new(ctx.clone());
        client.watch(
            destination,
            race["run"].as_str().unwrap().to_owned(),
            0,
            0,
            bus.publisher(),
            Arc::new(AtomicBool::new(false)),
        );
        let mut transcript = Transcript::default();
        loop {
            match bus.rx.recv_timeout(Duration::from_secs(15)).unwrap() {
                Update::Batch(_, _, batch) => {
                    for event in batch["events"].as_array().unwrap() {
                        transcript.apply(event).unwrap();
                    }
                    if batch["done"] == true {
                        break;
                    }
                }
                Update::WatchError(_, _, error) => panic!("{error}"),
                _ => {}
            }
        }
        transcript.decorate_answers(|answer| {
            answer.yours = race["local_attempt"] == answer.attempt_id;
            answer.machine_label = "Other fixture desktop".into();
        });
        let gate = transcript
            .rows
            .iter()
            .find(|row| !row.refused_answers.is_empty())
            .unwrap();
        let winner = gate.answer.as_ref().unwrap();
        assert_eq!(winner.state, crate::model::AnswerState::Written);
        assert!(!winner.yours);
        assert_eq!(winner.decision, "Decline");
        assert!(winner.at.is_some());
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(760., 340.))),
                ..Default::default()
            },
            |ui| view::activity_row(ui, gate),
        );
        assert!(visible_text(&output, &winner.label()).is_some());
        assert!(visible_text(&output, "Your Approve once was refused before writing").is_some());

        let run = std::env::var("BOXEN_GUI_TEST_INSPECTOR_RUN").unwrap();
        let bus = Bus::new(egui::Context::default());
        let destination: Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        client.watch(
            destination,
            run,
            0,
            0,
            bus.publisher(),
            Arc::new(AtomicBool::new(false)),
        );
        let mut transcript = Transcript::default();
        loop {
            match bus.rx.recv_timeout(Duration::from_secs(15)).unwrap() {
                Update::Batch(_, _, batch) => {
                    for event in batch["events"].as_array().unwrap() {
                        transcript.apply(event).unwrap();
                    }
                    if batch["done"] == true {
                        break;
                    }
                }
                Update::WatchError(_, _, error) => panic!("{error}"),
                _ => {}
            }
        }
        let tests = transcript.observed_tests.unwrap();
        assert_eq!(
            (tests.passed, tests.total, tests.command),
            (244, 246, "dotnet test")
        );
        let exchange = transcript.exchange_usage.unwrap();
        assert_eq!((exchange.input, exchange.output), (Some(61000), Some(400)));
    }
}
