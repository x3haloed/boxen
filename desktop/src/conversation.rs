//! Messages and Activity are two projections of one conversation. The composer
//! sits outside both projections, so switching views does not change the draft
//! or its destination.
use crate::{
    Action, Data,
    attachments::{self, FileAction, FileRef, Preview, Target},
    console_style,
    model::{ContextChange, ContextMeasure, Destination, Row, ThreadKey, Transcript},
};
use eframe::egui::{self, RichText};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub struct View {
    visible: Option<(String, f32)>,
    anchor: Option<String>,
    latest: bool,
    at_latest: bool,
    message_layouts: HashMap<String, (MessageLayout, egui::Rect)>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct MessageLayout {
    pub sequence: u64,
    pub width: f32,
    pub pixels_per_point: f32,
    pub latest_reply: bool,
}

impl View {
    pub fn anchors_run(&self, id: &str) -> bool {
        self.anchor
            .as_deref()
            .and_then(|anchor| anchor.split_once(':'))
            .is_some_and(|(run, _)| run == id)
    }

    pub fn offscreen_run(
        &self,
        ui: &egui::Ui,
        id: &str,
        layout: MessageLayout,
    ) -> Option<egui::Rect> {
        if self.anchors_run(id) {
            return None;
        }
        let (previous, bounds) = self.message_layouts.get(id)?;
        let bounds = bounds.translate(ui.next_widget_position().to_vec2());
        (*previous == layout && !bounds.intersects(ui.clip_rect().expand(96.))).then_some(bounds)
    }

    pub fn measure_run(
        &mut self,
        id: &str,
        layout: MessageLayout,
        origin: egui::Pos2,
        bounds: egui::Rect,
    ) {
        self.message_layouts
            .insert(id.into(), (layout, bounds.translate(-origin.to_vec2())));
    }
}
pub fn header(ui: &mut egui::Ui, data: &mut Data, key: &ThreadKey) -> bool {
    let mut activity = data.saved.activity_views.contains(key);
    ui.horizontal(|ui| {
        for (label, target) in [("Messages", false), ("Activity", true)] {
            if ui.selectable_label(activity == target, label).clicked() && activity != target {
                let anchor = data
                    .views
                    .entry(key.clone())
                    .or_default()
                    .visible
                    .as_ref()
                    .map(|(id, _)| id.clone());
                switch(data, key, target, anchor);
                activity = target;
            }
        }
    });
    ui.separator();
    activity
}
pub fn switch(data: &mut Data, key: &ThreadKey, activity: bool, anchor: Option<String>) {
    if activity {
        data.saved.activity_views.insert(key.clone());
    } else {
        data.saved.activity_views.remove(key);
    }
    let view = data.views.entry(key.clone()).or_default();
    view.anchor = anchor;
    view.latest = view.anchor.is_none();
}
pub fn history(
    ui: &mut egui::Ui,
    data: &mut Data,
    key: &ThreadKey,
    activity: bool,
    reserved: f32,
    show: impl FnOnce(&mut egui::Ui, &mut Data),
) {
    let view = data.views.entry(key.clone()).or_default();
    if !view.at_latest && view.visible.is_some() && ui.small_button("Jump to latest").clicked() {
        view.latest = true;
        view.anchor = None;
    }
    view.visible = None;
    let height = (ui.available_height() - reserved).max(32.);
    let view = egui::ScrollArea::vertical()
        .id_salt(("conversation-history", key, activity))
        .auto_shrink([false, false])
        .max_height(height)
        .stick_to_bottom(true)
        .show_viewport(ui, |ui, _| {
            show(ui, data);
            if data.views.get(key).is_some_and(|v| v.latest) {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                data.views.entry(key.clone()).or_default().latest = false;
            }
        });
    data.views.entry(key.clone()).or_default().at_latest =
        view.state.offset.y + view.inner_rect.height() + 12. >= view.content_size.y;
}
pub fn record(
    ui: &egui::Ui,
    views: &mut HashMap<ThreadKey, View>,
    key: &ThreadKey,
    id: &str,
    rect: egui::Rect,
) {
    let view = views.entry(key.clone()).or_default();
    if view.anchor.as_deref() == Some(id) {
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
        view.anchor = None;
    }
    if rect.intersects(ui.clip_rect()) {
        let distance = (rect.center().y - ui.clip_rect().center().y).abs();
        if view
            .visible
            .as_ref()
            .is_none_or(|(_, best)| distance < *best)
        {
            view.visible = Some((id.into(), distance));
        }
    }
}
pub fn reserve(text: &str, files: &[FileRef], extra: usize, width: f32) -> f32 {
    let capacity = ((width - 170.) / 7.).max(10.) as usize;
    let lines = text
        .split('\n')
        .map(|line| line.chars().count().max(1).div_ceil(capacity))
        .sum::<usize>();
    50. + (lines.saturating_sub(1).min(5) * 17) as f32
        + if files.is_empty() { 0. } else { 25. }
        + (extra * 22) as f32
}

pub fn route_hint(ui: &mut egui::Ui, hint: &str) {
    let font = egui::FontId::monospace(10.5);
    let height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.add(
                egui::Label::new(RichText::new(hint).font(font).weak())
                    .halign(egui::Align::Min)
                    .truncate(),
            );
        },
    );
}

