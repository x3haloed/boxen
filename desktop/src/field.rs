use eframe::egui::{self, Color32 as C, Pos2, Stroke, Vec2, pos2, vec2};
use std::time::{Duration, Instant};

pub fn matches(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("there is no cow level")
}
struct Unit {
    p: Pos2,
    target: Pos2,
    hp: f32,
    selected: bool,
    cooldown: f32,
}
struct Spark {
    from: Pos2,
    to: Pos2,
    life: f32,
    friendly: bool,
}
pub struct Field {
    units: Vec<Unit>,
    bugs: Vec<Unit>,
    sparks: Vec<Spark>,
    time: f32,
    spawn: f32,
    minerals: f32,
    hull: f32,
    kills: u32,
    last: Instant,
    paused: bool,
    destination: Option<Pos2>,
}
impl Default for Field {
    fn default() -> Self {
        Self {
            units: (0..6)
                .map(|i| Unit {
                    p: pos2(265. + (i % 3) as f32 * 24., 280. + (i / 3) as f32 * 28.),
                    target: pos2(265. + (i % 3) as f32 * 24., 280. + (i / 3) as f32 * 28.),
                    hp: 100.,
                    selected: true,
                    cooldown: i as f32 * 0.12,
                })
                .collect(),
            bugs: vec![],
            sparks: vec![],
            time: 0.,
            spawn: 2.,
            minerals: 150.,
            hull: 100.,
            kills: 0,
            last: Instant::now(),
            paused: false,
            destination: None,
        }
    }
}
fn travel(p: &mut Pos2, target: Pos2, speed: f32, dt: f32) {
    let d = target - *p;
    let n = d.length();
    if n > 0.1 {
        *p += d / n * (speed * dt).min(n);
    }
}
impl Field {
    fn order(&mut self, target: Pos2) {
        let target = pos2(target.x.clamp(30., 770.), target.y.clamp(70., 450.));
        self.destination = Some(target);
        let count = self.units.iter().filter(|u| u.selected).count();
        let mut n = 0;
        for u in &mut self.units {
            if u.selected {
                u.target = target
                    + vec2(
                        (n % 3) as f32 * 18. - 18.,
                        (n / 3) as f32 * 20. - (count / 3) as f32 * 10.,
                    );
                n += 1;
            }
        }
    }
    fn step(&mut self, dt: f32) {
        if self.paused {
            return;
        }
        let dt = dt.clamp(0., 0.05);
        self.time += dt;
        self.spawn -= dt;
        self.minerals = (self.minerals + dt * 7.).min(9999.);
        self.hull = (self.hull + dt * 0.7).min(100.);
        if self.spawn <= 0. && self.bugs.len() < 24 {
            let n = (self.time * 13.) as u32;
            let p = pos2(748., 90. + (n % 330) as f32);
            self.bugs.push(Unit {
                p,
                target: pos2(160., 245.),
                hp: 45.,
                selected: false,
                cooldown: 0.,
            });
            self.spawn = (4.5 - self.time / 180.).max(1.8);
        }
        for u in &mut self.units {
            travel(&mut u.p, u.target, 55., dt);
            u.cooldown -= dt;
            if u.cooldown <= 0.
                && let Some(b) = self
                    .bugs
                    .iter_mut()
                    .filter(|b| b.hp > 0. && b.p.distance(u.p) < 165.)
                    .min_by(|a, b| a.p.distance(u.p).total_cmp(&b.p.distance(u.p)))
            {
                b.hp -= 22.;
                u.cooldown = 0.65;
                self.sparks.push(Spark {
                    from: u.p,
                    to: b.p,
                    life: 0.13,
                    friendly: true,
                });
            }
        }
        for b in &mut self.bugs {
            if b.hp <= 0. {
                continue;
            }
            b.cooldown -= dt;
            if let Some(u) = self
                .units
                .iter_mut()
                .filter(|u| u.hp > 0. && u.p.distance(b.p) < 25.)
                .min_by(|a, c| a.p.distance(b.p).total_cmp(&c.p.distance(b.p)))
            {
                if b.cooldown <= 0. {
                    u.hp -= 7.;
                    b.cooldown = 0.9;
                    self.sparks.push(Spark {
                        from: b.p,
                        to: u.p,
                        life: 0.15,
                        friendly: false,
                    });
                }
            } else {
                travel(&mut b.p, b.target, 24., dt);
                if b.p.distance(b.target) < 45. {
                    self.hull = (self.hull - dt * 2.).max(10.);
                    b.hp -= dt * 12.;
                }
            }
        }
        let before = self.bugs.len();
        self.bugs.retain(|b| b.hp > 0.);
        self.kills += (before - self.bugs.len()) as u32;
        self.units.retain(|u| u.hp > 0.);
        for s in &mut self.sparks {
            s.life -= dt;
        }
        self.sparks.retain(|s| s.life > 0.);
    }
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("field"),
            egui::ViewportBuilder::default()
                .with_title("Somewhere, on the outer rim")
                .with_inner_size([800., 600.])
                .with_min_inner_size([640., 480.])
                .with_resizable(true)
                .with_maximize_button(true),
            |ctx, _| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    open = false;
                    return;
                }
                let now = Instant::now();
                let dt = (now - self.last).as_secs_f32();
                self.last = now;
                if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
                    self.paused = !self.paused;
                }
                if ctx.input(|i| i.key_pressed(egui::Key::R)) {
                    *self = Self::default();
                }
                self.step(dt);
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(C::from_rgb(9, 14, 18)))
                    .show(ctx, |ui| self.draw(ui));
                if !self.paused {
                    ctx.request_repaint_after(Duration::from_millis(33));
                }
            },
        );
        open
    }
    fn draw(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let scale = (rect.width() / 800.).min(rect.height() / 600.);
        let origin = rect.center() - vec2(400., 300.) * scale;
        let point = |p: Pos2| origin + p.to_vec2() * scale;
        let box_ = |x: f32, y: f32, w: f32, h: f32| {
            egui::Rect::from_min_size(point(pos2(x, y)), vec2(w, h) * scale)
        };
        let p = ui
            .painter()
            .with_clip_rect(egui::Rect::from_min_size(origin, vec2(800., 600.) * scale));
        let text = |at: Pos2, s: &str, size: f32, color: C| {
            p.text(
                point(at),
                egui::Align2::LEFT_TOP,
                s,
                egui::FontId::monospace(size * scale),
                color,
            );
        };
        p.rect_filled(box_(0., 0., 800., 470.), 0., C::from_rgb(22, 32, 34));
        // Quiet terrain: staggered plates, scars and dim navigation lights.
        for row in 0..16 {
            for col in 0..25 {
                let x = col as f32 * 34. - (row % 2) as f32 * 17.;
                let y = row as f32 * 32.;
                let c = if (col * 7 + row * 11) % 9 == 0 {
                    C::from_rgb(32, 42, 42)
                } else {
                    C::from_rgb(25, 35, 37)
                };
                p.rect_filled(box_(x, y, 32., 30.), 0., c);
            }
        }
        for i in 0..38 {
            let x = ((i * 137 + 31) % 780) as f32;
            let y = ((i * 79 + 45) % 460) as f32;
            p.line_segment(
                [point(pos2(x, y)), point(pos2(x + 7., y - 3.))],
                Stroke::new(scale, C::from_rgb(42, 49, 45)),
            );
        }
        text(
            pos2(22., 16.),
            "FRINGE OUTPOST  /  SECTOR 07",
            14.,
            C::from_rgb(129, 171, 166),
        );
        text(
            pos2(550., 16.),
            &format!(
                "◆ {:04}   /   {} CONTACTS",
                self.minerals as u32, self.kills
            ),
            14.,
            C::from_rgb(93, 211, 240),
        );
        let base = pos2(160., 245.);
        p.circle_filled(
            point(base + vec2(0., 12.)),
            64. * scale,
            C::from_rgb(16, 25, 27),
        );
        p.rect_filled(
            box_(107., 207., 106., 70.),
            4. * scale,
            C::from_rgb(66, 84, 88),
        );
        p.rect_stroke(
            box_(107., 207., 106., 70.),
            4. * scale,
            Stroke::new(2. * scale, C::from_rgb(118, 142, 145)),
            egui::StrokeKind::Inside,
        );
        p.rect_filled(
            box_(127., 192., 66., 55.),
            3. * scale,
            C::from_rgb(34, 76, 108),
        );
        p.rect_filled(box_(137., 202., 46., 22.), 1., C::from_rgb(68, 141, 173));
        for i in 0..4 {
            p.rect_filled(
                box_(117. + i as f32 * 25., 255., 12., 8.),
                0.,
                C::from_rgb(170, 133, 64),
            );
        }
        p.line_segment(
            [point(pos2(163., 194.)), point(pos2(163., 165.))],
            Stroke::new(2. * scale, C::GRAY),
        );
        p.circle_filled(
            point(pos2(163., 165.)),
            3. * scale,
            C::from_rgb(100, 225, 176),
        );
        p.rect_filled(box_(112., 291., 96., 3.), 0., C::from_rgb(46, 52, 52));
        p.rect_filled(
            box_(112., 291., self.hull * 0.96, 3.),
            0.,
            C::from_rgb(67, 188, 131),
        );
        // Mineral seam and autonomous haulers: a small world already at work.
        for i in 0..7 {
            let c = pos2(55. + (i % 3) as f32 * 21., 115. + (i / 3) as f32 * 18.);
            p.add(egui::Shape::convex_polygon(
                vec![
                    point(c + vec2(-9., 6.)),
                    point(c + vec2(-6., -11.)),
                    point(c + vec2(2., -21.)),
                    point(c + vec2(11., 0.)),
                    point(c + vec2(7., 10.)),
                ],
                C::from_rgb(43, 148 + i * 10, 207),
                Stroke::new(scale, C::from_rgb(102, 221, 244)),
            ));
        }
        for i in 0..4 {
            let t = (self.time * 0.055 + i as f32 * 0.25).fract();
            let a = (t * std::f32::consts::TAU).cos() * 0.5 + 0.5;
            let c = pos2(85. + a * 64., 157. + a * 60. + i as f32 * 7.);
            p.rect_filled(
                egui::Rect::from_center_size(point(c), vec2(13., 9.) * scale),
                2. * scale,
                C::from_rgb(177, 149, 79),
            );
            if t < 0.5 {
                p.circle_filled(point(c + vec2(0., -5.)), 3. * scale, C::LIGHT_BLUE);
            }
        }
        if let Some(dest) = self.destination {
            p.circle_stroke(
                point(dest),
                (9. + (self.time * 5.).sin() * 2.) * scale,
                Stroke::new(scale, C::from_rgb(85, 183, 135)),
            );
        }
        for u in &self.units {
            if u.selected {
                p.circle_stroke(
                    point(u.p + vec2(0., 3.)),
                    12. * scale,
                    Stroke::new(scale, C::from_rgb(63, 220, 138)),
                );
            }
            p.circle_filled(
                point(u.p + vec2(2., 5.)),
                8. * scale,
                C::from_rgb(13, 21, 25),
            );
            p.rect_filled(
                egui::Rect::from_center_size(point(u.p), vec2(12., 14.) * scale),
                3. * scale,
                C::from_rgb(51, 110, 173),
            );
            p.circle_filled(
                point(u.p + vec2(0., -7.)),
                4. * scale,
                C::from_rgb(131, 168, 176),
            );
            let facing = self
                .bugs
                .iter()
                .min_by(|a, b| a.p.distance(u.p).total_cmp(&b.p.distance(u.p)))
                .map(|b| (b.p - u.p).normalized())
                .unwrap_or(Vec2::X);
            p.line_segment(
                [point(u.p), point(u.p + facing * 14.)],
                Stroke::new(3. * scale, C::from_rgb(152, 163, 157)),
            );
            p.rect_filled(
                egui::Rect::from_min_size(
                    point(u.p + vec2(-9., -17.)),
                    vec2(u.hp * 0.18, 2.) * scale,
                ),
                0.,
                C::from_rgb(69, 206, 130),
            );
        }
        for b in &self.bugs {
            let wobble = (self.time * 14. + b.p.y).sin() * 3.;
            for sign in [-1., 1.] {
                p.line_segment(
                    [
                        point(b.p + vec2(-7., sign * 3.)),
                        point(b.p + vec2(-13. + wobble, sign * 10.)),
                    ],
                    Stroke::new(2. * scale, C::from_rgb(169, 92, 75)),
                );
            }
            p.circle_filled(point(b.p), 8. * scale, C::from_rgb(132, 56, 65));
            p.circle_filled(
                point(b.p + vec2(-5., 0.)),
                4. * scale,
                C::from_rgb(222, 139, 84),
            );
        }
        for s in &self.sparks {
            p.line_segment(
                [point(s.from), point(s.to)],
                Stroke::new(
                    1.5 * scale,
                    if s.friendly {
                        C::from_rgb(255, 220, 131)
                    } else {
                        C::from_rgb(233, 106, 88)
                    },
                ),
            );
            p.circle_filled(point(s.to), 3. * scale, C::from_rgb(253, 194, 105));
        }
        p.rect_filled(box_(0., 470., 800., 130.), 0., C::from_rgb(15, 24, 29));
        p.line_segment(
            [point(pos2(0., 470.)), point(pos2(800., 470.))],
            Stroke::new(2. * scale, C::from_rgb(83, 106, 108)),
        );
        // A live tactical map rather than a decorative placeholder.
        p.rect_filled(box_(18., 486., 160., 96.), 1., C::from_rgb(27, 43, 45));
        for u in &self.units {
            p.circle_filled(
                point(pos2(18. + u.p.x * 0.2, 486. + u.p.y * 0.2)),
                2. * scale,
                C::LIGHT_GREEN,
            );
        }
        for b in &self.bugs {
            p.circle_filled(
                point(pos2(18. + b.p.x * 0.2, 486. + b.p.y * 0.2)),
                2. * scale,
                C::LIGHT_RED,
            );
        }
        text(
            pos2(200., 490.),
            "OUTPOST OPERATIONS",
            13.,
            C::from_rgb(174, 188, 184),
        );
        text(
            pos2(200., 514.),
            &format!(
                "{} selected / {} on station",
                self.units.iter().filter(|u| u.selected).count(),
                self.units.len()
            ),
            12.,
            C::from_rgb(101, 191, 157),
        );
        text(
            pos2(200., 539.),
            "Click: select   ·   Right-click: move",
            11.,
            C::from_rgb(145, 164, 169),
        );
        text(
            pos2(200., 560.),
            "A: select all   Space: pause   R: restart",
            11.,
            C::from_rgb(145, 164, 169),
        );
        let recruit = box_(620., 495., 160., 38.);
        p.rect_filled(recruit, 3. * scale, C::from_rgb(35, 67, 77));
        p.rect_stroke(
            recruit,
            3. * scale,
            Stroke::new(scale, C::from_rgb(84, 131, 141)),
            egui::StrokeKind::Inside,
        );
        text(
            pos2(635., 506.),
            "REINFORCE  ◆ 50",
            12.,
            C::from_rgb(160, 211, 210),
        );
        text(
            pos2(623., 549.),
            if self.paused {
                "SIMULATION PAUSED"
            } else {
                "ALL YOUR BOX…"
            },
            11.,
            C::from_rgb(155, 143, 104),
        );
        if ui.input(|i| i.key_pressed(egui::Key::A)) {
            for u in &mut self.units {
                u.selected = true;
            }
        }
        if let Some(at) = response.interact_pointer_pos() {
            let local = pos2((at.x - origin.x) / scale, (at.y - origin.y) / scale);
            if response.clicked_by(egui::PointerButton::Secondary) && local.y < 470. {
                self.order(local);
            }
            if response.clicked() {
                if recruit.contains(at) && self.minerals >= 50. && self.units.len() < 24 {
                    self.minerals -= 50.;
                    let q = pos2(220., 265.);
                    self.units.push(Unit {
                        p: q,
                        target: q + vec2(40., 25.),
                        hp: 100.,
                        selected: true,
                        cooldown: 0.,
                    });
                } else if local.y < 470. {
                    let nearest = self
                        .units
                        .iter()
                        .enumerate()
                        .filter(|(_, u)| u.p.distance(local) < 22.)
                        .min_by(|(_, a), (_, b)| {
                            a.p.distance(local).total_cmp(&b.p.distance(local))
                        })
                        .map(|(i, _)| i);
                    for (i, u) in self.units.iter_mut().enumerate() {
                        u.selected = Some(i) == nearest;
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_entry_only() {
        assert!(matches(" there is no cow level "));
        assert!(!matches("cow level"));
        assert!(!matches("there is no cow level please"));
    }
    #[test]
    fn orders_and_pause_are_local() {
        let mut f = Field::default();
        f.order(pos2(500., 350.));
        let old = f.units[0].p;
        f.step(0.05);
        assert!(f.units[0].p.distance(old) > 0.);
        f.paused = true;
        let old = f.units[0].p;
        f.step(0.05);
        assert_eq!(old, f.units[0].p);
    }
    #[test]
    fn bounded_scene_keeps_running() {
        let mut f = Field::default();
        for _ in 0..30000 {
            f.step(0.05);
        }
        assert!(f.bugs.len() <= 24 && f.units.len() <= 24 && f.sparks.len() < 100);
        assert!(f.hull >= 10.);
    }
}
