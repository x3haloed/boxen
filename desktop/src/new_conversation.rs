use crate::{
    Action, Data, Desktop,
    attachments::{self, Target},
    client::{self, Pending},
    conversation,
    model::{Destination, ThreadKey},
    model_catalogs::{Catalogs, Runtime},
    new_id, runtimes,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    machine: Option<String>,
    cwd: String,
    backend: String,
    model: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            machine: None,
            cwd: String::new(),
            backend: "codex".into(),
            model: String::new(),
        }
    }
}

impl Settings {
    fn destination(
        &self,
        destinations: &[Destination],
        local: &Destination,
    ) -> Option<Destination> {
        match &self.machine {
            Some(key) => destinations.iter().find(|d| &d.key == key).cloned(),
            None if !local.key.is_empty() => Some(local.clone()),
            None => None,
        }
    }
}

const MESSAGE_LIMIT: usize = 256 * 1024;

pub fn dialog(ctx: &egui::Context, data: &mut Data, actions: &mut Vec<Action>) {
    let pending = data.saved.new_thread_request.as_deref();
    let sending = pending.is_some_and(|id| data.sending.contains(id));
    let editable = pending.is_none();
    let local = conversation::local(data);
    let working = data
        .preparing
        .get(&Target::NewThread)
        .is_some_and(|p| p.working);
    let modal = egui::Modal::new(egui::Id::new("new-thread")).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 40.).clamp(180., 424.));
        ui.heading("New Thread");
        ui.separator();

        let editor_id = ui.make_persistent_id("first-message");
        let enter = editable && conversation::send_on_enter(ui, editor_id);
        let editor = egui::ScrollArea::vertical()
            .max_height(104.)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut data.saved.new_thread_text)
                        .id(editor_id)
                        .interactive(editable)
                        .hint_text("First message…")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .return_key(egui::KeyboardShortcut::new(
                            egui::Modifiers::SHIFT,
                            egui::Key::Enter,
                        )),
                )
            })
            .inner;
        if std::mem::take(&mut data.focus_new_conversation) && editable {
            editor.request_focus();
        }
        ui.horizontal_wrapped(|ui| {
            let attach = ui
                .add_enabled(
                    editable
                        && !working
                        && data.saved.new_thread_files.len() < attachments::MAX_FILES,
                    egui::Button::new(if working { "Preparing…" } else { "Attach" }),
                )
                .clicked();
            attachments::add_files(
                ui,
                &editor,
                Target::NewThread,
                data.saved.new_thread_files.len(),
                editable && !working,
                attach,
                actions,
            );
            conversation::chips(
                ui,
                &mut data.saved.new_thread_files,
                &local,
                editable,
                &mut data.file_previews,
                actions,
            );
        });
        if let Some(preparation) = data.preparing.get_mut(&Target::NewThread) {
            for error in &preparation.errors {
                ui.label(error);
            }
            if !preparation.errors.is_empty() && ui.small_button("Dismiss").clicked() {
                preparation.errors.clear();
            }
        }
        if data.show_new_thread_settings {
            ui.add_enabled_ui(editable, |ui| {
                settings(
                    ui,
                    &mut data.saved.new_thread_settings,
                    &data.saved.destinations,
                    &local,
                    &data.model_catalogs,
                    &data.runtimes,
                    actions,
                )
            });
        }
        let settings = &data.saved.new_thread_settings;
        let destination = settings.destination(&data.saved.destinations, &local);
        let runtime_valid = destination
            .as_ref()
            .is_some_and(|destination| data.runtimes.available(destination, &settings.backend));
        let model_valid = data.model_catalogs.can_submit(
            destination.as_ref(),
            &settings.backend,
            &settings.model,
        );
        if editable && let Some(destination) = &destination {
            data.runtimes.render_status(ui, destination, actions);
        }
        if sending {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Waiting for the daemon to queue the run…");
            });
        } else if let Some(issue) = &data.new_conversation_issue {
            ui.label(issue);
        } else if !editable {
            ui.label("Queue status is unknown. Retrying uses the same request.");
        } else if destination.is_none() {
            ui.label("Waiting for the selected machine.");
        } else if !runtime_valid
            && destination
                .as_ref()
                .is_some_and(|d| data.runtimes.catalog(d).is_some())
        {
            ui.label("Choose an available runtime in Run settings.");
        } else if runtime_valid && !model_valid {
            ui.label("Choose an available model in Run settings.");
        }
        let text = &data.saved.new_thread_text;
        if text.len() > MESSAGE_LIMIT {
            ui.label("Text limit: 256 KiB");
        }
        let can_start = editable
            && !working
            && destination.is_some()
            && runtime_valid
            && model_valid
            && text.len() <= MESSAGE_LIMIT
            && (!text.trim().is_empty() || !data.saved.new_thread_files.is_empty());
        ui.horizontal(|ui| {
            let settings_label = if data.show_new_thread_settings {
                "Hide settings"
            } else {
                "Run settings"
            };
            if ui
                .add_enabled(editable, egui::Button::new(settings_label))
                .clicked()
            {
                data.show_new_thread_settings = !data.show_new_thread_settings;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if editable {
                    let label = if data.new_conversation_issue.is_some() {
                        "Try again"
                    } else {
                        "Start thread"
                    };
                    if ui
                        .add_enabled(can_start, egui::Button::new(label))
                        .clicked()
                        || (can_start && enter)
                    {
                        actions.push(Action::StartConversation);
                    }
                } else if !sending
                    && ui.button("Retry same request").clicked()
                    && let Some(index) = data
                        .saved
                        .pending
                        .iter()
                        .position(|p| p.spec["id"].as_str() == pending)
                {
                    actions.push(Action::Retry(index));
                }
                !sending
                    && ui
                        .button(if editable { "Cancel" } else { "Close" })
                        .clicked()
            })
            .inner
        })
        .inner
    });
    if !sending && (modal.inner || modal.should_close()) {
        data.new_thread_dialog = false;
        actions.push(Action::SaveDesktop);
    }
}

