//! Identity marks drawn instead of names.
//!
//! A machine's glyph is angular and comes from its public key; an agent's
//! sigil is round and comes from its own resident ID, so it stays the same
//! wherever the agent runs. Both are deterministic: every client draws the
//! same mark without storing anything.
use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, vec2};
use sha2::{Digest, Sha256};

/// Muted hues kept clear of the status colours (green ok, amber needs-you,
/// red failed, blue running).
const HUES: [Color32; 5] = [
    Color32::from_rgb(217, 138, 166), // rose
    Color32::from_rgb(82, 179, 166),  // teal
    Color32::from_rgb(199, 174, 124), // sand
    Color32::from_rgb(168, 148, 221), // lilac
    Color32::from_rgb(141, 162, 184), // slate
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineShape {
    Square,
    Diamond,
    Triangle,
    Wedge,
    Hexagon,
    Cross,
    Spark,
    Slant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMark {
    Dot,
    Bar,
    Stem,
    Pair,
    Crescent,
    Ring,
    Half,
    Slash,
}

fn digest(salt: &str, value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

pub fn machine(key: &str) -> (MachineShape, Color32) {
    use MachineShape::*;
    let h = digest("boxen-machine-glyph", key);
    let shape = [Square, Diamond, Triangle, Wedge, Hexagon, Cross, Spark, Slant][h[0] as usize % 8];
    (shape, HUES[h[1] as usize % HUES.len()])
}

/// Instances of one definition share its hue and differ by mark.
pub fn agent(resident: &str, definition: Option<&str>) -> (AgentMark, Color32) {
    use AgentMark::*;
    let h = digest("boxen-agent-sigil", resident);
    let mark = [Dot, Bar, Stem, Pair, Crescent, Ring, Half, Slash][h[0] as usize % 8];
    let family = digest("boxen-agent-hue", definition.unwrap_or(resident));
    (mark, HUES[family[0] as usize % HUES.len()])
}

/// Points of a shape drawn in a 12 × 12 box, as one or more convex parts.
fn machine_parts(shape: MachineShape) -> Vec<Vec<[f32; 2]>> {
    use MachineShape::*;
    match shape {
        Square => vec![vec![[1., 1.], [11., 1.], [11., 11.], [1., 11.]]],
        Diamond => vec![vec![[6., 0.], [12., 6.], [6., 12.], [0., 6.]]],
        Triangle => vec![vec![[6., 0.5], [11.8, 11.], [0.2, 11.]]],
        Wedge => vec![vec![[0.2, 1.], [11.8, 1.], [6., 11.5]]],
        Hexagon => vec![vec![
            [3., 1.],
            [9., 1.],
            [12., 6.],
            [9., 11.],
            [3., 11.],
            [0., 6.],
        ]],
        Cross => vec![
            vec![[4., 0.], [8., 0.], [8., 12.], [4., 12.]],
            vec![[0., 4.], [12., 4.], [12., 8.], [0., 8.]],
        ],
        Spark => vec![
            vec![[6., 0.], [7.7, 6.], [6., 12.], [4.3, 6.]],
            vec![[0., 6.], [6., 4.3], [12., 6.], [6., 7.7]],
        ],
        Slant => vec![vec![[3.5, 1.], [12., 1.], [8.5, 11.], [0., 11.]]],
    }
}

/// The closed outline of a shape, for the hollow unreachable state.
fn machine_outline(shape: MachineShape) -> Vec<[f32; 2]> {
    use MachineShape::*;
    match shape {
        Cross => vec![
            [4., 0.],
            [8., 0.],
            [8., 4.],
            [12., 4.],
            [12., 8.],
            [8., 8.],
            [8., 12.],
            [4., 12.],
            [4., 8.],
            [0., 8.],
            [0., 4.],
            [4., 4.],
        ],
        Spark => vec![
            [6., 0.],
            [7.7, 4.3],
            [12., 6.],
            [7.7, 7.7],
            [6., 12.],
            [4.3, 7.7],
            [0., 6.],
            [4.3, 4.3],
        ],
        other => machine_parts(other).remove(0),
    }
}

fn place(rect: Rect, points: &[[f32; 2]]) -> Vec<Pos2> {
    let k = rect.width().min(rect.height()) / 12.;
    let origin = rect.center() - vec2(6. * k, 6. * k);
    points
        .iter()
        .map(|[x, y]| origin + vec2(x * k, y * k))
        .collect()
}

pub fn paint_machine(painter: &egui::Painter, rect: Rect, key: &str, reachable: bool) {
    let (shape, hue) = machine(key);
    if reachable {
        for part in machine_parts(shape) {
            painter.add(Shape::convex_polygon(place(rect, &part), hue, Stroke::NONE));
        }
    } else {
        // Hollow and dimmed: the machine can't be reached.
        painter.add(Shape::closed_line(
            place(rect, &machine_outline(shape)),
            Stroke::new(1.2, hue.gamma_multiply(0.75)),
        ));
    }
}

/// `background` is the surface behind the sigil; the cut-out marks are drawn in it.
pub fn paint_agent(
    painter: &egui::Painter,
    rect: Rect,
    resident: &str,
    definition: Option<&str>,
    background: Color32,
) {
    use AgentMark::*;
    let (mark, hue) = agent(resident, definition);
    let size = rect.width().min(rect.height());
    let k = size / 12.;
    let disc = Rect::from_center_size(rect.center(), vec2(size, size));
    let at = |x: f32, y: f32| disc.min + vec2(x * k, y * k);
    painter.circle_filled(disc.center(), size / 2., hue);
    let cut_circle = |x: f32, y: f32, d: f32| {
        painter.circle_filled(at(x + d / 2., y + d / 2.), d * k / 2., background);
    };
    let cut_rect = |x: f32, y: f32, w: f32, h: f32| {
        painter.rect_filled(Rect::from_min_size(at(x, y), vec2(w * k, h * k)), 0., background);
    };
    match mark {
        Dot => cut_circle(4., 4., 4.),
        Bar => cut_rect(2., 5., 8., 2.),
        Stem => cut_rect(5., 2., 2., 8.),
        Pair => {
            cut_circle(2.5, 4.5, 3.);
            cut_circle(6.5, 4.5, 3.);
        }
        Crescent => {
            // Only the part of the cut-out circle inside the disc is visible.
            painter
                .with_clip_rect(disc)
                .circle_filled(at(9., 5.), 4.5 * k, background);
        }
        Ring => {
            cut_circle(3., 3., 6.);
            painter.circle_filled(disc.center(), 1.5 * k, hue);
        }
        Half => {
            let lower = Rect::from_min_max(at(0., 6.5), disc.max);
            painter
                .with_clip_rect(lower)
                .circle_filled(disc.center(), size / 2. + 0.5, background);
        }
        Slash => {
            painter.with_clip_rect(disc).add(Shape::convex_polygon(
                vec![at(8., 0.), at(10.5, 0.), at(3.5, 12.), at(1., 12.)],
                background,
                Stroke::NONE,
            ));
        }
    }
}

#[derive(Clone, Copy)]
pub enum Identity<'a> {
    Machine { key: &'a str, reachable: bool },
    Agent {
        resident: &'a str,
        definition: Option<&'a str>,
    },
    /// A definition row: the family's hue with no individual mark.
    Family { definition: &'a str },
}

impl Identity<'_> {
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, background: Color32) {
        match *self {
            Identity::Machine { key, reachable } => paint_machine(painter, rect, key, reachable),
            Identity::Agent {
                resident,
                definition,
            } => paint_agent(painter, rect, resident, definition, background),
            Identity::Family { definition } => {
                let (_, hue) = agent(definition, Some(definition));
                let size = rect.width().min(rect.height());
                painter.circle_stroke(rect.center(), size / 2. - 0.6, Stroke::new(1.2, hue));
            }
        }
    }

    /// Allocates a square of `size` points in the current layout and paints the mark.
    pub fn show(&self, ui: &mut egui::Ui, size: f32, background: Color32) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
        self.paint(ui.painter(), rect, background);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_deterministic_and_agents_ignore_their_host() {
        assert_eq!(machine("key-a"), machine("key-a"));
        assert_eq!(agent("res-1", None), agent("res-1", None));
        // A definition's instances share a hue.
        let (_, a) = agent("res-1", Some("def"));
        let (_, b) = agent("res-2", Some("def"));
        assert_eq!(a, b);
        // Different keys spread across the set.
        let shapes: std::collections::HashSet<_> =
            (0..64).map(|i| machine(&format!("key-{i}")).0 as u8).collect();
        assert!(shapes.len() >= 6);
    }
}
