//! Messages and Activity are two projections of one conversation. The composer
//! sits outside both projections, so switching views does not change the draft
//! or its destination.
use crate::{
    Action, Data,
    attachments::{self, FileAction, FileRef, Preview, Target},
    model::{Destination, Row, ThreadKey},
};
use eframe::egui::{self, RichText};
use std::collections::HashMap;

#[derive(Default)]
pub struct View {
    visible: Option<(String, f32)>,
    anchor: Option<String>,
    latest: bool,
    at_latest: bool,
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
    // The frame's modifier snapshot can already be clear by the time
    // Shift+Enter is processed. Match the modifiers on the key event itself.
    let enter = ui.memory(|m| m.has_focus(editor_id))
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
        });
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
pub fn bubble(
    ui: &mut egui::Ui,
    outgoing: bool,
    text: &str,
    rich: bool,
    body: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let width = ui.available_width();
    let max = (width * 0.82).clamp(28., 520.);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text_width = ui.fonts_mut(|f| {
        text.lines()
            .map(|line| {
                f.layout_no_wrap(line.into(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            })
            .fold(0., f32::max)
    });
    // Lay out the message at its measured width first, then place that block
    // on its sender's side. Its contents always flow from the left edge.
    let bubble_width = (text_width + 20.).max(100.0_f32.min(max)).min(max);
    ui.horizontal(|ui| {
        if outgoing {
            ui.add_space((width - bubble_width - ui.spacing().item_spacing.x).max(0.));
        }
        ui.allocate_ui_with_layout(
            egui::vec2(bubble_width, 0.),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(bubble_width);
                egui::Frame::group(ui.style())
                    .show(ui, |ui| {
                        ui.set_min_width((bubble_width - 12.).max(0.));
                        if rich {
                            markdown(ui, text);
                        } else if !text.is_empty() {
                            ui.add(egui::Label::new(text).wrap().selectable(true));
                        }
                        body(ui);
                    })
                    .response
            },
        )
        .inner
    })
    .inner
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
pub fn is_message(row: &Row) -> bool {
    matches!(row.label.as_str(), "You" | "You · steering" | "Agent")
}
pub fn incident(row: &Row) -> bool {
    row.tool.as_ref().is_some_and(|t| t.failed)
        || row.label == "Incident"
        || row.label == "Backend input requested"
        || row.label == "Status"
            && ["failed", "interrupted", "cancelled"]
                .iter()
                .any(|s| row.text.starts_with(s))
}
pub fn activity_rows(ui: &mut egui::Ui, rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    let ordinary = rows.iter().filter(|r| !incident(r)).count();
    if ordinary != 0 {
        egui::CollapsingHeader::new(format!(
            "{ordinary} activity {}",
            if ordinary == 1 { "event" } else { "events" }
        ))
        .id_salt(&rows[0].id)
        .show(ui, |ui| {
            for row in rows {
                if !incident(row) {
                    activity_row(ui, row);
                }
            }
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