fn settings(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    destinations: &[Destination],
    local: &Destination,
    catalogs: &Catalogs,
    runtimes: &runtimes::Catalogs,
    actions: &mut Vec<Action>,
) {
    let before_machine = settings.machine.clone();
    let before_backend = settings.backend.clone();
    let machine_label = match &settings.machine {
        None => "This machine (default)",
        Some(key) => destinations
            .iter()
            .find(|d| &d.key == key)
            .map_or("Unavailable machine", |d| d.label.as_str()),
    };
    let width = (ui.available_width() - 144.).max(90.);
    egui::Grid::new("new-thread-run-settings")
        .min_col_width(124.)
        .show(ui, |ui| {
            ui.label("Machine");
            egui::ComboBox::from_id_salt("machine")
                .width(width)
                .selected_text(machine_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut settings.machine, None, "This machine (default)");
                    for d in destinations.iter().filter(|d| d.key != local.key) {
                        ui.selectable_value(&mut settings.machine, Some(d.key.clone()), &d.label);
                    }
                });
            ui.end_row();

            ui.label("Working directory");
            ui.add(
                egui::TextEdit::singleline(&mut settings.cwd)
                    .desired_width(width)
                    .hint_text("Automatic working area"),
            );
            ui.end_row();

            ui.label("Runtime");
            let destination = settings.destination(destinations, local);
            let catalog = destination.as_ref().and_then(|d| runtimes.catalog(d));
            let available =
                catalog.is_some_and(|catalog| catalog.available().any(|id| id == settings.backend));
            egui::ComboBox::from_id_salt("runtime")
                .width(width)
                .selected_text(if available {
                    runtimes::label(&settings.backend)
                } else {
                    "Choose runtime"
                })
                .show_ui(ui, |ui| {
                    if let Some(catalog) = catalog {
                        let mut options = catalog.available().peekable();
                        if options.peek().is_none() {
                            ui.label("No runtimes configured");
                        }
                        for id in options {
                            ui.selectable_value(
                                &mut settings.backend,
                                id.into(),
                                runtimes::label(id),
                            );
                        }
                    } else {
                        ui.label("Runtime catalog unavailable");
                    }
                });
            ui.end_row();

            if settings.backend != before_backend || settings.machine != before_machine {
                settings.model.clear();
            }
            ui.label("Model");
            if !catalog.is_some_and(|catalog| catalog.available().any(|id| id == settings.backend))
            {
                ui.add_enabled(false, egui::Label::new("Choose a runtime"));
            } else if let Some(runtime) = Runtime::for_backend(&settings.backend) {
                ui.vertical(|ui| {
                    catalogs.picker(
                        ui,
                        width,
                        runtime,
                        destination,
                        &mut settings.model,
                        actions,
                    );
                });
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut settings.model)
                        .desired_width(width)
                        .hint_text("Default model"),
                );
            }
            ui.end_row();
        });
}

impl Desktop {
    pub(super) fn start_conversation(&mut self) {
        if self.data.saved.new_thread_request.is_some() {
            return;
        }
        let settings = &self.data.saved.new_thread_settings;
        let Some(destination) = settings.destination(
            &self.data.saved.destinations,
            &conversation::local(&self.data),
        ) else {
            return;
        };
        if !self
            .data
            .runtimes
            .available(&destination, &settings.backend)
            || !self.data.model_catalogs.can_submit(
                Some(&destination),
                &settings.backend,
                &settings.model,
            )
        {
            return;
        }
        let id = new_id();
        let spec = json!({
            "id": id, "thread_id": id, "backend": settings.backend,
            "cwd": settings.cwd, "model": settings.model, "prompt": self.data.saved.new_thread_text,
            "attachments": self.data.saved.new_thread_files,
        });
        self.data.saved.new_thread_request = Some(id);
        self.data.new_conversation_issue = None;
        self.submit(Pending {
            destination,
            spec,
            after: None,
            refused_update: None,
        });
    }

    pub(super) fn is_new_thread_request(&self, id: &str) -> bool {
        self.data.saved.new_thread_request.as_deref() == Some(id)
    }

    pub(super) fn open_queued_thread(&mut self, key: ThreadKey) {
        let focus = self.data.new_thread_dialog;
        self.data.saved.new_thread_request = None;
        self.data.saved.new_thread_text.clear();
        self.data.saved.new_thread_files.clear();
        self.data.saved.new_thread_settings = Settings::default();
        self.data.new_thread_dialog = false;
        self.data.show_new_thread_settings = false;
        self.data.new_conversation_issue = None;
        if focus {
            conversation::switch(&mut self.data, &key, false, None);
            self.action(Action::Open(key, true));
        }
    }

    pub(super) fn new_thread_failed(&mut self, id: &str, error: client::SubmitError) {
        if error.safe_to_edit {
            self.data.saved.new_thread_request = None;
            self.data.saved.pending.retain(|p| p.spec["id"] != id);
            self.data.new_conversation_issue = Some(error.detail);
            self.data.focus_new_conversation = self.data.new_thread_dialog;
            self.persist();
        } else {
            self.data.new_conversation_issue = Some(format!(
                "Queue status is unknown. Retrying uses the same request. {}",
                error.detail
            ));
        }
    }
}
