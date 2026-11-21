use crate::{Action, Desktop, model::Destination};
use eframe::egui;
use serde::Deserialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
pub struct Runtime {
    pub id: String,
    pub configured: bool,
}

#[derive(Deserialize)]
pub struct Catalog {
    backends: Vec<Runtime>,
}

impl Catalog {
    pub fn available(&self) -> impl Iterator<Item = &str> {
        self.backends
            .iter()
            .filter(|backend| backend.configured)
            .map(|backend| backend.id.as_str())
    }
}

pub fn label(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "openrouter" => "OpenRouter",
        "antigravity" => "Antigravity",
        id => id,
    }
}

enum Lookup {
    Loading(u64),
    Ready(Catalog, Instant),
    Failed(String),
}

#[derive(Default)]
pub struct Catalogs {
    lookups: HashMap<String, (String, Lookup)>,
    serial: u64,
}

impl Catalogs {
    pub fn clear(&mut self) {
        self.lookups.clear();
    }

    fn lookup(&self, destination: &Destination) -> Option<&Lookup> {
        let (address, lookup) = self.lookups.get(&destination.key)?;
        (address == &destination.address).then_some(lookup)
    }

    fn begin(&mut self, destination: &Destination) -> Option<u64> {
        if matches!(self.lookup(destination), Some(Lookup::Loading(_))) {
            return None;
        }
        self.serial += 1;
        self.lookups.insert(
            destination.key.clone(),
            (destination.address.clone(), Lookup::Loading(self.serial)),
        );
        Some(self.serial)
    }

    pub fn received(
        &mut self,
        destination: Destination,
        serial: u64,
        result: Result<Catalog, String>,
    ) {
        if matches!(self.lookup(&destination), Some(Lookup::Loading(active)) if *active == serial) {
            let lookup = match result {
                Ok(catalog) => Lookup::Ready(catalog, Instant::now()),
                Err(error) => Lookup::Failed(error),
            };
            self.lookups
                .insert(destination.key, (destination.address, lookup));
        }
    }

    pub fn catalog(&self, destination: &Destination) -> Option<&Catalog> {
        match self.lookup(destination) {
            Some(Lookup::Ready(catalog, _)) => Some(catalog),
            _ => None,
        }
    }

    pub fn available(&self, destination: &Destination, runtime: &str) -> bool {
        self.catalog(destination)
            .is_some_and(|catalog| catalog.available().any(|id| id == runtime))
    }

    pub fn render_status(
        &self,
        ui: &mut egui::Ui,
        destination: &Destination,
        actions: &mut Vec<Action>,
    ) {
        match self.lookup(destination) {
            None => actions.push(Action::LoadRuntimes(destination.clone())),
            Some(Lookup::Loading(_)) => {
                ui.label("Loading runtimes…");
            }
            Some(Lookup::Ready(_, fetched)) if fetched.elapsed() >= Duration::from_secs(30) => {
                actions.push(Action::LoadRuntimes(destination.clone()));
            }
            Some(Lookup::Failed(error)) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Runtime list unavailable").on_hover_text(error);
                    if ui.small_button("Retry").clicked() {
                        actions.push(Action::LoadRuntimes(destination.clone()));
                    }
                });
            }
            _ => {}
        }
    }
}

impl Desktop {
    pub fn load_runtimes(&mut self, destination: Destination) {
        if let Some(client) = &self.client
            && let Some(serial) = self.data.runtimes.begin(&destination)
        {
            client.runtimes(destination, serial, self.bus.publisher());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_previous_endpoint_or_connection_cannot_replace_the_selected_catalog() {
        let mut catalogs = Catalogs::default();
        let target = Destination {
            key: "fixture-peer".into(),
            label: "Remote fixture".into(),
            address: "127.0.0.1:42424".into(),
        };
        let old_target = Destination {
            address: "127.0.0.1:42426".into(),
            ..target.clone()
        };
        let result = |antigravity| {
            serde_json::from_value(json!({"backends":[
                {"id":"codex","configured":true},
                {"id":"antigravity","configured":antigravity}
            ]}))
            .map_err(|error| error.to_string())
        };
        let old = catalogs.begin(&old_target).unwrap();
        let before_reconnect = catalogs.begin(&target).unwrap();
        catalogs.clear();
        let current = catalogs.begin(&target).unwrap();
        catalogs.received(old_target, old, result(true));
        catalogs.received(target.clone(), before_reconnect, result(true));
        assert!(catalogs.catalog(&target).is_none());
        catalogs.received(target.clone(), current, result(false));
        assert!(catalogs.available(&target, "codex"));
        assert!(!catalogs.available(&target, "antigravity"));
    }
}
