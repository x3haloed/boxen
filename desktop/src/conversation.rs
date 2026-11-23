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
pub fn header(
    ui: &mut egui::Ui,
    data: &mut Data,
    key: &ThreadKey,
    extra: impl FnOnce(&mut egui::Ui, &mut Data, bool),
) -> bool {
    let mut activity = data.saved.activity_views.contains(key);
    ui.horizontal(|ui| {
        for (label, target) in [("Mail", false), ("Activity", true)] {
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
        extra(ui, data, activity);
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
    if !view.at_latest && view.visible.is_some() && ui.button("Jump to latest").clicked() {
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

const ACCENT_HINT: egui::Color32 = crate::console_style::ACCENT;

pub struct Composition {
    pub send: bool,
    pub changed: bool,
}
pub struct Composer<'a> {
    pub text: &'a mut String,
    pub files: &'a mut Vec<FileRef>,
    pub target: Target,
    pub label: &'a str,
    /// Route hint shown inside the composer; clicking it sends.
    pub hint: Option<String>,
    pub placeholder: String,
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
        hint,
        placeholder,
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
                    crate::console_style::icon_button(if working { "…" } else { "+" }),
                )
                .on_hover_text("Attach files · or drop files onto the composer")
                .clicked();
            let send_text = match &hint {
                Some(hint) => format!("{hint} ↵"),
                None => format!("{label} ↵"),
            };
            let send_font = egui::FontId::monospace(10.5);
            let send_width = ui.fonts_mut(|f| {
                f.layout_no_wrap(send_text.clone(), send_font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            }) + ui.spacing().button_padding.x * 2.
                + 4.;
            let send_width = send_width.min(ui.available_width() * 0.6);
            let width = (ui.available_width() - send_width - ui.spacing().item_spacing.x).max(40.);
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
                                        .hint_text(placeholder.as_str())
                                        .frame(egui::Frame::NONE)
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
                .add_enabled_ui(valid, |ui| {
                    ui.add_sized(
                        egui::vec2(send_width, crate::console_style::MIN_TARGET),
                        egui::Button::new(RichText::new(&send_text).font(send_font).color(
                            if valid {
                                ACCENT_HINT
                            } else {
                                ui.visuals().weak_text_color()
                            },
                        ))
                        .frame(false)
                        .truncate(),
                    )
                })
                .inner
                .on_hover_text(format!("{label} · Enter sends · Shift+Enter adds a line"))
                .on_disabled_hover_text(send_text.clone())
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
            if ui.button("Dismiss").clicked() {
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

/// Who a message comes from. Threads and one-to-one resident mail have two
/// participants, so depth rather than a name tells them apart: your turns
/// are raised on a band with an accent edge, the agent's sit flush.
pub enum MessageAuthor {
    You,
    Agent,
}

/// How far a raised band reaches left of the time gutter, so the gutter and
/// text column stay aligned with flush rows.
const BAND_BLEED: i8 = 6;

pub fn message_row(
    ui: &mut egui::Ui,
    at: Option<u64>,
    author: MessageAuthor,
    text: &str,
    rich: bool,
    body: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let you = matches!(author, MessageAuthor::You);
    let frame = if you {
        // A little air above marks the start of your turn.
        ui.add_space(4.);
        egui::Frame::new()
            .fill(console_style::RAISED)
            .outer_margin(egui::Margin {
                left: -BAND_BLEED,
                ..Default::default()
            })
            .inner_margin(egui::Margin {
                left: BAND_BLEED,
                right: 6,
                top: 4,
                bottom: 4,
            })
    } else {
        egui::Frame::new()
    };
    let response = frame
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if you {
                ui.visuals_mut().override_text_color = Some(console_style::RAISED_TEXT);
            }
            ui.horizontal_top(|ui| {
                journal_time(ui, at);
                // Holds the text column where the old role glyph sat, so
                // messages stay aligned with step groups and run dividers.
                ui.add_space(9.);
                ui.vertical(|ui| {
                    ui.set_min_width(ui.available_width());
                    if rich {
                        markdown(ui, text);
                    } else if !text.is_empty() {
                        ui.add(egui::Label::new(text).wrap().selectable(true));
                    }
                    body(ui);
                });
            });
        })
        .response;
    if you {
        let edge = egui::Rect::from_min_size(
            response.rect.left_top(),
            egui::vec2(2., response.rect.height()),
        );
        ui.painter().rect_filled(edge, 0., console_style::ACCENT);
    }
    response
}

pub fn span(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// `── run 40 · 14:02:11 · 6m48s · ok`. `end` is the last observed event of
/// a settled run; the daemon snapshot carries no settlement time.
pub fn run_divider(
    ui: &mut egui::Ui,
    run: &Value,
    number: usize,
    end: Option<u64>,
    detail: Option<&str>,
    machine: Option<&str>,
) {
    let created = run["record"]["created_at"].as_u64();
    let status = run["status"].as_str().unwrap_or("");
    let (word, color) = match status {
        "completed" => ("ok".to_owned(), ui.visuals().weak_text_color()),
        "starting" | "running" => ("running".to_owned(), console_style::ACCENT),
        "waiting" | "blocked" => (
            format!("waiting · {}", detail.unwrap_or("input")),
            console_style::WARN,
        ),
        "failed" | "interrupted" => (status.to_owned(), console_style::ERROR),
        other => (other.to_owned(), ui.visuals().weak_text_color()),
    };
    let current = matches!(status, "waiting" | "blocked");
    let mut parts = vec![format!("run {number}")];
    if let Some(at) = created {
        let second = at / 1000;
        parts.push(format!(
            "{:02}:{:02}:{:02}",
            (second / 3600) % 24,
            (second / 60) % 60,
            second % 60
        ));
        let finished = if crate::model::terminal(run) {
            end
        } else {
            Some(crate::pulse::now())
        };
        if let Some(finished) = finished.filter(|f| *f >= at) {
            parts.push(span(finished - at));
        }
    }
    parts.push(word);
    if let Some(machine) = machine {
        parts.push(machine.to_owned());
    }
    ui.add_space(4.);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(28., 14.), egui::Sense::hover());
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            egui::Stroke::new(1., console_style::EDGE),
        );
        let text = parts.join(" · ");
        let _ = current;
        ui.add(
            egui::Label::new(RichText::new(text).monospace().size(10.5).color(color)).truncate(),
        )
        .on_hover_text("Journal times are UTC");
    });
    ui.add_space(2.);
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
        let heading = line.starts_with('#') && line.trim_start_matches('#').starts_with(' ');
        let line = if heading {
            line.trim_start_matches('#').trim_start()
        } else {
            line
        };
        let line = if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            format!("• {rest}")
        } else {
            line.into()
        };
        let font = egui::TextStyle::Body.resolve(ui.style());
        let strong = egui::Color32::from_gray(if ui.visuals().dark_mode { 245 } else { 10 });
        let normal = ui.visuals().text_color();
        let segments = inline_markdown(&line);
        let format_for = |style: Inline| {
            let mut format = egui::TextFormat::simple(
                if style.code {
                    egui::FontId::monospace(font.size - 1.)
                } else {
                    font.clone()
                },
                if style.bold || heading {
                    strong
                } else {
                    normal
                },
            );
            format.italics = style.italic;
            if style.code {
                format.background = console_style::FIELD;
            }
            format
        };
        // One galley per line keeps wrapping and layout identical to plain
        // text; links are found by character position when clicked.
        let mut job = egui::text::LayoutJob::default();
        let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        let mut chars = 0;
        for segment in &segments {
            match segment {
                Segment::Text(text, style) => {
                    job.append(text, 0., format_for(*style));
                    chars += text.chars().count();
                }
                Segment::Link(title, url) => {
                    let mut format = format_for(Inline::default());
                    format.color = ui.visuals().hyperlink_color;
                    format.underline = egui::Stroke::new(1., ui.visuals().hyperlink_color);
                    job.append(title, 0., format);
                    let count = title.chars().count();
                    links.push((chars..chars + count, url.clone()));
                    chars += count;
                }
            }
        }
        if links.is_empty() {
            ui.add(egui::Label::new(job).wrap().selectable(true));
        } else {
            let (pos, galley, response) = egui::Label::new(job)
                .wrap()
                .selectable(false)
                .sense(egui::Sense::click())
                .layout_in_ui(ui);
            let link_at = |point: egui::Pos2| {
                let index = galley.cursor_from_pos(point - pos).index.0;
                links
                    .iter()
                    .find(|(range, _)| range.contains(&index))
                    .map(|(_, url)| url.clone())
            };
            let hovered = response.hover_pos().and_then(link_at);
            if let Some(url) = &hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                response.clone().on_hover_text(url);
            }
            if response.clicked()
                && let Some(url) = response.interact_pointer_pos().and_then(link_at)
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            ui.painter().galley(pos, galley, ui.visuals().text_color());
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Inline {
    bold: bool,
    italic: bool,
    code: bool,
}

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String, Inline),
    Link(String, String),
}

