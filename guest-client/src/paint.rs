use fontdue::{Font, FontSettings, Metrics};
use image::RgbaImage;
use softer_gui::Framebuffer;
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(self, (x, y): (i32, i32)) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }

    pub fn inset(self, margin: i32) -> Self {
        Self::new(
            self.x + margin,
            self.y + margin,
            self.w - 2 * margin,
            self.h - 2 * margin,
        )
    }
}

pub struct Type {
    font: Font,
    glyphs: HashMap<(char, u16), (Metrics, Vec<u8>)>,
}

impl Type {
    pub fn load() -> Result<Self, &'static str> {
        Font::from_bytes(
            include_bytes!("../assets/DM-Sans.ttf").as_slice(),
            FontSettings::default(),
        )
        .map(|font| Self {
            font,
            glyphs: HashMap::new(),
        })
    }

    pub fn width(&self, text: &str, size: i32) -> i32 {
        text.chars()
            .map(|ch| self.advance(ch, size))
            .sum::<f32>()
            .ceil() as i32
    }

    fn advance(&self, ch: char, size: i32) -> f32 {
        self.font.metrics(ch, size as f32).advance_width
    }

    pub fn wrap(&self, text: &str, width: i32, size: i32) -> Vec<String> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            let mut used = 0.0;
            for word in paragraph.split_inclusive(char::is_whitespace) {
                let word_width = self.width(word, size) as f32;
                if used > 0.0 && used + word_width > width as f32 {
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                }
                if word_width <= width as f32 {
                    line.push_str(word);
                    used += word_width;
                    continue;
                }
                for ch in word.chars() {
                    let advance = self.advance(ch, size);
                    if used > 0.0 && used + advance > width as f32 {
                        lines.push(std::mem::take(&mut line));
                        used = 0.0;
                    }
                    line.push(ch);
                    used += advance;
                }
            }
            lines.push(line);
        }
        lines
    }

    pub fn ellipsis(&self, text: &str, width: i32, size: i32) -> String {
        if self.width(text, size) <= width {
            return text.into();
        }
        let mut kept = String::new();
        let mut available = (width - self.width("…", size)) as f32;
        for ch in text.chars() {
            available -= self.advance(ch, size);
            if available < 0.0 {
                break;
            }
            kept.push(ch);
        }
        kept.push('…');
        kept
    }
}

pub struct Canvas<'a> {
    pub width: i32,
    pub height: i32,
    pub clip: Rect,
    scale: f32,
    side: usize,
    pixels: &'a mut [u32],
}

impl<'a> Canvas<'a> {
    pub fn new(frame: &'a mut Framebuffer, scale: f32) -> Self {
        let width = (frame.width as f32 / scale).ceil() as i32;
        let height = (frame.height as f32 / scale).ceil() as i32;
        let side = frame.side;
        Self {
            width,
            height,
            scale,
            side,
            clip: Rect::new(0, 0, width, height),
            pixels: frame.slice(),
        }
    }

    fn px(&self, point: i32) -> i32 {
        (point as f32 * self.scale).round() as i32
    }

    fn physical(&self, rect: Rect) -> Rect {
        let (left, top) = (self.px(rect.x), self.px(rect.y));
        Rect::new(
            left,
            top,
            self.px(rect.x + rect.w) - left,
            self.px(rect.y + rect.h) - top,
        )
    }

    pub fn fill(&mut self, rect: Rect, color: u32) {
        self.round(rect, 0, color);
    }

    pub fn round(&mut self, rect: Rect, radius: i32, color: u32) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let rect = self.physical(rect);
        let clip = self.physical(self.clip);
        let r = self.px(radius).min(rect.w / 2).min(rect.h / 2);
        for y in
            rect.y.max(clip.y).max(0)..(rect.y + rect.h).min(clip.y + clip.h).min(self.side as i32)
        {
            let dy = (rect.y + r - y).max(y - (rect.y + rect.h - r - 1)).max(0);
            let inset = if dy == 0 || r == 0 {
                0
            } else {
                r - ((r * r - dy * dy).max(0) as f32).sqrt() as i32
            };
            let left = (rect.x + inset).max(clip.x).max(0);
            let right = (rect.x + rect.w - inset)
                .min(clip.x + clip.w)
                .min(self.side as i32);
            if left < right {
                let row = y as usize * self.side;
                self.pixels[row + left as usize..row + right as usize].fill(color);
            }
        }
    }

    pub fn text(&mut self, kind: &mut Type, x: i32, top: i32, size: i32, text: &str, color: u32) {
        let font_size = self.px(size).max(1) as u16;
        let mut pen = self.px(x) as f32;
        let baseline = self.px(top) + (font_size as f32 * 0.83).round() as i32;
        let clip = self.physical(self.clip);
        if kind.glyphs.len() > 2400 {
            kind.glyphs.clear();
        }
        for ch in text.chars() {
            let (metrics, pixels) = kind
                .glyphs
                .entry((ch, font_size))
                .or_insert_with(|| kind.font.rasterize(ch, f32::from(font_size)));
            let x = pen.round() as i32 + metrics.xmin;
            let y = baseline - metrics.ymin - metrics.height as i32;
            for (row, values) in pixels.chunks_exact(metrics.width.max(1)).enumerate() {
                for (col, &alpha) in values.iter().enumerate() {
                    let p = (x + col as i32, y + row as i32);
                    if alpha > 0 && clip.contains(p) {
                        self.pixel(p, color, alpha);
                    }
                }
            }
            pen += metrics.advance_width;
            if pen as i32 >= clip.x + clip.w {
                break;
            }
        }
    }

    pub fn image(&mut self, rect: Rect, image: &RgbaImage) {
        let rect = self.physical(rect);
        let clip = self.physical(self.clip);
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let (width, height) = image.dimensions();
        for y in rect.y.max(clip.y)..(rect.y + rect.h).min(clip.y + clip.h) {
            for x in rect.x.max(clip.x)..(rect.x + rect.w).min(clip.x + clip.w) {
                let sample = image
                    .get_pixel(
                        (x - rect.x) as u32 * width / rect.w as u32,
                        (y - rect.y) as u32 * height / rect.h as u32,
                    )
                    .0;
                let [r, g, b, a] = sample.map(u32::from);
                self.pixel((x, y), 0xff000000 | r << 16 | g << 8 | b, a as u8);
            }
        }
    }

    fn pixel(&mut self, (x, y): (i32, i32), fg: u32, alpha: u8) {
        if x < 0 || y < 0 || x >= self.side as i32 || y >= self.side as i32 {
            return;
        }
        let pixel = &mut self.pixels[y as usize * self.side + x as usize];
        let alpha = u32::from(alpha);
        let mix = |shift: u32| {
            let f = (fg >> shift) & 255u32;
            let b = (*pixel >> shift) & 255u32;
            ((f * alpha + b * (255 - alpha) + 127) / 255) << shift
        };
        *pixel = 0xff000000 | mix(16) | mix(8) | mix(0);
    }
}
