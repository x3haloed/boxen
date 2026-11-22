//! Projections of active run catalogs and their bounded journal observers.
use crate::{
    Action, Data, activity,
    client::run_key,
    conversation, find_thread,
    model::{Row, ThreadKey, terminal},
    thread_view,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub struct Active<'a> {
    pub key: ThreadKey,
    pub run: &'a Value,
    resident: bool,
}

pub fn active_runs(data: &Data) -> impl Iterator<Item = Active<'_>> {
    let threads = data.catalogs.values().flatten().flat_map(|thread| {
        thread
            .runs
            .iter()
            .filter(|run| !terminal(run))
            .map(|run| Active {
                key: thread.key.clone(),
                run,
                resident: false,
            })
    });
    threads.chain(data.agents.active_runs().map(|(key, run)| Active {
        key,
        run,
        resident: true,
    }))
}

impl Active<'_> {
    pub fn id(&self) -> &str {
        self.run["record"]["spec"]["id"]
            .as_str()
            .unwrap_or_default()
    }

    fn reachable(&self, data: &Data) -> bool {
        !data.errors.contains_key(&self.key.peer)
            && (!self.resident || data.agents.error(&self.key).is_none())
    }

    pub fn observed(&self, data: &Data, tag: &str) -> bool {
        self.reachable(data) && replay_current(data, tag, self.run)
    }
}

pub fn replay_current(data: &Data, tag: &str, run: &Value) -> bool {
    if terminal(run) {
        return false;
    }
    let frontier = data
        .watches
        .get(tag)
        .filter(|watch| !watch.done && watch.error.is_none())
        .and_then(|watch| watch.frontier);
    data.transcripts.get(tag).is_some_and(|transcript| {
        frontier.is_some_and(|frontier| {
            let catalog = run["last_seq"].as_u64().unwrap_or(frontier);
            transcript.seq >= frontier.max(catalog)
        })
    })
}

#[derive(Default)]
pub struct Coverage {
    pub observed: usize,
    pub total: usize,
}

pub fn coverage(data: &Data) -> std::collections::HashMap<&str, Coverage> {
    let mut hosts = std::collections::HashMap::<&str, Coverage>::new();
    for active in active_runs(data) {
        let peer = data
            .saved
            .destinations
            .iter()
            .find(|host| host.key == active.key.peer)
            .map(|host| host.key.as_str());
        if let Some(peer) = peer {
            let count = hosts.entry(peer).or_default();
            count.total += 1;
            let tag = run_key(peer, active.id());
            count.observed += usize::from(active.observed(data, &tag));
        }
    }
    hosts
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ThreadReply {
    pub key: ThreadKey,
    pub run: String,
    pub row: String,
    pub at: u64,
}

pub fn settle_reply(data: &mut Data, key: &ThreadKey, run: &Value) -> bool {
    let Some(id) = run["record"]["spec"]["id"].as_str() else {
        return false;
    };
    let Some(watched) = data.saved.reply_watches.get_mut(&key.peer) else {
        return false;
    };
    if !watched.remove(id) {
        return false;
    }
    if run["status"] == "completed"
        && let Some(transcript) = data.transcripts.get(&run_key(&key.peer, id))
        && let (Some(row), Some(at)) = (&transcript.final_reply, transcript.completed_at)
        && data
            .saved
            .thread_replies
            .iter()
            .filter(|reply| &reply.key == key)
            .all(|reply| (reply.at, reply.run.as_str()) < (at, id))
    {
        data.saved.thread_replies.retain(|reply| &reply.key != key);
        data.saved.thread_replies.push(ThreadReply {
            key: key.clone(),
            run: id.into(),
            row: row.clone(),
            at,
        });
    }
    true
}

const REPLY_LIMIT: usize = 5;

fn unread_mail<'a>(data: &'a Data) -> impl Iterator<Item = (&'a ThreadKey, &'a Value)> + 'a {
    data.mail.messages.iter().filter(|(key, message)| {
        message["to"]["type"] == "client"
            && message["to"]["id"] == data.local_key
            && message["sender"]["type"] == "resident"
            && !data.saved.mail.read.contains(*key)
    })
}

fn unread_replies(data: &Data) -> usize {
    unread_mail(data).count() + data.saved.thread_replies.len()
}

