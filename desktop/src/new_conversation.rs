use crate::{
    Action, Data, Desktop,
    attachments::Target,
    client::Pending,
    conversation::{self, Composer},
    model::{Destination, Pane, ThreadKey},
    new_id,
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
    fn destination(&self, data: &Data) -> Option<Destination> {
        match &self.machine {
            Some(key) => data
                .saved
                .destinations
                .iter()
                .find(|d| &d.key == key)
                .cloned(),
            None if !data.local_key.is_empty() => Some(conversation::local(data)),
            None => None,
        }
    }
}

pub fn panel(ui: &mut egui::Ui, data: &mut Data, actions: &mut Vec<Action>) {
    ui.heading("New Conversation");
    if ui.button("Run settings").clicked() {
        data.editing_run_settings = Some(data.saved.new_thread_settings.clone());
    }
    let blocked = data.saved.new_thread_settings.destination(data).is_none();
    if blocked {
        ui.label("Waiting for the selected machine.");
    }
    let mut text = std::mem::take(&mut data.saved.new_thread_text);
    let mut files = std::mem::take(&mut data.saved.new_thread_files);
    let send = conversation::composer(
        ui,
        data,
        Composer {
            text: &mut text,
            files: &mut files,
            target: Target::NewThread,
            label: "Send",
            limit: 256 * 1024,
            blocked,
        },
        actions,
    )
    .send;
    data.saved.new_thread_text = text;
    data.saved.new_thread_files = files;
    if send {
        actions.push(Action::StartConversation);
    }
    if let Some(mut settings) = data.editing_run_settings.take() {
        let modal =
            egui::Modal::new(egui::Id::new("new-conversation-settings")).show(ui.ctx(), |ui| {
                ui.heading("Run settings");
                egui::ComboBox::from_label("Machine")
                    .selected_text(settings.destination(data).map(|d| d.label).unwrap_or_else(
                        || {
                            if settings.machine.is_some() {
                                "Unavailable machine".into()
                            } else {
                                "This machine".into()
                            }
                        },
                    ))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.machine, None, "This machine (default)");
                        for d in data
                            .saved
                            .destinations
                            .iter()
                            .filter(|d| d.key != data.local_key)
                        {
                            ui.selectable_value(
                                &mut settings.machine,
                                Some(d.key.clone()),
                                &d.label,
                            );
                        }
                    });
                ui.label("Working directory");
                ui.add(
                    egui::TextEdit::singleline(&mut settings.cwd)
                        .hint_text("Automatic working area"),
                );
                egui::ComboBox::from_label("Runtime")
                    .selected_text(&settings.backend)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.backend, "codex".into(), "Codex");
                        ui.selectable_value(
                            &mut settings.backend,
                            "openrouter".into(),
                            "OpenRouter",
                        );
                    });
                ui.label("Model");
                ui.add(egui::TextEdit::singleline(&mut settings.model).hint_text("Default model"));
                ui.horizontal(|ui| {
                    let cancel = ui.button("Cancel").clicked();
                    let done = ui.button("Done").clicked();
                    (cancel, done)
                })
                .inner
            });
        if modal.inner.1 {
            data.saved.new_thread_settings = settings;
        } else if !modal.inner.0 && !modal.should_close() {
            data.editing_run_settings = Some(settings);
        }
    }
}

impl Desktop {
    pub(super) fn start_conversation(&mut self) {
        let settings = &self.data.saved.new_thread_settings;
        let Some(destination) = settings.destination(&self.data) else {
            return;
        };
        let id = new_id();
        let key = ThreadKey {
            peer: destination.key.clone(),
            id: id.clone(),
        };
        let prompt = std::mem::take(&mut self.data.saved.new_thread_text);
        let files = std::mem::take(&mut self.data.saved.new_thread_files);
        let spec = json!({
            "id": id, "thread_id": id, "backend": settings.backend,
            "cwd": settings.cwd, "model": settings.model, "prompt": prompt, "attachments": files,
        });
        self.data.saved.new_thread_settings = Settings::default();
        self.data.saved.drafts.push((key.clone(), prompt));
        self.data.saved.thread_files.push((key.clone(), files));
        let workspace = &mut self.data.saved.workspace;
        if let Some(path) = workspace.dock.find_tab(&Pane::NewConversation) {
            workspace.dock[path.surface][path.node].tabs_mut().unwrap()[path.tab.0] =
                Pane::Thread(key.clone());
        } else {
            workspace.open(key.clone(), true);
        }
        self.data.selected_thread = Some(key);
        self.submit(Pending {
            destination,
            spec,
            after: None,
        });
    }
}