/// Inline Markdown used by agents: `**bold**`, `*italic*`/`_italic_`,
/// `` `code` `` and `[title](https://…)`. Anything unmatched stays literal.
fn inline_markdown(line: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut style = Inline::default();
    let mut buffer = String::new();
    let flush = |buffer: &mut String, out: &mut Vec<Segment>, style: Inline| {
        if !buffer.is_empty() {
            out.push(Segment::Text(std::mem::take(buffer), style));
        }
    };
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let rest: String = chars[i..].iter().collect();
        if c == '`' {
            if style.code {
                flush(&mut buffer, &mut out, style);
                style.code = false;
                i += 1;
                continue;
            }
            if chars[i + 1..].contains(&'`') {
                flush(&mut buffer, &mut out, style);
                style.code = true;
                i += 1;
                continue;
            }
        } else if style.code {
            buffer.push(c);
            i += 1;
            continue;
        } else if c == '['
            && let Some((title, after)) = rest[1..].split_once("](")
            && !title.contains(']')
            && let Some((url, _)) = after.split_once(')')
            && (url.starts_with("https://") || url.starts_with("http://"))
            && !url.contains(char::is_whitespace)
        {
            flush(&mut buffer, &mut out, style);
            let consumed = 1 + title.chars().count() + 2 + url.chars().count() + 1;
            out.push(Segment::Link(title.replace(['*', '_'], ""), url.to_owned()));
            i += consumed;
            continue;
        } else if let Some(after) = rest.strip_prefix("**") {
            let closing = style.bold || after.contains("**");
            if closing {
                flush(&mut buffer, &mut out, style);
                style.bold = !style.bold;
                i += 2;
                continue;
            }
        } else if c == '*' || c == '_' {
            let prev = i.checked_sub(1).map(|p| chars[p]);
            let next = chars.get(i + 1).copied();
            let opens = !style.italic
                && next.is_some_and(|n| !n.is_whitespace())
                && prev.is_none_or(|p| !p.is_alphanumeric())
                && chars[i + 1..].contains(&c);
            let closes = style.italic
                && prev.is_some_and(|p| !p.is_whitespace())
                && next.is_none_or(|n| !n.is_alphanumeric());
            if opens || closes {
                flush(&mut buffer, &mut out, style);
                style.italic = !style.italic;
                i += 1;
                continue;
            }
        }
        buffer.push(c);
        i += 1;
    }
    flush(&mut buffer, &mut out, style);
    out
}

