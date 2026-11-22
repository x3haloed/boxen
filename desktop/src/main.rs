mod activity;
mod agents;
mod attachments;
mod client;
mod console_style;
mod conversation;
mod correspondence;
mod field;
mod grid;
mod hold;
mod menu;
mod model;
mod model_catalogs;
mod needs;
mod new_conversation;
#[cfg(test)]
mod performance;
mod pulse;
mod runtimes;
mod thread_view;
mod tool_approvals;
mod watchlist;
mod work;
mod workspace;
mod workspace_observation;
use client::{Bus, Client, Pending, Update, run_key};
use eframe::egui::{self, RichText};
use egui_dock::TabViewer;
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
    hidden_machines: HashSet<String>,
    drafts: Vec<(ThreadKey, String)>,
    new_thread_text: String,
    new_thread_settings: new_conversation::Settings,
    new_thread_files: Vec<attachments::FileRef>,
    new_thread_request: Option<String>,
    thread_files: Vec<(ThreadKey, Vec<attachments::FileRef>)>,
    thread_settings: Vec<(ThreadKey, thread_view::RunSettings)>,
    moves: Vec<thread_view::PendingMove>,
    activity_views: HashSet<ThreadKey>,
    steering: Vec<thread_view::Steering>,
    pending: Vec<Pending>,
    refused_followups: HashMap<String, String>,
    answers: Vec<AnswerAttempt>,
    reply_watches: HashMap<String, HashSet<String>>,
    thread_replies: Vec<needs::ThreadReply>,
    holds: Vec<hold::Contribution>,
    hold_edits: Vec<hold::Contribution>,
    hold_drafts: Vec<hold::Draft>,
    hold_releases: Vec<hold::Release>,
    mail: correspondence::SavedMail,
    correspondence_added: bool,
    agents_added: bool,
    work_added: bool,
    work: work::Saved,
}