pub struct Composition {
    pub send: bool,
    pub changed: bool,
}
pub struct Composer<'a> {
    pub text: &'a mut String,
    pub files: &'a mut Vec<FileRef>,
    pub target: Target,
    pub label: &'a str,
    pub limit: usize,
    pub blocked: bool,
}
pub fn send_on_enter(ui: &mut egui::Ui, editor_id: egui::Id) -> bool {
    // The frame's modifier snapshot can already be clear by the time
    // Shift+Enter is processed. Match the modifiers on the key event itself.
    ui.memory(|m| m.has_focus(editor_id))
        && ui.input_mut(|input| {
            let mut send = false;
            input.events.retain(|event| {
                if let egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } = event
                    && modifiers.is_none()
                {
                    send |= !repeat;
                    false
                } else {
                    true
                }
            });
            send
        })
}

pub fn composer(
    ui: &mut egui::Ui,
    data: &mut Data,
    composer: Composer<'_>,
    actions: &mut Vec<Action>,
) -> Composition {
    let Composer {
        text,
        files,
        target,
        label,
        limit,
        blocked,
    } = composer;
    let local = local(data);
    let (preparing, previews) = (&mut data.preparing, &mut data.file_previews);
    ui.separator();
    let mut changed = chips(ui, files, &local, true, previews, actions);
    let working = preparing.get(&target).is_some_and(|p| p.working);
    let editor_id = ui.make_persistent_id(("composer", &target));
    let enter = send_on_enter(ui, editor_id);
    let (editor, valid, click) = ui
        .horizontal(|ui| {
            let attach = ui
                .add_enabled(
                    !working && files.len() < attachments::MAX_FILES,
                    egui::Button::new(if working { "Preparing…" } else { "Attach" }),
                )
                .on_hover_text("Choose files or drop files onto the composer")
                .clicked();
            let font = egui::TextStyle::Button.resolve(ui.style());
            let send_width = ui.fonts_mut(|f| {
                f.layout_no_wrap(label.into(), font, egui::Color32::WHITE)
                    .size()
                    .x
            }) + ui.spacing().button_padding.x * 2.
                + ui.spacing().item_spacing.x;
            let width = (ui.available_width() - send_width).max(40.);
            let editor = ui
                .allocate_ui_with_layout(
                    egui::vec2(width, 0.),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        egui::ScrollArea::vertical()
                            .id_salt(("composer-input", &target))
                            .max_height(104.)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(text)
                                        .id(editor_id)
                                        .return_key(egui::KeyboardShortcut::new(
                                            egui::Modifiers::SHIFT,
                                            egui::Key::Enter,
                                        ))
                                        .hint_text("Message…")
                                        .desired_rows(1)
                                        .desired_width(f32::INFINITY),
                                )
                            })
                            .inner
                    },
                )
                .inner;
            let valid = !blocked
                && !working
                && (!text.trim().is_empty() || !files.is_empty())
                && text.len() <= limit;
            let click = ui
                .add_enabled(valid, egui::Button::new(label))
                .on_hover_text("Enter to send · Shift+Enter for a new line")
                .clicked();
            attachments::add_files(
                ui,
                &editor,
                target.clone(),
                files.len(),
                !working,
                attach,
                actions,
            );
            (editor, valid, click)
        })
        .inner;
    changed |= editor.changed();
    if text.len() > limit {
        ui.label(format!("Text limit: {} KiB", limit / 1024));
    }
    if let Some(p) = preparing.get_mut(&target)
        && !p.errors.is_empty()
    {
        ui.horizontal_wrapped(|ui| {
            for error in &p.errors {
                ui.label(error);
            }
            if ui.small_button("Dismiss").clicked() {
                p.errors.clear();
            }
        });
    }
    if valid && (enter || click) {
        editor.request_focus();
    }
    Composition {
        send: valid && (enter || click),
        changed,
    }
}

