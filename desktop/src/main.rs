mod activity;
mod agents;
mod attachments;
mod client;
mod conversation;
mod correspondence;
mod field;
mod menu;
mod model;
mod thread_view;
mod work;
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
    new_thread_text: String,
    new_thread_files: Vec<attachments::FileRef>,
    thread_files: Vec<(ThreadKey, Vec<attachments::FileRef>)>,
    activity_views: HashSet<ThreadKey>,
    draft_runs: Vec<(ThreadKey, String)>,
    steering: Vec<thread_view::Steering>,
    pending: Vec<Pending>,
    mail: correspondence::SavedMail,
    correspondence_added: bool,
    agents_added: bool,
    work_added: bool,
    work: work::Saved,
}
struct Observing {
    epoch: u64,
    stop: Arc<AtomicBool>,
    retry: Instant,
    error: Option<String>,
    done: bool,
}
#[derive(Default)]
struct Data {
    work: work::State,
    views: HashMap<ThreadKey, conversation::View>,
    preparing: HashMap<attachments::Target, attachments::Preparation>,
    file_previews: HashMap<String, Option<Result<attachments::Preview, String>>>,
    thread_issues: HashMap<ThreadKey, String>,
    active_steering: HashSet<ThreadKey>,
    preparing_followups: HashMap<String, ThreadKey>,
    local_key: String,
    mail: correspondence::Mailbox,
    agents: agents::State,
    activity: HashMap<ThreadKey, activity::Window>,
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
    Work(work::Action),
    File(attachments::FileAction),
    Respond(ThreadKey, String, Value, Value),
    OpenActivity(ThreadKey, bool),
    OpenChat(ThreadKey, bool),
    Mail(correspondence::MailAction),
    Open(ThreadKey, bool),
    Pin(ThreadKey),
    Send(ThreadKey),
    Cancel(ThreadKey),
    Retry(usize),
    Split(Pane, egui_dock::Split),
}
struct Desktop {
    data: Data,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    native_menu: Option<menu::NativeMenu>,
    client: Option<Client>,
    bus: Bus,
    home: PathBuf,
    state_path: PathBuf,
    origin: String,
    polls: HashMap<String, Arc<AtomicBool>>,
    field: Option<Box<field::Field>>,
    palette: bool,
    palette_fresh: bool,
    query: String,
    choice: usize,
    connection: bool,
    new_thread: bool,
    cwd: String,
    backend: String,
    model: String,
    last_save: Instant,
    next_watch: u64,
    next_discovery: Instant,
    delivery_cursor: usize,
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
        let mut saved: Saved = std::fs::read(&state_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !saved.correspondence_added {
            saved.workspace.show_correspondence();
            saved.correspondence_added = true;
        }
        if !saved.agents_added {
            saved.workspace.show_agents();
            saved.agents_added = true;
        }
        if !saved.work_added {
            saved.workspace.show_work();
            saved.work_added = true;
        }
        saved
            .workspace
            .merge_resident_views(&mut saved.activity_views);
        let bus = Bus::new(cc.egui_ctx.clone());
        let mut app = Self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            native_menu: match menu::NativeMenu::install(cc) {
                Ok(menu) => Some(menu),
                Err(err) => {
                    eprintln!("Could not install native menus: {err}");
                    None
                }
            },
            data: Data {
                work: work::State::new(&home, &origin),
                saved,
                ..Default::default()
            },
            client: None,
            bus,
            home,
            state_path,
            origin: origin.clone(),
            polls: HashMap::new(),
            field: None,
            palette: false,
            palette_fresh: false,
            query: String::new(),
            choice: 0,
            connection: false,
            new_thread: false,
            cwd: String::new(),
            backend: "codex".into(),
            model: String::new(),
            last_save: Instant::now(),
            next_watch: 0,
            next_discovery: Instant::now() + Duration::from_secs(10),
            delivery_cursor: 0,
            ready: false,
            extra,
        };
        app.connect();
        app
    }
    fn menu_command(&mut self, command: menu::Command) {
        match command {
            menu::Command::NewThread => self.new_thread = true,
            menu::Command::Reconnect if !self.ready => self.connect(),
            menu::Command::Reconnect => {}
            menu::Command::ShowWork => self.data.saved.workspace.show_work(),
            menu::Command::Palette => self.open_palette(),
        }
    }
    fn open_palette(&mut self) {
        self.palette = true;
        self.palette_fresh = true;
        self.query.clear();
        self.choice = 0;
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
                address: p["address"]
                    .as_str()
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .or_else(|| existing.map(|d| d.address.clone()))
                    .unwrap_or_default(),
            });
        }
        if let Some(extra) = self.extra.take()
            && let Some(d) = discovered.iter_mut().find(|d| d.key == extra.key)
        {
            d.address = extra.address
        }
        let changed: Vec<_> = self
            .data
            .saved
            .destinations
            .iter()
            .filter(|old| {
                discovered
                    .iter()
                    .find(|d| d.key == old.key)
                    .is_none_or(|new| new.address != old.address)
            })
            .map(|d| d.key.clone())
            .collect();
        for key in changed {
            if let Some(stop) = self.polls.remove(&key) {
                stop.store(true, Ordering::Relaxed);
            }
            self.data.watches.retain(|tag, w| {
                if tag.starts_with(&format!("{key}:")) {
                    w.stop.store(true, Ordering::Relaxed);
                    false
                } else {
                    true
                }
            });
        }
        self.data.saved.destinations = discovered;
        self.data.saved.selected = self
            .data
            .saved
            .selected
            .min(self.data.saved.destinations.len().saturating_sub(1));
        if !self.ready {
            self.data.notice.clear();
        }
        self.ready = true;
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
        if Instant::now() >= self.next_discovery {
            self.next_discovery = Instant::now() + Duration::from_secs(10);
            if let Some(client) = &self.client {
                client.discover(self.bus.publisher());
                let queued: Vec<_> = self
                    .data
                    .saved
                    .mail
                    .drafts
                    .iter()
                    .filter_map(|d| {
                        d.last_receipt
                            .as_ref()
                            .filter(|v| {
                                v["delivery"]["status"] == "queued"
                                    && v["network"]["source_machine"] == self.data.local_key
                            })
                            .and_then(|v| v["id"].as_str())
                            .map(|id| (d.id.clone(), id.to_owned()))
                    })
                    .collect();
                if !queued.is_empty() {
                    let checks = queued
                        .iter()
                        .cycle()
                        .skip(self.delivery_cursor % queued.len())
                        .take(8.min(queued.len()))
                        .cloned()
                        .collect();
                    self.delivery_cursor = self.delivery_cursor.wrapping_add(8);
                    client.mail_receipts(
                        Destination {
                            key: self.data.local_key.clone(),
                            label: "This machine".into(),
                            address: String::new(),
                        },
                        checks,
                        self.bus.publisher(),
                    );
                }
            }
        }
        if !self.ready {
            return;
        }
        let Some(client) = self.client.as_ref() else {
            return;
        };
        *self.data.mail.watching.lock().unwrap() = self
            .data
            .saved
            .workspace
            .dock
            .iter_all_tabs()
            .filter_map(|(_, pane)| {
                if let Pane::Chat(key) = pane {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();
        for dest in &self.data.saved.destinations {
            if dest.address.is_empty() && dest.key != self.data.local_key {
                continue;
            }
            if !self.polls.contains_key(&dest.key) {
                let stop = Arc::new(AtomicBool::new(false));
                client.catalog(dest.clone(), self.bus.publisher(), stop.clone());
                client.mail_poll(
                    dest.clone(),
                    self.data.local_key.clone(),
                    self.bus.publisher(),
                    stop.clone(),
                    self.data.mail.watching.clone(),
                );
                self.polls.insert(dest.key.clone(), stop);
            }
        }
    }
    fn updates(&mut self) {
        while let Ok(update) = self.bus.rx.try_recv() {
            match update {
                Update::Mail(update) => self.mail_update(update),
                Update::Discovered(result) => match result {
                    Ok(status) => self.discovered(status),
                    Err(error) => self.data.notice = format!("Daemon unavailable: {error}"),
                },
                Update::Catalog(dest, result) => {
                    self.data.agents.activity(&dest.key, &result);
                    match result {
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
                    }
                }
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
                    self.reconcile_steering(&id);
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
                Update::Followup(previous, result) => {
                    let key = self.data.preparing_followups.remove(&previous);
                    match result {
                        Ok(pending) => self.submit(pending),
                        Err(error) => {
                            if let Some(key) = key {
                                self.data
                                    .thread_issues
                                    .insert(key, format!("Could not prepare follow-up: {error}"));
                            }
                        }
                    }
                }
                Update::Files(update) => self.file_update(update),
                Update::Steered(key, result, sent) => self.steered(key, result, sent),
                Update::Controlled(id, _dest, _text, result) => {
                    self.data.sending.remove(&id);
                    if let Err(error) = result {
                        self.data.notice = format!("Control outcome uncertain: {error}");
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
                                self.clear_submitted_draft(
                                    &key,
                                    p.spec["prompt"].as_str().unwrap_or(""),
                                    &attachments::FileRef::parse(&p.spec["attachments"]),
                                );
                                self.data.thread_issues.remove(&key);
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
                            }
                        }
                        Err(error) => {
                            if let Some(p) =
                                self.data.saved.pending.iter().find(|p| p.spec["id"] == id)
                            {
                                let key = ThreadKey {
                                    peer: p.destination.key.clone(),
                                    id: p.spec["thread_id"].as_str().unwrap_or("").into(),
                                };
                                self.data.thread_issues.insert(key, error);
                            }
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
        self.clear_submitted_draft(
            &key,
            p.spec["prompt"].as_str().unwrap_or(""),
            &attachments::FileRef::parse(&p.spec["attachments"]),
        );
        self.data.thread_issues.remove(&key);
        self.data
            .saved
            .pending
            .retain(|v| v.destination.key != p.destination.key || v.spec["id"] != p.spec["id"]);
    }
    fn clear_submitted_draft(
        &mut self,
        key: &ThreadKey,
        text: &str,
        files: &[attachments::FileRef],
    ) {
        let draft = self
            .data
            .saved
            .drafts
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, d)| d.as_str());
        let attached = self
            .data
            .saved
            .thread_files
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&[][..], |(_, f)| f.as_slice());
        if draft == text && attached == files {
            self.data.saved.drafts.retain(|(k, _)| k != key);
            self.data.saved.thread_files.retain(|(k, _)| k != key);
            self.data.saved.draft_runs.retain(|(k, _)| k != key);
        }
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
        let mut selected = Vec::new();
        for (_, pane) in self.data.saved.workspace.dock.iter_all_tabs() {
            match pane {
                Pane::Thread(key) => {
                    if let Some(thread) = find_thread(&self.data, key) {
                        for run in &thread.runs {
                            selected.push((key.clone(), run.clone()));
                        }
                    }
                }
                Pane::Chat(key) | Pane::Activity(key)
                    if self.data.saved.activity_views.contains(key) =>
                {
                    for run in activity::runs(&self.data, key) {
                        selected.push((key.clone(), run));
                    }
                }
                _ => {}
            }
        }
        // Live work gets an observer before older replay, across every open surface.
        selected.sort_by_key(|(_, r)| {
            (
                terminal(r),
                std::cmp::Reverse(r["record"]["created_at"].as_u64().unwrap_or(0)),
            )
        });
        let mut needed = HashSet::new();
        for (key, run) in selected {
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
            let id = run["record"]["spec"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let tag = run_key(&key.peer, &id);
            if !needed.insert(tag.clone()) {
                continue;
            }
            let seq = self.data.transcripts.get(&tag).map_or(0, |t| t.seq);
            let last = run["last_seq"].as_u64().unwrap_or(0);
            if terminal(&run) && seq >= last {
                continue;
            }
            let retry = match self.data.watches.get(&tag) {
                None => true,
                Some(w) => {
                    (w.done && seq < last) || (w.error.is_some() && Instant::now() >= w.retry)
                }
            };
            if retry {
                if self
                    .data
                    .watches
                    .values()
                    .filter(|w| !w.done && w.error.is_none())
                    .count()
                    >= 16
                {
                    continue;
                }
                let stop = Arc::new(AtomicBool::new(false));
                self.next_watch += 1;
                client.watch(
                    dest,
                    id,
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
        self.data.watches.retain(|id, w| {
            if !needed.contains(id) {
                w.stop.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
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
            Action::Work(action) => match action {
                work::Action::Submit => {
                    if self.data.work.prepare(&mut self.data.saved.work) && self.persist() {
                        self.data.work.send(&self.data.saved.work, &self.bus.ctx);
                    }
                }
                work::Action::Retry => {
                    if self.persist() {
                        self.data.work.send(&self.data.saved.work, &self.bus.ctx);
                    }
                }
                work::Action::Discard => {
                    self.data.saved.work.pending = None;
                    self.persist();
                }
            },
            Action::File(action) => self.file_action(action),
            Action::Respond(key, id, request_id, result) => {
                if let Some(client) = &self.client {
                    client.control(
                        conversation::destination(&self.data, &key.peer),
                        id,
                        json!({"type":"respond","request_id":request_id,"result":result}),
                        self.bus.publisher(),
                    );
                }
            }
            Action::OpenChat(key, pin) => {
                self.data.saved.workspace.open_editor(Pane::Chat(key), pin)
            }
            Action::OpenActivity(key, pin) => {
                conversation::switch(&mut self.data, &key, true, None);
                self.data.saved.workspace.open_editor(Pane::Chat(key), pin);
            }
            Action::Mail(a) => self.mail_action(a),
            Action::Split(pane, direction) => {
                if let Some(path) = self.data.saved.workspace.dock.find_tab(&pane) {
                    self.data.saved.workspace.pin_pane(&pane);
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
            ("Show mail".into(), None),
            ("New message…".into(), None),
            ("Show agents".into(), None),
            ("Show work".into(), None),
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
        let mut entered = false;
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
                    if field::matches(&self.query) {
                        entered = true;
                    } else {
                        picked = visible.get(self.choice).map(|(i, _)| *i);
                    }
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
        if entered {
            self.palette = false;
            self.query.clear();
            self.field = Some(Box::default());
        }
        if let Some(index) = picked {
            self.palette = false;
            if let Some(key) = commands[index].1.clone() {
                self.action(Action::Open(key, true))
            } else {
                match index {
                    4 => self.data.saved.workspace.show_correspondence(),
                    5 => self.mail_action(correspondence::MailAction::New),
                    6 => self.data.saved.workspace.show_agents(),
                    7 => self.data.saved.workspace.show_work(),
                    0 => self.new_thread = true,
                    1 => self.data.saved.workspace.show_list(),
                    2 => {
                        let panes: Vec<_> = self
                            .data
                            .saved
                            .workspace
                            .dock
                            .iter_all_tabs()
                            .filter(|(_, p)| {
                                matches!(
                                    p,
                                    Pane::Thread(_)
                                        | Pane::Chat(_)
                                        | Pane::Activity(_)
                                        | Pane::Message(_)
                                        | Pane::Compose(_)
                                )
                            })
                            .map(|(_, p)| (p.clone(), self.data.saved.workspace.is_preview(p)))
                            .collect();
                        self.data.saved.workspace = Workspace::default();
                        for (pane, preview) in panes {
                            self.data.saved.workspace.open_editor(pane, !preview);
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
            Pane::Chat(key) => {
                let text = RichText::new(activity::title(self.data, key));
                if self.data.saved.workspace.is_preview(tab) {
                    text.italics().into()
                } else {
                    text.into()
                }
            }
            Pane::Activity(key) => {
                let name = RichText::new(format!("{} · Activity", activity::title(self.data, key)));
                if self.data.saved.workspace.is_preview(tab) {
                    name.italics().into()
                } else {
                    name.into()
                }
            }
            Pane::Agents => "Agents".into(),
            Pane::Work => "Work".into(),
            Pane::Correspondence => "Mail".into(),
            Pane::Compose(_) => "New message".into(),
            Pane::Message(k) => {
                let text = RichText::new(correspondence::message_title(self.data, k));
                if self.data.saved.workspace.is_preview(tab) {
                    text.italics().into()
                } else {
                    text.into()
                }
            }
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
            Pane::Chat(k) => egui::Id::new(("chat", &k.peer, &k.id)),
            Pane::Activity(k) => egui::Id::new(("activity", &k.peer, &k.id)),
            Pane::Thread(k) => egui::Id::new((&k.peer, &k.id)),
            Pane::Message(k) => egui::Id::new(("mail", &k.peer, &k.id)),
            Pane::Compose(id) => egui::Id::new(("compose", id)),
            Pane::Agents => egui::Id::new("agents"),
            Pane::Work => egui::Id::new("work"),
            Pane::Correspondence => egui::Id::new("correspondence"),
            Pane::Threads => egui::Id::new("threads"),
            Pane::Welcome => egui::Id::new("welcome"),
        }
    }
    fn scroll_bars(&self, _: &Pane) -> [bool; 2] {
        [false, false]
    }
    fn on_tab_button(&mut self, tab: &mut Pane, response: &egui::Response) {
        if matches!(
            tab,
            Pane::Chat(_) | Pane::Activity(_) | Pane::Message(_) | Pane::Compose(_)
        ) && (response.double_clicked() || response.drag_started())
        {
            self.actions
                .push(Action::Mail(correspondence::MailAction::Pin(tab.clone())));
        }
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
        if matches!(
            tab,
            Pane::Thread(_) | Pane::Chat(_) | Pane::Activity(_) | Pane::Message(_)
        ) && ui.button("Keep open").clicked()
        {
            self.actions
                .push(Action::Mail(correspondence::MailAction::Pin(tab.clone())));
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
            Pane::Agents => agents::panel(ui, self.data, self.actions),
            Pane::Work => {
                if let Some(action) = self.data.work.panel(ui, &mut self.data.saved.work) {
                    self.actions.push(Action::Work(action));
                }
            }
            Pane::Chat(key) => correspondence::chat(ui, self.data, key, self.actions),
            Pane::Activity(key) => correspondence::chat(ui, self.data, key, self.actions),
            Pane::Correspondence => correspondence::inbox(ui, self.data, self.actions),
            Pane::Message(_) | Pane::Compose(_) => {
                egui::ScrollArea::vertical()
                    .id_salt(("mail-details", format!("{tab:?}")))
                    .show(ui, |ui| {
                        correspondence::details(ui, tab, self.data, self.actions)
                    });
            }
            Pane::Welcome => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.);
                    ui.heading("Boxen");
                    ui.label("A single click previews a thread or message. A double-click keeps it open.");
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
            Pane::Thread(key) => thread_view::panel(ui, self.data, key, self.actions),
        }
    }
}
impl eframe::App for Desktop {
    fn logic(&mut self, _: &egui::Context, _: &mut eframe::Frame) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        while let Some(command) = self
            .native_menu
            .as_ref()
            .and_then(menu::NativeMenu::next_command)
        {
            self.menu_command(command);
        }
        let visible = self
            .data
            .saved
            .workspace
            .dock
            .find_tab(&Pane::Work)
            .is_some();
        if self
            .data
            .work
            .pump(&mut self.data.saved.work, visible, &self.bus.ctx)
        {
            self.persist();
        }
        self.updates();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(menu) = &self.native_menu {
            menu.set_ready(self.ready);
        }
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
            if self.palette {
                self.palette = false;
            } else {
                self.open_palette();
            }
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let show_in_window_menu = self.native_menu.is_none();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let show_in_window_menu = true;
        if show_in_window_menu && let Some(command) = menu::in_window(ui, self.ready) {
            self.menu_command(command);
        }
        // State supplies ordinary success. This strip exists only when there
        // is a notice the operator can act on or dismiss.
        if !self.data.notice.is_empty() {
            egui::Panel::bottom("statusbar").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&self.data.notice);
                    if ui.small_button("Dismiss").clicked() {
                        self.data.notice.clear();
                    }
                });
            });
        }
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
            let mut prompt = std::mem::take(&mut self.data.saved.new_thread_text);
            let mut files = std::mem::take(&mut self.data.saved.new_thread_files);
            let mut file_actions = Vec::new();
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
                    ui.small("Execution uses the daemon’s current Full access policy.");
                    submit = conversation::composer(
                        ui,
                        &mut self.data,
                        conversation::Composer {
                            text: &mut prompt,
                            files: &mut files,
                            target: attachments::Target::NewThread,
                            label: "Start thread",
                            limit: 256 * 1024,
                            blocked: self.cwd.is_empty() || self.client.is_none(),
                        },
                        &mut file_actions,
                    )
                    .send;
                });
            self.new_thread = open;
            self.data.saved.new_thread_text = prompt;
            self.data.saved.new_thread_files = files;
            for action in file_actions {
                self.action(action);
            }
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
                let prompt = std::mem::take(&mut self.data.saved.new_thread_text);
                let attachments = std::mem::take(&mut self.data.saved.new_thread_files);
                self.submit(Pending{destination:dest,spec:json!({"id":id,"thread_id":key.id,"backend":self.backend,"cwd":self.cwd,"model":self.model,"prompt":prompt,"attachments":attachments})});
                self.data.saved.workspace.open(key.clone(), true);
                self.data.selected_thread = Some(key);
                self.new_thread = false;
            }
        }
        self.command_palette(&ctx);
        if let Some(field) = &mut self.field
            && !field.show(&ctx)
        {
            self.field = None;
        }
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