#[derive(Default)]
struct ContextReading {
    measure: Option<ContextMeasure>,
    measured_at: Option<u64>,
    change: Option<(ContextChange, u64)>,
    current: bool,
}

impl ContextReading {
    fn warning(&self) -> bool {
        self.measure.is_some_and(ContextMeasure::past_warning_line)
    }

    fn recent_consequence(&self) -> Option<(ContextChange, u64)> {
        const RECENT_MS: u64 = 30 * 60 * 1000;
        self.change.filter(|(change, at)| {
            *change != ContextChange::Projection
                && crate::pulse::now()
                    .checked_sub(*at)
                    .is_some_and(|age| age <= RECENT_MS)
        })
    }

    fn mark(&self) -> Option<(egui::Color32, String)> {
        let recent = self.recent_consequence();
        if let Some((ContextChange::Shortage, at)) = recent {
            Some((
                console_style::ERROR,
                format!("Context shortage · {} UTC", utc_minute(at)),
            ))
        } else if self.warning() {
            Some((console_style::WARN, "Past Boxen’s 85% warning line".into()))
        } else {
            recent.map(|(change, at)| {
                (
                    console_style::SECONDARY,
                    format!("{} · {} UTC", change.label(), utc_minute(at)),
                )
            })
        }
    }
}