fn file_detail(
    ui: &mut egui::Ui,
    file: &FileRef,
    destination: &Destination,
    draft: bool,
    previews: &mut HashMap<String, Option<Result<Preview, String>>>,
    actions: &mut Vec<Action>,
) -> bool {
    ui.label(&file.name);
    ui.weak(file.info());
    let id = format!("{}:{}", destination.key, file.digest);
    match previews.get(&id) {
        Some(Some(Ok(Preview::Text(text)))) => {
            egui::ScrollArea::vertical()
                .max_height(220.)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(text).wrap().selectable(true));
                });
        }
        Some(Some(Ok(Preview::Image(image)))) => {
            let handle = ui
                .ctx()
                .load_texture(&id, image.clone(), egui::TextureOptions::LINEAR);
            ui.image((handle.id(), handle.size_vec2()));
        }
        Some(Some(Ok(Preview::Unsupported))) => {
            ui.weak("No inline preview for this file.");
        }
        Some(Some(Err(error))) => {
            ui.label(format!("Original unavailable: {error}"));
        }
        Some(None) => {
            ui.weak("Opening original…");
        }
        None => {}
    }
    let mut remove = false;
    ui.horizontal(|ui| {
        if previews.get(&id).is_none_or(Option::is_some)
            && ui
                .button(if previews.contains_key(&id) {
                    "Reload preview"
                } else {
                    "Preview"
                })
                .clicked()
        {
            actions.push(Action::File(FileAction::Open(
                destination.clone(),
                file.clone(),
                false,
            )));
        }
        if draft {
            remove = ui.button("Remove").clicked();
        } else if ui.button("Save as…").clicked() {
            actions.push(Action::File(FileAction::Open(
                destination.clone(),
                file.clone(),
                true,
            )));
        }
    });
    remove
}
pub fn chips(
    ui: &mut egui::Ui,
    files: &mut Vec<FileRef>,
    destination: &Destination,
    draft: bool,
    previews: &mut HashMap<String, Option<Result<Preview, String>>>,
    actions: &mut Vec<Action>,
) -> bool {
    if files.is_empty() {
        return false;
    }
    let visible = if ui.available_width() < 380. { 1 } else { 2 };
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for (i, file) in files.iter().take(visible).enumerate() {
            ui.push_id(i, |ui| {
                let label: String = file.name.chars().take(22).collect();
                let label = if label.len() < file.name.len() {
                    format!("{label}…")
                } else {
                    label
                };
                ui.menu_button(label, |ui| {
                    if file_detail(ui, file, destination, draft, previews, actions) {
                        remove = Some(i);
                        ui.close();
                    }
                })
                .response
                .on_hover_text(&file.name);
            });
        }
        if files.len() > visible {
            ui.menu_button(format!("+{} files", files.len() - visible), |ui| {
                for (i, file) in files.iter().enumerate().skip(visible) {
                    ui.push_id(i, |ui| {
                        ui.collapsing(&file.name, |ui| {
                            if file_detail(ui, file, destination, draft, previews, actions) {
                                remove = Some(i);
                                ui.close();
                            }
                        });
                    });
                }
            });
        }
    });
    if let Some(i) = remove {
        files.remove(i);
        true
    } else {
        false
    }
}
pub fn local(data: &Data) -> Destination {
    Destination {
        key: data.local_key.clone(),
        label: "This machine".into(),
        address: String::new(),
    }
}
pub fn destination(data: &Data, key: &str) -> Destination {
    data.saved
        .destinations
        .iter()
        .find(|d| d.key == key)
        .cloned()
        .unwrap_or(Destination {
            key: key.into(),
            label: "Machine".into(),
            address: String::new(),
        })
}
pub fn utc_minute(at: u64) -> String {
    let minute = at / 60_000;
    format!("{:02}:{:02}", (minute / 60) % 24, minute % 60)
}

