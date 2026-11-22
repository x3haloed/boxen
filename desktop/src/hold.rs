//! Desktop projection and exact local drafts for daemon-owned Claude holds.
use crate::{
    Action, Data, Desktop,
    attachments::FileRef,
    client::{Update, run_key},
    conversation,
    model::ThreadKey,
    new_id,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
pub struct Contribution {
    pub key: ThreadKey,
    pub after: String,
    pub id: String,
    pub text: String,
    pub files: Vec<FileRef>,
    #[serde(default)]
    pub accepted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    Append,
    Edit,
    Release,
}

pub struct Reply {
    pub contribution: Contribution,
    pub mutation: Mutation,
    pub result: Result<Value, String>,
    pub attempted: bool,
}

impl Desktop {
    pub fn restore_interrupted_hold_edits(&mut self) {
        let interrupted = std::mem::take(&mut self.data.saved.hold_edits);
        if interrupted.is_empty() {
            return;
        }
        for entry in interrupted {
            self.restore_held_draft(&entry);
            self.data.thread_issues.insert(
                entry.key,
                "A previous hold edit is unconfirmed. The text is in the draft.".into(),
            );
        }
        self.persist();
    }

    pub fn hold_message(
        &mut self,
        key: ThreadKey,
        after: String,
        text: String,
        files: Vec<FileRef>,
    ) {
        if self.data.hold_pending.contains(&key) {
            return;
        }
        let contribution = Contribution {
            key,
            after,
            id: new_id(),
            text,
            files,
            accepted: false,
        };
        self.data.saved.holds.push(contribution.clone());
        if self.persist() {
            self.mutate_hold(contribution, None, Mutation::Append);
        } else {
            self.data
                .saved
                .holds
                .retain(|item| item.id != contribution.id);
        }
    }

    pub fn mutate_hold(
        &mut self,
        contribution: Contribution,
        revision: Option<u64>,
        mutation: Mutation,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.data.hold_pending.insert(contribution.key.clone()) {
            return;
        }
        if mutation == Mutation::Edit {
            self.data
                .saved
                .hold_edits
                .retain(|entry| entry.id != contribution.id);
            self.data.saved.hold_edits.push(contribution.clone());
            if !self.persist() {
                self.data.hold_pending.remove(&contribution.key);
                return;
            }
        }
        let dest = conversation::destination(&self.data, &contribution.key.peer);
        let publisher = self.bus.publisher();
        std::thread::spawn(move || {
            let mut attempted = false;
            let result = (|| {
                let revision = match revision {
                    Some(revision) => revision,
                    None => client.operation(
                        &dest,
                        json!({
                            "op":"held_followup","id":contribution.after,"action":"get"
                        }),
                    )?["hold"]["revision"]
                        .as_u64()
                        .unwrap_or(0),
                };
                let action = match mutation {
                    Mutation::Append => "append",
                    Mutation::Edit => "edit",
                    Mutation::Release => "release",
                };
                let files = json!(contribution.files);
                if mutation == Mutation::Append {
                    client.copy_files(&dest, &files)?;
                }
                let request = json!({
                    "op":"held_followup","id":contribution.after,"action":action,
                    "expected_revision":revision,"contribution_id":contribution.id,
                    "contribution":{"id":contribution.id,"text":contribution.text,"attachments":files}
                });
                attempted = true;
                client.operation(&dest, request)
            })();
            publisher.send(Update::Held(Reply {
                contribution,
                mutation,
                result,
                attempted,
            }));
        });
    }

    pub fn hold_reply(&mut self, reply: Reply) {
        let entry = reply.contribution;
        self.data.hold_pending.remove(&entry.key);
        if reply.mutation == Mutation::Edit {
            self.data
                .saved
                .hold_edits
                .retain(|saved| saved.id != entry.id);
        }
        match reply.result {
            Ok(result) => {
                if reply.mutation == Mutation::Release {
                    self.restore_held_draft(&entry);
                    self.data.saved.holds.retain(|saved| saved.id != entry.id);
                } else if reply.mutation == Mutation::Edit
                    && let Some(saved) = self
                        .data
                        .saved
                        .holds
                        .iter_mut()
                        .find(|saved| saved.id == entry.id)
                {
                    saved.text = entry.text.clone();
                }
                let tag = run_key(&entry.key.peer, &entry.after);
                self.data
                    .transcripts
                    .entry(tag.clone())
                    .or_default()
                    .see_hold(&result["hold"]);
                self.data.thread_issues.remove(&entry.key);
                self.reconcile_hold(&tag);
            }
            Err(error) => {
                let nonacceptance = !reply.attempted || error.starts_with("HTTP 422:");
                if reply.mutation == Mutation::Append && nonacceptance {
                    self.data.saved.holds.retain(|saved| saved.id != entry.id);
                } else if reply.mutation == Mutation::Edit {
                    self.restore_held_draft(&entry);
                }
                self.data.thread_issues.insert(
                    entry.key,
                    if nonacceptance {
                        format!("Hold change was refused. {error}")
                    } else {
                        format!("Hold change is unconfirmed. {error}")
                    },
                );
            }
        }
        self.persist();
    }

    pub fn reconcile_hold(&mut self, tag: &str) {
        let Some(hold) = self
            .data
            .transcripts
            .get(tag)
            .and_then(|transcript| transcript.hold.clone())
        else {
            return;
        };
        let phase = hold["phase"].as_str().unwrap_or("");
        let accepted: Vec<_> = self
            .data
            .saved
            .holds
            .iter()
            .filter(|saved| run_key(&saved.key.peer, &saved.after) == tag)
            .filter(|saved| {
                hold["contributions"].as_array().is_some_and(|entries| {
                    entries.iter().any(|item| item["input"]["id"] == saved.id)
                })
            })
            .cloned()
            .collect();
        if accepted.is_empty() {
            return;
        }
        let mut changed = false;
        for entry in accepted {
            if phase == "returned" {
                self.restore_held_draft(&entry);
                self.data.saved.holds.retain(|saved| saved.id != entry.id);
                self.data
                    .thread_issues
                    .insert(entry.key, "Held message returned to the draft.".into());
                changed = true;
            } else {
                if !entry.accepted {
                    self.clear_submitted_draft(&entry.key, &entry.text, &entry.files);
                    if let Some(saved) = self
                        .data
                        .saved
                        .holds
                        .iter_mut()
                        .find(|saved| saved.id == entry.id)
                    {
                        saved.accepted = true;
                    }
                    changed = true;
                }
                if phase == "started" {
                    self.data.saved.holds.retain(|saved| saved.id != entry.id);
                    changed = true;
                }
            }
        }
        if changed {
            self.persist();
        }
    }

    fn restore_held_draft(&mut self, entry: &Contribution) {
        let saved = &mut self.data.saved;
        if !entry.text.is_empty() {
            if let Some((_, current)) = saved.drafts.iter_mut().find(|(key, _)| key == &entry.key) {
                if *current != entry.text {
                    *current = if current.is_empty() {
                        entry.text.clone()
                    } else {
                        format!("{}\n\n{}", entry.text, current)
                    };
                }
            } else {
                saved.drafts.push((entry.key.clone(), entry.text.clone()));
            }
        }
        if !entry.files.is_empty() {
            if let Some((_, current)) = saved
                .thread_files
                .iter_mut()
                .find(|(key, _)| key == &entry.key)
            {
                for file in &entry.files {
                    if !current.contains(file) {
                        current.push(file.clone());
                    }
                }
            } else {
                saved
                    .thread_files
                    .push((entry.key.clone(), entry.files.clone()));
            }
        }
    }
}

pub fn panel(
    ui: &mut egui::Ui,
    data: &Data,
    key: &ThreadKey,
    after: &str,
    hold: &Value,
    actions: &mut Vec<Action>,
) {
    match hold["phase"].as_str() {
        Some("held" | "claimed") => {
            ui.weak("Held after this run · starts after successful completion");
            if let Some(entries) = hold["contributions"].as_array() {
                for entry in entries {
                    let input = &entry["input"];
                    let id = input["id"].as_str().unwrap_or("");
                    let text = input["text"].as_str().unwrap_or("");
                    let own = data.saved.holds.iter().find(|item| item.id == id);
                    ui.push_id(id, |ui| {
                        if let Some(machine) = entry["machine"].as_str()
                            && let Some(destination) = data
                                .saved
                                .destinations
                                .iter()
                                .find(|host| host.key == machine)
                        {
                            ui.weak(&destination.label);
                        }
                        ui.add(egui::Label::new(text).wrap().selectable(true));
                        if let (Some(own), Some(revision)) = (own, hold["revision"].as_u64())
                            && hold["phase"] == "held"
                            && !data.hold_pending.contains(key)
                        {
                            ui.horizontal(|ui| {
                                if ui.small_button("Release to draft").clicked() {
                                    let mut contribution = own.clone();
                                    contribution.text = text.into();
                                    actions.push(Action::HoldChange(
                                        contribution,
                                        revision,
                                        Mutation::Release,
                                    ));
                                }
                            });
                            egui::CollapsingHeader::new("Edit").show(ui, |ui| {
                                let edit =
                                    ui.make_persistent_id(("hold-edit", after, id, revision));
                                let mut changed = ui
                                    .data_mut(|data| data.get_temp::<String>(edit))
                                    .unwrap_or_else(|| text.into());
                                ui.text_edit_multiline(&mut changed);
                                if ui.small_button("Save edit").clicked() {
                                    let mut contribution = own.clone();
                                    contribution.text = changed.clone();
                                    actions.push(Action::HoldChange(
                                        contribution,
                                        revision,
                                        Mutation::Edit,
                                    ));
                                }
                                ui.data_mut(|data| data.insert_temp(edit, changed));
                            });
                        }
                    });
                }
            }
        }
        Some("returned") => {
            ui.weak("Held message returned to draft");
        }
        Some("started") => {
            ui.weak("Held follow-up started");
        }
        Some("uncertain") => {
            ui.weak("Held follow-up acceptance is uncertain");
        }
        _ => {}
    }
}
