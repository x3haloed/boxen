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
    if ui
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
pub fn active(last: Option<&Value>) -> Option<&str> {
    last.filter(|r| !terminal(r))
        .and_then(|r| r["record"]["spec"]["id"].as_str())
}
pub fn capture(data: &mut Data, key: &ThreadKey, run: Option<&str>) {
    if let Some(run) = run
        && !data.saved.draft_runs.iter().any(|(k, _)| k == key)
    {
        data.saved.draft_runs.push((key.clone(), run.into()));
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
                if let Some(transcript) = self.data.transcripts.remove(&run_key(&key.peer, &run)) {
                    self.data
                        .transcripts
                        .entry(run_key(machine, &run))
                        .or_insert(transcript);
                }
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
        if self.data.saved.moves.iter().any(|m| m.key == key) {
            return;
        }
        let Some(last) = find_thread(&self.data, &key)
            .and_then(|t| t.runs.last())
            .cloned()
        else {
            return;
        };
        let text = self
            .data
            .saved
            .drafts
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        let files = self
            .data
            .saved
            .thread_files
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, f)| f.clone())
            .unwrap_or_default();
        if (text.trim().is_empty() && files.is_empty()) || text.len() > 256 * 1024 {
            return;
        }
        let dest = view::destination(&self.data, &key.peer);
        let running = active(Some(&last));
        if let Some((_, intended)) = self.data.saved.draft_runs.iter().find(|(k, _)| k == &key)
            && running != Some(intended)
        {
            self.data.thread_issues.insert(
                key,
                "The intended run ended. The draft is ready to send as a separate request.".into(),
            );
            return;
        }
        self.data.thread_issues.remove(&key);
        if let Some(run) = running {
            if self.data.active_steering.contains(&key)
                || self.data.saved.steering.iter().any(|s| s.key == key)
            {
                return;
            }
            let Some(client) = self.client.clone() else {
                return;
            };
            let steer = Steering {
                key: key.clone(),
                run: run.into(),
                id: new_id(),
                text,
                files,
            };
            self.data.saved.steering.push(steer.clone());
            if !self.persist() {
                self.data.saved.steering.retain(|s| s.id != steer.id);
                return;
            }
            self.data.active_steering.insert(key);
            let publisher = self.bus.publisher();
            std::thread::spawn(move || {
                let files = json!(steer.files);
                let (result, attempted) = match client.copy_files(&dest,&files) {
                    Err(e) => (Err(e),false),
                    Ok(()) => (client.operation(&dest,json!({"op":"control","id":steer.run,"action":{"type":"steer","text":steer.text,"attachments":files,"update_id":steer.id}})),true)
                };
                publisher.send(Update::Steered(steer.key, result, attempted));
            });
        } else {
            if self
                .data
                .saved
                .pending
                .iter()
                .any(|p| p.destination.key == key.peer && p.spec["thread_id"] == key.id)
            {
                return;
            }
            let Some(previous) = last["record"]["spec"]["id"].as_str() else {
                return;
            };
            if self
                .data
                .preparing_followups
                .values()
                .any(|target| target == &key)
            {
                return;
            }
            let settings = settings_for(&self.data, &key);
            if let Some(client) = &self.client {
                self.data
                    .preparing_followups
                    .insert(run_key(&dest.key, previous), key);
                client.followup(
                    dest,
                    previous.into(),
                    new_id(),
                    text,
                    files,
                    settings,
                    self.bus.publisher(),
                );
            }
        }
    }
    pub fn steered(&mut self, key: ThreadKey, result: Result<Value, String>, attempted: bool) {
        self.data.active_steering.remove(&key);
        if let Err(error) = result {
            if !attempted || error.starts_with("HTTP 422:") {
                self.data.saved.steering.retain(|s| s.key != key);
                self.data
                    .thread_issues
                    .insert(key, format!("Update was not sent: {error}"));
            } else {
                self.data
                    .thread_issues
                    .insert(key, format!("Update delivery is unconfirmed: {error}"));
            }
            self.persist();
        }
    }
    pub fn reconcile_steering(&mut self, tag: &str) {
        let Some(transcript) = self.data.transcripts.get(tag) else {
            return;
        };
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
                    .any(|r| r.update_id.as_deref() == Some(&s.id))
                {
                    Some((s.clone(), true))
                } else if transcript.rejected_updates.contains(&s.id) {
                    Some((s.clone(), false))
                } else {
                    None
                }
            })
            .collect();
        for (s, accepted) in &done {
            self.data.saved.steering.retain(|p| p.id != s.id);
            self.data.active_steering.remove(&s.key);
            if *accepted {
                self.clear_submitted_draft(&s.key, &s.text, &s.files);
                self.data.thread_issues.remove(&s.key);
            } else {
                self.data.thread_issues.insert(
                    s.key.clone(),
                    "The runtime rejected the update. The draft is still here.".into(),
                );
            }
        }
        if !done.is_empty() {
            self.persist();
        }
    }
}

