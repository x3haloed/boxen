use crate::{Action, Desktop, model::Destination};
use eframe::egui;
use serde::Deserialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Runtime {
    Codex,
    Antigravity,
    Claude,
}

impl Runtime {
    pub fn for_backend(backend: &str) -> Option<Self> {
        match backend {
            "codex" => Some(Self::Codex),
            "antigravity" => Some(Self::Antigravity),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }

    pub fn operation(self) -> &'static str {
        match self {
            Self::Codex => "codex_models",
            Self::Antigravity => "antigravity_models",
            Self::Claude => "claude_models",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub model: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub models: Vec<Model>,
}

enum Lookup {
    Loading(u64),
    Ready(Catalog, Instant),
    Failed(String),
}

#[derive(Default)]
pub struct Catalogs {
    machines: HashMap<String, HashMap<Runtime, (String, Lookup)>>,
    serial: u64,
}

impl Catalogs {
    pub fn clear(&mut self) {
        self.machines.clear();
    }

    fn lookup(&self, runtime: Runtime, destination: &Destination) -> Option<&Lookup> {
        let (address, lookup) = self.machines.get(&destination.key)?.get(&runtime)?;
        (address == &destination.address).then_some(lookup)
    }

    fn store(&mut self, runtime: Runtime, destination: Destination, lookup: Lookup) {
        self.machines
            .entry(destination.key)
            .or_default()
            .insert(runtime, (destination.address, lookup));
    }

    fn begin(&mut self, runtime: Runtime, destination: &Destination) -> Option<u64> {
        if matches!(self.lookup(runtime, destination), Some(Lookup::Loading(_))) {
            return None;
        }
        self.serial += 1;
        self.store(runtime, destination.clone(), Lookup::Loading(self.serial));
        Some(self.serial)
    }

    pub fn received(
        &mut self,
        runtime: Runtime,
        destination: Destination,
        serial: u64,
        result: Result<Catalog, String>,
    ) {
        if matches!(self.lookup(runtime, &destination), Some(Lookup::Loading(active)) if *active == serial)
        {
            let lookup = match result {
                Ok(catalog) => Lookup::Ready(catalog, Instant::now()),
                Err(error) => Lookup::Failed(error),
            };
            self.store(runtime, destination, lookup);
        }
    }

    pub fn can_submit(
        &self,
        destination: Option<&Destination>,
        backend: &str,
        model: &str,
    ) -> bool {
        let Some(runtime) = Runtime::for_backend(backend) else {
            return true;
        };
        model.is_empty() || destination.is_some_and(|d| {
            matches!(self.lookup(runtime, d), Some(Lookup::Ready(catalog, _)) if catalog.models.iter().any(|item| item.model == model))
        })
    }

    pub fn picker(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        runtime: Runtime,
        destination: Option<Destination>,
        model: &mut String,
        actions: &mut Vec<Action>,
    ) -> bool {
        let lookup = destination.as_ref().and_then(|d| self.lookup(runtime, d));
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
        egui::ComboBox::from_id_salt(("model-catalog", runtime))
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
                None => actions.push(Action::LoadModelCatalog(runtime, destination)),
                Some(Lookup::Loading(_)) => {
                    ui.spinner().on_hover_text("Loading models");
                }
                Some(Lookup::Failed(error)) => {
                    if ui
                        .small_button("Retry model list")
                        .on_hover_text(error)
                        .clicked()
                    {
                        actions.push(Action::LoadModelCatalog(runtime, destination));
                    }
                }
                Some(Lookup::Ready(_, fetched)) if fetched.elapsed() >= Duration::from_secs(60) => {
                    actions.push(Action::LoadModelCatalog(runtime, destination));
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
    pub fn load_model_catalog(&mut self, runtime: Runtime, destination: Destination) {
        if let Some(client) = &self.client
            && let Some(serial) = self.data.model_catalogs.begin(runtime, &destination)
        {
            client.model_catalog(runtime, destination, serial, self.bus.publisher());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_choices_are_scoped_to_runtime_machine_and_connection() {
        let mut catalogs = Catalogs::default();
        let target = Destination {
            key: "fixture-local".into(),
            label: "Fixture local".into(),
            address: String::new(),
        };
        let other = Destination {
            key: "fixture-remote".into(),
            label: "Fixture remote".into(),
            address: "127.0.0.1:42426".into(),
        };
        let model = || {
            Ok(Catalog {
                models: vec![Model {
                    model: "fixture-model".into(),
                    label: "Fixture model".into(),
                }],
            })
        };
        let old = catalogs.begin(Runtime::Antigravity, &target).unwrap();
        catalogs.clear();
        let current = catalogs.begin(Runtime::Antigravity, &target).unwrap();
        catalogs.received(Runtime::Antigravity, target.clone(), old, model());
        assert!(!catalogs.can_submit(Some(&target), "antigravity", "fixture-model"));
        assert!(catalogs.can_submit(Some(&target), "antigravity", ""));
        catalogs.received(Runtime::Antigravity, target.clone(), current, model());
        assert!(catalogs.can_submit(Some(&target), "antigravity", "fixture-model"));
        assert!(!catalogs.can_submit(Some(&target), "codex", "fixture-model"));
        assert!(!catalogs.can_submit(Some(&other), "antigravity", "fixture-model"));
    }
}