/// The inspector's reading: `61% of window` with its source and time.
pub fn context_summary(data: &Data, key: &ThreadKey, runs: &[Value]) -> (String, Option<String>) {
    let backend = runs
        .last()
        .and_then(|run| run["record"]["spec"]["backend"].as_str())
        .unwrap_or("unknown");
    let reading = context_reading(data, key, runs);
    let Some(measure) = reading.measure else {
        return ("Unknown".into(), None);
    };
    let text = match measure {
        ContextMeasure::Codex { .. } => measure.label().replace("ctx ", "") + " of window",
        _ => measure.label(),
    };
    let text = if reading.warning() {
        format!("{text} · past 85% line")
    } else {
        text
    };
    let source = reading
        .measured_at
        .map(|at| format!("{backend} · {} UTC", utc_minute(at)))
        .or_else(|| Some(format!("{backend}-reported")));
    (text, source)
}

fn context_reading(data: &Data, key: &ThreadKey, runs: &[Value]) -> ContextReading {
    let Some(run) = runs.last() else {
        return ContextReading::default();
    };
    // Imported run history can still carry measurements from its original host.
    if run["machine"]
        .as_str()
        .is_some_and(|machine| machine != key.peer)
    {
        return ContextReading::default();
    }
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
        current: true,
    }
}

