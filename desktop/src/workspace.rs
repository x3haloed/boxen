use crate::model::{Pane, ThreadKey};
use egui_dock::{DockState, NodeIndex, SurfaceIndex, TabIndex, TabPath};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub struct Workspace {
    pub dock: DockState<Pane>,
    pub preview: Option<ThreadKey>,
    #[serde(default)]
    pub mail_preview: Option<ThreadKey>,
    #[serde(default)]
    pub activity_preview: Option<ThreadKey>,
    #[serde(default)]
    pub chat_preview: Option<ThreadKey>,
}
impl Default for Workspace {
    fn default() -> Self {
        let mut dock = DockState::new(vec![Pane::Welcome]);
        dock.main_surface_mut().split_left(
            NodeIndex::root(),
            0.24,
            vec![Pane::Correspondence, Pane::Threads],
        );
        let path = dock.find_tab(&Pane::Threads).unwrap();
        dock.split(
            path.node_path(),
            egui_dock::Split::Above,
            0.28,
            egui_dock::Node::leaf(Pane::Agents),
        );
        Self {
            dock,
            preview: None,
            mail_preview: None,
            activity_preview: None,
            chat_preview: None,
        }
    }
}
impl Workspace {
    pub fn merge_resident_views(&mut self, activity: &mut std::collections::HashSet<ThreadKey>) {
        let legacy: Vec<_> = self
            .dock
            .iter_all_tabs()
            .filter_map(|(_, pane)| match pane {
                Pane::Activity(key) => Some(key.clone()),
                _ => None,
            })
            .collect();
        for key in legacy {
            let old = Pane::Activity(key.clone());
            let new = Pane::Chat(key.clone());
            let Some(path) = self.dock.find_tab(&old) else {
                continue;
            };
            if self.dock.find_tab(&new).is_some() {
                if !self.is_preview(&old) {
                    self.pin_pane(&new);
                }
                self.dock.remove_tab(path);
            } else {
                let was_preview = self.is_preview(&old);
                self.dock[path.surface][path.node].tabs_mut().unwrap()[path.tab.0] = new;
                if was_preview {
                    self.chat_preview = Some(key.clone());
                }
                activity.insert(key);
            }
        }
        self.activity_preview = None;
    }
    pub fn show_work(&mut self) {
        if let Some(path) = self.dock.find_tab(&Pane::Work) {
            self.focus(path);
        } else {
            self.open_editor(Pane::Work, true);
        }
    }
    pub fn open(&mut self, key: ThreadKey, pin: bool) {
        self.open_editor(Pane::Thread(key), pin);
    }
    pub fn open_message(&mut self, key: ThreadKey, pin: bool) {
        self.open_editor(Pane::Message(key), pin);
    }
    pub fn open_compose(&mut self, id: String) {
        self.open_editor(Pane::Compose(id), true);
    }
    pub fn is_preview(&self, pane: &Pane) -> bool {
        match pane {
            Pane::Chat(k) => self.chat_preview.as_ref() == Some(k),
            Pane::Activity(k) => self.activity_preview.as_ref() == Some(k),
            Pane::Thread(k) => self.preview.as_ref() == Some(k),
            Pane::Message(k) => self.mail_preview.as_ref() == Some(k),
            _ => false,
        }
    }
    pub fn pin_pane(&mut self, pane: &Pane) {
        match pane {
            Pane::Chat(k) if self.chat_preview.as_ref() == Some(k) => self.chat_preview = None,
            Pane::Activity(k) if self.activity_preview.as_ref() == Some(k) => {
                self.activity_preview = None
            }
            Pane::Thread(k) => self.pin(k),
            Pane::Message(k) if self.mail_preview.as_ref() == Some(k) => self.mail_preview = None,
            _ => {}
        }
    }
    fn preview_pane(&self) -> Option<Pane> {
        self.preview
            .clone()
            .map(Pane::Thread)
            .or_else(|| self.mail_preview.clone().map(Pane::Message))
            .or_else(|| self.activity_preview.clone().map(Pane::Activity))
            .or_else(|| self.chat_preview.clone().map(Pane::Chat))
    }
    fn set_preview(&mut self, pane: Pane) {
        self.preview = None;
        self.mail_preview = None;
        self.activity_preview = None;
        self.chat_preview = None;
        match pane {
            Pane::Chat(k) => self.chat_preview = Some(k),
            Pane::Activity(k) => self.activity_preview = Some(k),
            Pane::Thread(k) => self.preview = Some(k),
            Pane::Message(k) => self.mail_preview = Some(k),
            _ => {}
        }
    }
    pub fn open_editor(&mut self, pane: Pane, pin: bool) {
        if let Some(path) = self.dock.find_tab(&pane) {
            self.focus(path);
            if pin {
                self.pin_pane(&pane)
            }
            return;
        }
        if !pin
            && let Some(old) = self.preview_pane()
            && let Some(path) = self.dock.find_tab(&old)
        {
            self.dock[path.surface][path.node].tabs_mut().unwrap()[path.tab.0] = pane.clone();
            self.focus(path);
            self.set_preview(pane);
            return;
        }
        let preview_pane = pane.clone();
        let target = self
            .dock
            .iter_all_tabs()
            .find(|(_, p)| {
                matches!(
                    p,
                    Pane::Thread(_)
                        | Pane::Work
                        | Pane::Chat(_)
                        | Pane::Activity(_)
                        | Pane::Message(_)
                        | Pane::Compose(_)
                        | Pane::Welcome
                )
            })
            .map(|(p, _)| p);
        if let Some(path) = target {
            let welcome = self.dock.find_tab(&Pane::Welcome);
            if let Some(w) = welcome {
                self.dock[w.surface][w.node].tabs_mut().unwrap()[w.tab.0] = pane;
                self.focus(w);
            } else {
                let leaf = self.dock.leaf_mut(path.node_path()).unwrap();
                leaf.tabs.push(pane);
                leaf.active = TabIndex(leaf.tabs.len() - 1);
                self.dock.set_focused_node_and_surface(path.node_path());
            }
        } else {
            // The list may have been moved or the last editor closed. A new editor splits beside it.
            let list = self
                .dock
                .find_tab(&Pane::Threads)
                .or_else(|| self.dock.find_tab(&Pane::Correspondence));
            if let Some(path) = list {
                self.dock.split(
                    path.node_path(),
                    egui_dock::Split::Right,
                    0.72,
                    egui_dock::Node::leaf(pane),
                );
            } else {
                self.dock.main_surface_mut().push_to_first_leaf(pane);
            }
        }
        if !pin {
            self.set_preview(preview_pane)
        }
    }
    fn focus(&mut self, path: TabPath) {
        let _ = self.dock.set_active_tab(path);
        self.dock.set_focused_node_and_surface(path.node_path());
    }
    pub fn pin(&mut self, key: &ThreadKey) {
        if self.preview.as_ref() == Some(key) {
            self.preview = None
        }
    }
    pub fn show_correspondence(&mut self) {
        if let Some(path) = self.dock.find_tab(&Pane::Correspondence) {
            self.focus(path);
        } else if let Some(path) = self.dock.find_tab(&Pane::Threads) {
            let leaf = self.dock.leaf_mut(path.node_path()).unwrap();
            leaf.tabs.push(Pane::Correspondence);
            leaf.active = TabIndex(leaf.tabs.len() - 1);
        } else if self.dock.main_surface().is_empty() {
            self.dock = DockState::new(vec![Pane::Correspondence]);
        } else {
            self.dock.split(
                egui_dock::NodePath::new(SurfaceIndex::main(), NodeIndex::root()),
                egui_dock::Split::Left,
                0.24,
                egui_dock::Node::leaf(Pane::Correspondence),
            );
        }
    }
    pub fn show_agents(&mut self) {
        if let Some(path) = self.dock.find_tab(&Pane::Agents) {
            self.focus(path);
            return;
        }
        if let Some(path) = self
            .dock
            .find_tab(&Pane::Threads)
            .or_else(|| self.dock.find_tab(&Pane::Correspondence))
        {
            self.dock.split(
                path.node_path(),
                egui_dock::Split::Above,
                0.28,
                egui_dock::Node::leaf(Pane::Agents),
            );
        } else if self.dock.main_surface().is_empty() {
            self.dock = DockState::new(vec![Pane::Agents]);
        } else {
            self.dock.split(
                egui_dock::NodePath::new(SurfaceIndex::main(), NodeIndex::root()),
                egui_dock::Split::Left,
                0.24,
                egui_dock::Node::leaf(Pane::Agents),
            );
        }
    }
    pub fn show_list(&mut self) {
        if let Some(path) = self.dock.find_tab(&Pane::Threads) {
            self.focus(path)
        } else if self.dock.main_surface().is_empty() {
            self.dock = DockState::new(vec![Pane::Threads]);
        } else {
            self.dock.split(
                egui_dock::NodePath::new(SurfaceIndex::main(), NodeIndex::root()),
                egui_dock::Split::Left,
                0.24,
                egui_dock::Node::leaf(Pane::Threads),
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key(id: &str) -> ThreadKey {
        ThreadKey {
            peer: "p".into(),
            id: id.into(),
        }
    }
    #[test]
    fn chat_preview_is_global_and_drafting_pins_it() {
        let mut w = Workspace::default();
        let chat = Pane::Chat(key("agent"));
        w.open_editor(chat.clone(), false);
        w.open_editor(Pane::Activity(key("agent")), false);
        assert!(w.dock.find_tab(&chat).is_none());
        w.open_editor(chat.clone(), false);
        w.pin_pane(&chat);
        w.open(key("thread"), false);
        assert!(w.dock.find_tab(&chat).is_some());
        assert!(!w.is_preview(&chat));
    }
    #[test]
    fn preview_replaces_globally_after_docking() {
        let mut w = Workspace::default();
        w.open(key("a"), false);
        w.open(key("b"), true);
        let a = w.dock.find_tab(&Pane::Thread(key("a"))).unwrap();
        w.dock.detach_tab(
            a,
            eframe::egui::Rect::from_min_size(
                eframe::egui::pos2(0., 0.),
                eframe::egui::vec2(300., 300.),
            ),
        );
        w.open(key("c"), false);
        assert!(w.dock.find_tab(&Pane::Thread(key("a"))).is_none());
        assert!(w.dock.find_tab(&Pane::Thread(key("b"))).is_some());
        assert!(w.dock.find_tab(&Pane::Thread(key("c"))).is_some());
        w.open(key("c"), true);
        w.open(key("d"), false);
        assert_eq!(
            w.dock
                .iter_all_tabs()
                .filter(|(_, t)| matches!(t, Pane::Thread(_)))
                .count(),
            3
        );
    }
    #[test]
    fn mail_and_threads_share_one_preview_and_pinned_mail_survives() {
        let mut w = Workspace::default();
        w.open_message(key("mail-a"), false);
        let path = w.dock.find_tab(&Pane::Message(key("mail-a"))).unwrap();
        w.dock.detach_tab(
            path,
            eframe::egui::Rect::from_min_size(
                eframe::egui::pos2(0., 0.),
                eframe::egui::vec2(300., 300.),
            ),
        );
        w.open(key("thread"), false);
        assert!(w.dock.find_tab(&Pane::Message(key("mail-a"))).is_none());
        w.open_message(key("mail-b"), false);
        assert!(w.dock.find_tab(&Pane::Thread(key("thread"))).is_none());
        w.open_message(key("mail-b"), true);
        w.open_message(key("mail-c"), false);
        assert!(w.dock.find_tab(&Pane::Message(key("mail-b"))).is_some());
        w.pin_pane(&Pane::Message(key("mail-c")));
        w.open(key("next"), false);
        assert!(w.dock.find_tab(&Pane::Message(key("mail-c"))).is_some());
        w.open_editor(Pane::Activity(key("resident-kept")), true);
        struct Draw;
        impl egui_dock::TabViewer for Draw {
            type Tab = Pane;
            fn title(&mut self, _: &mut Pane) -> eframe::egui::WidgetText {
                "Panel".into()
            }
            fn ui(&mut self, _: &mut eframe::egui::Ui, _: &mut Pane) {}
        }
        let ctx = eframe::egui::Context::default();
        let _ = ctx.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::pos2(0., 0.),
                    eframe::egui::vec2(1000., 700.),
                )),
                ..Default::default()
            },
            |ui| {
                egui_dock::DockArea::new(&mut w.dock).show_inside(ui, &mut Draw);
            },
        );
        let restored: Workspace = serde_json::from_slice(&serde_json::to_vec(&w).unwrap()).unwrap();
        assert!(restored.is_preview(&Pane::Thread(key("next"))));
        assert!(
            restored
                .dock
                .find_tab(&Pane::Activity(key("resident-kept")))
                .is_some()
        );
        let mut legacy = serde_json::to_value(&w).unwrap();
        legacy.as_object_mut().unwrap().remove("activity_preview");
        let migrated: Workspace = serde_json::from_value(legacy).unwrap();
        assert!(migrated.activity_preview.is_none());
    }
    #[test]
    fn activity_shares_preview_and_pins_across_docking() {
        let mut w = Workspace::default();
        let a = Pane::Activity(key("a"));
        w.open_editor(a.clone(), false);
        assert!(w.is_preview(&a));
        w.open(key("thread"), false);
        assert!(w.dock.find_tab(&a).is_none());
        w.open_editor(a.clone(), false);
        w.open_editor(a.clone(), true);
        assert!(!w.is_preview(&a));
        w.open_message(key("mail"), false);
        let b = Pane::Activity(key("b"));
        w.open_editor(b.clone(), false);
        assert!(w.dock.find_tab(&a).is_some());
        assert!(w.dock.find_tab(&Pane::Message(key("mail"))).is_none());
        let path = w.dock.find_tab(&b).unwrap();
        w.dock.detach_tab(
            path,
            eframe::egui::Rect::from_min_size(
                eframe::egui::pos2(0., 0.),
                eframe::egui::vec2(300., 300.),
            ),
        );
        w.open_message(key("next-mail"), false);
        assert!(w.dock.find_tab(&b).is_none());
        assert!(w.dock.find_tab(&a).is_some());
    }
    #[test]
    fn agents_panel_restores_once_and_preserves_existing_editors() {
        let mut w = Workspace::default();
        let path = w.dock.find_tab(&Pane::Agents).unwrap();
        w.dock.remove_tab(path);
        w.open_message(key("kept-mail"), true);
        w.open_compose("kept-draft".into());
        w.show_agents();
        w.show_agents();
        assert_eq!(
            w.dock
                .iter_all_tabs()
                .filter(|(_, p)| **p == Pane::Agents)
                .count(),
            1
        );
        assert!(w.dock.find_tab(&Pane::Message(key("kept-mail"))).is_some());
        assert!(
            w.dock
                .find_tab(&Pane::Compose("kept-draft".into()))
                .is_some()
        );
        struct Draw;
        impl egui_dock::TabViewer for Draw {
            type Tab = Pane;
            fn title(&mut self, _: &mut Pane) -> eframe::egui::WidgetText {
                "Panel".into()
            }
            fn ui(&mut self, _: &mut eframe::egui::Ui, _: &mut Pane) {}
        }
        let ctx = eframe::egui::Context::default();
        let _ = ctx.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::pos2(0., 0.),
                    eframe::egui::vec2(1000., 700.),
                )),
                ..Default::default()
            },
            |ui| {
                egui_dock::DockArea::new(&mut w.dock).show_inside(ui, &mut Draw);
            },
        );
        let restored: Workspace = serde_json::from_slice(&serde_json::to_vec(&w).unwrap()).unwrap();
        assert!(restored.dock.find_tab(&Pane::Agents).is_some());
    }
    #[test]
    fn same_thread_does_not_duplicate_and_peer_keys_are_distinct() {
        let mut w = Workspace::default();
        w.open(key("a"), false);
        w.open(key("a"), true);
        w.open(
            ThreadKey {
                peer: "other".into(),
                id: "a".into(),
            },
            false,
        );
        assert_eq!(
            w.dock
                .iter_all_tabs()
                .filter(|(_, t)| matches!(t, Pane::Thread(_)))
                .count(),
            2
        );
    }
}