pub fn journal_time(ui: &mut egui::Ui, at: Option<u64>) {
    let label = at.map_or_else(|| "—".into(), utc_minute);
    ui.add_sized(
        [40., 18.],
        egui::Label::new(RichText::new(label).monospace().size(10.5).weak())
            .halign(egui::Align::Min),
    )
    .on_hover_text("Journal time (UTC)");
}

pub enum MessageAuthor<'a> {
    You,
    Assistant,
    Mail(&'a str),
}

pub fn message_row(
    ui: &mut egui::Ui,
    at: Option<u64>,
    author: MessageAuthor<'_>,
    text: &str,
    rich: bool,
    body: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    ui.horizontal_top(|ui| {
        journal_time(ui, at);
        match author {
            MessageAuthor::You => {
                ui.label(RichText::new("›").monospace().color(console_style::ACCENT))
                    .on_hover_text("You");
            }
            MessageAuthor::Assistant => {
                console_style::dot(ui, console_style::SECONDARY);
            }
            MessageAuthor::Mail(_) => {}
        }
        ui.vertical(|ui| {
            ui.set_min_width(ui.available_width());
            if let MessageAuthor::Mail(name) = author {
                ui.label(RichText::new(name).monospace().size(10.5).strong());
            }
            if rich {
                markdown(ui, text);
            } else if !text.is_empty() {
                ui.add(egui::Label::new(text).wrap().selectable(true));
            }
            body(ui);
        });
    })
    .response
}

pub fn run_divider(ui: &mut egui::Ui, run: &Value, label: &str) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        journal_time(ui, run["record"]["created_at"].as_u64());
        ui.label(RichText::new(label).monospace().size(10.5).strong());
        if let Some(status) = run["status"].as_str() {
            ui.label(RichText::new(status).monospace().size(10.5).weak());
        }
    });
}

fn markdown(ui: &mut egui::Ui, text: &str) {
    // Keep the transcript readable during streaming; links are only rendered
    // for explicit HTTP(S) destinations. Source text is retained in the log.
    let mut fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if line.is_empty() {
            ui.add_space(4.);
            continue;
        }
        if fence {
            ui.add(
                egui::Label::new(RichText::new(line).monospace())
                    .wrap()
                    .selectable(true),
            );
            continue;
        }
        if let Some((title, url, rest)) = line
            .strip_prefix('[')
            .and_then(|s| s.split_once("]("))
            .and_then(|(label, s)| s.split_once(')').map(|(url, rest)| (label, url, rest)))
            && (url.starts_with("https://") || url.starts_with("http://"))
        {
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to(title, url);
                ui.add(egui::Label::new(rest.trim_start()).wrap().selectable(true));
            });
        } else {
            let heading = line.starts_with('#') && line.trim_start_matches('#').starts_with(' ');
            let line = if heading {
                line.trim_start_matches('#').trim_start()
            } else {
                line
            };
            let line = if let Some(rest) = line.strip_prefix("- ") {
                format!("• {rest}")
            } else {
                line.into()
            };
            let font = egui::TextStyle::Body.resolve(ui.style());
            let strong = egui::Color32::from_gray(if ui.visuals().dark_mode { 245 } else { 10 });
            let mut job = egui::text::LayoutJob::default();
            for (i, part) in line.split("**").enumerate() {
                let color = if i % 2 == 1 || heading {
                    strong
                } else {
                    ui.visuals().text_color()
                };
                job.append(part, 0., egui::TextFormat::simple(font.clone(), color));
            }
            ui.add(egui::Label::new(job).wrap().selectable(true));
        }
    }
}
#[derive(Default)]
struct ContextReading {
    measure: Option<ContextMeasure>,
    measured_at: Option<u64>,
    change: Option<(ContextChange, u64)>,
}

impl ContextReading {
    fn warning(&self) -> bool {
        self.measure.is_some_and(ContextMeasure::past_warning_line)
    }

