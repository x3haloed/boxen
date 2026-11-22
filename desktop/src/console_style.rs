//! Boxen Console's compact typography and surfaces.
use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

pub const CANVAS: Color32 = Color32::from_rgb(11, 13, 16);
pub const PANEL: Color32 = Color32::from_rgb(16, 19, 23);
pub const FIELD: Color32 = Color32::from_rgb(21, 25, 32);
pub const HOVER: Color32 = Color32::from_rgb(27, 32, 39);
pub const SELECTION: Color32 = Color32::from_rgb(23, 40, 60);
pub const RULE: Color32 = Color32::from_rgb(32, 38, 46);
pub const EDGE: Color32 = Color32::from_rgb(42, 49, 59);
pub const TEXT: Color32 = Color32::from_rgb(213, 218, 224);
pub const SECONDARY: Color32 = Color32::from_rgb(154, 163, 174);
pub const ACCENT: Color32 = Color32::from_rgb(76, 158, 255);
pub const OK: Color32 = Color32::from_rgb(63, 185, 80);
pub const WARN: Color32 = Color32::from_rgb(216, 162, 58);
pub const ERROR: Color32 = Color32::from_rgb(239, 91, 80);

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, family, bytes) in [
        (
            "Inter",
            FontFamily::Proportional,
            include_bytes!("../assets/fonts/Inter.ttf").as_slice(),
        ),
        (
            "IBM Plex Mono",
            FontFamily::Monospace,
            include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.into());
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(5., 2.);
        style.spacing.button_padding = egui::vec2(5., 2.);
        style.spacing.interact_size.y = 20.;
        for (role, font) in [
            (TextStyle::Heading, FontId::proportional(14.)),
            (TextStyle::Body, FontId::proportional(12.)),
            (TextStyle::Button, FontId::proportional(11.)),
            (TextStyle::Small, FontId::proportional(10.5)),
            (TextStyle::Monospace, FontId::monospace(11.)),
        ] {
            style.text_styles.insert(role, font);
        }
        let visuals = &mut style.visuals;
        visuals.override_text_color = Some(TEXT);
        visuals.weak_text_color = Some(SECONDARY);
        visuals.panel_fill = CANVAS;
        visuals.window_fill = PANEL;
        visuals.window_stroke = Stroke::new(1., EDGE);
        visuals.extreme_bg_color = FIELD;
        visuals.faint_bg_color = PANEL;
        visuals.selection.bg_fill = SELECTION;
        visuals.selection.stroke = Stroke::new(1., ACCENT);
        visuals.hyperlink_color = ACCENT;
        visuals.warn_fg_color = WARN;
        visuals.error_fg_color = ERROR;
        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_stroke = Stroke::new(1., RULE);
        widgets.inactive.weak_bg_fill = FIELD;
        widgets.inactive.bg_fill = FIELD;
        widgets.inactive.bg_stroke = Stroke::new(1., EDGE);
        widgets.hovered.weak_bg_fill = HOVER;
        widgets.hovered.bg_fill = HOVER;
        widgets.hovered.bg_stroke = Stroke::new(1., EDGE);
        widgets.active.weak_bg_fill = SELECTION;
        widgets.active.bg_fill = SELECTION;
        widgets.active.bg_stroke = Stroke::new(1., ACCENT);
    });
}

pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9., 14.), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 2.7, color);
}

pub fn drag_grip(ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(11., 16.), egui::Sense::hover());
    let center = rect.center();
    for x in [-2., 2.] {
        for y in [-4., 0., 4.] {
            ui.painter()
                .circle_filled(center + egui::vec2(x, y), 0.85, SECONDARY);
        }
    }
    response.on_hover_text("Move pane");
}

pub fn dashed_rect(ui: &egui::Ui, rect: egui::Rect, color: Color32) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    ui.painter().extend(egui::Shape::dashed_line(
        &corners,
        Stroke::new(1., color),
        4.,
        3.,
    ));
}