pub fn panel(ui: &mut egui::Ui, data: &mut Data, key: &ThreadKey, actions: &mut Vec<Action>) {
    let activity = view::header(ui, data, key);
    let dest = view::destination(data, &key.peer);
    let runs = find_thread(data, key)
        .map(|t| t.runs.clone())
        .unwrap_or_default();
    let last = runs.last();
    let running = active(last).map(str::to_owned);
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
    let mut switched = None;
    let mut stale = data
        .saved
        .draft_runs
        .iter()
        .find(|(k, _)| k == key)
        .is_some_and(|(_, id)| Some(id.as_str()) != running.as_deref());
    ui.horizontal_wrapped(|ui| {
        next_run_settings(
            ui,
            data,
            key,
            last.is_some()
                && running.is_none()
                && pending.is_none()
                && !moving
                && !data
                    .preparing_followups
                    .values()
                    .any(|target| target == key),
            last.and_then(|run| run["record"]["spec"]["backend"].as_str())
                .unwrap_or("codex"),
            actions,
        );
        if activity {
            ui.weak(&dest.label);
        }
        if let Some(error) = data.errors.get(&key.peer) {
            ui.label(format!("Machine unavailable · {error}"));
        } else if let Some(last) = last {
            match last["status"].as_str() {
                Some("starting" | "running") => {
                    ui.label("Running");
                }
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
                        .unwrap_or("Waiting for input");
                    ui.label(detail);
                }
                Some("failed") => {
                    ui.label("Run failed");
                }
                Some("interrupted") => {
                    ui.label("Run interrupted");
                }
                _ => {}
            }
        } else {
            ui.weak("Waiting for the thread catalog…");
        }
        if activity && running.is_some() && ui.small_button("Cancel run").clicked() {
            actions.push(Action::Cancel(key.clone()));
        }
    });
    let live_id = running.as_deref().unwrap_or("");
    let live_tag = run_key(&key.peer, live_id);
    if let Some(last) = last {
        let tag = run_key(
            &key.peer,
            last["record"]["spec"]["id"].as_str().unwrap_or(""),
        );
        let execution = data
            .transcripts
            .get(&tag)
            .and_then(|transcript| transcript.execution.as_ref())
            .unwrap_or(&last["record"]["execution"]);
        if execution.is_object() {
            ui.collapsing("Execution context", |ui| {
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
                        .or_else(|| last["record"]["spec"]["cwd"].as_str())
                        .unwrap_or("Unknown")
                ));
                if let Some(available) = execution["desktop_available"].as_bool() {
                    ui.label(if available {
                        "Desktop session available"
                    } else {
                        "Desktop session unavailable"
                    });
                }
            });
        }
    }
    if let Some(error) = data.watches.get(&live_tag).and_then(|w| w.error.as_ref()) {
        ui.label(format!("Reconnecting to activity · {error}"));
    }
    if let Some(t) = data.transcripts.get(&live_tag) {
        for row in &t.rows {
            if row
                .request
                .as_ref()
                .is_some_and(|request| !activity || request["method"] == COMMAND_APPROVAL)
            {
                backend_request(ui, row, key, live_id, actions);
            }
        }
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
    let extra = errors
        + usize::from(stale)
        + usize::from(waiting.is_some())
        + usize::from(pending.is_some())
        + usize::from(data.thread_issues.contains_key(key))
        + usize::from(
            data.preparing_followups
                .values()
                .any(|target| target == key),
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
            for run in &runs {
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
                    if activity
                        && let Some(machine) = run["machine"].as_str().filter(|m| *m != key.peer)
                    {
                        let label = data
                            .saved
                            .destinations
                            .iter()
                            .find(|dest| dest.key == machine)
                            .map(|dest| dest.label.clone())
                            .unwrap_or_else(|| {
                                format!("Machine {}", machine.chars().take(8).collect::<String>())
                            });
                        ui.weak(label);
                    }
                    let rows = data
                        .transcripts
                        .get(&tag)
                        .map(|t| t.rows.as_slice())
                        .unwrap_or_default();
                    let mut group_start = 0;
                    for (i, row) in rows.iter().enumerate() {
                        if view::is_message(row) {
                            if activity {
                                render_rows(
                                    ui,
                                    &rows[group_start..i],
                                    key,
                                    id,
                                    !terminal(run),
                                    actions,
                                );
                            }
                            let anchor = format!("{id}:{}", row.id);
                            let mut attached = row.attachments.clone();
                            let outgoing = row.label.starts_with("You");
                            let response = ui
                                .push_id(&anchor, |ui| {
                                    view::bubble(ui, outgoing, &row.text, !outgoing, |ui| {
                                        view::chips(
                                            ui,
                                            &mut attached,
                                            &dest,
                                            false,
                                            &mut data.file_previews,
                                            actions,
                                        );
                                        if !activity
                                            && latest_reply.as_ref() == Some(&anchor)
                                            && ui.small_button("Show in Activity").clicked()
                                        {
                                            switched = Some(anchor.clone());
                                        }
                                    })
                                })
                                .inner;
                            view::record(ui, &mut data.views, key, &anchor, response.rect);
                            if !activity {
                                response.context_menu(|ui| {
                                    if ui.button("Show in Activity").clicked() {
                                        switched = Some(anchor.clone());
                                        ui.close();
                                    }
                                });
                            }
                            ui.add_space(4.);
                            group_start = i + 1;
                        }
                    }
                    if activity {
                        render_rows(ui, &rows[group_start..], key, id, !terminal(run), actions);
                    }
                    if rows.is_empty() {
                        let spec = &run["record"]["spec"];
                        let mut files = FileRef::parse(&spec["attachments"]);
                        if run["record"]["imported"].is_null()
                            && (!spec["prompt"].as_str().unwrap_or("").is_empty()
                                || !files.is_empty())
                        {
                            view::bubble(
                                ui,
                                true,
                                spec["prompt"].as_str().unwrap_or(""),
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
                                },
                            );
                        }
                        if run["last_seq"].as_u64().unwrap_or(0) != 0 {
                            ui.weak("Loading retained conversation…");
                        }
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
    if let Some(anchor) = switched {
        view::switch(data, key, true, Some(anchor));
    }
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
        .any(|target| target == key)
    {
        ui.weak("Preparing follow-up…");
    }
    if let Some(issue) = data.thread_issues.get(key) {
        ui.label(issue);
    }
    if let Some(steering) = waiting {
        ui.horizontal_wrapped(|ui| {
            if data.active_steering.contains(key) {
                ui.label("Sending update…");
            } else if running.as_deref() != Some(&steering.run)
                || data.thread_issues.contains_key(key)
            {
                ui.label("Update outcome is unconfirmed. It will not be sent again automatically.");
                if ui.small_button("Keep as draft").clicked() {
                    data.saved.steering.retain(|s| s.key != *key);
                    data.thread_issues.remove(key);
                }
            } else {
                ui.label("Waiting for the runtime to take the update…");
                if ui
                    .small_button("Keep as draft")
                    .on_hover_text("The runtime may still take the earlier update.")
                    .clicked()
                {
                    data.saved.steering.retain(|s| s.key != *key);
                }
            }
        });
    }
    if stale {
        ui.horizontal_wrapped(|ui| {
            ui.label("The run this draft was written for ended.");
            if ui
                .small_button(if running.is_some() {
                    "Use current run"
                } else {
                    "Prepare as follow-up"
                })
                .clicked()
            {
                data.saved.draft_runs.retain(|(k, _)| k != key);
                capture(data, key, running.as_deref());
                data.thread_issues.remove(key);
                stale = false;
            }
        });
    }
    let frozen = moving
        || pending.is_some()
        || data.saved.steering.iter().any(|s| s.key == *key)
        || data
            .preparing_followups
            .values()
            .any(|target| target == key);
    let compose = view::composer(
        ui,
        data,
        view::Composer {
            text: &mut draft,
            files: &mut files,
            target,
            label: if running.is_some() {
                "Send update"
            } else {
                "Send"
            },
            limit: 256 * 1024,
            blocked: frozen || stale || last.is_none(),
        },
        actions,
    );
    if compose.changed && (!draft.is_empty() || !files.is_empty()) {
        capture(data, key, running.as_deref());
        actions.push(Action::Pin(key.clone()));
    }
    if draft.is_empty()
        && files.is_empty()
        && !data
            .preparing
            .get(&Target::Thread(key.clone()))
            .is_some_and(|p| p.working)
    {
        data.saved.draft_runs.retain(|(k, _)| k != key);
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
    actions: &mut Vec<Action>,
) {
    let mut start = 0;
    for (i, row) in rows.iter().enumerate() {
        if view::incident(row) {
            view::activity_rows(ui, &rows[start..i]);
            if live && let Some(request) = &row.request {
                if request["method"] != COMMAND_APPROVAL {
                    backend_request(ui, row, key, run, actions);
                }
            } else {
                view::activity_row(ui, row);
            }
            start = i + 1;
        }
    }
    view::activity_rows(ui, &rows[start..]);
}
fn backend_request(
    ui: &mut egui::Ui,
    row: &Row,
    key: &ThreadKey,
    run: &str,
    actions: &mut Vec<Action>,
) {
    let Some(req) = &row.request else { return };
    let method = req["method"].as_str().unwrap_or("");
    let p = &req["params"];
    ui.push_id((&row.id, run), |ui| {
        if method == COMMAND_APPROVAL {
            if let Some(decision) = command_approval(ui, p) {
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
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
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
                    if ui.button(label).clicked() {
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

fn command_approval(ui: &mut egui::Ui, params: &Value) -> Option<Value> {
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
            if ui.button(label).clicked() {
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
    fn choosing_a_model_on_an_idle_antigravity_thread_loads_its_machine_catalog() {
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
                |ui| next_run_settings(ui, &mut data, &key, true, "antigravity", &mut actions),
            );
            (output, actions)
        };
        frame(vec![]);
        let (output, _) = frame(vec![]);
        let choose = visible_text(&output, "Choose a model").expect("visible model checkbox");
        let (_, actions) = click(&mut frame, choose);
        assert!(actions.iter().any(|action| matches!(action,
            Action::LoadModelCatalog(Runtime::Antigravity, target)
                if target.key == destination.key && target.address == destination.address)));
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
    }
}
