//! Projections of active run catalogs and their bounded journal observers.
use crate::{
    Action, Data, activity,
    client::run_key,
    conversation, find_thread,
    model::{Row, ThreadKey, terminal},
    thread_view,
};
use eframe::egui;
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
        if gates.is_empty() {
            ui.weak("No pending requests observed");
        }
        egui::ScrollArea::vertical()
            .max_height(460.)
            .show(ui, |ui| {
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
