use crate::model::Destination;
use eframe::egui;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct State {
    open: bool,
    machine: String,
    values: HashMap<String, Value>,
    pending: HashSet<String>,
    errors: HashMap<String, String>,
}

impl State {
    pub fn open(&mut self) {
        self.open = true;
        self.values.clear();
        self.errors.clear();
    }

    pub fn received(&mut self, machine: String, result: Result<Value, String>) {
        self.pending.remove(&machine);
        match result {
            Ok(value) => {
                self.errors.remove(&machine);
                self.values.insert(machine, value);
            }
            Err(error) => {
                self.errors.insert(machine, error);
            }
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        destinations: &[Destination],
    ) -> Option<(Destination, Value)> {
        if !self.open {
            return None;
        }
        if !destinations.iter().any(|d| d.key == self.machine) {
            self.machine = destinations.first()?.key.clone();
        }
        let mut request = None;
        let mut open = self.open;
        egui::Window::new("Tool approvals")
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ComboBox::from_label("Machine")
                    .selected_text(
                        destinations
                            .iter()
                            .find(|d| d.key == self.machine)
                            .map_or("", |d| d.label.as_str()),
                    )
                    .show_ui(ui, |ui| {
                        for destination in destinations {
                            ui.selectable_value(
                                &mut self.machine,
                                destination.key.clone(),
                                &destination.label,
                            );
                        }
                    });
                if let Some(error) = self.errors.get(&self.machine) {
                    ui.label(error);
                    if ui.button("Retry").clicked() {
                        request = Some(json!({"action":"get"}));
                    }
                }
                if let Some(settings) = self.values.get(&self.machine) {
                    ui.add_enabled_ui(!self.pending.contains(&self.machine), |ui| {
                        for (field, label) in [
                            ("computer_use", "Automatically approve computer use"),
                            ("browser_use", "Automatically approve browser use"),
                        ] {
                            let mut enabled = settings[field].as_bool().unwrap_or(false);
                            if ui.checkbox(&mut enabled, label).changed() {
                                request = Some(json!({"action":"set",field:enabled}));
                            }
                        }
                    });
                } else if !self.pending.contains(&self.machine)
                    && !self.errors.contains_key(&self.machine)
                {
                    request = Some(json!({"action":"get"}));
                }
                if self.pending.contains(&self.machine) {
                    ui.spinner();
                }
            });
        self.open = open;
        let args = request?;
        let destination = destinations.iter().find(|d| d.key == self.machine)?.clone();
        self.pending.insert(self.machine.clone());
        Some((destination, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_loaded_from_the_selected_daemon_and_failures_can_retry() {
        let destination = Destination {
            key: "fixture-machine".into(),
            label: "Fixture".into(),
            address: String::new(),
        };
        let destinations = [destination];
        let ctx = egui::Context::default();
        let mut state = State::default();
        state.open();
        let mut request = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx();
            request = state.show(ctx, &destinations);
        });
        assert_eq!(request.as_ref().unwrap().1, json!({"action":"get"}));
        state.received(
            "fixture-machine".into(),
            Ok(json!({"computer_use":true,"browser_use":false})),
        );
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx();
            request = state.show(ctx, &destinations);
        });
        assert!(request.is_none());
        assert_eq!(state.values["fixture-machine"]["browser_use"], false);
        state.received("fixture-machine".into(), Err("Disconnected".into()));
        assert!(state.pending.is_empty());
        assert_eq!(state.errors["fixture-machine"], "Disconnected");
        state.open();
        assert!(state.errors.is_empty());
    }
}
