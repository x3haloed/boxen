//! Window and keyboard feasibility probe for the portable guest client.
use font8x8::{BASIC_FONTS, UnicodeFonts};
use softer_gui::{
    AXIS_MOUSE_X, AXIS_MOUSE_Y, BTN_LEFT, EVENT_AXES, EVENT_BUTTONS, EVENT_CLOSE, EVENT_RENDER,
    EVENT_TEXT, Event, Framebuffer, KEY_BACKSPACE, KEY_ENTER, KEY_LEFTSHIFT, KEY_RIGHTSHIFT,
};
use std::collections::VecDeque;

const KEY_KPENTER: u32 = 96;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut gui = softer_gui::open("Boxen Guest Client", "org.boxen.guest-client", 620, 440)
        .ok_or("Could not open the guest client window")?;
    let mut chat = LocalChat::default();
    let mut event = Event::default();
    let mut buttons = [0; 8];
    let mut dirty = true;
    let mut pointer = (0, 0);
    let mut previous_size = (0, 0);
    gui.request_frame();
    loop {
        gui.wait();
        while gui.next_event(&mut event) {
            match event.kind {
                EVENT_CLOSE => return Ok(()),
                EVENT_TEXT => {
                    for ch in event.text.into_iter().take(event.text_len as usize) {
                        if !ch.is_control() && chat.draft.len() + ch.len_utf8() <= 8192 {
                            chat.draft.push(ch);
                            dirty = true;
                        }
                    }
                }
                EVENT_AXES => {
                    for axis in event.axes() {
                        match axis.axis {
                            AXIS_MOUSE_X => pointer.0 = i64::from(axis.delta >> 8),
                            AXIS_MOUSE_Y => pointer.1 = i64::from(axis.delta >> 8),
                            _ => {}
                        }
                    }
                }
                EVENT_BUTTONS => {
                    if [KEY_ENTER, KEY_KPENTER]
                        .iter()
                        .any(|&key| pressed(&event.bits, key) && !pressed(&buttons, key))
                    {
                        if pressed(&event.bits, KEY_LEFTSHIFT)
                            || pressed(&event.bits, KEY_RIGHTSHIFT)
                        {
                            if chat.draft.len() < 8192 {
                                chat.draft.push('\n');
                            }
                        } else {
                            chat.send();
                        }
                        dirty = true;
                    }
                    if pressed(&event.bits, KEY_BACKSPACE) && !pressed(&buttons, KEY_BACKSPACE) {
                        chat.draft.pop();
                        dirty = true;
                    }
                    if event.button(BTN_LEFT)
                        && !pressed(&buttons, BTN_LEFT)
                        && hits_send(previous_size, pointer)
                    {
                        chat.send();
                        dirty = true;
                    }
                    buttons = event.bits;
                }
                EVENT_RENDER => {
                    let size = (event.width, event.height);
                    pointer = (event.cursor_x >> 32, event.cursor_y >> 32);
                    if dirty || size != previous_size {
                        let mut frame = gui.get_framebuffer();
                        if frame.ok() {
                            chat.draw(&mut frame);
                            gui.submit();
                            previous_size = size;
                            dirty = false;
                        }
                    }
                }
                _ => {}
            }
        }
        if dirty {
            gui.request_frame();
        }
    }
}

fn pressed(bits: &[u64; 8], key: u32) -> bool {
    bits[(key / 64) as usize] & (1 << (key % 64)) != 0
}

#[derive(Default)]
struct LocalChat {
    draft: String,
    history: VecDeque<(bool, String)>,
}

impl LocalChat {
    fn send(&mut self) {
        if self.draft.trim().is_empty() {
            return;
        }
        let message = std::mem::take(&mut self.draft);
        self.history.push_back((true, message.clone()));
        self.history.push_back((false, message));
        if self.history.len() > 64 {
            self.history.drain(..2);
        }
    }

