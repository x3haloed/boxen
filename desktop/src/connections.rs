use crate::model::Destination;
use eframe::egui;
use std::collections::HashMap;

#[derive(Default)]
pub struct Dialog {
    pub open: bool,
    pub local_label: Option<String>,
    rows: HashMap<String, Row>,
}

#[derive(Clone)]
pub struct Edit {
    pub key: String,
    pub name: Option<(String, String)>,
    pub address: Option<String>,
}

#[derive(Default)]
struct Row {
    original_name: String,
    name: String,
    original_address: String,
    address: String,
    working: bool,
    notice: Option<Result<(), String>>,
}

impl Dialog {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        local: &str,
        destinations: &[Destination],
        ready: bool,
    ) -> Option<Edit> {
        if !self.open {
            self.rows.retain(|_, row| row.working);
            return None;
        }

        let mut edit = None;
        egui::Window::new("Machine connections")
            .default_width(530.)
            .open(&mut self.open)
            .show(ctx, |ui| {
                if destinations.is_empty() {
                    ui.label("Waiting for the local daemon…");
                }
                egui::ScrollArea::vertical()
                    .max_height(480.)
                    .show(ui, |ui| {
                        for (index, dest) in destinations.iter().enumerate() {
                            let is_local = dest.key == local;
                            let name = if is_local {
                                self.local_label.as_deref()
                            } else {
                                Some(dest.label.as_str())
                            };
                            if index != 0 {
                                ui.separator();
                            }
                            ui.push_id(&dest.key, |ui| {
                                if is_local {
                                    ui.strong("This machine");
                                } else {
                                    ui.strong(&dest.label);
                                }
                                let row = self.rows.entry(dest.key.clone()).or_default();
                                if !row.working {
                                    if row.name == row.original_name {
                                        row.name = name.unwrap_or_default().to_owned();
                                        row.original_name = row.name.clone();
                                    }
                                    if row.address == row.original_address {
                                        row.address = dest.address.clone();
                                        row.original_address = row.address.clone();
                                    }
                                }

                                ui.horizontal_top(|ui| {
                                    ui.vertical(|ui| {
                                        let label = ui.label("Display name");
                                        ui.add_enabled(
                                            name.is_some() && ready && !row.working,
                                            egui::TextEdit::singleline(&mut row.name)
                                                .desired_width(205.),
                                        )
                                        .labelled_by(label.id);
                                        if name.is_some() {
                                            ui.weak("Shown to paired machines");
                                        } else {
                                            ui.weak("The daemon has no display name available");
                                        }
                                    });
                                    if !is_local {
                                        ui.vertical(|ui| {
                                            let label = ui.label("Peer address");
                                            ui.add_enabled(
                                                ready && !row.working,
                                                egui::TextEdit::singleline(&mut row.address)
                                                    .desired_width(205.)
                                                    .hint_text("host:port"),
                                            )
                                            .labelled_by(label.id);
                                            ui.weak("Blank uses the advertised address");
                                        });
                                    }
                                });
                                let name = row.name.trim();
                                let address = row.address.trim();
                                let name_changed = name != row.original_name;
                                let address_changed = !is_local && address != row.original_address;
                                let dirty = row.name != row.original_name || address_changed;
                                let invalid = if row.name != row.original_name
                                    && (name.is_empty()
                                        || name.len() > 100
                                        || name.chars().any(char::is_control))
                                {
                                    Some("Display name needs 1–100 UTF-8 bytes of printable text.")
                                } else if address_changed && !valid_address(address) {
                                    Some("Enter a hostname or IP and port, or clear the override.")
                                } else {
                                    None
                                };

                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            ready
                                                && !row.working
                                                && invalid.is_none()
                                                && (name_changed || address_changed),
                                            egui::Button::new("Save"),
                                        )
                                        .clicked()
                                    {
                                        row.working = true;
                                        row.notice = None;
                                        edit = Some(Edit {
                                            key: dest.key.clone(),
                                            name: name_changed.then(|| {
                                                (row.original_name.clone(), name.to_owned())
                                            }),
                                            address: address_changed.then(|| address.to_owned()),
                                        });
                                    }
                                    if row.working {
                                        ui.weak("Saving…");
                                    } else if let Some(message) = invalid {
                                        ui.weak(message);
                                    } else if let Some(notice) = &row.notice {
                                        match notice {
                                            Ok(()) if !dirty => {
                                                ui.weak("Saved");
                                            }
                                            Err(error) => {
                                                ui.label(error);
                                            }
                                            _ => {}
                                        }
                                    }
                                });
                                egui::CollapsingHeader::new("Details").show(ui, |ui| {
                                    ui.label("Public key");
                                    ui.add(egui::Label::new(&dest.key).selectable(true).wrap());
                                });
                            });
                        }
                    });
            });
        edit
    }

    pub fn finished(
        &mut self,
        edit: &Edit,
        accepted_name: Option<&str>,
        result: Result<(), String>,
    ) {
        if let Some(row) = self.rows.get_mut(&edit.key) {
            row.working = false;
            if let Some(name) = accepted_name {
                row.name = name.into();
                row.original_name = row.name.clone();
            }
            if result.is_ok()
                && let Some(address) = &edit.address
            {
                row.address = address.clone();
                row.original_address = address.clone();
            }
            row.notice = Some(result);
        }
    }
}

fn valid_address(address: &str) -> bool {
    address.is_empty()
        || (address.len() <= 256
            && !address.chars().any(|c| {
                c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '@' | '?' | '#')
            })
            && address.rsplit_once(':').is_some_and(|(host, port)| {
                !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0)
            }))
}
