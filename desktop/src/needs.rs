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
    ui.label(egui::RichText::new("REPLIES").monospace().small().weak());
    if replies.is_empty() {
        ui.weak("No unread replies observed");
    }
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
            ui.label(egui::RichText::new("REQUEST").monospace().small().weak());
            let mut preview: String = initial.text.chars().take(180).collect();
            if preview.len() < initial.text.len() {
                preview.push('…');
            }
            ui.label(preview).on_hover_text(&initial.text);
        });
    }
}

pub fn toolbar(ui: &mut egui::Ui, data: &Data, actions: &mut Vec<Action>) {
    let mut gates: Vec<(Active<'_>, &Row, bool)> = Vec::new();
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
                gates.push((
                    Active {
                        key: active.key.clone(),
                        ..active
                    },
                    row,
                    current,
                ));
            }
        }
    }
    let counted = gates.iter().filter(|(_, _, current)| *current).count();
    ui.menu_button(format!("Needs you · {counted}"), |ui| {
        ui.weak(format!("From {observed} of {total} active runs"));
        egui::ScrollArea::vertical()
            .max_height(460.)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("BLOCKING REQUESTS")
                        .monospace()
                        .small()
                        .weak(),
                );
                if gates.is_empty() {
                    ui.weak("No pending requests observed");
                }
                gates.sort_by(|(a, a_row, _), (b, b_row, _)| {
                    (&a.key.peer, a.id(), &a_row.id).cmp(&(&b.key.peer, b.id(), &b_row.id))
                });
                for (active, row, current) in gates {
                    let host = conversation::destination(data, &active.key.peer);
                    let title = if active.resident {
                        activity::title(data, &active.key)
                    } else {
                        find_thread(data, &active.key)
                            .map(|thread| thread.title.clone())
                            .unwrap_or_else(|| "Thread".into())
                    };
                    ui.push_id((&active.key.peer, active.id(), &row.id), |ui| {
                        ui.strong(title);
                        ui.weak(host.label);
                        request_context(ui, data, &active, row);
                        if current {
                            thread_view::backend_request(
                                ui,
                                row,
                                &active.key,
                                active.id(),
                                actions,
                            );
                        } else {
                            ui.weak(if active.reachable(data) {
                                "Waiting for current replay"
                            } else {
                                "Machine unreachable · request last seen pending"
                            });
                        }
                        if ui.small_button("Go to context").clicked() {
                            actions.push(Action::OpenGate(
                                active.key.clone(),
                                active.resident,
                                format!("{}:{}", active.id(), row.id),
                            ));
                            ui.close();
                        }
                        ui.separator();
                    });
                }
                ui.separator();
                replies(ui, data, actions);
            });
    });
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
}