#[cfg(test)]
mod pointer_tests {
    use super::*;
    use eframe::egui::{self, Event, Modifiers, PointerButton, pos2, vec2};
    use std::collections::HashMap;
    #[derive(Default)]
    struct Geometry {
        tabs: HashMap<String, egui::Rect>,
    }
    impl egui_dock::TabViewer for Geometry {
        type Tab = String;
        fn title(&mut self, t: &mut String) -> egui::WidgetText {
            t.clone().into()
        }
        fn ui(&mut self, ui: &mut egui::Ui, _: &mut String) {
            ui.label("Conversation");
        }
        fn on_tab_button(&mut self, t: &mut String, r: &egui::Response) {
            self.tabs.insert(t.clone(), r.rect);
        }
    }
    struct Harness {
        ctx: egui::Context,
        dock: DockState<String>,
        viewer: Geometry,
        time: f64,
    }
    impl Harness {
        fn frame(&mut self, events: Vec<Event>) {
            self.time += 1. / 60.;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(pos2(0., 0.), vec2(1000., 700.))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let _ = self.ctx.run_ui(input, |ui| {
                let mut style = egui_dock::Style::from_egui(ui.style());
                style.overlay.overlay_type = egui_dock::OverlayType::HighlightedAreas;
                egui_dock::DockArea::new(&mut self.dock)
                    .style(style)
                    .show_inside(ui, &mut self.viewer);
            });
        }
        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
            self.frame(vec![Event::PointerMoved(from)]);
            self.frame(vec![Event::PointerButton {
                pos: from,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }]);
            for step in 1..=30 {
                self.frame(vec![Event::PointerMoved(
                    from + (to - from) * (step as f32 / 30.),
                )]);
            }
            for _ in 0..30 {
                self.frame(vec![]);
            }
            self.frame(vec![Event::PointerButton {
                pos: to,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
            for _ in 0..3 {
                self.frame(vec![]);
            }
        }
    }
    #[test]
    fn real_pointer_resize_and_tab_reorder() {
        let mut h = Harness {
            ctx: egui::Context::default(),
            dock: DockState::new(vec!["one".into(), "two".into(), "three".into()]),
            viewer: Geometry::default(),
            time: 0.,
        };
        h.dock
            .main_surface_mut()
            .split_left(NodeIndex::root(), 0.24, vec!["Threads".into()]);
        for _ in 0..3 {
            h.frame(vec![]);
        }
        let list = h.dock.find_tab(&"Threads".into()).unwrap();
        let before = h.dock.leaf(list.node_path()).unwrap().rect.max.x;
        h.drag(pos2(before + 1., 350.), pos2(before + 100., 350.));
        let after = h.dock.leaf(list.node_path()).unwrap().rect.max.x;
        assert!(
            after > before + 40.,
            "Dragging the divider should widen the sidebar: {before} -> {after}"
        );
        let from = h.viewer.tabs["three"].center();
        let to = h.viewer.tabs["one"].left_center() + vec2(2., 0.);
        h.drag(from, to);
        let one = h.dock.find_tab(&"one".into()).unwrap();
        let three = h.dock.find_tab(&"three".into()).unwrap();
        assert_eq!(one.node, three.node);
        assert!(
            three.tab.0 < one.tab.0,
            "Dragging onto the start of a tab bar should reorder"
        );
    }
    #[test]
    fn real_pointer_tab_splits_and_sidebar_moves() {
        let mut h = Harness {
            ctx: egui::Context::default(),
            dock: DockState::new(vec!["one".into(), "two".into()]),
            viewer: Geometry::default(),
            time: 0.,
        };
        h.dock
            .main_surface_mut()
            .split_left(NodeIndex::root(), 0.24, vec!["Threads".into()]);
        for _ in 0..3 {
            h.frame(vec![]);
        }
        let from = h.viewer.tabs["two"].center();
        h.drag(from, pos2(985., 350.));
        let one = h.dock.find_tab(&"one".into()).unwrap();
        let two = h.dock.find_tab(&"two".into()).unwrap();
        assert_eq!(
            two.surface,
            SurfaceIndex::main(),
            "Edge drop should dock, not float"
        );
        assert_ne!(
            one.node, two.node,
            "A pointer drop at the right edge creates a split"
        );
        let from = h.viewer.tabs["Threads"].center();
        h.drag(from, pos2(500., 680.));
        let list = h.dock.find_tab(&"Threads".into()).unwrap();
        assert!(
            h.dock.leaf(list.node_path()).unwrap().rect.min.y > 300.,
            "The thread list can dock at the bottom"
        );
    }
}