fn context_details(ui: &mut egui::Ui, backend: &str, reading: &ContextReading) {
    ui.strong("Context");
    ui.separator();
    ui.weak("READING");
    if let Some(measure) = reading.measure {
        ui.label(measure.detail());
        if let Some(at) = reading.measured_at {
            let source = match backend {
                "codex" => "Codex reported · latest exchange",
                "claude" => "Claude Code reported · latest exchange",
                "antigravity" => "Antigravity reported · session usage",
                "openrouter" => "Boxen context plan",
                _ => "Run journal",
            };
            ui.weak(format!("{source} · {} UTC", utc_minute(at)));
        }
    } else {
        ui.label("Context reading unknown");
    }
    if backend == "codex" {
        ui.weak("LINE");
        if reading.warning() {
            ui.colored_label(context_warning_color(), "Past Boxen’s 85% warning line");
        } else {
            ui.label("Boxen’s warning line is 85% of the reported window");
        }
        ui.weak("Codex controls compaction and its threshold is unknown.");
    }
    ui.weak("VALIDITY");
    ui.label("Until the next reading");
    ui.weak("A new session, model change, or machine move clears the reading.");
    ui.weak("EVENTS");
    if let Some((change, at)) = reading.change {
        ui.label(format!("{} · {} UTC", change.label(), utc_minute(at)));
    } else if reading.current {
        ui.label("No context event observed");
    } else {
        ui.label("Context events unknown");
    }
    ui.separator();
    ui.weak("WHAT EACH RUNTIME GIVES BOXEN");
    for (runtime, measure, evidence) in [
        (
            "Codex",
            "% of model window",
            "Native compaction when Codex reports it",
        ),
        (
            "Claude Code",
            "Last-turn input tokens",
            "Compaction boundary, trigger, and tokens before when reported",
        ),
        (
            "OpenRouter",
            "KiB of selectable budget",
            "Recorded projections, including masking, and run-stopping shortages",
        ),
        (
            "Antigravity",
            "Session usage when reported",
            "Compaction consequence unknown",
        ),
    ] {
        ui.label(format!("{runtime} · {measure}"));
        ui.weak(evidence);
    }
    ui.weak("Missed events and missing samples stay unknown.");
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
    egui::containers::menu::MenuButton::from_button(
        egui::Button::new(
            RichText::new(format!("· {backend} · {label}"))
                .monospace()
                .size(10.5)
                .weak(),
        )
        .frame(false),
    )
    .ui(ui, |ui| {
        ui.set_max_width(425.);
        context_details(ui, backend, &reading);
    })
    .0
    .on_hover_text("Context reading · click for its source");
    if reading.warning() {
        ui.colored_label(context_warning_color(), "past 85% line")
            .on_hover_text("Boxen's warning line. Codex controls its own compaction.");
    }
    if let Some((change, at)) = reading.recent_consequence() {
        let color = if change == ContextChange::Shortage {
            console_style::ERROR
        } else {
            ui.visuals().weak_text_color()
        };
        ui.colored_label(color, format!("{} {}", change.label(), utc_minute(at)))
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
    if let Some((color, label)) = reading.mark() {
        context_icon(ui, color).on_hover_text(label);
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
/// A short name for one collapsed step: the command or tool line, or the
/// kind of item when the backend gives no shorter description.
fn step_title(row: &Row) -> String {
    match row.label.as_str() {
        "Thinking" => "thinking".into(),
        "Tool" | "Command" => row
            .text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(48).collect())
            .unwrap_or_else(|| row.label.to_lowercase()),
        other => {
            // Backend item types such as `fileChange` or `webSearch`.
            let mut words = String::new();
            for c in other.chars() {
                if c.is_uppercase() && !words.is_empty() {
                    words.push(' ');
                }
                words.extend(c.to_lowercase());
            }
            words
        }
    }
}
pub fn activity_rows(ui: &mut egui::Ui, rows: &[Row]) {
    let ordinary: Vec<_> = rows
        .iter()
        // Status rows repeat what the run divider already says.
        .filter(|row| !(incident(row) || row.label == "Status"))
        .collect();
    if let Some(first) = ordinary.first() {
        let count = ordinary.len();
        let mut titles: Vec<String> = ordinary.iter().map(|row| step_title(row)).collect();
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
                    let area = ui
                        .allocate_ui_with_layout(
                            egui::vec2(width, crate::console_style::MIN_TARGET),
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
                        ;
                    // The step title's whole row toggles the group, not only its glyphs.
                    let row = area.response.interact(egui::Sense::click());
                    clicked = area.inner.clicked() || row.clicked();
                    row.on_hover_text(&title);
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
mod inline_tests {
    use super::{Inline, Segment, inline_markdown};

    #[test]
    fn inline_markdown_handles_mid_line_links_italics_and_code() {
        let segments =
            inline_markdown("See [Gergle, *Visual*](https://example.com/a) and *this* `x*y` here");
        assert_eq!(
            segments,
            vec![
                Segment::Text("See ".into(), Inline::default()),
                Segment::Link("Gergle, Visual".into(), "https://example.com/a".into()),
                Segment::Text(" and ".into(), Inline::default()),
                Segment::Text(
                    "this".into(),
                    Inline {
                        italic: true,
                        ..Default::default()
                    }
                ),
                Segment::Text(" ".into(), Inline::default()),
                Segment::Text(
                    "x*y".into(),
                    Inline {
                        code: true,
                        ..Default::default()
                    }
                ),
                Segment::Text(" here".into(), Inline::default()),
            ]
        );
        assert_eq!(
            inline_markdown("a * b and snake_case_name"),
            vec![Segment::Text(
                "a * b and snake_case_name".into(),
                Inline::default()
            )]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_uses_the_current_machine_and_keeps_the_warning_below_85_percent() {
        let key = ThreadKey {
            peer: "fixture-machine".into(),
            id: "thread".into(),
        };
        let mut data = Data::default();
        let mut transcript = Transcript::default();
        transcript
            .apply(&serde_json::json!({
                "seq": 1, "at": 4000, "kind":"backend",
                "data":{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{
                    "last":{"totalTokens":849},"modelContextWindow":1000
                }}}
            }))
            .unwrap();
        data.transcripts
            .insert(crate::client::run_key(&key.peer, "run"), transcript);
        let mut run = serde_json::json!({
            "last_seq": 1, "machine":key.peer,"record":{"spec":{"id":"run","backend":"codex"}}
        });
        let reading = context_reading(&data, &key, std::slice::from_ref(&run));
        assert_eq!(reading.measure.unwrap().label(), "ctx 84%");
        assert!(!reading.warning());
        run["machine"] = serde_json::json!("old-fixture-machine");
        let stale = context_reading(&data, &key, &[run]);
        assert!(stale.measure.is_none() && stale.change.is_none() && !stale.current);
    }

    #[test]
    fn context_mark_prioritizes_shortage_and_skips_routine_projections() {
        let now = crate::pulse::now();
        let mut reading = ContextReading {
            change: Some((ContextChange::Projection, now)),
            ..Default::default()
        };
        assert!(reading.mark().is_none());
        reading.change = Some((ContextChange::Compaction, now));
        assert_eq!(reading.mark().unwrap().0, console_style::SECONDARY);
        reading.measure = Some(ContextMeasure::Codex {
            tokens: 88,
            window: 100,
        });
        assert_eq!(reading.mark().unwrap().0, console_style::WARN);
        reading.change = Some((ContextChange::Shortage, now));
        assert_eq!(reading.mark().unwrap().0, console_style::ERROR);
    }

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
                        hint: None,
                        placeholder: "Message…".into(),
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
