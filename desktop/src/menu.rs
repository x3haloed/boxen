use eframe::egui;

#[derive(Clone, Copy)]
pub(crate) enum Command {
    NewThread,
    Reconnect,
    ShowWork,
    Palette,
}

impl Command {
    fn label(self) -> &'static str {
        match self {
            Self::NewThread => "New Thread…",
            Self::Reconnect => "Reconnect Daemon",
            Self::ShowWork => "Show Work",
            Self::Palette => "Command Palette…",
        }
    }
}

const GROUPS: &[(&str, &[Command])] = &[
    ("File", &[Command::NewThread, Command::Reconnect]),
    ("View", &[Command::ShowWork, Command::Palette]),
];

pub(crate) fn in_window(ui: &mut egui::Ui, ready: bool) -> Option<Command> {
    let mut selected = None;
    egui::Panel::top("main-menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            for &(title, commands) in GROUPS {
                ui.menu_button(title, |ui| {
                    for &command in commands {
                        if ui
                            .add_enabled(
                                !ready || !matches!(command, Command::Reconnect),
                                egui::Button::new(command.label()),
                            )
                            .clicked()
                        {
                            selected = Some(command);
                            ui.close();
                        }
                    }
                });
            }
        });
    });
    selected
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) use native::NativeMenu;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::{Command, GROUPS};
    use muda::{Menu, MenuEvent, MenuItem, Submenu};
    use std::sync::mpsc::{self, Receiver};

    pub(crate) struct NativeMenu {
        _menu: Menu,
        reconnect: MenuItem,
        commands: Receiver<Command>,
    }

    impl NativeMenu {
        pub(crate) fn install(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
            let menu = Menu::new();
            #[cfg(target_os = "macos")]
            add_app_menu(&menu).map_err(|err| err.to_string())?;

            let reconnect = MenuItem::new(Command::Reconnect.label(), true, None);
            let mut bindings = Vec::new();
            for &(title, commands) in GROUPS {
                // The ampersand provides a native Alt+letter mnemonic on Windows.
                let submenu = Submenu::new(format!("&{title}"), true);
                for &command in commands {
                    let item = match command {
                        Command::Reconnect => reconnect.clone(),
                        _ => MenuItem::new(command.label(), true, None),
                    };
                    bindings.push((item.id().clone(), command));
                    submenu.append(&item).map_err(|err| err.to_string())?;
                }
                menu.append(&submenu).map_err(|err| err.to_string())?;
            }

            #[cfg(target_os = "macos")]
            menu.init_for_nsapp();
            #[cfg(target_os = "windows")]
            attach_to_window(&menu, cc)?;

            let (tx, commands) = mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some((_, command)) = bindings.iter().find(|(id, _)| id == event.id())
                    && tx.send(*command).is_ok()
                {
                    ctx.request_repaint();
                }
            }));

            Ok(Self {
                _menu: menu,
                reconnect,
                commands,
            })
        }

        pub(crate) fn next_command(&self) -> Option<Command> {
            self.commands.try_recv().ok()
        }

        pub(crate) fn set_ready(&self, ready: bool) {
            let enabled = !ready;
            if self.reconnect.is_enabled() != enabled {
                self.reconnect.set_enabled(enabled);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn add_app_menu(menu: &Menu) -> muda::Result<()> {
        use muda::PredefinedMenuItem as Item;

        // Installing a main menu replaces winit's default application menu.
        menu.append(&Submenu::with_items(
            "Boxen",
            true,
            &[
                &Item::about(Some("About Boxen"), None),
                &Item::separator(),
                &Item::services(None),
                &Item::hide(Some("Hide Boxen")),
                &Item::hide_others(None),
                &Item::show_all(None),
                &Item::separator(),
                &Item::quit(Some("Quit Boxen")),
            ],
        )?)
    }

    #[cfg(target_os = "windows")]
    fn attach_to_window(menu: &Menu, cc: &eframe::CreationContext<'_>) -> Result<(), String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let window = cc.window_handle().map_err(|err| err.to_string())?;
        let RawWindowHandle::Win32(handle) = window.as_raw() else {
            return Err("Expected a Win32 window handle".into());
        };
        // eframe supplies the live root HWND on its window thread.
        unsafe { menu.init_for_hwnd(handle.hwnd.get()) }.map_err(|err| err.to_string())
    }
}