fn replies(ui: &mut egui::Ui, data: &Data, actions: &mut Vec<Action>) {
    enum Reply<'a> {
        Mail(&'a ThreadKey, &'a Value),
        Thread(&'a ThreadReply),
    }
    let mut replies: Vec<(u64, Reply<'_>)> = data
        .mail
        .messages
        .iter()
        .filter(|(key, message)| {
            message["to"]["type"] == "client"
                && message["to"]["id"] == data.local_key
                && message["sender"]["type"] == "resident"
                && !data.saved.mail.read.contains(*key)
        })
        .map(|(key, message)| {
            (
                message["accepted_at"].as_u64().unwrap_or(0),
                Reply::Mail(key, message),
            )
        })
        .chain(
            data.saved
                .thread_replies
                .iter()
                .map(|reply| (reply.at, Reply::Thread(reply))),
        )
        .collect();
    replies.sort_unstable_by_key(|(at, _)| std::cmp::Reverse(*at));
    ui.label(
        egui::RichText::new("REPLIES · NOT BLOCKING")
            .monospace()
            .size(10.5)
            .weak(),
    );
    if replies.is_empty() {
        ui.weak("No unread replies observed");
    }
    let older = replies.len().saturating_sub(REPLY_LIMIT);
    replies.truncate(REPLY_LIMIT);
    for (at, reply) in replies {
        let action = ui
            .horizontal_wrapped(|ui| {
                let action = match reply {
                    Reply::Mail(key, message) => {
                        let author = ThreadKey {
                            peer: message["network"]["source_machine"]
                                .as_str()
                                .unwrap_or(&key.peer)
                                .into(),
                            id: message["sender"]["id"].as_str().unwrap_or("").into(),
                        };
                        let sender = activity::title(data, &author);
                        let subject = crate::correspondence::message_title(data, key);
                        ui.link(format!("{sender} to you · {subject}"))
                            .clicked()
                            .then(|| {
                                Action::Mail(crate::correspondence::MailAction::Open(
                                    key.clone(),
                                    false,
                                ))
                            })
                    }
                    Reply::Thread(reply) => {
                        let title = find_thread(data, &reply.key)
                            .map(|thread| thread.title.as_str())
                            .unwrap_or("Conversation");
                        ui.link(format!("{title} · finished with a reply"))
                            .clicked()
                            .then(|| {
                                Action::OpenReply(
                                    reply.key.clone(),
                                    format!("{}:{}", reply.run, reply.row),
                                )
                            })
                    }
                };
                if at > 0 {
                    ui.weak(crate::pulse::age(at));
                }
                action
            })
            .inner;
        if let Some(action) = action {
            actions.push(action);
            ui.close();
        }
    }
    if older > 0 {
        ui.weak(format!("{older} older unread · in Mail"));
    }
}

fn request_context(ui: &mut egui::Ui, data: &Data, active: &Active<'_>, gate: &Row) {
    let tag = run_key(&active.key.peer, active.id());
    let Some(transcript) = data.transcripts.get(&tag) else {
        return;
    };
    if active.resident {
        if let Some(count) = transcript.activation_mail_count
            && transcript
                .rows
                .iter()
                .any(|row| row.label == "Activation input")
        {
            ui.weak(format!(
                "Activation input · {count} {} presented",
                if count == 1 { "mail" } else { "mails" }
            ));
        }
        return;
    }
    if let Some(initial) = transcript
        .rows
        .iter()
        .take_while(|row| row.id != gate.id)
        .find(|row| row.label == "You" && row.update_id.is_none())
    {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("REQUEST").monospace().size(10.5).weak());
            let mut preview: String = initial.text.chars().take(180).collect();
            if preview.len() < initial.text.len() {
                preview.push('…');
            }
            ui.label(preview).on_hover_text(&initial.text);
        });
    }
    let before: Vec<_> = transcript
        .rows
        .iter()
        .position(|row| row.id == gate.id)
        .into_iter()
        .flat_map(|position| transcript.rows[..position].iter().rev())
        .filter(|row| row.label == "Command" && row.tool.as_ref().is_some_and(|tool| tool.finished))
        .take(2)
        .collect();
    if !before.is_empty() {
        ui.weak("BEFORE");
        for row in before.into_iter().rev() {
            ui.horizontal_wrapped(|ui| {
                ui.monospace(row.text.lines().next().unwrap_or(""));
                if let Some(result) = row.tool.as_ref().and_then(|tool| tool.result.as_deref()) {
                    ui.weak(result);
                }
            });
        }
    }
}