    fn recent(&self) -> Option<(ContextChange, u64)> {
        const RECENT_MS: u64 = 30 * 60 * 1000;
        self.change.filter(|(_, at)| {
            crate::pulse::now()
                .checked_sub(*at)
                .is_some_and(|age| age <= RECENT_MS)
        })
    }
}

fn context_reading(data: &Data, key: &ThreadKey, runs: &[Value]) -> ContextReading {
    let Some(run) = runs.last() else {
        return ContextReading::default();
    };
    let caught_up = |run: &Value| -> Option<&Transcript> {
        let id = run["record"]["spec"]["id"].as_str()?;
        let tag = crate::client::run_key(&key.peer, id);
        let transcript = data.transcripts.get(&tag)?;
        (transcript.seq >= run["last_seq"].as_u64().unwrap_or(0)
            && data
                .watches
                .get(&tag)
                .is_none_or(|watch| watch.error.is_none()))
        .then_some(transcript)
    };
    if data.errors.contains_key(&key.peer)
        || (data.agents.resident(key).is_some() && data.agents.error(key).is_some())
    {
        return ContextReading::default();
    }
    let Some(current) = caught_up(run) else {
        return ContextReading::default();
    };
    let previous_change = || {
        let session = current.backend_session.as_deref()?;
        let machine = run["machine"].as_str().unwrap_or(&key.peer);
        let model = current.backend_model.as_deref();
        runs.iter()
            .rev()
            .skip(1)
            .take_while(|prior| prior["machine"].as_str().unwrap_or(&key.peer) == machine)
            .map_while(caught_up)
            .take_while(|prior| {
                prior.backend_model.as_deref() == model
                    && prior.backend_session.as_deref() == Some(session)
            })
            .filter_map(|prior| prior.context_change)
            .next()
    };
    ContextReading {
        measure: current.context,
        measured_at: current.context_at,
        change: current.context_change.or_else(previous_change),
    }
}

fn context_details(ui: &mut egui::Ui, backend: &str, reading: &ContextReading) {
    if let Some(measure) = reading.measure {
        ui.label(measure.description());
        if let Some(at) = reading.measured_at {
            let source = if backend == "openrouter" {
                "Boxen context plan"
            } else {
                backend
            };
            ui.weak(format!("{source} · {} UTC", utc_minute(at)));
        }
    } else {
        ui.weak("Context reading unknown");
    }
    if reading.warning() {
        ui.colored_label(context_warning_color(), "Past Boxen’s 85% warning line");
    }
    ui.weak(match backend {
        "codex" => "Codex controls compaction. Its threshold is unknown.",
        "claude" => {
            "Claude Code reports compaction boundaries and the before-token count when available."
        }
        "openrouter" => "Boxen records changes to history selection and shortages as they occur.",
        "antigravity" => {
            "Antigravity reports session usage when available. Compaction evidence is unknown."
        }
        _ => "Runtime context evidence is unknown.",
    });
    if let Some((change, at)) = reading.change {
        ui.label(format!(
            "Last observed event · {} · {} UTC",
            change.label(),
            utc_minute(at)
        ));
    }
}

pub fn context_label(ui: &mut egui::Ui, data: &Data, key: &ThreadKey, runs: &[Value]) {
    let backend = runs
        .last()
        .and_then(|run| run["record"]["spec"]["backend"].as_str())
        .unwrap_or("unknown");
    let reading = context_reading(data, key, runs);
    let label = reading
        .measure
        .map_or_else(|| "ctx unknown".into(), ContextMeasure::label);
    ui.menu_button(
        RichText::new(format!("{backend} · {label}"))
            .monospace()
            .size(10.5)
            .weak(),
        |ui| {
            ui.set_max_width(320.);
            context_details(ui, backend, &reading);
        },
    );
    if reading.warning() {
        ui.colored_label(context_warning_color(), "past 85% line")
            .on_hover_text("Boxen's warning line. Codex controls its own compaction.");
    }
    if let Some((change, at)) = reading.recent() {
        ui.weak(format!("{} {}", change.label(), utc_minute(at)))
            .on_hover_text("Recorded context event · UTC");
    }
}

fn context_warning_color() -> egui::Color32 {
    egui::Color32::from_rgb(216, 162, 58)
}

