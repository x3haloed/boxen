mod client;
mod model;
mod workspace;
use client::{Bus, Client, Pending, Update, run_key};
use eframe::egui::{self, RichText};
use egui_dock::{DockArea, TabViewer};
use model::{Conversation, Destination, Pane, ThreadKey, Transcript, catalog, terminal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use workspace::Workspace;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    workspace: Workspace,
    destinations: Vec<Destination>,
    selected: usize,
    hidden_machines: HashSet<String>,
    drafts: Vec<(ThreadKey, String)>,
    pending: Vec<Pending>,
}
struct Observing {
    epoch: u64,
    stop: Arc<AtomicBool>,
    retry: Instant,
    error: Option<String>,
    done: bool,
}
struct Data {
    local_key: String,
    saved: Saved,
    catalogs: HashMap<String, Vec<Conversation>>,
    errors: HashMap<String, String>,
    transcripts: HashMap<String, Transcript>,
    watches: HashMap<String, Observing>,
    selected_thread: Option<ThreadKey>,
    filter: String,
    notice: String,
    sending: HashSet<String>,
}
enum Action {
    Open(ThreadKey, bool),
    Pin(ThreadKey),
    Send(ThreadKey),
    Cancel(ThreadKey),
    Retry(usize),
    Split(Pane, egui_dock::Split),
}
struct Desktop {
    data: Data,
    client: Option<Client>,
    bus: Bus,
    home: PathBuf,
    state_path: PathBuf,
    origin: String,
    polls: HashMap<String, Arc<AtomicBool>>,
    palette: bool,
    palette_fresh: bool,
    query: String,
    choice: usize,
    connection: bool,
    new_thread: bool,
    cwd: String,
    backend: String,
    model: String,
    prompt: String,
    last_save: Instant,
    next_watch: u64,
    ready: bool,
    extra: Option<Destination>,
}
fn private_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let temporary = path.with_extension("next");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&temporary).map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}
impl Desktop {
    fn new(
        cc: &eframe::CreationContext<'_>,
        home: PathBuf,
        origin: String,
        extra: Option<Destination>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut_of(egui::Theme::Dark, |s| {
            s.spacing.item_spacing = egui::vec2(8., 8.);
            s.visuals.selection.bg_fill = egui::Color32::from_rgb(46, 72, 91);
        });
        let state_path = home.join("desktop.json");
        let saved = std::fs::read(&state_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let bus = Bus::new(cc.egui_ctx.clone());
        let mut app = Self {
            data: Data {
                local_key: String::new(),
                saved,
                catalogs: HashMap::new(),
                errors: HashMap::new(),
                transcripts: HashMap::new(),
                watches: HashMap::new(),
                selected_thread: None,
                filter: String::new(),
                notice: String::new(),
                sending: HashSet::new(),
            },
            client: None,
            bus,
            home,
            state_path,
            origin: origin.clone(),
            polls: HashMap::new(),
            palette: false,
            palette_fresh: false,
            query: String::new(),
            choice: 0,
            connection: false,
            new_thread: false,
            cwd: String::new(),
            backend: "codex".into(),
            model: String::new(),
            prompt: String::new(),
            last_save: Instant::now(),
            next_watch: 0,
            ready: false,
            extra,
        };
        app.connect();
        app
    }
    fn connect(&mut self) {
        self.ready = false;
        for (_, stop) in self.polls.drain() {
            stop.store(true, Ordering::Relaxed)
        }
        for (_, w) in self.data.watches.drain() {
            w.stop.store(true, Ordering::Relaxed)
        }
        match Client::new(self.origin.clone(), &self.home) {
            Ok(client) => {
                client.discover(self.bus.publisher());
                self.client = Some(client);
                self.data.notice = "Connecting to daemon…".into();
            }
            Err(error) => {
                self.client = None;
                self.data.notice = format!("Daemon unavailable: {error}");
            }
        }
    }
    fn discovered(&mut self, status: Value) {
        if status["api_version"] != 2 {
            self.data.notice = "This desktop client requires daemon API version 2.".into();
            return;
        }
        let local = status["identity"].as_str().unwrap_or("");
        self.data.local_key = local.into();
        let mut discovered = vec![Destination {
            key: local.into(),
            label: "This machine".into(),
            address: String::new(),
        }];
        for p in status["peers"].as_array().into_iter().flatten() {
            let key = p["key"].as_str().unwrap_or("");
            let existing = self.data.saved.destinations.iter().find(|d| d.key == key);
            discovered.push(Destination {
                key: key.into(),
                label: p["label"].as_str().unwrap_or("Paired machine").into(),
                address: existing.map(|d| d.address.clone()).unwrap_or_default(),
            });
        }
        if let Some(extra) = self.extra.take()
            && let Some(d) = discovered.iter_mut().find(|d| d.key == extra.key)
        {
            d.address = extra.address
        }
        self.data.saved.destinations = discovered;
        self.data.saved.selected = self
            .data
            .saved
            .selected
            .min(self.data.saved.destinations.len().saturating_sub(1));
        self.ready = true;
        self.data.notice.clear();
    }
    fn persist(&mut self) -> bool {
        let result = serde_json::to_vec_pretty(&self.data.saved)
            .map_err(|e| e.to_string())
            .and_then(|b| private_write(&self.state_path, &b));
        self.last_save = Instant::now();
        if let Err(error) = result {
            self.data.notice = format!("Desktop state could not be saved: {error}");
            false
        } else {
            true
        }
    }
    fn poll(&mut self) {
        if !self.ready {
            return;
        }
        let Some(client) = self.client.as_ref() else {
            return;
        };
        for dest in &self.data.saved.destinations {
            if dest.address.is_empty() && dest.key != self.data.local_key {
                continue;
            }
            if !self.polls.contains_key(&dest.key) {
                let stop = Arc::new(AtomicBool::new(false));
                client.catalog(dest.clone(), self.bus.publisher(), stop.clone());
                self.polls.insert(dest.key.clone(), stop);
            }
        }
    }
    fn updates(&mut self) {
        while let Ok(update) = self.bus.rx.try_recv() {
            match update {
                Update::Discovered(result) => match result {
                    Ok(status) => self.discovered(status),
                    Err(error) => self.data.notice = format!("Daemon unavailable: {error}"),
                },
                Update::Catalog(dest, result) => match result {
                    Ok(runs) => {
                        self.data.errors.remove(&dest.key);
                        // An accepted pending request is reconciled by exact ID; no new request is generated.
                        let accepted: HashSet<&str> = runs
                            .iter()
                            .filter_map(|r| r["record"]["spec"]["id"].as_str())
                            .collect();
                        let reconciled: Vec<_> = self
                            .data
                            .saved
                            .pending
                            .iter()
                            .filter(|p| {
                                p.destination.key == dest.key
                                    && accepted.contains(p.spec["id"].as_str().unwrap_or(""))
                            })
                            .cloned()
                            .collect();
                        for p in reconciled {
                            self.accept_pending(&p);
                        }
                        self.data
                            .catalogs
                            .insert(dest.key.clone(), catalog(&dest.key, &runs));
                    }
                    Err(error) => {
                        self.data.errors.insert(dest.key, error);
                    }
                },
                Update::Batch(id, epoch, batch) => {
                    if self.data.watches.get(&id).is_none_or(|w| w.epoch != epoch) {
                        continue;
                    }
                    let transcript = self.data.transcripts.entry(id.clone()).or_default();
                    let mut failed = None;
                    for event in batch["events"].as_array().into_iter().flatten() {
                        if let Err(e) = transcript.apply(event) {
                            failed = Some(e);
                            break;
                        }
                    }
                    if let Some(w) = self.data.watches.get_mut(&id) {
                        w.error = failed;
                        w.done = batch["done"] == true && w.error.is_none();
                        if w.error.is_some() {
                            w.stop.store(true, Ordering::Relaxed);
                            w.retry = Instant::now() + Duration::from_secs(2);
                        }
                    }
                    if let Some(s) = batch.get("snapshot") {
                        self.replace_snapshot(id.split(':').next().unwrap_or(""), s.clone());
                    }
                }
                Update::WatchError(id, epoch, error) => {
                    if let Some(w) = self.data.watches.get_mut(&id)
                        && w.epoch == epoch
                    {
                        w.error = Some(error);
                        w.retry = Instant::now() + Duration::from_secs(2);
                    }
                }
                Update::Controlled(id, dest, text, result) => {
                    self.data.sending.remove(&id);
                    match result {
                        Ok(_) => {
                            if !text.is_empty()
                                && let Some(thread) = self
                                    .data
                                    .catalogs
                                    .get(&dest.key)
                                    .into_iter()
                                    .flatten()
                                    .find(|t| {
                                        t.runs.iter().any(|r| r["record"]["spec"]["id"] == id)
                                    })
                                && let Some((_, draft)) = self
                                    .data
                                    .saved
                                    .drafts
                                    .iter_mut()
                                    .find(|(k, d)| k == &thread.key && d.trim() == text)
                            {
                                draft.clear();
                            }
                            self.data.notice =
                                "Control accepted; runtime outcome appears in the stream.".into();
                        }
                        Err(error) => {
                            self.data.notice = format!(
                                "Control outcome uncertain: {error}. The draft is retained; this action has not been automatically repeated."
                            )
                        }
                    }
                }
                Update::Submitted(id, result) => {
                    self.data.sending.remove(&id);
                    match result {
                        Ok(snapshot) => {
                            if let Some(index) = self
                                .data
                                .saved
                                .pending
                                .iter()
                                .position(|p| p.spec["id"] == id)
                            {
                                let p = self.data.saved.pending.remove(index);
                                let key = ThreadKey {
                                    peer: p.destination.key.clone(),
                                    id: p.spec["thread_id"].as_str().unwrap_or("").into(),
                                };
                                if let Some(draft) =
                                    self.data.saved.drafts.iter_mut().find(|(k, d)| {
                                        k == &key
                                            && d.trim() == p.spec["prompt"].as_str().unwrap_or("")
                                    })
                                {
                                    draft.1.clear()
                                }
                                let list = self.data.catalogs.entry(key.peer.clone()).or_default();
                                if let Some(t) = list.iter_mut().find(|t| t.key == key) {
                                    if !t.runs.iter().any(|r| r["record"]["spec"]["id"] == id) {
                                        t.runs.push(snapshot)
                                    }
                                } else {
                                    list.insert(
                                        0,
                                        Conversation {
                                            key,
                                            title: p.spec["prompt"]
                                                .as_str()
                                                .unwrap_or("")
                                                .chars()
                                                .take(72)
                                                .collect(),
                                            runs: vec![snapshot],
                                        },
                                    );
                                }
                                self.persist();
                            } else {
                                self.data.notice =
                                    "Control accepted; runtime outcome appears in the stream."
                                        .into();
                            }
                        }
                        Err(error) => {
                            self.data.notice = format!(
                                "Request outcome: {error}. A retained pending submission can be retried with the same ID; control actions are not automatically repeated."
                            )
                        }
                    }
                }
            }
        }
    }
    fn accept_pending(&mut self, p: &Pending) {
        let key = ThreadKey {
            peer: p.destination.key.clone(),
            id: p.spec["thread_id"].as_str().unwrap_or("").into(),
        };
        if let Some((_, draft)) = self
            .data
            .saved
            .drafts
            .iter_mut()
            .find(|(k, d)| k == &key && d.trim() == p.spec["prompt"].as_str().unwrap_or(""))
        {
            draft.clear();
        }
        self.data
            .saved
            .pending
            .retain(|v| v.destination.key != p.destination.key || v.spec["id"] != p.spec["id"]);
    }
    fn replace_snapshot(&mut self, peer: &str, snapshot: Value) {
        if let Some(threads) = self.data.catalogs.get_mut(peer) {
            for t in threads {
                if let Some(run) = t
                    .runs
                    .iter_mut()
                    .find(|r| r["record"]["spec"]["id"] == snapshot["record"]["spec"]["id"])
                {
                    *run = snapshot;
                    return;
                }
            }
        }
    }
    fn observe(&mut self) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        let open: HashSet<ThreadKey> = self
            .data
            .saved
            .workspace
            .dock
            .iter_all_tabs()
            .filter_map(|(_, p)| {
                if let Pane::Thread(k) = p {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut needed = HashSet::new();
        for key in open {
            let Some(thread) = find_thread(&self.data, &key) else {
                continue;
            };
            let runs = thread.runs.clone();
            let Some(dest) = self
                .data
                .saved
                .destinations
                .iter()
                .find(|d| d.key == key.peer)
                .cloned()
            else {
                continue;
            };
            for run in runs {
                let id = run["record"]["spec"]["id"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                let tag = run_key(&key.peer, &id);
                needed.insert(tag.clone());
                let seq = self.data.transcripts.get(&tag).map(|t| t.seq).unwrap_or(0);
                if terminal(&run) && seq >= run["last_seq"].as_u64().unwrap_or(0) {
                    continue;
                }
                let retry = match self.data.watches.get(&tag) {
                    None => true,
                    Some(w) => w.error.is_some() && Instant::now() >= w.retry,
                };
                if retry {
                    // Bound simultaneously observed runs. Replay of additional runs proceeds as streams finish.
                    let active = self
                        .data
                        .watches
                        .values()
                        .filter(|w| !w.done && w.error.is_none())
                        .count();
                    if active >= 16 {
                        continue;
                    }
                    let stop = Arc::new(AtomicBool::new(false));
                    self.next_watch += 1;
                    client.watch(
                        dest.clone(),
                        id.clone(),
                        seq,
                        self.next_watch,
                        self.bus.publisher(),
                        stop.clone(),
                    );
                    self.data.watches.insert(
                        tag,
                        Observing {
                            epoch: self.next_watch,
                            stop,
                            retry: Instant::now(),
                            error: None,
                            done: false,
                        },
                    );
                }
            }
        }
        self.data.watches.retain(|id, w| {
            if !needed.contains(id) {
                w.stop.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }
    fn send(&mut self, key: ThreadKey) {
        let Some(thread) = find_thread(&self.data, &key) else {
            return;
        };
        let Some(last) = thread.runs.last().cloned() else {
            return;
        };
        let text = self
            .data
            .saved
            .drafts
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, d)| d.trim().to_owned())
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let Some(dest) = self
            .data
            .saved
            .destinations
            .iter()
            .find(|d| d.key == key.peer)
            .cloned()
        else {
            return;
        };
        if !terminal(&last) {
            let id = last["record"]["spec"]["id"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            if self.data.sending.insert(id.clone())
                && let Some(client) = &self.client
            {
                client.control(
                    dest,
                    id,
                    json!({"type":"steer","text":text}),
                    self.bus.publisher(),
                );

                self.data.notice =
                    "Steering submitted once; an uncertain response is not automatically retried."
                        .into();
            }
        } else {
            let mut spec = last["record"]["spec"].clone();
            spec["id"] = json!(new_id());
            spec["prompt"] = json!(text);
            self.submit(Pending {
                destination: dest,
                spec,
            });
        }
    }
    fn submit(&mut self, pending: Pending) {
        let id = pending.spec["id"].as_str().unwrap_or("").to_owned();
        if self.data.sending.contains(&id) {
            return;
        }
        if !self.data.saved.pending.iter().any(|p| p.spec["id"] == id) {
            self.data.saved.pending.push(pending.clone());
        }
        // The idempotency key and exact spec are durable before the network request.
        if self.persist()
            && let Some(client) = &self.client
        {
            self.data.sending.insert(id);
            client.submit(pending, self.bus.publisher());
        }
    }
    fn action(&mut self, action: Action) {
        match action {
            Action::Split(pane, direction) => {
                if let Some(path) = self.data.saved.workspace.dock.find_tab(&pane) {
                    if let Pane::Thread(ref key) = pane {
                        self.data.saved.workspace.pin(key);
                    }
                    // Move the same pane into a new group; a split never clones conversation state.
                    let target = if self
                        .data
                        .saved
                        .workspace
                        .dock
                        .leaf(path.node_path())
                        .is_ok_and(|n| n.tabs.len() > 1)
                    {
                        path.node_path()
                    } else {
                        self.data
                            .saved
                            .workspace
                            .dock
                            .iter_all_tabs()
                            .find(|(p, _)| p.node_path() != path.node_path())
                            .map(|(p, _)| p.node_path())
                            .unwrap_or(path.node_path())
                    };
                    self.data.saved.workspace.dock.move_tab(
                        path,
                        egui_dock::TabDestination::Node(
                            target,
                            egui_dock::TabInsert::Split(direction),
                        ),
                    );
                }
            }
            Action::Open(key, pin) => {
                self.data.selected_thread = Some(key.clone());
                self.data.saved.workspace.open(key, pin)
            }
            Action::Pin(key) => self.data.saved.workspace.pin(&key),
            Action::Send(key) => self.send(key),
            Action::Retry(index) => {
                if let Some(p) = self.data.saved.pending.get(index).cloned() {
                    self.submit(p)
                }
            }
            Action::Cancel(key) => {
                if let Some(thread) = find_thread(&self.data, &key)
                    && let Some(run) = thread.runs.last()
                    && let Some(dest) = self
                        .data
                        .saved
                        .destinations
                        .iter()
                        .find(|d| d.key == key.peer)
                    && let Some(client) = &self.client
                {
                    client.control(
                        dest.clone(),
                        run["record"]["spec"]["id"].as_str().unwrap_or("").into(),
                        json!({"type":"cancel"}),
                        self.bus.publisher(),
                    );
                }
            }
        }
    }
    fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette {
            return;
        }
        let mut commands: Vec<(String, Option<ThreadKey>)> = vec![
            ("New thread…".into(), None),
            ("Show thread list".into(), None),
            ("Reset docking layout".into(), None),
            ("Machine connections…".into(), None),
        ];
        for dest in &self.data.saved.destinations {
            for thread in self.data.catalogs.get(&dest.key).into_iter().flatten() {
                commands.push((
                    format!("{} · {}", thread.title, dest.label),
                    Some(thread.key.clone()),
                ));
            }
        }
        let query = self.query.to_lowercase();
        let visible: Vec<_> = commands
            .iter()
            .enumerate()
            .filter(|(_, c)| c.0.to_lowercase().contains(&query))
            .collect();
        self.choice = self.choice.min(visible.len().saturating_sub(1));
        let mut picked = None;
        egui::Window::new("Command palette")
            .id(egui::Id::new("palette"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0., 72.])
            .default_width(620.)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search commands and threads…")
                        .desired_width(f32::INFINITY),
                );
                if self.palette_fresh {
                    response.request_focus();
                    self.palette_fresh = false;
                }
                if response.changed() {
                    self.choice = 0
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                    self.choice = (self.choice + 1).min(visible.len().saturating_sub(1));
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                    self.choice = self.choice.saturating_sub(1);
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                    picked = visible.get(self.choice).map(|(i, _)| *i);
                }
                egui::ScrollArea::vertical()
                    .max_height(400.)
                    .show(ui, |ui| {
                        for (row, (index, (label, _))) in visible.iter().enumerate() {
                            let response = ui.selectable_label(row == self.choice, label);
                            if row == self.choice {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                picked = Some(*index)
                            }
                        }
                    });
                ui.small("↑ ↓ navigate   Enter open   Esc close");
            });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.palette = false;
        }
        if let Some(index) = picked {
            self.palette = false;
            if let Some(key) = commands[index].1.clone() {
                self.action(Action::Open(key, true))
            } else {
                match index {
                    0 => self.new_thread = true,
                    1 => self.data.saved.workspace.show_list(),
                    2 => {
                        let keys: Vec<_> = self
                            .data
                            .saved
                            .workspace
                            .dock
                            .iter_all_tabs()
                            .filter_map(|(_, p)| {
                                if let Pane::Thread(k) = p {
                                    Some(k.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        let preview = self.data.saved.workspace.preview.clone();
                        self.data.saved.workspace = Workspace::default();
                        for k in keys {
                            self.data
                                .saved
                                .workspace
                                .open(k.clone(), preview.as_ref() != Some(&k));
                        }
                    }
                    3 => self.connection = true,
                    _ => {}
                }
            }
        }
    }
}
fn find_thread<'a>(data: &'a Data, key: &ThreadKey) -> Option<&'a Conversation> {
    data.catalogs.get(&key.peer)?.iter().find(|t| &t.key == key)
}
fn new_id() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS randomness unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn thread_rows(
    ui: &mut egui::Ui,
    data: &Data,
    actions: &mut Vec<Action>,
    dest: &Destination,
    query: &str,
) {
    if let Some(error) = data.errors.get(&dest.key) {
        ui.colored_label(egui::Color32::LIGHT_RED, format!("Unreachable · {error}"));
    }
    let Some(threads) = data.catalogs.get(&dest.key) else {
        ui.weak(if dest.address.is_empty() && dest.key != data.local_key {
            "An address is needed in Commands → Machine connections."
        } else {
            "Connecting…"
        });
        return;
    };
    if threads.is_empty() {
        ui.weak("No anonymous threads on this machine.");
        return;
    }
    let mut matches = false;
    for thread in threads
        .iter()
        .filter(|t| t.title.to_lowercase().contains(query))
    {
        matches = true;
        let selected = data.selected_thread.as_ref() == Some(&thread.key);
        let response = ui
            .push_id((&thread.key.peer, &thread.key.id), |ui| {
                ui.add(
                    egui::Button::selectable(selected, &thread.title)
                        .wrap()
                        .min_size(egui::vec2(ui.available_width(), 32.)),
                )
            })
            .inner;
        if response.double_clicked() {
            actions.push(Action::Open(thread.key.clone(), true));
        } else if response.clicked() {
            actions.push(Action::Open(thread.key.clone(), false));
        }
        response.on_hover_text(format!(
            "{}\n{}",
            thread.key.id,
            thread
                .runs
                .last()
                .and_then(|r| r["status"].as_str())
                .unwrap_or("")
        ));
    }
    if !matches {
        ui.weak("No matching threads.");
    }
}

struct Viewer<'a> {
    data: &'a mut Data,
    actions: &'a mut Vec<Action>,
}
impl TabViewer for Viewer<'_> {
    type Tab = Pane;
    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        match tab {
            Pane::Threads => "Threads".into(),
            Pane::Welcome => "Conversation".into(),
            Pane::Thread(key) => {
                let title = find_thread(self.data, key)
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| key.id.chars().take(12).collect());
                let text = RichText::new(title.chars().take(36).collect::<String>());
                if self.data.saved.workspace.preview.as_ref() == Some(key) {
                    text.italics().into()
                } else {
                    text.into()
                }
            }
        }
    }
    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        match tab {
            Pane::Thread(k) => egui::Id::new((&k.peer, &k.id)),
            Pane::Threads => egui::Id::new("threads"),
            Pane::Welcome => egui::Id::new("welcome"),
        }
    }
    fn scroll_bars(&self, _: &Pane) -> [bool; 2] {
        [false, false]
    }
    fn on_tab_button(&mut self, tab: &mut Pane, response: &egui::Response) {
        if let Pane::Thread(key) = tab {
            if response.clicked() {
                self.data.selected_thread = Some(key.clone())
            }
            if response.double_clicked() || response.drag_started() {
                self.actions.push(Action::Pin(key.clone()));
            }
            response
                .clone()
                .on_hover_text("Double-click to keep open. Drag to move, split, or float.");
        }
    }
    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Pane, _: egui_dock::NodePath) {
        if let Pane::Thread(key) = tab
            && ui.button("Keep open").clicked()
        {
            self.actions.push(Action::Pin(key.clone()));
            ui.close();
        }
        for (label, direction) in [
            ("Move to split right", egui_dock::Split::Right),
            ("Move to split left", egui_dock::Split::Left),
            ("Move to split below", egui_dock::Split::Below),
            ("Move to split above", egui_dock::Split::Above),
        ] {
            if ui.button(label).clicked() {
                self.actions.push(Action::Split(tab.clone(), direction));
                ui.close();
            }
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        match tab {
            Pane::Welcome => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.);
                    ui.heading("Boxen");
                    ui.label("A single click previews a thread. A double-click keeps it open.");
                    ui.label("Tabs can move to any edge, form groups, and float.");
                    ui.label("⌘/Ctrl + Shift + P opens the command palette.");
                });
            }
            Pane::Threads => {
                ui.horizontal(|ui| {
                    let search_width =
                        (ui.available_width() - 28. - ui.spacing().item_spacing.x).max(24.);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.data.filter)
                            .hint_text("Filter threads…")
                            .desired_width(search_width),
                    );
                    let response = ui
                        .add(egui::Button::new("").min_size(egui::vec2(28., 24.)))
                        .on_hover_text("Filter machines");
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            "Filter machines",
                        )
                    });
                    let center = response.rect.center();
                    let active = self
                        .data
                        .saved
                        .destinations
                        .iter()
                        .any(|d| self.data.saved.hidden_machines.contains(&d.key));
                    let color = if active {
                        ui.visuals().selection.stroke.color
                    } else {
                        ui.visuals().widgets.inactive.fg_stroke.color
                    };
                    ui.painter().add(egui::Shape::closed_line(
                        vec![
                            center + egui::vec2(-6., -5.),
                            center + egui::vec2(6., -5.),
                            center + egui::vec2(2., 0.),
                            center + egui::vec2(2., 5.),
                            center + egui::vec2(-2., 6.),
                            center + egui::vec2(-2., 0.),
                        ],
                        egui::Stroke::new(1.3, color),
                    ));
                    egui::Popup::menu(&response)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            let destinations = &self.data.saved.destinations;
                            let hidden = &mut self.data.saved.hidden_machines;
                            let count = destinations
                                .iter()
                                .filter(|d| !hidden.contains(&d.key))
                                .count();
                            let mut all = count == destinations.len();
                            if ui
                                .add(
                                    egui::Checkbox::new(&mut all, "All")
                                        .indeterminate(count > 0 && count < destinations.len()),
                                )
                                .changed()
                            {
                                for d in destinations {
                                    if all {
                                        hidden.remove(&d.key);
                                    } else {
                                        hidden.insert(d.key.clone());
                                    }
                                }
                            }
                            ui.separator();
                            for dest in destinations {
                                let mut checked = !hidden.contains(&dest.key);
                                if ui
                                    .push_id(&dest.key, |ui| ui.checkbox(&mut checked, &dest.label))
                                    .inner
                                    .changed()
                                {
                                    if checked {
                                        hidden.remove(&dest.key);
                                    } else {
                                        hidden.insert(dest.key.clone());
                                    }
                                }
                            }
                        });
                });
                ui.separator();
                let destinations: Vec<_> = self
                    .data
                    .saved
                    .destinations
                    .iter()
                    .filter(|d| !self.data.saved.hidden_machines.contains(&d.key))
                    .cloned()
                    .collect();
                let query = self.data.filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if destinations.is_empty() {
                            ui.weak(if self.data.saved.destinations.is_empty() {
                                "Connecting to machines…"
                            } else {
                                "No machines selected."
                            });
                        }
                        let grouped = destinations.len() > 1;
                        for dest in &destinations {
                            if grouped {
                                egui::CollapsingHeader::new(&dest.label)
                                    .id_salt(("thread-machine", &dest.key))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        thread_rows(ui, self.data, self.actions, dest, &query);
                                    });
                            } else {
                                thread_rows(ui, self.data, self.actions, dest, &query);
                            }
                        }
                    });
            }
            Pane::Thread(key) => {
                let Some(thread) = find_thread(self.data, key) else {
                    ui.label("Waiting for this machine’s thread catalog…");
                    if let Some((index, pending)) =
                        self.data.saved.pending.iter().enumerate().find(|(_, p)| {
                            p.destination.key == key.peer && p.spec["thread_id"] == key.id
                        })
                    {
                        ui.label(pending.spec["prompt"].as_str().unwrap_or(""));
                        ui.label("Submission retained; acceptance may be unknown.");
                        if ui
                            .add_enabled(
                                !self
                                    .data
                                    .sending
                                    .contains(pending.spec["id"].as_str().unwrap_or("")),
                                egui::Button::new("Retry same request"),
                            )
                            .clicked()
                        {
                            self.actions.push(Action::Retry(index));
                        }
                    }
                    return;
                };
                let runs = thread.runs.clone();
                let last = runs.last();
                let status = last.and_then(|r| r["status"].as_str()).unwrap_or("unknown");
                let busy = last.is_some_and(|r| !terminal(r));
                let dest = self
                    .data
                    .saved
                    .destinations
                    .iter()
                    .find(|d| d.key == key.peer);
                ui.horizontal_wrapped(|ui| {
                    ui.weak(dest.map(|d| d.label.as_str()).unwrap_or("Machine"));
                    ui.weak("/");
                    ui.weak(status);
                    if self.data.saved.workspace.preview.as_ref() == Some(key) {
                        ui.weak("· preview");
                    }
                    if busy && ui.small_button("Cancel run").clicked() {
                        self.actions.push(Action::Cancel(key.clone()));
                    }
                });
                if let Some(error) = self.data.errors.get(&key.peer) {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("Machine unreachable · {error}"),
                    );
                }
                for run in &runs {
                    let id = run["record"]["spec"]["id"].as_str().unwrap_or("");
                    if let Some(error) = self
                        .data
                        .watches
                        .get(&run_key(&key.peer, id))
                        .and_then(|w| w.error.as_ref())
                    {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            format!("Reconnecting observation · {error}"),
                        );
                    }
                }
                ui.separator();
                // The composer owns its draft per conversation, independent of tab position or preview replacement.
                let height = (ui.available_height() - 132.).max(40.);
                egui::ScrollArea::vertical()
                    .id_salt(("history", &key.peer, &key.id))
                    .auto_shrink([false, false])
                    .max_height(height)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_height(height);
                        for run in &runs {
                            let id = run["record"]["spec"]["id"].as_str().unwrap_or("");
                            if let Some(transcript) =
                                self.data.transcripts.get(&run_key(&key.peer, id))
                            {
                                for row in &transcript.rows {
                                    ui.push_id((id, &row.id), |ui| {
                                        if matches!(
                                            row.label.as_str(),
                                            "Command" | "Tool" | "Tool result" | "Tool output"
                                        ) {
                                            egui::CollapsingHeader::new(&row.label)
                                                .default_open(true)
                                                .show(ui, |ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&row.text).monospace(),
                                                        )
                                                        .wrap()
                                                        .selectable(true),
                                                    );
                                                });
                                        } else {
                                            ui.label(RichText::new(&row.label).small().color(
                                                if row.label == "You" {
                                                    egui::Color32::from_rgb(137, 188, 211)
                                                } else {
                                                    egui::Color32::GRAY
                                                },
                                            ));
                                            ui.add(
                                                egui::Label::new(&row.text).wrap().selectable(true),
                                            );
                                        }
                                        ui.add_space(8.);
                                    });
                                }
                            }
                        }
                    });
                ui.separator();
                let pending = self
                    .data
                    .saved
                    .pending
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.destination.key == key.peer && p.spec["thread_id"] == key.id)
                    .map(|(i, _)| i);
                if let Some(index) = pending {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Submission awaiting acceptance reconciliation.");
                        if ui.button("Retry same request").clicked() {
                            self.actions.push(Action::Retry(index));
                        }
                    });
                }
                let draft_index = match self.data.saved.drafts.iter().position(|(k, _)| k == key) {
                    Some(i) => i,
                    None => {
                        self.data.saved.drafts.push((key.clone(), String::new()));
                        self.data.saved.drafts.len() - 1
                    }
                };
                let draft = &mut self.data.saved.drafts[draft_index].1;
                let edit = ui.add(
                    egui::TextEdit::multiline(draft)
                        .id_salt(("draft", &key.peer, &key.id))
                        .hint_text(if busy {
                            "Message the running thread…"
                        } else {
                            "Message…"
                        })
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                if edit.changed() && !draft.is_empty() {
                    self.actions.push(Action::Pin(key.clone()));
                }
                let shortcut = edit.has_focus()
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
                let blocked = pending.is_some()
                    || draft.trim().is_empty()
                    || last.is_some_and(|r| {
                        self.data
                            .sending
                            .contains(r["record"]["spec"]["id"].as_str().unwrap_or(""))
                    });
                ui.horizontal(|ui| {
                    if (ui
                        .add_enabled(
                            !blocked,
                            egui::Button::new(if busy { "Send steering" } else { "Send" }),
                        )
                        .clicked()
                        || shortcut)
                        && !blocked
                    {
                        self.actions.push(Action::Send(key.clone()));
                    }
                    ui.weak("⌘/Ctrl + Enter");
                });
            }
        }
    }
}
impl eframe::App for Desktop {
    fn logic(&mut self, _: &egui::Context, _: &mut eframe::Frame) {
        self.updates();
        self.poll();
        self.observe();
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::P,
            ) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::P)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::F1)
        }) {
            self.palette = !self.palette;
            self.palette_fresh = true;
            self.query.clear();
            self.choice = 0;
        }
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Boxen").strong());
                if !self.ready && ui.button("Reconnect daemon").clicked() {
                    self.connect();
                }
                if ui.button("New thread").clicked() {
                    self.new_thread = true;
                }
                if ui.button("Commands…").clicked() {
                    self.palette = true;
                    self.palette_fresh = true;
                    self.query.clear();
                }
            });
        });
        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.data.notice.is_empty() {
                    ui.weak("Daemon owns execution · closing a pane detaches its viewer");
                } else {
                    ui.label(&self.data.notice);
                    if ui.small_button("Dismiss").clicked() {
                        self.data.notice.clear();
                    }
                }
            });
        });
        let mut actions = vec![];
        // Dock state is temporarily moved out so viewer data and layout never alias.
        let mut dock = std::mem::replace(
            &mut self.data.saved.workspace.dock,
            egui_dock::DockState::new(vec![]),
        );
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let mut style = egui_dock::Style::from_egui(ui.style().as_ref());
                style.overlay.overlay_type = egui_dock::OverlayType::HighlightedAreas;
                DockArea::new(&mut dock)
                    .id(egui::Id::new("workspace"))
                    .style(style)
                    .show_inside(
                        ui,
                        &mut Viewer {
                            data: &mut self.data,
                            actions: &mut actions,
                        },
                    );
            });
        self.data.saved.workspace.dock = dock;
        for action in actions {
            self.action(action)
        }
        if self.connection {
            let mut open = true;
            let mut changed = false;
            egui::Window::new("Machine connections").open(&mut open).show(&ctx,|ui|{
                ui.label("The local daemon authenticates already-paired machines. Addresses locate those pinned identities.");
                for dest in &mut self.data.saved.destinations {if dest.label!="This machine"{ui.label(&dest.label);ui.small(&dest.key);changed|=ui.text_edit_singleline(&mut dest.address).lost_focus();}}
            });
            self.connection = open;
            if changed {
                for (_, stop) in self.polls.drain() {
                    stop.store(true, Ordering::Relaxed);
                }
                self.persist();
            }
        }
        if self.new_thread {
            let mut open = true;
            let mut submit = false;
            egui::Window::new("New thread")
                .open(&mut open)
                .collapsible(false)
                .default_width(520.)
                .show(&ctx, |ui| {
                    ui.label("Execution machine");
                    egui::ComboBox::from_id_salt("new-machine")
                        .selected_text(
                            self.data
                                .saved
                                .destinations
                                .get(self.data.saved.selected)
                                .map(|d| d.label.as_str())
                                .unwrap_or("None"),
                        )
                        .show_ui(ui, |ui| {
                            for (i, d) in self.data.saved.destinations.iter().enumerate() {
                                ui.selectable_value(&mut self.data.saved.selected, i, &d.label);
                            }
                        });
                    ui.label("Working directory on that machine");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cwd)
                            .hint_text("/absolute/path")
                            .desired_width(f32::INFINITY),
                    );
                    egui::ComboBox::from_label("Runtime")
                        .selected_text(&self.backend)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.backend, "codex".into(), "Codex");
                            ui.selectable_value(
                                &mut self.backend,
                                "openrouter".into(),
                                "OpenRouter",
                            );
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model)
                            .hint_text("Model (blank uses daemon default)")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut self.prompt)
                            .hint_text("Message")
                            .desired_width(f32::INFINITY)
                            .desired_rows(4),
                    );
                    ui.small("Execution uses the daemon’s current Full access policy.");
                    submit = ui
                        .add_enabled(
                            !self.cwd.is_empty()
                                && !self.prompt.trim().is_empty()
                                && self.client.is_some(),
                            egui::Button::new("Start thread"),
                        )
                        .clicked();
                });
            self.new_thread = open;
            if submit
                && let Some(dest) = self
                    .data
                    .saved
                    .destinations
                    .get(self.data.saved.selected)
                    .cloned()
            {
                let id = new_id();
                let key = ThreadKey {
                    peer: dest.key.clone(),
                    id: id.clone(),
                };
                self.submit(Pending{destination:dest,spec:json!({"id":id,"thread_id":key.id,"backend":self.backend,"cwd":self.cwd,"model":self.model,"prompt":self.prompt})});
                self.data.saved.workspace.open(key.clone(), true);
                self.data.selected_thread = Some(key);
                self.new_thread = false;
                self.prompt.clear();
            }
        }
        self.command_palette(&ctx);
        // Save edited drafts/layout after user interaction, without a permanent repaint loop.
        if self.last_save.elapsed() > Duration::from_secs(1)
            && ctx.input(|i| i.pointer.any_released() || !i.events.is_empty())
        {
            self.persist();
        }
    }
    fn save(&mut self, _: &mut dyn eframe::Storage) {
        self.persist();
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        for stop in self.polls.values() {
            stop.store(true, Ordering::Relaxed)
        }
        for w in self.data.watches.values() {
            w.stop.store(true, Ordering::Relaxed)
        }
    }
}
fn main() -> eframe::Result {
    let mut home = std::env::var_os("BOXEN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .unwrap_or_default(),
            )
            .join(".boxen")
        });
    let mut origin = "http://127.0.0.1:42425".to_owned();
    let mut args = std::env::args().skip(1);
    let mut peer = None;
    let mut address = String::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => home = PathBuf::from(args.next().expect("--data-dir requires a path")),
            "--url" => origin = args.next().expect("--url requires an origin"),
            "--peer" => peer = args.next(),
            "--address" => address = args.next().expect("--address requires host:port"),
            "--help" => {
                println!(
                    "boxen-desktop [--data-dir PATH] [--url http://127.0.0.1:PORT] [--peer PUBLIC_KEY --address HOST:PORT]"
                );
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {arg}");
                return Ok(());
            }
        }
    }
    let extra = peer
        .map(|key| Destination {
            key,
            label: String::new(),
            address,
        })
        .or_else(|| {
            let v: Value =
                serde_json::from_slice(&std::fs::read(home.join("lan-peer.json")).ok()?).ok()?;
            Some(Destination {
                key: v["remote_key"].as_str()?.into(),
                label: String::new(),
                address: v["remote_address"].as_str()?.into(),
            })
        });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180., 800.])
            .with_min_inner_size([760., 480.])
            .with_app_id("dev.boxen.desktop"),
        ..Default::default()
    };
    eframe::run_native(
        "Boxen",
        options,
        Box::new(move |cc| Ok(Box::new(Desktop::new(cc, home, origin, extra)))),
    )
}