struct Gate<'a> {
    active: Active<'a>,
    row: &'a Row,
    current: bool,
}

impl Gate<'_> {
    fn identity(&self) -> (&str, &str, &str) {
        (&self.active.key.peer, self.active.id(), &self.row.id)
    }

    fn open(&self) -> Action {
        Action::OpenGate(
            self.active.key.clone(),
            self.active.resident,
            format!("{}:{}", self.active.id(), self.row.id),
        )
    }
}

#[derive(Clone)]
struct SelectedGate {
    identity: (String, String, String),
    focus: egui::Id,
}

pub fn toolbar(ui: &mut egui::Ui, data: &Data, actions: &mut Vec<Action>) {
    let mut gates = Vec::new();
    let mut total = 0;
    let mut observed = 0;
    for active in active_runs(data) {
        total += 1;
        let tag = run_key(&active.key.peer, active.id());
        let current = active.observed(data, &tag);
        observed += usize::from(current);
        if active.run["record"]["spec"]["backend"] == "claude" {
            continue;
        }
        if let Some(transcript) = data.transcripts.get(&tag) {
            for row in transcript.rows.iter().filter(|row| row.request.is_some()) {
                gates.push(Gate {
                    active: Active {
                        key: active.key.clone(),
                        ..active
                    },
                    row,
                    current,
                });
            }
        }
    }
    gates.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| a.identity().cmp(&b.identity()))
    });
    let counted = gates.iter().filter(|gate| gate.current).count();
    let selection_id = ui.make_persistent_id("needs-gate-selection");
    let unread = unread_replies(data);
    if counted == 0 && unread == 0 && gates.is_empty() {
        return;
    }
    let button = if counted > 0 {
        egui::Button::new(
            egui::RichText::new(format!("! {counted} need you"))
                .monospace()
                .size(10.5)
                .color(crate::console_style::WARN),
        )
        .fill(egui::Color32::from_rgb(34, 27, 14))
        .stroke(egui::Stroke::new(1., egui::Color32::from_rgb(90, 67, 24)))
    } else {
        egui::Button::new(
            egui::RichText::new(format!("{unread} unread"))
                .monospace()
                .size(10.5)
                .weak(),
        )
        .frame(false)
    };
    let menu = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        ui.set_width(420.);
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!("From {observed} of {total} active runs"));
            if !gates.is_empty() {
                ui.weak("J/K select · ↵ first decision · G context");
            }
        });
        let previous = ui.data_mut(|state| state.get_temp::<SelectedGate>(selection_id));
        let previous_index = previous.as_ref().and_then(|selected| {
            let (peer, run, row) = &selected.identity;
            gates
                .iter()
                .position(|gate| gate.identity() == (peer, run, row))
        });
        let owns_keys = previous
            .as_ref()
            .is_some_and(|previous| ui.memory(|memory| memory.focused() == Some(previous.focus)));
        let mut index = previous_index.unwrap_or_default();
        let mut open = false;
        let mut enter = false;
        if owns_keys && previous_index.is_some() {
            let (down, up, context, decision) = ui.input_mut(|input| {
                let modifiers = egui::Modifiers::NONE;
                let fresh_enter = input.events.iter().any(|event| {
                    matches!(event, egui::Event::Key {
                        key: egui::Key::Enter, pressed: true, repeat: false, modifiers, ..
                    } if *modifiers == egui::Modifiers::NONE)
                });
                (
                    input.consume_key(modifiers, egui::Key::J),
                    input.consume_key(modifiers, egui::Key::K),
                    input.consume_key(modifiers, egui::Key::G),
                    input.consume_key(modifiers, egui::Key::Enter)
                        && fresh_enter
                        && !input.pointer.any_released(),
                )
            });
            if down {
                index = (index + 1).min(gates.len().saturating_sub(1));
            } else if up {
                index = index.saturating_sub(1);
            } else {
                open = context;
                enter = decision && !context;
            }
        }
        let move_focus = previous_index != Some(index);
        egui::ScrollArea::vertical()
            .max_height(460.)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("BLOCKING REQUESTS")
                        .monospace()
                        .size(10.5)
                        .weak(),
                );
                if gates.is_empty() {
                    ui.weak("No pending requests observed");
                }
                let mut selected = None;
                for (position, gate) in gates.iter().enumerate() {
                    let Gate {
                        active,
                        row,
                        current,
                    } = gate;
                    let host = conversation::destination(data, &active.key.peer);
                    let title = if active.resident {
                        activity::title(data, &active.key)
                    } else {
                        find_thread(data, &active.key)
                            .map(|thread| thread.title.clone())
                            .unwrap_or_else(|| "Thread".into())
                    };
                    ui.push_id(gate.identity(), |ui| {
                        let header = ui.selectable_label(
                            index == position,
                            format!("{title} · {}", host.label),
                        );
                        if header.clicked() {
                            index = position;
                            header.request_focus();
                        }
                        if index != position {
                            ui.label(
                                egui::RichText::new(row.text.lines().next().unwrap_or("")).weak(),
                            );
                            if let Some(answer) = &row.answer {
                                ui.weak(answer.label());
                            }
                        } else {
                            if move_focus {
                                header.request_focus();
                                header.scroll_to_me(None);
                            }
                            let (peer, run, request) = gate.identity();
                            selected = Some(SelectedGate {
                                identity: (peer.into(), run.into(), request.into()),
                                focus: header.id,
                            });
                            request_context(ui, data, active, row);
                            if *current {
                                thread_view::request_controls(
                                    ui,
                                    row,
                                    &active.key,
                                    active.id(),
                                    actions,
                                    Some(enter && previous_index == Some(position)),
                                );
                            }
                            if ui.small_button("Go to context (G)").clicked()
                                || (open && previous_index == Some(position))
                            {
                                actions.push(gate.open());
                                ui.close();
                            }
                        }
                        if !current {
                            ui.weak(if active.reachable(data) {
                                "Waiting for current replay"
                            } else {
                                "Machine unreachable · controls return if replay shows it pending"
                            });
                        }
                        ui.separator();
                    });
                }
                if let Some(selected) = selected {
                    ui.data_mut(|state| state.insert_temp(selection_id, selected));
                }
                ui.separator();
                replies(ui, data, actions);
            });
    });
    if menu.1.is_none() || gates.is_empty() {
        ui.data_mut(|state| state.remove::<SelectedGate>(selection_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Observing, model::Destination};
    use serde_json::json;
    use std::time::Instant;

    #[test]
    fn thread_reply_requires_completed_work_and_final_answer() {
        let mut data = Data::default();
        let key = ThreadKey {
            peer: "fixture".into(),
            id: "conversation".into(),
        };
        let snapshot = |id| json!({"status":"completed","record":{"spec":{"id":id}}});
        for (id, phase) in [("commentary", "commentary"), ("final", "final_answer")] {
            data.saved
                .reply_watches
                .entry(key.peer.clone())
                .or_default()
                .insert(id.into());
            let transcript = data.transcripts.entry(run_key(&key.peer, id)).or_default();
            transcript.apply(&json!({"seq":1,"at":1000,"kind":"backend","data":{
                "method":"item/completed","params":{"item":{"type":"agentMessage","phase":phase,"id":"answer","text":"Fixture reply"}}
            }})).unwrap();
            transcript
                .apply(&json!({"seq":2,"at":2000,"kind":"status","data":{"status":"completed"}}))
                .unwrap();
            assert!(settle_reply(&mut data, &key, &snapshot(id)));
            assert_eq!(data.saved.thread_replies.len(), usize::from(id == "final"));
            assert!(!settle_reply(&mut data, &key, &snapshot(id)));
        }
        assert_eq!(data.saved.thread_replies[0].row, "answer");
    }

    #[test]
    fn reachable_host_can_have_partial_coverage() {
        let mut data = Data::default();
        let host = "fixture-host";
        data.saved.destinations.push(Destination {
            key: host.into(),
            label: "Fixture host".into(),
            address: String::new(),
        });
        let snapshot = |id| json!({"status":"waiting","last_seq":2,"record":{"spec":{"id":id,"thread_id":"thread"}}});
        data.catalogs.insert(
            host.into(),
            crate::model::catalog(host, &[snapshot("old"), snapshot("head")]),
        );
        let tag = run_key(host, "head");
        data.watches.insert(
            tag.clone(),
            Observing {
                key: ThreadKey {
                    peer: host.into(),
                    id: "thread".into(),
                },
                epoch: 1,
                stop: Default::default(),
                retry: Instant::now(),
                error: None,
                done: false,
                frontier: Some(2),
            },
        );
        data.transcripts.entry(tag.clone()).or_default().seq = 1;
        let hosts = coverage(&data);
        assert_eq!((hosts[host].observed, hosts[host].total), (0, 2));
        data.transcripts.get_mut(&tag).unwrap().seq = 2;
        let hosts = coverage(&data);
        assert_eq!((hosts[host].observed, hosts[host].total), (1, 2));
        data.errors.insert(host.into(), "offline".into());
        let hosts = coverage(&data);
        assert_eq!((hosts[host].observed, hosts[host].total), (0, 2));
    }

    #[test]
    fn queue_keys_act_on_the_displayed_exact_request() {
        let mut data = Data::default();
        let host = "test-host";
        let mut runs = vec![];
        for (run, request) in [("first", 5), ("second", 8)] {
            let key = ThreadKey {
                peer: host.into(),
                id: run.into(),
            };
            let tag = run_key(host, run);
            data.transcripts.entry(tag.clone()).or_default().apply(&json!({
                "seq":1,"kind":"backend","data":{
                    "id":request,"method":"item/commandExecution/requestApproval",
                    "params":{"command":format!("{run} command"),"availableDecisions":["decline","accept"]}
                }
            })).unwrap();
            data.watches.insert(
                tag,
                Observing {
                    key,
                    epoch: 1,
                    stop: Default::default(),
                    retry: Instant::now(),
                    error: None,
                    done: false,
                    frontier: Some(1),
                },
            );
            runs.push(json!({
                "status":"waiting","last_seq":1,
                "record":{"spec":{"id":run,"thread_id":run,"backend":"codex","prompt":run}}
            }));
        }
        data.catalogs
            .insert(host.into(), crate::model::catalog(host, &runs));
        let ctx = egui::Context::default();
        let frame = |data: &Data, events| {
            let mut actions = vec![];
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(960., 720.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| toolbar(ui, data, &mut actions),
            );
            (actions, output)
        };
        let (_, output) = frame(&data, vec![]);
        let click = output
            .shapes
            .iter()
            .find_map(|shape| {
                let egui::Shape::Text(text) = &shape.shape else {
                    return None;
                };
                (text.galley.job.text == "! 2 need you").then(|| text.pos + text.galley.size() / 2.)
            })
            .unwrap();
        let pointer = |pressed| egui::Event::PointerButton {
            pos: click,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&data, vec![egui::Event::PointerMoved(click), pointer(true)]);
        assert!(frame(&data, vec![pointer(false)]).0.is_empty());
        frame(&data, vec![]);
        let key = |key| {
            [true, false].map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        };
        assert!(frame(&data, key(egui::Key::J).to_vec()).0.is_empty());
        let (actions, _) = frame(&data, vec![key(egui::Key::Enter)[0].clone()]);
        assert!(matches!(actions.as_slice(),
            [Action::Respond(target, run, id, result)]
                if target.id == "second" && run == "second" && *id == 8
                    && result["decision"] == "decline"
        ));
        // A second press event without an intervening release is key repeat.
        assert!(frame(&data, key(egui::Key::Enter).to_vec()).0.is_empty());
        data.errors.insert(host.into(), "offline".into());
        assert!(frame(&data, key(egui::Key::Enter).to_vec()).0.is_empty());
        data.errors.clear();
        assert!(frame(&data, key(egui::Key::K).to_vec()).0.is_empty());
        let (actions, _) = frame(&data, key(egui::Key::G).to_vec());
        assert!(matches!(actions.as_slice(),
            [Action::OpenGate(target, false, anchor)]
                if target.id == "first" && anchor == "first:event-1"
        ));
    }
}