fn context_icon(ui: &mut egui::Ui, color: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(8., 12.), egui::Sense::hover());
    let center = rect.center();
    let radius = 3.;
    ui.painter()
        .circle_stroke(center, radius, egui::Stroke::new(1., color));
    let half = (0..=8)
        .map(|step| {
            let angle = std::f32::consts::PI * step as f32 / 8.;
            center + egui::vec2(-angle.sin(), angle.cos()) * radius
        })
        .collect();
    ui.painter()
        .add(egui::Shape::convex_polygon(half, color, egui::Stroke::NONE));
    response
}

pub fn context_mark(ui: &mut egui::Ui, data: &Data, key: &ThreadKey, runs: &[Value]) {
    let reading = context_reading(data, key, runs);
    if reading.warning() {
        context_icon(ui, context_warning_color()).on_hover_text("Past Boxen’s 85% warning line");
    } else if let Some((change, at)) = reading.recent() {
        context_icon(ui, ui.visuals().weak_text_color()).on_hover_text(format!(
            "{} · {} UTC",
            change.label(),
            utc_minute(at),
        ));
    } else {
        ui.allocate_space(egui::vec2(8., 12.));
    }
}

pub fn is_message(row: &Row) -> bool {
    matches!(row.label.as_str(), "You" | "You · steering" | "Agent")
}
pub fn incident(row: &Row) -> bool {
    row.label == "Incident"
        || row.label == "Context"
        || row.answer.is_some()
        || row.label == "Resolved by runtime · no answer recorded"
        || row.label == "Closed · no answer recorded"
        || row.label == "Backend input requested"
        || row.label == "Status"
            && ["failed", "interrupted", "cancelled"]
                .iter()
                .any(|s| row.text.starts_with(s))
}
pub fn activity_rows(ui: &mut egui::Ui, rows: &[Row]) {
    let ordinary: Vec<_> = rows
        .iter()
        .filter(|row| !(incident(row) || row.label == "Status" && row.text == "completed"))
        .collect();
    if let Some(first) = ordinary.first() {
        let count = ordinary.len();
        let mut titles: Vec<_> = ordinary
            .iter()
            .filter(|row| matches!(row.label.as_str(), "Tool" | "Command" | "Thinking"))
            .map(|row| {
                if row.label == "Thinking" {
                    "thinking"
                } else {
                    row.text
                        .lines()
                        .next()
                        .filter(|line| !line.is_empty())
                        .unwrap_or(&row.label)
                }
            })
            .collect();
        titles.dedup();
        let title = format!(
            "{count} {}{}",
            if count == 1 { "step" } else { "steps" },
            if titles.is_empty() {
                String::new()
            } else {
                format!(
                    " · {}",
                    titles.into_iter().take(3).collect::<Vec<_>>().join(", ")
                )
            },
        );
        let failed = ordinary
            .iter()
            .filter(|row| row.tool.as_ref().is_some_and(|tool| tool.failed))
            .count();
        let passed = ordinary
            .iter()
            .filter(|row| {
                row.tool
                    .as_ref()
                    .is_some_and(|tool| tool.finished && !tool.failed)
            })
            .count();
        let result = if failed > 0 {
            Some((format!("{failed} failed"), console_style::ERROR))
        } else if passed > 0 {
            Some((format!("{passed} ok"), console_style::OK))
        } else {
            None
        };
        ui.push_id(&first.id, |ui| {
            ui.indent("step-gutter", |ui| {
                let mut clicked = false;
                let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    ui.make_persistent_id("step-group"),
                    false,
                );
                let mut header = state.show_header(ui, |ui| {
                    let outcome_width = if result.is_some() { 58. } else { 0. };
                    let width = (ui.available_width() - outcome_width).max(16.);
                    let response = ui
                        .allocate_ui_with_layout(
                            egui::vec2(width, 18.),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&title).monospace().size(10.5).weak(),
                                    )
                                    .truncate()
                                    .sense(egui::Sense::click()),
                                )
                            },
                        )
                        .inner
                        .on_hover_text(&title);
                    clicked = response.clicked();
                    if let Some((result, color)) = &result {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(result).monospace().size(10.5).color(*color));
                        });
                    }
                });
                if clicked {
                    header.toggle();
                }
                header.body(|ui| {
                    for row in ordinary {
                        activity_row(ui, row);
                    }
                });
            });
        });
    }
    for row in rows {
        if incident(row) {
            activity_row(ui, row);
        }
    }
}