    fn draw(&self, frame: &mut Framebuffer) {
        let width = frame.width;
        let height = frame.height;
        let side = frame.side;
        let mut screen = Screen {
            width,
            height,
            side,
            pixels: frame.slice(),
        };
        screen.band(0, height, 0xfff4f7f6);
        screen.band(0, 76, 0xff18483a);
        screen.text(20, 20, "Boxen Guest Client", 0xffffffff);
        screen.text(20, 46, "Local loopback", 0xffd2eee4);
        let composer = height.saturating_sub(102);
        screen.band(composer, height, 0xffffffff);
        screen.text(20, composer + 12, "Message", 0xff18483a);
        if let Some((x, y)) = send_button_at(width, height) {
            screen.rect(x, y, 90, 28, 0xff18483a);
            screen.text(x + 12, y + 6, "Send", 0xffffffff);
        }
        screen.text(
            20,
            height.saturating_sub(22),
            "Enter sends / Shift+Enter adds a line",
            0xff526663,
        );

        let columns = width.saturating_sub(40).div_ceil(16).max(1);
        let mut transcript = Vec::new();
        for (sent, message) in &self.history {
            let author = if *sent { "You" } else { "Echo" };
            transcript.extend(lines(&format!("{author}  {message}"), columns));
        }
        let visible = composer.saturating_sub(92) / 20;
        for (row, text) in transcript
            .iter()
            .skip(transcript.len().saturating_sub(visible))
            .enumerate()
        {
            screen.text(20, 90 + row * 20, text, 0xff202d2b);
        }
        let draft = lines(&self.draft, columns);
        for (row, text) in draft.iter().skip(draft.len().saturating_sub(2)).enumerate() {
            screen.text(20, composer + 36 + row * 20, text, 0xff202d2b);
        }
    }
}

fn lines(text: &str, columns: usize) -> Vec<String> {
    let mut wrapped = Vec::new();
    for paragraph in text.split('\n') {
        let chars: Vec<_> = paragraph.chars().collect();
        if chars.is_empty() {
            wrapped.push(String::new());
        } else {
            wrapped.extend(
                chars
                    .chunks(columns)
                    .map(|chunk| chunk.iter().collect::<String>()),
            );
        }
    }
    wrapped
}

fn send_button_at(width: usize, height: usize) -> Option<(usize, usize)> {
    (width >= 250 && height >= 102).then(|| (width - 110, height - 96))
}

fn hits_send((width, height): (u32, u32), (pointer_x, pointer_y): (i64, i64)) -> bool {
    send_button_at(width as usize, height as usize).is_some_and(|(x, y)| {
        let (x, y) = (x as i64, y as i64);
        (x..x + 90).contains(&pointer_x) && (y..y + 28).contains(&pointer_y)
    })
}

struct Screen<'a> {
    width: usize,
    height: usize,
    side: usize,
    pixels: &'a mut [u32],
}

impl Screen<'_> {
    fn band(&mut self, top: usize, bottom: usize, color: u32) {
        for row in top..bottom.min(self.height) {
            let start = row * self.side;
            self.pixels[start..start + self.width].fill(color);
        }
    }

    fn rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u32) {
        for row in y..y + height {
            let start = row * self.side + x;
            self.pixels[start..start + width].fill(color);
        }
    }

    fn text(&mut self, mut x: usize, y: usize, text: &str, color: u32) {
        for ch in text.chars() {
            if x + 16 > self.width || y + 16 > self.height {
                break;
            }
            let glyph = BASIC_FONTS
                .get(ch)
                .or_else(|| BASIC_FONTS.get('?'))
                .unwrap_or([0; 8]);
            for (row, bits) in glyph.into_iter().enumerate() {
                for column in 0..8 {
                    if bits & (1 << column) != 0 {
                        let index = (y + 2 * row) * self.side + x + 2 * column;
                        self.pixels[index..index + 2].fill(color);
                        self.pixels[index + self.side..index + self.side + 2].fill(color);
                    }
                }
            }
            x += 16;
        }
    }
}
