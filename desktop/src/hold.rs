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

#[derive(Clone, Serialize, Deserialize)]
pub struct Release {
    pub contribution: Contribution,
    pub revision: u64,
}

impl Release {
    fn observed(&self, hold: &Value) -> bool {
        hold["revision"]
            .as_u64()
            .is_some_and(|revision| revision > self.revision)
            && hold["contributions"].as_array().is_some_and(|entries| {
                !entries
                    .iter()
                    .any(|entry| entry["input"]["id"] == self.contribution.id)
            })
    }
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
        if mutation == Mutation::Release
            && let Some(revision) = revision
        {
            self.data
                .saved
                .hold_releases
                .retain(|release| release.contribution.id != contribution.id);
            self.data.saved.hold_releases.push(Release {
                contribution: contribution.clone(),
                revision,
            });
            if !self.persist() {
                self.data
                    .saved
                    .hold_releases
                    .retain(|release| release.contribution.id != contribution.id);
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
        if reply.mutation == Mutation::Release
            && !self
                .data
                .saved
                .hold_releases
                .iter()
                .any(|release| release.contribution.id == entry.id)
        {
            return;
        }
        if reply.mutation == Mutation::Edit {
            self.data
                .saved
                .hold_edits
                .retain(|saved| saved.id != entry.id);
        }
        match reply.result {
            Ok(result) => {
                if reply.mutation == Mutation::Edit
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
                } else if reply.mutation == Mutation::Release && nonacceptance {
                    self.data
                        .saved
                        .hold_releases
                        .retain(|release| release.contribution.id != entry.id);
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
        let released: Vec<_> = self
            .data
            .saved
            .hold_releases
            .iter()
            .filter(|release| {
                run_key(&release.contribution.key.peer, &release.contribution.after) == tag
            })
            .filter(|release| release.observed(&hold))
            .map(|release| release.contribution.clone())
            .collect();
        let mut changed = !released.is_empty();
        for entry in released {
            self.restore_held_draft(&entry);
            self.data
                .saved
                .hold_releases
                .retain(|release| release.contribution.id != entry.id);
            self.data.saved.holds.retain(|saved| saved.id != entry.id);
            self.data
                .thread_issues
                .insert(entry.key, "Released to draft".into());
        }
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
        for entry in accepted {
            if matches!(phase, "started" | "returned" | "uncertain") {
                let count = self.data.saved.hold_releases.len();
                self.data
                    .saved
                    .hold_releases
                    .retain(|release| release.contribution.id != entry.id);
                if count != self.data.saved.hold_releases.len() {
                    changed = true;
                    if phase == "started" {
                        self.data
                            .thread_issues
                            .insert(entry.key.clone(), "Follow-up started before release".into());
                    }
                }
            }
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
        self.restore_thread_draft(&entry.key, &entry.text, &entry.files);
    }

    pub fn restore_thread_draft(&mut self, target: &ThreadKey, text: &str, files: &[FileRef]) {
        let saved = &mut self.data.saved;
        if !text.is_empty() {
            if let Some((_, current)) = saved.drafts.iter_mut().find(|(key, _)| key == target) {
                if *current != text {
                    *current = if current.is_empty() {
                        text.to_owned()
                    } else {
                        format!("{}\n\n{}", text, current)
                    };
                }
            } else {
                saved.drafts.push((target.clone(), text.to_owned()));
            }
        }
        if !files.is_empty() {
            if let Some((_, current)) = saved.thread_files.iter_mut().find(|(key, _)| key == target)
            {
                for file in files {
                    if !current.contains(file) {
                        current.push(file.clone());
                    }
                }
            } else {
                saved.thread_files.push((target.clone(), files.to_vec()));
            }
        }
    }
}

pub fn panel(
    ui: &mut egui::Ui,
    data: &Data,
    key: &ThreadKey,
    after: &str,
    number: usize,
    hold: &Value,
    actions: &mut Vec<Action>,
) {
    use crate::console_style;
    match hold["phase"].as_str() {
        Some("held" | "claimed") => {
            let row = egui::Frame::new()
                .fill(egui::Color32::from_rgb(29, 25, 19))
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.colored_label(
                        console_style::WARN,
                        format!("Held after run {number} · starts after successful completion"),
                    );
                    for entry in hold["contributions"].as_array().into_iter().flatten() {
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
                                let edit =
                                    ui.make_persistent_id(("hold-edit", after, id, revision));
                                let mut draft =
                                    ui.data_mut(|memory| memory.get_temp::<String>(edit));
                                ui.horizontal(|ui| {
                                    if ui.small_button("Edit").clicked() {
                                        draft = Some(text.into());
                                    }
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
                                if let Some(changed) = &mut draft {
                                    ui.text_edit_multiline(changed);
                                    if ui.small_button("Save edit").clicked() {
                                        let mut contribution = own.clone();
                                        contribution.text = changed.clone();
                                        actions.push(Action::HoldChange(
                                            contribution,
                                            revision,
                                            Mutation::Edit,
                                        ));
                                    }
                                }
                                if let Some(draft) = draft {
                                    ui.data_mut(|memory| memory.insert_temp(edit, draft));
                                }
                            }
                        });
                    }
                })
                .response
                .rect;
            console_style::dashed_rect(ui, row, console_style::WARN);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_needs_a_newer_revision_without_its_exact_contribution() {
        let release = Release {
            contribution: Contribution {
                key: ThreadKey {
                    peer: "fixture".into(),
                    id: "conversation".into(),
                },
                id: "own".into(),
                after: "predecessor".into(),
                text: "Held message".into(),
                files: vec![],
                accepted: true,
            },
            revision: 3,
        };
        assert!(!release.observed(&json!({"revision":3,"contributions":[]})));
        assert!(!release.observed(
            &json!({"revision":4,"phase":"started","contributions":[{"input":{"id":"own"}}]})
        ));
        assert!(release.observed(
            &json!({"revision":4,"phase":"held","contributions":[{"input":{"id":"other"}}]})
        ));
        assert!(release.observed(&json!({"revision":4,"phase":"released","contributions":[]})));
    }
}