pub fn activity_row(ui: &mut egui::Ui, row: &Row) {
    ui.push_id(&row.id, |ui| {
        if row.answer.is_some()
            || matches!(
                row.label.as_str(),
                "Resolved by runtime · no answer recorded" | "Closed · no answer recorded"
            )
        {
            ui.horizontal_top(|ui| {
                journal_time(ui, row.at);
                ui.vertical(|ui| {
                    if !row.text.is_empty() {
                        let first = row.text.lines().next().unwrap_or_default();
                        let preview: String = first.chars().take(160).collect();
                        ui.add(egui::Label::new(RichText::new(preview).monospace()).wrap())
                            .on_hover_text(&row.text);
                    }
                    if let Some(answer) = &row.answer {
                        ui.weak(answer.label());
                        if answer.state == crate::model::AnswerState::Written
                            && !answer.yours
                            && let Some(refused) = row
                                .refused_answers
                                .iter()
                                .rev()
                                .find(|attempt| attempt.yours)
                        {
                            ui.weak(format!(
                                "Your {} was refused before writing",
                                refused.decision
                            ));
                        }
                    } else {
                        ui.weak(&row.label);
                    }
                });
            });
            return;
        }
        if row.label == "Context" {
            ui.horizontal_wrapped(|ui| {
                journal_time(ui, row.at);
                context_icon(ui, ui.visuals().weak_text_color());
                ui.weak(&row.text);
            });
            return;
        }
        let label = if row.label == "Agent" {
            "Private narration"
        } else {
            &row.label
        };
        let preview: String = row
            .text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(85)
            .collect();
        if row.label == "Status" && row.text == "completed" {
            return;
        }
        let label = if row.tool.as_ref().is_some_and(|t| t.failed) {
            format!("{label} failed")
        } else {
            label.into()
        };
        let title = if preview.is_empty() {
            label
        } else {
            format!("{label} · {preview}")
        };
        egui::CollapsingHeader::new(title)
            .default_open(incident(row) || row.label == "Agent")
            .show(ui, |ui| {
                ui.add(egui::Label::new(&row.text).wrap().selectable(true));
                if let Some(tool) = &row.tool {
                    if !tool.output.is_empty() {
                        ui.weak("Output");
                        ui.add(
                            egui::Label::new(RichText::new(&tool.output).monospace())
                                .wrap()
                                .selectable(true),
                        );
                    }
                    if let Some(result) = &tool.result {
                        ui.weak("Result");
                        ui.add(egui::Label::new(result).wrap().selectable(true));
                    }
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_composer_inserts_shift_enter_and_sends_on_plain_enter() {
        let ctx = egui::Context::default();
        let mut data = Data::default();
        let mut text = "First line".to_owned();
        let mut files = Vec::new();
        let mut actions = Vec::new();
        let target = Target::Mail("fixture".into());
        let mut frame = |modifiers: Option<egui::Modifiers>, focus: bool| {
            let mut sent = false;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480., 240.),
                )),
                events: modifiers
                    .into_iter()
                    .flat_map(|modifiers| {
                        [true, false].map(|pressed| egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed,
                            repeat: false,
                            modifiers,
                        })
                    })
                    .collect(),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                if focus {
                    let id = ui.make_persistent_id(("composer", &target));
                    ui.memory_mut(|memory| memory.request_focus(id));
                }
                sent = composer(
                    ui,
                    &mut data,
                    Composer {
                        text: &mut text,
                        files: &mut files,
                        target: target.clone(),
                        label: "Send",
                        limit: 32768,
                        blocked: false,
                    },
                    &mut actions,
                )
                .send;
            });
            (sent, text.clone())
        };
        assert!(
            !frame(Some(egui::Modifiers::NONE), false).0,
            "Enter outside the editor sent a draft"
        );
        frame(None, true);
        let (sent, draft) = frame(Some(egui::Modifiers::SHIFT), false);
        assert!(
            !sent && draft.contains('\n'),
            "Shift+Enter must insert a newline"
        );
        assert!(frame(Some(egui::Modifiers::NONE), false).0);
    }
}
