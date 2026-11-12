use crate::{
    Action, Data, Desktop,
    attachments::{FileRef, Target},
    client::{Update, run_key},
    conversation as view, find_thread,
    model::{Row, ThreadKey, terminal},
    new_id,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
            let mut change_model = settings.model.is_some();
            ui.checkbox(&mut change_model, "Choose a model");
            if change_model {
                ui.add(
                    egui::TextEdit::singleline(settings.model.get_or_insert_with(String::new))
                        .hint_text("Default model"),
                );
            } else {
                settings.model = None;
            }
            ui.horizontal(|ui| (ui.button("Cancel").clicked(), ui.button("Done").clicked()))
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
                    ui.label("Waiting for input");
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
    if let Some(error) = data.watches.get(&live_tag).and_then(|w| w.error.as_ref()) {
        ui.label(format!("Reconnecting to activity · {error}"));
    }
    if !activity && let Some(t) = data.transcripts.get(&live_tag) {
        for row in &t.rows {
            if row.request.is_some() {
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
                ui.push_id(&tag, |ui| {
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
            if live && row.request.is_some() {
                backend_request(ui, row, key, run, actions);
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
        ui.strong(if method.contains("requestApproval") {
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
        if method.contains("requestApproval") {
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
        }
    });
}
