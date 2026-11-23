//! Boxen Console's compact typography and surfaces.
use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

/// Apple's macOS minimum control size, in points.
pub const MIN_TARGET: f32 = 20.;

/// Single-line field padding: one row of 12 pt body text plus 3 pt above and
/// below reaches [`MIN_TARGET`]. egui's `TextEdit::min_size` ignores height.
pub const FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(4, 3);

/// A borderless glyph button whose click target meets [`MIN_TARGET`] in both
/// dimensions regardless of the glyph's width.
pub fn icon_button<'a>(text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::new(text)
        .frame(false)
        .min_size(egui::vec2(MIN_TARGET, MIN_TARGET))
}

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
/// Your turns sit on a raised band, a step lighter than the canvas; the
/// agent's narration sits flush on the canvas. Depth tells them apart.
pub const RAISED: Color32 = Color32::from_rgb(24, 33, 45);
pub const RAISED_TEXT: Color32 = Color32::from_rgb(238, 242, 246);

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
    // Plex Mono lacks the arrows and marks used in receipts and hints.
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(1, "Inter".into());
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(5., 2.);
        style.spacing.button_padding = egui::vec2(5., 2.);
        style.spacing.interact_size.y = MIN_TARGET;
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

#[cfg(test)]
mod tests {
    use eframe::egui::{self, FontFamily, FontId};

    #[test]
    fn interface_glyphs_exist_in_the_installed_fonts() {
        let ctx = egui::Context::default();
        super::install(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let missing: Vec<String> = ctx.fonts_mut(|fonts| {
            let mut missing = Vec::new();
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                for c in "◇↗↵●○→⚙⌘↳…·×⇧–✓←↩".chars() {
                    if !fonts.has_glyph(&FontId::new(12., family.clone()), c) {
                        missing.push(format!("{family:?} {c} U+{:04X}", c as u32));
                    }
                }
            }
            missing
        });
        assert!(missing.is_empty(), "{missing:#?}");
    }

    #[test]
    fn glyph_buttons_buttons_and_fields_meet_the_macos_minimum_target() {
        let ctx = egui::Context::default();
        super::install(&ctx);
        let mut rects = Vec::new();
        let mut text = String::new();
        for _ in 0..2 {
            rects.clear();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                rects.push(("icon +", ui.add(super::icon_button("+")).rect));
                rects.push(("button", ui.button("Dismiss").rect));
                rects.push((
                    "field",
                    ui.add(egui::TextEdit::singleline(&mut text).margin(super::FIELD_MARGIN))
                        .rect,
                ));
            });
        }
        for (name, rect) in &rects {
            assert!(
                rect.width() >= super::MIN_TARGET - 0.01
                    && rect.height() >= super::MIN_TARGET - 0.01,
                "{name} is {:?}",
                rect.size()
            );
        }
    }
}