impl Saved {
    fn remember_refused_followup(&mut self, pending: &Pending) {
        if let (Some(update), Some(run)) =
            (&pending.refused_update, pending.spec["id"].as_str())
        {
            self.refused_followups
                .insert(run_key(&pending.destination.key, run), update.clone());
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct AnswerAttempt {
    id: String,
    run_tag: String,
    request: Value,
}
struct Observing {
    key: ThreadKey,
    epoch: u64,
    stop: Arc<AtomicBool>,
    retry: Instant,
    error: Option<String>,
    done: bool,
    frontier: Option<u64>,
}
#[derive(Default)]
struct Data {
    model_catalogs: model_catalogs::Catalogs,
    runtimes: runtimes::Catalogs,
    tool_approvals: tool_approvals::State,
    work: work::State,
    views: HashMap<ThreadKey, conversation::View>,
    preparing: HashMap<attachments::Target, attachments::Preparation>,
    file_previews: HashMap<String, Option<Result<attachments::Preview, String>>>,
    thread_issues: HashMap<ThreadKey, String>,
    active_steering: HashSet<ThreadKey>,
    answer_rechecks: HashSet<String>,
    fresh_steering: HashSet<String>,
    fallback_inflight: HashSet<String>,
    send_checking: HashSet<ThreadKey>,
    hold_pending: HashSet<ThreadKey>,
    moving: HashSet<ThreadKey>,
    preparing_followups: HashMap<String, thread_view::SendIntent>,
    local_key: String,
    new_thread_dialog: bool,
    show_new_thread_settings: bool,
    focus_new_conversation: bool,
    new_conversation_issue: Option<String>,
    editing_thread_settings: Option<(ThreadKey, thread_view::RunSettings)>,
    mail: correspondence::Mailbox,
    agents: agents::State,
    activity: HashMap<ThreadKey, activity::Window>,
    saved: Saved,
    catalogs: HashMap<String, Vec<Conversation>>,
    errors: HashMap<String, String>,
    transcripts: HashMap<String, Transcript>,
    watches: HashMap<String, Observing>,
    pulse: pulse::Pulse,
    selected_thread: Option<ThreadKey>,
    repo_observations: HashMap<ThreadKey, watchlist::RepoObservation>,
    filter: String,
    notice: String,
    sending: HashSet<String>,
}
enum Action {
    LoadModelCatalog(model_catalogs::Runtime, Destination),
    LoadRuntimes(Destination),
    NewConversation,
    StartConversation,
    SaveDesktop,
    MoveConversation(ThreadKey, thread_view::RunSettings),
    RetryMove(ThreadKey),
    Work(work::Action),
    File(attachments::FileAction),
    Respond(ThreadKey, String, Value, Value),
    OpenGate(ThreadKey, bool, String),
    OpenReply(ThreadKey, String),
    ProbeRepo(ThreadKey, String),
    OpenActivity(ThreadKey, bool),
    OpenChat(ThreadKey, bool),
    Mail(correspondence::MailAction),
    Open(ThreadKey, bool),
    Pin(ThreadKey),
    Send(ThreadKey),
    RestoreSteer(thread_view::Steering),
    HoldChange(hold::Contribution, u64, hold::Mutation),
    HoldDraft(hold::Contribution, u64, bool),
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
        console_style::install(&cc.egui_ctx);
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
            saved.work_added = true;
        }
        saved
            .workspace
            .merge_resident_views(&mut saved.activity_views);
        while let Some(path) = saved.workspace.dock.find_tab(&Pane::LegacyNewConversation) {
            saved.workspace.dock.remove_tab(path);
        }
        saved.workspace.enable_grid();
        let new_thread_dialog = saved.new_thread_request.is_some();
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
                new_thread_dialog,
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
            last_save: Instant::now(),
            next_watch: 0,
            next_discovery: Instant::now() + Duration::from_secs(10),
            delivery_cursor: 0,
            ready: false,
            extra,
        };
        app.restore_interrupted_hold_edits();
        app.connect();
        app
    }
    fn menu_command(&mut self, command: menu::Command) {
        match command {
            menu::Command::NewThread => self.action(Action::NewConversation),
            menu::Command::Reconnect if !self.ready => self.connect(),
            menu::Command::Reconnect => {}
            menu::Command::ShowWork => self.data.saved.workspace.show_work(),
            menu::Command::Palette if !self.data.new_thread_dialog => self.open_palette(),
            menu::Command::Palette => {}
            menu::Command::ToolApprovals => self.data.tool_approvals.open(),
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
        self.data.runtimes.clear();
        self.data.model_catalogs.clear();
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
            .visible_editors()
            .into_iter()
            .filter_map(|pane| {
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
                Update::Runtimes(destination, serial, result) => {
                    self.data.runtimes.received(destination, serial, result)
                }
                Update::ModelCatalog(runtime, destination, serial, result) => self
                    .data
                    .model_catalogs
                    .received(runtime, destination, serial, result),
                Update::RepoObserved(key, id, result) => {
                    if let Some(observation) = self.data.repo_observations.get_mut(&key)
                        && observation.run == id
                    {
                        observation.result = Some(result);
                    }
                }
                Update::ToolApprovals(machine, result) => {
                    self.data.tool_approvals.received(machine, result)
                }
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
                            for p in &reconciled {
                                self.accept_pending(p);
                            }
                            self.data
                                .catalogs
                                .insert(dest.key.clone(), catalog(&dest.key, &runs));
                            self.reconcile_placements();
                            if !reconciled.is_empty() {
                                self.persist();
                            }
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
                    transcript.see_accepted_input(&batch["snapshot"]["record"]["spec"]);
                    if transcript.execution.is_none()
                        && let Some(selected) = batch["snapshot"]["record"]["execution"].as_object()
                    {
                        let mut selected = Value::Object(selected.clone());
                        selected["cwd"] = batch["snapshot"]["record"]["spec"]["cwd"].clone();
                        transcript.execution = Some(selected);
                    }
                    if let Some(batch) = batch["snapshot"]["record"]["mailbox"]["message_ids"].as_array() {
                        transcript.activation_mail_count = Some(batch.len());
                    }
                    let watch = &self.data.watches[&id];
                    let key = watch.key.clone();
                    let replaying = watch.frontier.is_none_or(|frontier| transcript.seq < frontier);
                    let mut failed = None;
                    let mut replayed = 0;
                    for event in batch["events"].as_array().into_iter().flatten() {
                        let previous = transcript.seq;
                        if let Err(e) = transcript.apply(event) {
                            failed = Some(e);
                            break;
                        }
                        if previous < transcript.seq
                            && let Some(event) = &transcript.latest_visible
                            && event.0 == transcript.seq
                        {
                            if replaying {
                                replayed += 1;
                            } else {
                                self.data.pulse.live(&key, event.1.clone());
                            }
                        }
                    }
                    if replaying && failed.is_none() {
                        self.data.pulse.replayed(
                            &id,
                            replayed,
                            batch["snapshot"]["last_seq"]
                                .as_u64()
                                .is_some_and(|seq| transcript.seq >= seq),
                        );
                    }
                    if batch["events"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|event| {
                            matches!(
                                event["kind"].as_str(),
                                Some(
                                    "response_attempt"
                                        | "response_write_started"
                                        | "response_refused"
                                        | "request_answered"
                                )
                            )
                        })
                    {
                        let saved = &self.data.saved;
                        transcript.decorate_answers(|answer| {
                            answer.yours = saved
                                .answers
                                .iter()
                                .any(|attempt| attempt.id == answer.attempt_id);
                            answer.machine_label = saved
                                .destinations
                                .iter()
                                .find(|destination| destination.key == answer.machine)
                                .map(|destination| destination.label.clone())
                                .unwrap_or_else(|| "machine unavailable".into());
                        });
                    }
                    let frontier = batch["snapshot"]["last_seq"].as_u64();
                    if failed.is_none() && frontier.is_some_and(|frontier| transcript.seq >= frontier)
                        && self.data.answer_rechecks.remove(&id)
                    {
                        transcript.confirm_pending_answers();
                    }
                    if let Some(w) = self.data.watches.get_mut(&id) {
                        if failed.is_some() {
                            self.data.pulse.gap(&id, &key);
                        }
                        w.error = failed;
                        w.done = batch["done"] == true && w.error.is_none();
                        w.frontier = frontier;
                        if w.error.is_some() {
                            w.stop.store(true, Ordering::Relaxed);
                            w.retry = Instant::now() + Duration::from_secs(2);
                        }
                    }
                    if let Some(s) = batch.get("snapshot")
                        && let Some((peer, _)) = id.split_once(':')
                    {
                        self.data.mail.note_presentation(peer, s);
                        self.replace_snapshot(peer, s.clone());
                    }
                    if batch["done"] == true
                        && self.data.watches.get(&id).is_some_and(|watch| watch.done)
                        && needs::settle_reply(&mut self.data, &key, &batch["snapshot"])
                    {
                        self.persist();
                    }
                    self.reconcile_steering(&id);
                    self.reconcile_hold(&id);
                }
                Update::WatchError(id, epoch, error) => {
                    if let Some(w) = self.data.watches.get_mut(&id)
                        && w.epoch == epoch
                    {
                        self.data.pulse.gap(&id, &w.key);
                        for steering in &self.data.saved.steering {
                            if run_key(&steering.key.peer, &steering.run) == id {
                                self.data.fresh_steering.remove(&steering.id);
                            }
                        }
                        w.error = Some(error);
                        w.retry = Instant::now() + Duration::from_secs(2);
                    }
                }
                Update::Followup(previous, result) => {
                    let intent = self.data.preparing_followups.remove(&previous);
                    match result {
                        Ok(mut pending) => {
                            pending.refused_update = intent.and_then(|intent| intent.refused_id);
                            if let Some(id) = &pending.refused_update {
                                self.retire_refused_steer(id);
                            }
                            self.submit(pending);
                        }
                        Err(error) => {
                            if let Some(intent) = intent {
                                self.restore_send_intent(&intent);
                                let key = intent.key;
                                let receipt = thread_view::draft_receipt(intent.refused_id.is_some());
                                self.data
                                    .thread_issues
                                    .insert(key, format!("{receipt} · {error}"));
                            }
                        }
                    }
                }
                Update::Files(update) => self.file_update(update),
                Update::Moved(key, result) => self.moved(key, result),
                Update::Held(reply) => self.hold_reply(reply),
                Update::SendChecked(intent, destination, result) => {
                    self.send_checked(intent, destination, result)
                }
                Update::Steered(key, update, result, sent) => {
                    self.steered(key, update, result, sent)
                }
                Update::AnswerSubmitted(id, result) => {
                    if let Err(error) = result
                        && let Some(attempt) = self
                            .data
                            .saved
                            .answers
                            .iter()
                            .find(|attempt| attempt.id == id)
                        && let Some(transcript) = self.data.transcripts.get_mut(&attempt.run_tag)
                    {
                        let state = if error.starts_with("HTTP 422:") {
                            model::AnswerState::Refused
                        } else {
                            model::AnswerState::Unconfirmed
                        };
                        if transcript.answer_locally(
                            &attempt.request,
                            model::AnswerReceipt {
                                attempt_id: id,
                                machine: String::new(),
                                decision: String::new(),
                                state,
                                yours: true,
                                machine_label: String::new(),
                                at: None,
                            },
                        ) {
                            self.data.answer_rechecks.insert(attempt.run_tag.clone());
                            if let Some(watch) = self.data.watches.remove(&attempt.run_tag) {
                                watch.stop.store(true, Ordering::Relaxed);
                            }
                        }
                        if state == model::AnswerState::Refused {
                            self.data.notice = error;
                        }
                    }
                }
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
                                            title: snapshot["record"]["spec"]["prompt"]
                                                .as_str()
                                                .and_then(|prompt| prompt.lines().next())
                                                .filter(|line| !line.trim().is_empty())
                                                .or_else(|| {
                                                    p.spec["attachments"][0]["name"].as_str()
                                                })
                                                .unwrap_or("Untitled thread")
                                                .chars()
                                                .take(72)
                                                .collect(),
                                            runs: vec![snapshot],
                                        },
                                    );
                                }
                                self.accept_pending(&p);
                                self.persist();
                            }
                        }
                        Err(error) if self.is_new_thread_request(&id) => {
                            self.new_thread_failed(&id, error);
                        }
                        Err(error) => {
                            let safe_to_edit = error.safe_to_edit;
                            let error = error.detail;
                            if let Some(p) = self
                                .data
                                .saved
                                .pending
                                .iter()
                                .find(|p| p.spec["id"] == id)
                                .cloned()
                            {
                                let key = ThreadKey {
                                    peer: p.destination.key.clone(),
                                    id: p.spec["thread_id"].as_str().unwrap_or("").into(),
                                };
                                if p.after.is_some() && safe_to_edit {
                                    self.restore_thread_draft(
                                        &key,
                                        p.spec["prompt"].as_str().unwrap_or(""),
                                        &attachments::FileRef::parse(&p.spec["attachments"]),
                                    );
                                    self.data
                                        .saved
                                        .pending
                                        .retain(|request| request.spec["id"] != id);
                                    self.persist();
                                }
                                let receipt = if safe_to_edit {
                                    thread_view::draft_receipt(p.refused_update.is_some())
                                } else {
                                    "? not confirmed · draft kept"
                                };
                                self.data
                                    .thread_issues
                                    .insert(key, format!("{receipt} · {error}"));
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
        if let Some(run) = p.spec["id"].as_str() {
            self.data.saved.reply_watches.entry(p.destination.key.clone()).or_default().insert(run.into());
        }
        self.data.saved.remember_refused_followup(p);
        if self.is_new_thread_request(p.spec["id"].as_str().unwrap_or_default()) {
            self.open_queued_thread(key.clone());
        }
        self.clear_submitted_draft(
            &key,
            p.spec["prompt"].as_str().unwrap_or(""),
            &attachments::FileRef::parse(&p.spec["attachments"]),
        );
        self.data.thread_issues.remove(&key);
        if p.after.is_some() {
            self.data
                .saved
                .thread_settings
                .retain(|(target, _)| target != &key);
        }
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
        }
    }
    fn replace_snapshot(&mut self, peer: &str, mut snapshot: Value) {
        if let Some(threads) = self.data.catalogs.get_mut(peer) {
            for t in threads {
                if let Some(run) = t
                    .runs
                    .iter_mut()
                    .find(|r| r["record"]["spec"]["id"] == snapshot["record"]["spec"]["id"])
                {
                    if snapshot["last_seq"].as_u64() < run["last_seq"].as_u64() {
                        return;
                    }
                    for key in ["placement", "machine"] {
                        if let Some(value) = run.get(key) {
                            snapshot[key] = value.clone();
                        }
                    }
                    *run = snapshot;
                    return;
                }
            }
        }
    }
    fn observe(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let mut selected = Vec::new();
        for pane in self.data.saved.workspace.visible_editors() {
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
                Pane::Chat(key) | Pane::Activity(key) => {
                    if let Some(run) = self.data.agents.history(key).and_then(|runs| runs.last()) {
                        selected.push((key.clone(), run.clone()));
                    }
                }
                _ => {}
            }
        }
        for thread in self.data.catalogs.values().flatten() {
            for run in &thread.runs {
                let Some(id) = run["record"]["spec"]["id"].as_str() else {
                    continue;
                };
                if !terminal(run) {
                    self.data.saved.reply_watches.entry(thread.key.peer.clone()).or_default().insert(id.into());
                } else if self.data.saved.reply_watches.get(&thread.key.peer).is_some_and(|runs| runs.contains(id)) {
                    selected.push((thread.key.clone(), run.clone()));
                }
            }
        }
        for entry in hold::unsettled_changes(&self.data.saved) {
            if let Some(thread) = find_thread(&self.data, &entry.key)
                && let Some(run) = thread
                    .runs
                    .iter()
                    .find(|run| run["record"]["spec"]["id"] == entry.after)
            {
                selected.push((entry.key.clone(), run.clone()));
            }
        }
        for active in needs::active_runs(&self.data) {
            selected.push((active.key, active.run.clone()));
        }
        // Live work gets an observer before older replay, across every open surface.
        selected.sort_by_key(|(_, r)| {
            (
                terminal(r),
                std::cmp::Reverse(r["record"]["created_at"].as_u64().unwrap_or(0)),
            )
        });
        let mut needed = HashSet::new();
        let mut replies_changed = false;
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
                if hold::unsettled_changes(&self.data.saved)
                    .any(|entry| entry.key == key && entry.after == id)
                {
                    self.reconcile_hold(&tag);
                }
                replies_changed |= needs::settle_reply(&mut self.data, &key, &run);
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
                        key,
                        epoch: self.next_watch,
                        stop,
                        retry: Instant::now(),
                        error: None,
                        done: false,
                        frontier: None,
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
        if replies_changed {
            self.persist();
        }
    }
    fn submit(&mut self, pending: Pending) {
        let id = pending.spec["id"].as_str().unwrap_or("").to_owned();
        if self.data.sending.contains(&id) {
            return;
        }
        let retry = self.data.saved.pending.iter().any(|p| p.spec["id"] == id);
        if !retry {
            self.data.saved.pending.push(pending.clone());
        }
        // The idempotency key and exact spec are durable before the network request.
        if self.persist()
            && let Some(client) = &self.client
        {
            self.data.sending.insert(id);
            client.submit(pending, retry, self.bus.publisher());
        }
    }
    fn action(&mut self, action: Action) {
        match action {
            Action::LoadModelCatalog(runtime, destination) => {
                self.load_model_catalog(runtime, destination)
            }
            Action::LoadRuntimes(destination) => self.load_runtimes(destination),
            Action::NewConversation => {
                if !self.data.new_thread_dialog {
                    self.data.new_thread_dialog = true;
                    self.data.focus_new_conversation = true;
                }
            }
            Action::StartConversation => self.start_conversation(),
            Action::SaveDesktop => {
                self.persist();
            }
            Action::MoveConversation(key, settings) => self.move_conversation(key, settings),
            Action::RetryMove(key) => self.retry_move(&key),
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
                let tag = run_key(&key.peer, &id);
                let pending = needs::active_runs(&self.data)
                    .any(|run| run.key == key && run.id() == id && run.observed(&self.data, &tag))
                    && self.data.transcripts.get(&tag).is_some_and(|transcript| {
                        transcript.rows.iter().any(|row| {
                            row.request
                                .as_ref()
                                .is_some_and(|request| request["id"] == request_id)
                                && row.answer.as_ref().is_none_or(|receipt| !receipt.waiting())
                        })
                    });
                if pending && let Some(client) = self.client.clone() {
                    let attempt = new_id();
                    self.data.saved.answers.push(AnswerAttempt {
                        id: attempt.clone(),
                        run_tag: tag.clone(),
                        request: request_id.clone(),
                    });
                    if self.persist() {
                        if let Some(transcript) = self.data.transcripts.get_mut(&tag) {
                            transcript.answer_locally(
                                &request_id,
                                model::AnswerReceipt {
                                    attempt_id: attempt.clone(),
                                    machine: String::new(),
                                    decision: String::new(),
                                    state: model::AnswerState::Sending,
                                    yours: true,
                                    machine_label: String::new(),
                                    at: None,
                                },
                            );
                        }
                        client.respond(conversation::destination(&self.data, &key.peer),
                            json!({"op":"respond_attempt","id":id,"request_id":request_id,"attempt_id":attempt,"result":result}),
                            self.bus.publisher());
                    }
                }
            }
            Action::ProbeRepo(key, id) => {
                if let Some(client) = self.client.clone() {
                    let dest = conversation::destination(&self.data, &key.peer);
                    let publisher = self.bus.publisher();
                    self.data.repo_observations.insert(
                        key.clone(),
                        watchlist::RepoObservation {
                            run: id.clone(),
                            result: None,
                        },
                    );
                    std::thread::spawn(move || {
                        let result = client.operation(&dest, json!({"op":"repo_probe","id":id}));
                        publisher.send(Update::RepoObserved(key, id, result));
                    });
                }
            }
            Action::OpenReply(key, anchor) => {
                conversation::switch(&mut self.data, &key, false, Some(anchor));
                self.action(Action::Open(key, false));
            }
            Action::OpenGate(key, resident, anchor) => {
                conversation::switch(&mut self.data, &key, true, Some(anchor));
                if resident {
                    self.action(Action::OpenChat(key, false));
                } else {
                    self.action(Action::Open(key, false));
                }
            }
            Action::OpenChat(key, pin) => {
                self.data.selected_thread = Some(key.clone());
                self.data.saved.workspace.open_editor(Pane::Chat(key), pin)
            }
            Action::OpenActivity(key, pin) => {
                self.data.selected_thread = Some(key.clone());
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
            Action::RestoreSteer(steering) => self.return_steer(
                steering,
                false,
                "original delivery may still be uncertain".into(),
            ),
            Action::HoldChange(entry, revision, mutation) => {
                self.mutate_hold(entry, Some(revision), mutation)
            }
            Action::HoldDraft(entry, revision, changed) => {
                self.save_hold_draft(entry, revision, changed);
            }
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
            ("New Thread".into(), None),
            ("Show thread list".into(), None),
            ("Reset grid layout".into(), None),
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
            if let Some(grid) = &mut self.data.saved.workspace.grid {
                grid.open_here = None;
            }
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
                if index != 0
                    && let Some(grid) = &mut self.data.saved.workspace.grid
                {
                    grid.open_here = None;
                }
                match index {
                    4 => self.data.saved.workspace.show_correspondence(),
                    5 => self.mail_action(correspondence::MailAction::New),
                    6 => self.data.saved.workspace.show_agents(),
                    7 => self.data.saved.workspace.show_work(),
                    0 => self.action(Action::NewConversation),
                    1 => self.data.saved.workspace.show_list(),
                    2 => {
                        if let Some(grid) = &mut self.data.saved.workspace.grid {
                            for (column, slot) in grid.slots.iter_mut().enumerate() {
                                slot.home = grid::Cell {
                                    column,
                                    row: 0,
                                    width: 1,
                                    height: 2,
                                };
                            }
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
        let latest = thread.runs.last();
        let has_background = thread.runs.iter().rev().skip(1).any(|run| !terminal(run));
        let state = if data.errors.contains_key(&dest.key) {
            "unseen"
        } else {
            match latest.and_then(|run| run["status"].as_str()) {
                Some("completed") => "done",
                Some("running") => "running",
                Some("waiting") => "input",
                Some("failed") => "failed",
                Some("interrupted") => "interrupted",
                Some("cancelled") => "cancelled",
                Some("starting") => "starting",
                Some("cancelling") => "stopping",
                _ => "unknown",
            }
        };
        let observed = watchlist::latest_visible(data, &thread.key, &thread.runs);
        let status = if has_background && state != "unseen" {
            format!("{state} +bg")
        } else {
            state.into()
        };
        let color = match state {
            "running" | "starting" | "stopping" => egui::Color32::from_rgb(76, 158, 255),
            "input" => egui::Color32::from_rgb(216, 162, 58),
            "failed" | "interrupted" => egui::Color32::from_rgb(239, 91, 80),
            _ => ui.visuals().weak_text_color(),
        };
        let last = if state == "unseen" { "offline" } else { observed.map_or("—", |event| event.label) };
        let response = ui.push_id((&thread.key.peer, &thread.key.id), |ui| {
            watchlist::Row {
                title: &thread.title,
                state: &status,
                color,
                last,
                age: &watchlist::event_age(observed),
                selected,
                muted: state == "unseen",
                context: Some((&thread.key, &thread.runs)),
            }.show(ui, data)
        }).inner;
        if let Some(pin) = watchlist::opens(ui, &response) {
            actions.push(Action::Open(thread.key.clone(), pin));
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
            Pane::LivePulse => "Live pulse".into(),
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
            Pane::LegacyNewConversation => unreachable!("removed while loading the workspace"),
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
            Pane::LivePulse => egui::Id::new("live-pulse"),
            Pane::Correspondence => egui::Id::new("correspondence"),
            Pane::Threads => egui::Id::new("threads"),
            Pane::Welcome => egui::Id::new("welcome"),
            Pane::LegacyNewConversation => unreachable!("removed while loading the workspace"),
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
            Pane::LivePulse => pulse::panel(ui, self.data, self.actions),
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
            Pane::LegacyNewConversation => unreachable!("removed while loading the workspace"),
            Pane::Threads => {
                if ui.button("New Thread").clicked() {
                    self.actions.push(Action::NewConversation);
                }
                ui.horizontal(|ui| {
                    let search_width =
                        (ui.available_width() - 28. - ui.spacing().item_spacing.x).max(24.);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.data.filter)
                            .hint_text("Filter Watchlist…")
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
                watchlist::header(ui);
                let inspector_height = if self.data.selected_thread.is_some() {
                    310.
                } else {
                    0.
                };
                let list_height = (ui.available_height() - inspector_height).max(70.);

                egui::ScrollArea::vertical()
                    .max_height(list_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if destinations.is_empty() {
                            ui.weak(if self.data.saved.destinations.is_empty() {
                                "Connecting to machines…"
                            } else {
                                "No machines selected."
                            });
                        }
                        ui.label(RichText::new("THREADS").monospace().small().weak());
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
                        agents::watchlist(ui, self.data, &destinations, &query, self.actions);
                    });
                if let Some(key) = &self.data.selected_thread {
                    egui::ScrollArea::vertical()
                        .id_salt(("inspector", key))
                        .max_height(ui.available_height())
                        .show(ui, |ui| {
                            watchlist::inspector(ui, self.data, key, self.actions)
                        });
                }
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
            .visible_editors()
            .contains(&&Pane::Work);
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
        if !self.data.new_thread_dialog
            && ctx.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::P,
                ) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::P)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::F1)
            })
        {
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
        self.grid_workspace(ui, &mut actions);
        for action in actions {
            self.action(action)
        }
        if let Some(client) = &self.client
            && let Some((destination, args)) = self
                .data
                .tool_approvals
                .show(&ctx, &self.data.saved.destinations)
        {
            client.tool_approvals(destination, args, self.bus.publisher());
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
        self.command_palette(&ctx);
        if let Some(field) = &mut self.field
            && !field.show(&ctx)
        {
            self.field = None;
        }
        if self.data.new_thread_dialog {
            let mut actions = Vec::new();
            new_conversation::dialog(&ctx, &mut self.data, &mut actions);
            for action in actions {
                self.action(action);
            }
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
            .with_inner_size([1600., 940.])
            .with_min_inner_size([760., 720.])
            .with_app_id("dev.boxen.desktop"),
        ..Default::default()
    };
    eframe::run_native(
        "Boxen",
        options,
        Box::new(move |cc| Ok(Box::new(Desktop::new(cc, home, origin, extra)))),
    )
}
