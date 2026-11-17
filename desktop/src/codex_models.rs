use crate::{Action, Desktop, model::Destination};
use eframe::egui;
use serde::Deserialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Debug, Deserialize)]
pub struct Model {
    pub model: String,
    pub label: String,
}

#[derive(Deserialize)]
pub struct Catalog {
    pub models: Vec<Model>,
}

enum Lookup {
    Loading,
    Ready(Catalog, Instant),
    Failed(String),
}

#[derive(Default)]
pub struct Catalogs(HashMap<String, Lookup>);

impl Catalogs {
    pub fn received(&mut self, machine: String, result: Result<Catalog, String>) {
        let lookup = match result {
            Ok(catalog) => Lookup::Ready(catalog, Instant::now()),
            Err(error) => Lookup::Failed(error),
        };
        self.0.insert(machine, lookup);
    }

    pub fn can_submit(
        &self,
        destination: Option<&Destination>,
        backend: &str,
        model: &str,
    ) -> bool {
        backend != "codex" || model.is_empty() || destination.is_some_and(|d| {
            matches!(self.0.get(&d.key), Some(Lookup::Ready(catalog, _)) if catalog.models.iter().any(|item| item.model == model))
        })
    }

    pub fn picker(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        destination: Option<Destination>,
        model: &mut String,
        actions: &mut Vec<Action>,
    ) -> bool {
        let lookup = destination.as_ref().and_then(|d| self.0.get(&d.key));
        let models = match lookup {
            Some(Lookup::Ready(catalog, _)) => catalog.models.as_slice(),
            _ => &[],
        };
        let selected = models.iter().find(|item| item.model == *model);
        let label = if model.is_empty() {
            "Default model"
        } else {
            selected.map_or(model.as_str(), |item| item.label.as_str())
        };
        egui::ComboBox::from_id_salt("codex-model")
            .width(width)
            .selected_text(label)
            .show_ui(ui, |ui| {
                ui.selectable_value(model, String::new(), "Default model");
                for item in models {
                    ui.selectable_value(model, item.model.clone(), &item.label)
                        .on_hover_text(&item.model);
                }
                if models.is_empty() {
                    let status = match lookup {
                        Some(Lookup::Failed(_)) => "Model list unavailable",
                        Some(Lookup::Ready(_, _)) => "No models listed",
                        _ if destination.is_none() => "Waiting for the selected machine",
                        _ => "Loading models…",
                    };
                    ui.label(status);
                }
            });

        if let Some(destination) = destination {
            match lookup {
                None => actions.push(Action::LoadCodexModels(destination)),
                Some(Lookup::Loading) => {
                    ui.spinner().on_hover_text("Loading Codex models");
                }
                Some(Lookup::Failed(error)) => {
                    if ui
                        .small_button("Retry model list")
                        .on_hover_text(error)
                        .clicked()
                    {
                        actions.push(Action::LoadCodexModels(destination));
                    }
                }
                Some(Lookup::Ready(_, fetched)) if fetched.elapsed() >= Duration::from_secs(60) => {
                    actions.push(Action::LoadCodexModels(destination));
                }
                _ => {}
            }
        }
        let valid = model.is_empty() || models.iter().any(|item| item.model == *model);
        if !valid && matches!(lookup, Some(Lookup::Ready(_, _))) {
            ui.label("This model is unavailable on the selected machine.");
        }
        valid
    }
}

impl Desktop {
    pub fn load_codex_models(&mut self, destination: Destination) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if matches!(
            self.data.codex_models.0.get(&destination.key),
            Some(Lookup::Loading)
        ) {
            return;
        }
        self.data
            .codex_models
            .0
            .insert(destination.key.clone(), Lookup::Loading);
        client.codex_models(destination, self.bus.publisher());
    }
}
