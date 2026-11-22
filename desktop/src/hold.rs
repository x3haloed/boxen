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

#[derive(Clone, Serialize, Deserialize)]
pub struct Draft {
    contribution: Contribution,
    revision: u64,
}

pub(super) fn unsettled_changes(saved: &crate::Saved) -> impl Iterator<Item = &Contribution> {
    saved
        .hold_releases
        .iter()
        .map(|release| &release.contribution)
        .chain(saved.hold_drafts.iter().map(|draft| &draft.contribution))
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
        let drafts = std::mem::take(&mut self.data.saved.hold_drafts);
        if interrupted.is_empty() && drafts.is_empty() {
            return;
        }
        for draft in drafts {
            self.restore_held_draft(&draft.contribution);
            self.data.thread_issues.insert(
                draft.contribution.key,
                "Unsubmitted hold edit restored to the draft".into(),
            );
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

    pub fn save_hold_draft(&mut self, contribution: Contribution, revision: u64, changed: bool) {
        self.data
            .saved
            .hold_drafts
            .retain(|draft| draft.contribution.id != contribution.id);
        if changed {
            let tag = run_key(&contribution.key.peer, &contribution.after);
            self.data.saved.hold_drafts.push(Draft {
                contribution,
                revision,
            });
            self.reconcile_hold(&tag);
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
            let draft = self
                .data
                .saved
                .hold_drafts
                .iter()
                .position(|draft| draft.contribution.id == contribution.id)
                .map(|position| self.data.saved.hold_drafts.remove(position));
            self.data
                .saved
                .hold_edits
                .retain(|entry| entry.id != contribution.id);
            self.data.saved.hold_edits.push(contribution.clone());
            if !self.persist() {
                self.data
                    .saved
                    .hold_edits
                    .retain(|entry| entry.id != contribution.id);
                self.data.saved.hold_drafts.extend(draft);
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
        let phase = hold["phase"].as_str().unwrap_or("");
        let mut stale_drafts = Vec::new();
        self.data.saved.hold_drafts.retain(|draft| {
            let stale = run_key(&draft.contribution.key.peer, &draft.contribution.after) == tag
                && hold["revision"]
                    .as_u64()
                    .is_some_and(|revision| revision > draft.revision);
            if stale {
                stale_drafts.push(draft.contribution.clone());
            }
            !stale
        });
        for entry in &stale_drafts {
            self.restore_held_draft(entry);
            self.data.thread_issues.insert(
                entry.key.clone(),
                if phase == "started" {
                    "Follow-up started before the edit was saved · edit kept as draft"
                } else {
                    "Held follow-up changed · edit kept as draft"
                }
                .into(),
            );
        }
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
        let mut changed = !released.is_empty() || !stale_drafts.is_empty();
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
                if !stale_drafts.iter().any(|draft| draft.id == entry.id) {
                    self.restore_held_draft(&entry);
                    self.data.thread_issues.insert(
                        entry.key.clone(),
                        "Held message returned to the draft.".into(),
                    );
                }
                self.data.saved.holds.retain(|saved| saved.id != entry.id);
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
    run_numbers: (usize, Option<usize>),
    hold: &Value,
    actions: &mut Vec<Action>,
) {
    use crate::console_style;
    let (number, successor) = run_numbers;
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
                    for (position, entry) in hold["contributions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                    {
                        let input = &entry["input"];
                        let id = input["id"].as_str().unwrap_or("");
                        let text = input["text"].as_str().unwrap_or("");
                        let own = data.saved.holds.iter().find(|item| item.id == id);
                        ui.push_id(id, |ui| {
                            if position > 0 {
                                ui.weak("Appended to this hold");
                            }
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
                                    if ui.button("Edit").clicked() {
                                        draft = Some(
                                            data.saved
                                                .hold_drafts
                                                .iter()
                                                .find(|saved| {
                                                    saved.contribution.id == id
                                                        && saved.revision == revision
                                                })
                                                .map_or_else(
                                                    || text.into(),
                                                    |saved| saved.contribution.text.clone(),
                                                ),
                                        );
                                    }
                                    if ui
                                        .add_enabled(
                                            draft.is_none(),
                                            egui::Button::new("Release to draft").small(),
                                        )
                                        .clicked()
                                    {
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
                                    if ui.text_edit_multiline(changed).changed() {
                                        let mut contribution = own.clone();
                                        contribution.text = changed.clone();
                                        actions.push(Action::HoldDraft(
                                            contribution,
                                            revision,
                                            changed != text,
                                        ));
                                    }
                                    if ui.button("Save edit").clicked() {
                                        let mut contribution = own.clone();
                                        contribution.text = changed.clone();
                                        actions.push(Action::HoldChange(
                                            contribution,
                                            revision,
                                            Mutation::Edit,
                                        ));
                                        draft = None;
                                        ui.data_mut(|memory| memory.remove::<String>(edit));
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
            ui.weak(successor.map_or_else(
                || "Held follow-up started".into(),
                |number| format!("✓ held follow-up started run {number}"),
            ));
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
    #[ignore = "scripts/desktop-smoke.py supplies private paired daemons"]
    fn real_daemon_boundary_hold_release_and_edit_recovery() {
        use crate::{Saved, client::Client, model::Destination};
        use std::{
            path::PathBuf,
            time::{Duration, Instant},
        };

        let daemon_home = PathBuf::from(std::env::var_os("BOXEN_GUI_TEST_HOME").unwrap());
        let origin = std::env::var("BOXEN_GUI_TEST_URL").unwrap();
        let client = Client::new(origin.clone(), &daemon_home).unwrap();
        let drop_ack = Client::new(
            std::env::var("BOXEN_GUI_TEST_DROP_RESPONSE_URL").unwrap(),
            &daemon_home,
        )
        .unwrap();
        let destination: Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        let home = daemon_home.join("release-fixture");
        std::fs::create_dir(&home).unwrap();
        let state_path = home.join("desktop.json");
        let fixture = |client, saved| {
            crate::fixture_desktop(
                egui::Context::default(),
                Data {
                    saved,
                    ..Default::default()
                },
                home.clone(),
                origin.clone(),
                Some(client),
            )
        };
        let after = new_id();
        let key = ThreadKey {
            peer: destination.key.clone(),
            id: after.clone(),
        };
        client.operation(&destination, json!({
            "op":"start","spec":{"id":after,"thread_id":key.id,"backend":"claude","prompt":"INTERRUPT"}
        })).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while client
            .operation(&destination, json!({"op":"get","id":after}))
            .unwrap()["status"]
            != "running"
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        let id = new_id();
        let text = "Exact held input";
        let held = client
            .operation(
                &destination,
                json!({
                    "op":"held_followup","id":after,"action":"append","expected_revision":0,
                    "contribution":{"id":id,"text":text}
                }),
            )
            .unwrap();
        let contribution = Contribution {
            key: key.clone(),
            after: after.clone(),
            id,
            text: text.into(),
            files: vec![],
            accepted: true,
        };
        let mut saved = Saved {
            destinations: vec![destination.clone()],
            holds: vec![contribution.clone()],
            drafts: vec![(key.clone(), "Newer draft".into())],
            ..Default::default()
        };
        saved.workspace.enable_grid();
        let mut sender = fixture(drop_ack, saved);
        sender.mutate_hold(
            contribution,
            held["hold"]["revision"].as_u64(),
            Mutation::Release,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while sender.data.hold_pending.contains(&key) {
            sender.updates();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(sender.data.thread_issues[&key].contains("unconfirmed"));
        assert_eq!(sender.data.saved.hold_releases.len(), 1);
        drop(sender);

        let saved = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        let snapshot = client
            .operation(&destination, json!({"op":"get","id":after}))
            .unwrap();
        let mut restarted = fixture(client.clone(), saved);
        restarted.data.catalogs.insert(
            key.peer.clone(),
            crate::model::catalog(&key.peer, &[snapshot]),
        );
        restarted.observe();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !restarted.data.saved.hold_releases.is_empty() {
            restarted.updates();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(15));
        }
        restarted.reconcile_hold(&run_key(&key.peer, &after));
        assert_eq!(restarted.data.thread_issues[&key], "Released to draft");
        let recovered: Saved =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(
            recovered.drafts,
            [(key, "Exact held input\n\nNewer draft".into())]
        );
        assert!(recovered.holds.is_empty() && recovered.hold_releases.is_empty());
        assert_eq!(
            client
                .operation(&destination, json!({"op":"get","id":after}))
                .unwrap()["status"],
            "running"
        );
        client
            .operation(
                &destination,
                json!({"op":"control","id":after,"action":{"type":"cancel"}}),
            )
            .unwrap();

        let after = new_id();
        let key = ThreadKey {
            peer: destination.key.clone(),
            id: after.clone(),
        };
        let predecessor = client.operation(&destination, json!({
            "op":"start","spec":{"id":after,"thread_id":key.id,"backend":"claude","prompt":"HELD_FOLLOWUP_GATE"}
        })).unwrap();
        let id = new_id();
        let held = client
            .operation(
                &destination,
                json!({
                    "op":"held_followup","id":after,"action":"append","expected_revision":0,
                    "contribution":{"id":id,"text":"Accepted held input"}
                }),
            )
            .unwrap();
        let revision = held["hold"]["revision"].as_u64().unwrap();
        let actual = Contribution {
            key: key.clone(),
            after: after.clone(),
            id,
            text: "Accepted held input".into(),
            files: vec![],
            accepted: true,
        };
        let mut edited = actual.clone();
        edited.text = "Unsubmitted edit".into();
        restarted.data.saved.holds.push(actual);
        restarted.action(Action::HoldDraft(edited, revision, true));
        let saved: Saved = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(saved.hold_drafts[0].contribution.text, "Unsubmitted edit");
        restarted.data.catalogs.insert(
            key.peer.clone(),
            crate::model::catalog(&key.peer, std::slice::from_ref(&predecessor)),
        );
        restarted.observe();
        let cwd = predecessor["record"]["spec"]["cwd"].as_str().unwrap();
        std::fs::write(PathBuf::from(cwd).join("release-held-followup"), "").unwrap();
        let tag = run_key(&key.peer, &after);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            restarted.updates();
            if restarted
                .data
                .transcripts
                .get(&tag)
                .and_then(|t| t.hold.as_ref())
                .is_some_and(|hold| hold["phase"] == "started")
            {
                break;
            }
            let snapshot = client
                .operation(&destination, json!({"op":"get","id":after}))
                .unwrap();
            restarted.replace_snapshot(&key.peer, snapshot);
            // A terminal watch can end before the daemon journals the settled hold.
            // Keep this predecessor selected as an open conversation would.
            restarted.data.saved.workspace.open(key.clone(), true);
            restarted.observe();
            assert!(
                Instant::now() < deadline,
                "held successor did not reach desktop replay"
            );
            std::thread::sleep(Duration::from_millis(15));
        }
        let saved: Saved = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert!(saved.hold_drafts.is_empty());
        assert!(saved.drafts.contains(&(key, "Unsubmitted edit".into())));
        let successor = client
            .operation(
                &destination,
                json!({
                    "op":"get","id":held["hold"]["run_id"]
                }),
            )
            .unwrap();
        assert_eq!(successor["record"]["spec"]["prompt"], "Accepted held input");
    }

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
