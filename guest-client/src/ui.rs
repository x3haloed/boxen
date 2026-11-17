use crate::{
    markdown::Layout as Markdown,
    paint::{Canvas, Rect, Type},
    session::{Entry, Session},
    setup::Installer,
};
use boxen_guest_transport::{
    Attachment, FILE_LIMIT, GUEST_BODY_LIMIT, MAX_FILES, Presence, Result, new_id,
};
use image::{ImageReader, RgbaImage};
use softer_gui::{
    AXIS_MOUSE_X, AXIS_MOUSE_Y, AXIS_SCROLL_V, AXIS_WHEEL_V, BTN_LEFT, CP_COPY, CP_CUT, CP_PASTE,
    EVENT_AXES, EVENT_BUTTONS, EVENT_CLOSE, EVENT_COPYPASTE, EVENT_RENDER, EVENT_TEXT, Event,
    FileEvent, Gui, KEY_BACKSPACE, KEY_DELETE, KEY_END, KEY_ENTER, KEY_ESC, KEY_HOME, KEY_LEFT,
    KEY_LEFTCTRL, KEY_LEFTMETA, KEY_LEFTSHIFT, KEY_PAGEDOWN, KEY_PAGEUP, KEY_RIGHT, KEY_RIGHTCTRL,
    KEY_RIGHTMETA, KEY_RIGHTSHIFT, KEY_TAB, WHEEL_CLICK,
};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const PAPER: u32 = 0xfff6f7f2;
const WHITE: u32 = 0xffffffff;
const INK: u32 = 0xff1c3029;
const MUTED: u32 = 0xff50665b;
const BORDER: u32 = 0xffdbe3d9;
const GREEN: u32 = 0xff1c5944;
const MINT: u32 = 0xffe2eee2;
const TEXT_SIZE: i32 = 16;
const LEADING: i32 = 22;

pub fn run(session: Option<Session>, installer: Option<Installer>) -> Result<()> {
    let mut gui = softer_gui::open("Boxen Guest Chat", "org.boxen.guest-client", 790, 650)
        .ok_or("Could not open the guest client window")?;
    let mut font = Type::load().map_err(|_| "Could not load guest chat typography")?;
    let mut chat = Chat::new(session);
    chat.setup = installer.map(Setup::new);
    let mut event = Event::default();
    let mut buttons = [0; 8];
    let mut dirty = true;
    let mut pointer = (0, 0);
    let mut previous_size = (0, 0, 0);
    let mut scale = 1.0;
    gui.request_frame();
    loop {
        gui.wait_ms(150);
        dirty |= chat.refresh();
        while let Some(file_event) = gui.next_file_event() {
            chat.file_event(file_event);
            dirty = true;
        }
        while gui.next_event(&mut event) {
            let rising = |code| event.button(code) && !held(&buttons, code);
            match event.kind {
                EVENT_CLOSE => return Ok(()),
                EVENT_TEXT
                    if (chat.preview.is_none() || chat.picker.is_some())
                        && !chat.checking_setup() =>
                {
                    let text: String = event
                        .text()
                        .iter()
                        .copied()
                        .filter(|ch| !ch.is_control())
                        .collect();
                    if !text.is_empty() {
                        chat.editor().insert(&text);
                        dirty = true;
                    }
                }
                EVENT_COPYPASTE
                    if (chat.preview.is_none() || chat.picker.is_some())
                        && !chat.checking_setup() =>
                {
                    let clipboard = gui.clipboard();
                    match event.action {
                        CP_PASTE => {
                            if let Some(text) = clipboard.get() {
                                let text: String = text
                                    .chars()
                                    .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
                                    .collect();
                                chat.editor().insert(&text);
                            }
                        }
                        CP_COPY | CP_CUT => {
                            if let Some(text) = chat.editor().selected()
                                && clipboard.set(text)
                                && event.action == CP_CUT
                            {
                                chat.editor().insert("");
                            }
                        }
                        _ => {}
                    }
                    dirty = true;
                }
                EVENT_AXES => {
                    for axis in event.axes() {
                        match axis.axis {
                            AXIS_MOUSE_X => pointer.0 = axis.delta >> 8,
                            AXIS_MOUSE_Y => pointer.1 = axis.delta >> 8,
                            AXIS_WHEEL_V | AXIS_SCROLL_V
                                if chat.preview.is_none() || chat.picker.is_some() =>
                            {
                                let delta = if axis.axis == AXIS_WHEEL_V {
                                    axis.delta as f32 / WHEEL_CLICK as f32 * 54.0
                                } else {
                                    -(axis.delta as f32) / 256.0 / scale
                                };
                                if let Some(picker) = &mut chat.picker {
                                    picker.scroll((-delta / 26.0).round() as i32);
                                } else {
                                    chat.scroll =
                                        chat.scroll.saturating_add(delta.round() as i32).max(0);
                                }
                                dirty = true;
                            }
                            _ => {}
                        }
                    }
                }
                EVENT_BUTTONS => {
                    let shifted = event.button(KEY_LEFTSHIFT) || event.button(KEY_RIGHTSHIFT);
                    let shortcut = [KEY_LEFTCTRL, KEY_RIGHTCTRL, KEY_LEFTMETA, KEY_RIGHTMETA]
                        .iter()
                        .any(|&key| event.button(key));
                    if rising(BTN_LEFT) {
                        chat.focus = None;
                        chat.click(
                            (
                                (pointer.0 as f32 / scale) as i32,
                                (pointer.1 as f32 / scale) as i32,
                            ),
                            &mut gui,
                        );
                        dirty = true;
                    }
                    if rising(KEY_TAB) {
                        chat.tab(shifted);
                        dirty = true;
                    } else if !shifted
                        && (rising(KEY_ENTER) || rising(96))
                        && let Some(action) = chat.focus.take()
                    {
                        chat.activate(action, &mut gui);
                        dirty = true;
                    } else if rising(KEY_ESC)
                        && (chat.picker.take().is_some() || chat.preview.take().is_some())
                    {
                        chat.focus = None;
                        dirty = true;
                    } else if chat.preview.is_none() || chat.picker.is_some() {
                        let key = [
                            KEY_ENTER,
                            96,
                            KEY_BACKSPACE,
                            KEY_DELETE,
                            KEY_LEFT,
                            KEY_RIGHT,
                            KEY_HOME,
                            KEY_END,
                            KEY_PAGEUP,
                            KEY_PAGEDOWN,
                            30,
                            24,
                        ]
                        .into_iter()
                        .find(|&code| rising(code));
                        if let Some(key) = key {
                            if chat.picker.is_some() {
                                match key {
                                    KEY_ENTER | 96 => chat.picker_confirm(),
                                    KEY_PAGEUP => chat.picker.as_mut().unwrap().scroll(-8),
                                    KEY_PAGEDOWN => chat.picker.as_mut().unwrap().scroll(8),
                                    _ => chat.editor().key(key, shifted, shortcut),
                                }
                                dirty = true;
                                buttons = event.bits;
                                continue;
                            }
                            if chat.setup.is_some() {
                                match key {
                                    KEY_ENTER | 96 if shortcut => chat.connect_setup(),
                                    KEY_ENTER | 96 if !chat.checking_setup() => {
                                        chat.editor().insert("\n")
                                    }
                                    24 if shortcut => chat.pick(&mut gui),
                                    _ if !chat.checking_setup() => {
                                        chat.editor().key(key, shifted, shortcut)
                                    }
                                    _ => {}
                                }
                                dirty = true;
                                buttons = event.bits;
                                continue;
                            }
                            match key {
                                KEY_ENTER | 96 if shifted => chat.draft.insert("\n"),
                                KEY_ENTER | 96 => chat.send(),
                                KEY_PAGEUP => chat.scroll = chat.scroll.saturating_add(240),
                                KEY_PAGEDOWN => chat.scroll = (chat.scroll - 240).max(0),
                                24 if shortcut => chat.pick(&mut gui),
                                _ => chat.draft.key(key, shifted, shortcut),
                            }
                            dirty = true;
                        }
                    }
                    buttons = event.bits;
                }
                EVENT_RENDER => {
                    let size = (event.width, event.height, event.scale_fx);
                    pointer = ((event.cursor_x >> 32) as i32, (event.cursor_y >> 32) as i32);
                    scale = (event.scale_fx as f32 / 65536.0).clamp(1.0, 4.0);
                    if dirty || size != previous_size {
                        let mut frame = gui.get_framebuffer();
                        if frame.ok() {
                            chat.draw(&mut Canvas::new(&mut frame, scale), &mut font);
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

fn held(bits: &[u64; 8], code: u32) -> bool {
    bits[(code / 64) as usize] & (1 << (code % 64)) != 0
}

#[derive(Default)]
struct Draft {
    text: String,
    caret: usize,
    anchor: usize,
}

impl Draft {
    fn range(&self) -> std::ops::Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }
    fn selected(&self) -> Option<&str> {
        (self.caret != self.anchor).then(|| &self.text[self.range()])
    }
    fn insert(&mut self, text: &str) {
        let range = self.range();
        let capacity = GUEST_BODY_LIMIT - (self.text.len() - range.len());
        let end = text.floor_char_boundary(text.len().min(capacity));
        self.caret = range.start + end;
        self.anchor = self.caret;
        self.text.replace_range(range, &text[..end]);
    }
    fn key(&mut self, key: u32, shift: bool, shortcut: bool) {
        let previous = self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        let next = self.caret
            + self.text[self.caret..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        let selected = self.caret != self.anchor;
        match key {
            30 if shortcut => {
                self.anchor = 0;
                self.caret = self.text.len();
                return;
            }
            KEY_BACKSPACE | KEY_DELETE => {
                if !selected {
                    self.anchor = if key == KEY_BACKSPACE { previous } else { next };
                }
                self.insert("");
                return;
            }
            KEY_LEFT => {
                self.caret = if selected && !shift {
                    self.range().start
                } else {
                    previous
                }
            }
            KEY_RIGHT => {
                self.caret = if selected && !shift {
                    self.range().end
                } else {
                    next
                }
            }
            KEY_HOME => {
                self.caret = self.text[..self.caret]
                    .rfind('\n')
                    .map_or(0, |index| index + 1)
            }
            KEY_END => {
                self.caret += self.text[self.caret..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.caret)
            }
            _ => return,
        }
        if !shift {
            self.anchor = self.caret;
        }
    }
}

struct DraftFile {
    name: String,
    bytes: Arc<[u8]>,
    thumbnail: Option<RgbaImage>,
}

struct Preview {
    name: String,
    bytes: Arc<[u8]>,
    picture: RgbaImage,
}

#[derive(Clone, PartialEq, Eq)]
enum Click {
    Pick,
    Send,
    Remove(usize),
    DraftImage(usize),
    Open(Attachment),
    Link(Arc<str>),
    ClosePreview,
    SavePreview,
    Latest,
    Composer,
    PickerRow(usize),
    PickerParent,
    PickerConfirm,
    PickerCancel,
    PasteDetails,
    ConnectSetup,
}

struct Picker {
    directory: PathBuf,
    entries: Vec<(String, PathBuf, bool)>,
    selected: Vec<PathBuf>,
    input: Draft,
    save: Option<Arc<[u8]>>,
    offset: usize,
    message: Option<String>,
}

impl Picker {
    fn open(save: Option<(String, Arc<[u8]>)>) -> Self {
        let directory = std::env::current_dir()
            .ok()
            .filter(|path| path.is_dir())
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from("."));
        let mut picker = Self {
            directory: PathBuf::new(),
            entries: Vec::new(),
            selected: Vec::new(),
            input: Draft::default(),
            save: None,
            offset: 0,
            message: None,
        };
        picker.browse(boxen_guest_transport::host_path(&directory));
        if let Some((name, bytes)) = save {
            picker.input.insert(&name);
            picker.input.anchor = 0;
            picker.save = Some(bytes);
        }
        picker
    }

    fn browse(&mut self, path: PathBuf) {
        let Ok(directory) = fs::read_dir(&path) else {
            self.message = Some("This folder could not be opened.".into());
            return;
        };
        self.entries = directory
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let kind = entry.file_type().ok()?;
                let name = entry.file_name().to_string_lossy().into_owned();
                Some((name, entry.path(), kind.is_dir()))
            })
            .take(20_000)
            .collect();
        self.entries
            .sort_by(|a, b| (b.2, a.0.to_lowercase()).cmp(&(a.2, b.0.to_lowercase())));
        self.directory = path;
        self.offset = 0;
        self.message = None;
    }

    fn scroll(&mut self, steps: i32) {
        self.offset = self
            .offset
            .saturating_add_signed(steps as isize)
            .min(self.entries.len().saturating_sub(1));
    }

    fn path(&self) -> PathBuf {
        let text = self.input.text.trim();
        let path = boxen_guest_transport::host_path(Path::new(text));
        if path.is_absolute() || text.as_bytes().get(1) == Some(&b':') {
            path
        } else {
            self.directory.join(path)
        }
    }

    fn select(&mut self, index: usize) {
        let Some((name, path, directory)) = self.entries.get(index).cloned() else {
            return;
        };
        if directory {
            self.browse(path);
            return;
        }
        self.message = None;
        if self.save.is_some() {
            self.input = Draft::default();
            self.input.insert(&name);
        } else if let Some(index) = self.selected.iter().position(|other| *other == path) {
            self.selected.remove(index);
        } else if self.selected.len() < MAX_FILES {
            self.selected.push(path);
        }
    }
}

struct Chat {
    session: Option<Session>,
    history: Arc<Vec<Entry>>,
    revision: u64,
    resident: String,
    presence: Presence,
    connection: String,
    notice: Option<String>,
    scroll: i32,
    history_bounds: (usize, i32, i32),
    history_layout: HashMap<usize, Arc<Markdown>>,
    bubble_heights: Vec<i32>,
    draft: Draft,
    draft_files: Vec<DraftFile>,
    thumbnails: HashMap<String, Option<RgbaImage>>,
    preview: Option<Preview>,
    saving: Option<Arc<[u8]>>,
    hits: Vec<(Rect, Click)>,
    focus: Option<Click>,
    picker: Option<Picker>,
    setup: Option<Setup>,
}

struct Setup {
    installer: Installer,
    credential: Option<PathBuf>,
    details: Draft,
    checking: bool,
    error: Option<String>,
}

impl Setup {
    fn new(installer: Installer) -> Self {
        Self {
            installer,
            credential: None,
            details: Draft::default(),
            checking: false,
            error: None,
        }
    }

    fn ready(&self) -> bool {
        !self.checking && self.credential.is_some() && !self.details.text.trim().is_empty()
    }
}

impl Chat {
    fn new(session: Option<Session>) -> Self {
        Self {
            session,
            history: Arc::default(),
            revision: 0,
            resident: "Guest chat".into(),
            presence: Presence::Offline,
            connection: "Connecting".into(),
            notice: None,
            scroll: 0,
            history_bounds: (0, 0, 0),
            history_layout: HashMap::new(),
            bubble_heights: Vec::new(),
            draft: Draft::default(),
            draft_files: Vec::new(),
            thumbnails: HashMap::new(),
            preview: None,
            saving: None,
            hits: Vec::new(),
            focus: None,
            picker: None,
            setup: None,
        }
    }

    fn refresh(&mut self) -> bool {
        if let Some(setup) = &mut self.setup {
            if setup.checking
                && let Some(result) = setup.installer.result()
            {
                match result {
                    Ok(session) => *self = Self::new(Some(session)),
                    Err(error) => {
                        setup.error = Some(error.to_string());
                        setup.checking = false;
                    }
                }
                return true;
            }
            return false;
        }
        let Some(view) = self
            .session
            .as_ref()
            .and_then(|session| session.view_since(self.revision))
        else {
            return false;
        };
        self.revision = view.revision;
        self.history = view.history;
        if let Some(resident) = view.resident {
            self.resident = resident;
        }
        self.presence = view.presence;
        self.connection = if view.pending == 0 {
            view.status.into()
        } else {
            format!("{} · {} queued", view.status, view.pending)
        };
        true
    }

    fn send(&mut self) {
        if self.draft.text.trim().is_empty() && self.draft_files.is_empty() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let files = self
            .draft_files
            .iter()
            .map(|file| (file.name.clone(), file.bytes.as_ref()));
        if let Err(error) = session.send(&self.draft.text, files) {
            self.notice = Some(format!("Message kept in draft. {error}"));
            return;
        }
        self.refresh();
        self.draft = Draft::default();
        self.draft_files.clear();
        self.scroll = 0;
        self.notice = None;
    }

    fn pick(&mut self, gui: &mut Gui) {
        if self.checking_setup() {
            return;
        }
        if !gui.pick_files() {
            self.picker = Some(Picker::open(None));
        }
    }

    fn editor(&mut self) -> &mut Draft {
        if let Some(picker) = &mut self.picker {
            &mut picker.input
        } else if let Some(setup) = &mut self.setup {
            &mut setup.details
        } else {
            &mut self.draft
        }
    }

    fn checking_setup(&self) -> bool {
        self.setup.as_ref().is_some_and(|setup| setup.checking)
    }

    fn connect_setup(&mut self) {
        let Some(setup) = self.setup.as_mut().filter(|setup| setup.ready()) else {
            return;
        };
        let Some(path) = setup.credential.clone() else {
            return;
        };
        setup.error = None;
        match setup.installer.connect(setup.details.text.clone(), path) {
            Ok(()) => setup.checking = true,
            Err(error) => setup.error = Some(error.to_string()),
        }
    }

    fn picker_confirm(&mut self) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let path = picker.path();
        if (picker.save.is_some() || !picker.input.text.trim().is_empty()) && path.is_dir() {
            picker.browse(path);
            return;
        }
        if let Some(bytes) = &picker.save {
            if path.exists() {
                picker.message = Some("A file with that name already exists.".into());
                return;
            }
            if save_copy(&path, bytes).is_err() {
                picker.message = Some("The attachment could not be saved.".into());
                return;
            }
            self.notice = Some("Attachment saved.".into());
            self.picker = None;
        } else {
            let paths = if picker.input.text.trim().is_empty() {
                std::mem::take(&mut picker.selected)
            } else {
                vec![path]
            };
            if paths.is_empty() {
                return;
            }
            self.picker = None;
            self.file_event(FileEvent::Added(paths));
        }
    }

    fn file_event(&mut self, event: FileEvent) {
        match event {
            FileEvent::Added(paths) => {
                if paths.is_empty() {
                    return;
                }
                if let Some(setup) = &mut self.setup {
                    if !setup.checking {
                        match paths.as_slice() {
                            [path] => {
                                setup.credential = Some(path.clone());
                                setup.error = None;
                            }
                            _ => setup.error = Some("Choose one credential file.".into()),
                        }
                    }
                    return;
                }
                self.notice = None;
                for path in paths {
                    if self.draft_files.len() == MAX_FILES {
                        self.notice = Some("Each message can include up to eight files.".into());
                        break;
                    }
                    match read_draft(&path) {
                        Ok(file) => self.draft_files.push(file),
                        Err(_) => {
                            self.notice = Some("Choose a readable file of at most 32 MiB.".into())
                        }
                    }
                }
            }
            FileEvent::Save(path) => {
                if let (Some(bytes), Some(path)) = (self.saving.take(), path) {
                    self.notice = Some(if save_copy(&path, &bytes).is_ok() {
                        "Attachment saved.".into()
                    } else {
                        "The attachment could not be saved.".into()
                    });
                }
            }
        }
    }

    fn bytes(&self, file: &Attachment) -> Result<Arc<[u8]>> {
        let session = self.session.as_ref().ok_or("Attachment unavailable")?;
        Ok(session.read_file(file)?.into())
    }

    fn view_file(&mut self, name: String, bytes: Arc<[u8]>, gui: &mut Gui, save_nonimage: bool) {
        if let Some(picture) = picture(&bytes, 1600) {
            self.preview = Some(Preview {
                name,
                bytes,
                picture,
            });
        } else if save_nonimage {
            self.save(name, bytes, gui);
        }
    }

    fn save(&mut self, name: String, bytes: Arc<[u8]>, gui: &mut Gui) {
        if self.saving.is_none() && gui.save_file(name.clone()) {
            self.saving = Some(bytes);
        } else if self.saving.is_none() {
            self.picker = Some(Picker::open(Some((name, bytes))));
        }
    }

    fn click(&mut self, point: (i32, i32), gui: &mut Gui) {
        let Some(click) = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, click)| click.clone())
        else {
            return;
        };
        self.activate(click, gui);
    }

    fn tab(&mut self, reverse: bool) {
        if self.hits.is_empty() {
            return;
        }
        let mut links = std::collections::HashSet::new();
        let stops: Vec<_> = self
            .hits
            .iter()
            .filter(|(_, click)| {
                if let Click::Link(url) = click {
                    links.insert(url.as_ref())
                } else {
                    true
                }
            })
            .collect();
        let active = stops
            .iter()
            .position(|(_, click)| Some(click) == self.focus.as_ref())
            .or_else(|| {
                stops
                    .iter()
                    .position(|(_, click)| *click == Click::Composer)
            });
        let index = match active {
            Some(index) if reverse => (index + stops.len() - 1) % stops.len(),
            Some(index) => (index + 1) % stops.len(),
            None if reverse => stops.len() - 1,
            None => 0,
        };
        self.focus = if stops[index].1 == Click::Composer {
            None
        } else {
            Some(stops[index].1.clone())
        };
    }

    fn activate(&mut self, click: Click, gui: &mut Gui) {
        match click {
            Click::Pick => self.pick(gui),
            Click::PasteDetails => {
                if let Some(setup) = self.setup.as_mut().filter(|setup| !setup.checking)
                    && let Some(text) = gui.clipboard().get()
                {
                    setup.details = Draft::default();
                    setup.details.insert(&text.replace('\r', ""));
                    setup.error = None;
                }
            }
            Click::ConnectSetup => self.connect_setup(),
            Click::Send => self.send(),
            Click::Remove(index) if index < self.draft_files.len() => {
                self.draft_files.remove(index);
                self.notice = None;
            }
            Click::DraftImage(index) => {
                if let Some(file) = self.draft_files.get(index) {
                    self.view_file(file.name.clone(), file.bytes.clone(), gui, false);
                }
            }
            Click::Open(file) => match self.bytes(&file) {
                Ok(bytes) => self.view_file(file.name, bytes, gui, true),
                Err(_) => self.notice = Some("Attachment unavailable.".into()),
            },
            Click::Link(url) => {
                self.notice = Some(if gui.clipboard().set(&url) {
                    "Link copied.".into()
                } else {
                    "Could not copy the link.".into()
                });
            }
            Click::ClosePreview => self.preview = None,
            Click::SavePreview => {
                if let Some(preview) = &self.preview {
                    self.save(preview.name.clone(), preview.bytes.clone(), gui);
                }
            }
            Click::Latest => self.scroll = 0,
            Click::PickerRow(index) => {
                if let Some(picker) = &mut self.picker {
                    picker.select(index);
                }
            }
            Click::PickerParent => {
                if let Some(picker) = &mut self.picker
                    && let Some(parent) = picker.directory.parent().map(Path::to_path_buf)
                {
                    picker.browse(parent);
                }
            }
            Click::PickerConfirm => self.picker_confirm(),
            Click::PickerCancel => self.picker = None,
            Click::Composer => {
                let editor = self.editor();
                editor.caret = editor.text.len();
                editor.anchor = editor.caret;
            }
            _ => {}
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, font: &mut Type) {
        let (width, height) = (canvas.width, canvas.height);
        self.hits.clear();
        canvas.fill(Rect::new(0, 0, width, height), PAPER);
        if width < 240 || height < 200 {
            return;
        }
        if self.setup.is_some() {
            self.draw_setup(canvas, font);
            self.draw_layers(canvas, font);
            return;
        }
        let margin = if width > 560 { 28 } else { 14 };
        let rail = (width - 2 * margin).min(740);
        let left = (width - rail) / 2;
        let (state, dot) = match self.presence {
            Presence::Ready => ("Ready", 0xff347e53),
            Presence::Working => ("Working", 0xff99680b),
            Presence::Offline => ("Offline", 0xff777b74),
        };
        canvas.fill(Rect::new(0, 0, width, 73), WHITE);
        canvas.fill(Rect::new(0, 72, width, 1), BORDER);
        let name = font.ellipsis(&self.resident, rail - 48, 27);
        canvas.text(font, left, 13, 27, &name, INK);
        canvas.round(Rect::new(left, 52, 8, 8), 4, dot);
        canvas.text(font, left + 15, 48, 13, state, MUTED);
        let available = rail - 100;
        let status = font.ellipsis(&self.connection, available, 12);
        canvas.text(
            font,
            left + rail - font.width(&status, 12),
            49,
            12,
            &status,
            MUTED,
        );

        let columns = (rail / 170).clamp(2, 4) as usize;
        let rows = self.draft_files.len().div_ceil(columns) as i32;
        let card_height = if height > 490 { 48 } else { 33 };
        let editor_height = if height > 520 { 78 } else { 55 };
        let tray = rows * (card_height + 6);
        let composer = (height - editor_height - 72 - tray).max(108);
        let transcript = Rect::new(left, 84, rail, composer - 100);
        if transcript.h > 0 {
            self.draw_history(canvas, font, transcript);
        }
        self.draw_composer(
            canvas,
            font,
            Rect::new(left, composer, rail, height - composer - 10),
            columns,
            card_height,
            editor_height,
        );
        self.draw_layers(canvas, font);
    }

    fn draw_layers(&mut self, canvas: &mut Canvas, font: &mut Type) {
        if self.preview.is_some() {
            self.draw_preview(canvas, font);
        }
        if self.picker.is_some() {
            self.draw_picker(canvas, font);
        }
        if let Some((rect, _)) = self
            .hits
            .iter()
            .find(|(_, action)| Some(action) == self.focus.as_ref())
        {
            for (x, y, w, h) in [
                (rect.x, rect.y, rect.w, 2),
                (rect.x, rect.y + rect.h - 2, rect.w, 2),
                (rect.x, rect.y, 2, rect.h),
                (rect.x + rect.w - 2, rect.y, 2, rect.h),
            ] {
                canvas.fill(Rect::new(x, y, w, h), 0xff459e79);
            }
        } else {
            self.focus = None;
        }
    }

    fn draw_setup(&mut self, canvas: &mut Canvas, font: &mut Type) {
        let setup = self.setup.as_ref().expect("visible setup");
        let width = (canvas.width - 32).min(550);
        let height = (canvas.height - 24).min(482);
        let pad = if height >= 405 { 24 } else { 13 };
        let card = Rect::new(
            (canvas.width - width) / 2,
            (canvas.height - height) / 2,
            width,
            height,
        );
        let body = card.inset(pad);
        canvas.round(card, 18, BORDER);
        canvas.round(card.inset(1), 17, WHITE);
        canvas.text(font, body.x, body.y, 27, "Connect to Boxen", INK);
        let spaced = height >= 405;
        if spaced {
            canvas.text(
                font,
                body.x,
                body.y + 37,
                15,
                "Use the credential file and connection details you received.",
                MUTED,
            );
        }
        let key_label_y = body.y + if spaced { 78 } else { 37 };
        canvas.text(font, body.x, key_label_y, 15, "Credential file", INK);
        let choose = Rect::new(body.x, key_label_y + 22, 119, 32);
        button(
            canvas,
            font,
            choose,
            if setup.credential.is_some() {
                "Change file"
            } else {
                "Choose file"
            },
            false,
        );
        let filename = setup
            .credential
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "Choose or drop a JSON key".into(),
                |name| name.to_string_lossy(),
            );
        let filename = font.ellipsis(&filename, body.w - choose.w - 12, 13);
        canvas.text(
            font,
            choose.x + choose.w + 10,
            choose.y + 8,
            13,
            &filename,
            MUTED,
        );
        let label_y = choose.y + choose.h + if spaced { 21 } else { 9 };
        canvas.text(font, body.x, label_y + 5, 15, "Connection details", INK);
        let paste = Rect::new(body.x + body.w - 72, label_y, 72, 26);
        button(canvas, font, paste, "Paste", false);
        let connect = Rect::new(body.x + body.w - 112, body.y + body.h - 35, 112, 35);
        let entry = Rect::new(
            body.x,
            label_y + 31,
            body.w,
            (connect.y - label_y - 73).max(24),
        );
        canvas.round(entry, 9, BORDER);
        canvas.round(entry.inset(1), 8, PAPER);
        let interior = entry.inset(10);
        let content = if setup.details.text.is_empty() {
            "project_id=…\npublish_topic=…\nreceive_subscription=…"
        } else {
            &setup.details.text
        };
        let rows = font.wrap(content, interior.w, 14);
        let max_rows = (interior.h / 20).max(1) as usize;
        let old_clip = canvas.clip;
        canvas.clip = interior;
        // Invitations fit in three lines. For longer input, keep the end
        // visible so the user can see the text they just entered.
        for (row, text) in rows
            .iter()
            .skip(rows.len().saturating_sub(max_rows))
            .enumerate()
        {
            canvas.text(
                font,
                interior.x,
                interior.y + row as i32 * 20,
                14,
                text,
                if setup.details.text.is_empty() {
                    MUTED
                } else {
                    INK
                },
            );
        }
        canvas.clip = old_clip;
        button(
            canvas,
            font,
            connect,
            if setup.checking {
                "Checking…"
            } else {
                "Connect"
            },
            setup.ready(),
        );
        if !setup.checking {
            self.hits.extend([
                (choose, Click::Pick),
                (entry, Click::Composer),
                (paste, Click::PasteDetails),
            ]);
        }
        if setup.ready() {
            self.hits.push((connect, Click::ConnectSetup));
        }
        if let Some(status) = setup.error.as_deref().or(setup
            .checking
            .then_some("Verifying access to Google Cloud…"))
        {
            let color = if setup.error.is_some() {
                0xff9b4135
            } else {
                MUTED
            };
            for (row, line) in font
                .wrap(status, body.w - connect.w - 12, 13)
                .into_iter()
                .take(3)
                .enumerate()
            {
                canvas.text(
                    font,
                    body.x,
                    connect.y - 8 + row as i32 * 15,
                    13,
                    &line,
                    color,
                );
            }
        }
    }

    fn draw_history(&mut self, canvas: &mut Canvas, font: &mut Type, rect: Rect) {
        if self.history.is_empty() {
            let title = "Start a conversation";
            let hint = "Drop files anywhere to attach.";
            let top = rect.y + (rect.h / 2 - 28).max(0);
            canvas.text(
                font,
                rect.x + (rect.w - font.width(title, 23)) / 2,
                top,
                23,
                title,
                INK,
            );
            canvas.text(
                font,
                rect.x + (rect.w - font.width(hint, 14)) / 2,
                top + 34,
                14,
                hint,
                MUTED,
            );
            return;
        }
        let history = self.history.clone();
        canvas.clip = rect;
        let max_width = (rect.w * 4 / 5).clamp(155, 575);
        let (previous_count, previous_width, previous_height) = self.history_bounds;
        if previous_width != max_width || previous_count > history.len() {
            self.bubble_heights.clear();
            self.history_layout.clear();
        }
        if previous_count != history.len() || previous_width != max_width {
            for (index, entry) in history.iter().enumerate().skip(self.bubble_heights.len()) {
                let message = &entry.message;
                let layout = self.message_layout(index, &message.body, max_width, font);
                let file_rows = message.files.len() + message.attachment_names.len();
                self.bubble_heights
                    .push(41 + layout.height + 42 * file_rows as i32);
            }
            let height = self.bubble_heights.iter().map(|height| height + 10).sum();
            if self.scroll > 0 && previous_width == max_width {
                self.scroll = self.scroll.saturating_add(height - previous_height);
            }
            self.history_bounds = (history.len(), max_width, height);
        }
        self.scroll = self
            .scroll
            .min((self.history_bounds.2 - 10 - rect.h).max(0));
        let mut bottom = rect.y + rect.h + self.scroll;
        for (index, entry) in history.iter().enumerate().rev() {
            let message = &entry.message;
            let file_rows = message.files.len() + message.attachment_names.len();
            let height = self.bubble_heights[index];
            let top = bottom - height;
            if bottom <= rect.y {
                break;
            }
            if top < rect.y + rect.h {
                let layout = self.message_layout(index, &message.body, max_width, font);
                let file_width = if file_rows == 0 { 0 } else { 280 };
                let bubble_width = (layout.width.max(file_width) + 28).clamp(104, max_width);
                let x = if entry.sent {
                    rect.x + rect.w - bubble_width
                } else {
                    rect.x
                };
                let bubble = Rect::new(x, top, bubble_width, height);
                canvas.round(bubble, 15, if entry.sent { MINT } else { BORDER });
                if !entry.sent {
                    canvas.round(bubble.inset(1), 14, WHITE);
                }
                let author = if entry.sent { "You" } else { &self.resident };
                let author = font.ellipsis(author, bubble_width - 28, 12);
                canvas.text(font, x + 14, top + 12, 12, &author, GREEN);
                layout.draw(canvas, font, (x + 14, top + 31), |hit, url| {
                    let visible = clip(hit, rect);
                    if visible.w > 0 && visible.h > 0 {
                        self.hits.push((visible, Click::Link(url)));
                    }
                });
                let mut y = top + 31 + layout.height;
                for file in &message.files {
                    let card = Rect::new(x + 12, y, bubble_width - 24, 37);
                    if y < rect.y + rect.h && y + 37 > rect.y {
                        self.ensure_thumbnail(file);
                        let thumb = self.thumbnails.get(&file.digest).and_then(Option::as_ref);
                        attachment_card(
                            canvas,
                            font,
                            card,
                            &file.name,
                            &friendly_size(file.size),
                            thumb,
                        );
                        self.hits
                            .push((clip(card, rect), Click::Open(file.clone())));
                    }
                    y += 42;
                }
                for name in &message.attachment_names {
                    let label =
                        font.ellipsis(&format!("{name} · unavailable"), bubble_width - 28, 13);
                    canvas.text(font, x + 14, y + 8, 13, &label, MUTED);
                    y += 42;
                }
            }
            bottom = top - 10;
        }
        canvas.clip = Rect::new(0, 0, canvas.width, canvas.height);
        if self.scroll > 0 {
            let latest = Rect::new(rect.x + rect.w - 118, rect.y + rect.h - 31, 118, 27);
            button(canvas, font, latest, "Jump to latest", false);
            self.hits.push((latest, Click::Latest));
        }
    }

    fn message_layout(
        &mut self,
        index: usize,
        body: &str,
        width: i32,
        font: &Type,
    ) -> Arc<Markdown> {
        if let Some(layout) = self.history_layout.get(&index) {
            return layout.clone();
        }
        let layout = Arc::new(Markdown::message(body, width - 28, font));
        if self.history_layout.len() >= 96 {
            self.history_layout.clear();
        }
        self.history_layout.insert(index, layout.clone());
        layout
    }

    fn ensure_thumbnail(&mut self, file: &Attachment) {
        if !self.thumbnails.contains_key(&file.digest) {
            if self.thumbnails.len() > 128 {
                self.thumbnails.clear();
            }
            let format = self
                .session
                .as_ref()
                .and_then(|session| session.file_prefix(file).ok())
                .and_then(|prefix| image::guess_format(&prefix).ok());
            let thumbnail = if format.is_some_and(|format| format.can_read()) {
                self.bytes(file).ok().and_then(|bytes| picture(&bytes, 128))
            } else {
                None
            };
            self.thumbnails.insert(file.digest.clone(), thumbnail);
        }
    }

    fn draw_composer(
        &mut self,
        canvas: &mut Canvas,
        font: &mut Type,
        rect: Rect,
        columns: usize,
        card_height: i32,
        editor_height: i32,
    ) {
        canvas.round(rect, 17, BORDER);
        canvas.round(rect.inset(1), 16, WHITE);
        let content = rect.inset(12);
        let action_y = rect.y + rect.h - 48;
        let editor = Rect::new(
            content.x,
            content.y,
            content.w,
            (editor_height - 8).min((action_y - content.y - 5).max(14)),
        );
        self.hits.push((editor, Click::Composer));
        canvas.clip = editor;
        if self.draft.text.is_empty() {
            canvas.text(
                font,
                editor.x,
                editor.y + 2,
                TEXT_SIZE,
                "Write a message…",
                MUTED,
            );
        } else {
            let lines = font.wrap(&self.draft.text, editor.w - 6, TEXT_SIZE);
            let positions: Vec<_> = lines
                .iter()
                .scan(0, |offset, line| {
                    let start = *offset;
                    *offset += line.len();
                    if self.draft.text.as_bytes().get(*offset) == Some(&b'\n') {
                        *offset += 1;
                    }
                    Some(start)
                })
                .collect();
            let limit = (editor.h / LEADING).max(1) as usize;
            let current = positions
                .partition_point(|&at| at <= self.draft.caret)
                .saturating_sub(1);
            let skip = current.saturating_add(1).saturating_sub(limit);
            for (row, line) in lines.iter().skip(skip).take(limit).enumerate() {
                let offset = positions[skip + row];
                let y = editor.y + row as i32 * LEADING + 2;
                let range = self.draft.range();
                let a = range.start.saturating_sub(offset).min(line.len());
                let b = range.end.saturating_sub(offset).min(line.len());
                let a = line.floor_char_boundary(a);
                let b = line.floor_char_boundary(b);
                if a < b {
                    let x = font.width(&line[..a], TEXT_SIZE);
                    canvas.round(
                        Rect::new(
                            editor.x + x,
                            y - 2,
                            font.width(&line[a..b], TEXT_SIZE),
                            LEADING,
                        ),
                        3,
                        MINT,
                    );
                }
                canvas.text(font, editor.x, y, TEXT_SIZE, line, INK);
                if row + skip == current {
                    let at = self.draft.caret.saturating_sub(offset).min(line.len());
                    let caret = font.width(&line[..line.floor_char_boundary(at)], TEXT_SIZE);
                    canvas.fill(Rect::new(editor.x + caret, y - 1, 1, 19), GREEN);
                }
            }
        }
        canvas.clip = Rect::new(0, 0, canvas.width, canvas.height);
        let tray_y = editor.y + editor.h + 3;
        let cell_width = (content.w + 6) / columns as i32;
        for (index, file) in self.draft_files.iter().enumerate() {
            let card = Rect::new(
                content.x + (index % columns) as i32 * cell_width,
                tray_y + (index / columns) as i32 * (card_height + 6),
                cell_width - 6,
                card_height,
            );
            if card.y + card.h > action_y {
                break;
            }
            attachment_card(
                canvas,
                font,
                card,
                &file.name,
                &friendly_size(file.bytes.len() as u64),
                file.thumbnail.as_ref(),
            );
            if file.thumbnail.is_some() {
                self.hits.push((card, Click::DraftImage(index)));
            }
            let remove = Rect::new(card.x + card.w - 26, card.y + (card.h - 23) / 2, 23, 23);
            canvas.round(remove, 6, WHITE);
            canvas.text(font, remove.x + 6, remove.y + 3, 16, "×", GREEN);
            self.hits.push((remove, Click::Remove(index)));
        }
        let attach = Rect::new(content.x, action_y, 111, 35);
        let send = Rect::new(content.x + content.w - 78, action_y, 78, 35);
        button(canvas, font, attach, "Attach files", false);
        let enabled = !self.draft.text.trim().is_empty() || !self.draft_files.is_empty();
        button(canvas, font, send, "Send", enabled);
        self.hits.push((attach, Click::Pick));
        if enabled {
            self.hits.push((send, Click::Send));
        }
        let label = self
            .notice
            .as_deref()
            .unwrap_or("Shift+Enter for a new line");
        let available = send.x - attach.x - attach.w - 20;
        if available > 95 {
            let label = font.ellipsis(label, available, 12);
            canvas.text(
                font,
                attach.x + attach.w + 10,
                action_y + 10,
                12,
                &label,
                MUTED,
            );
        }
    }

    fn draw_preview(&mut self, canvas: &mut Canvas, font: &mut Type) {
        let preview = self.preview.as_ref().unwrap();
        self.hits.clear();
        let size = Rect::new(0, 0, canvas.width, canvas.height);
        canvas.fill(size, 0xff263b34);
        let sheet = size.inset(24);
        canvas.round(sheet, 16, WHITE);
        let full = sheet.inset(15);
        let heading = font.ellipsis(&preview.name, full.w - 163, 16);
        canvas.text(font, full.x, full.y + 7, 16, &heading, INK);
        let close = Rect::new(full.x + full.w - 60, full.y, 60, 32);
        let save = Rect::new(close.x - 89, close.y, 81, 32);
        button(canvas, font, close, "Close", false);
        button(canvas, font, save, "Save as…", true);
        self.hits.push((close, Click::ClosePreview));
        self.hits.push((save, Click::SavePreview));
        let stage = Rect::new(full.x, full.y + 46, full.w, full.h - 46);
        canvas.round(stage, 10, PAPER);
        canvas.image(fit(stage.inset(6), &preview.picture), &preview.picture);
    }

    fn draw_picker(&mut self, canvas: &mut Canvas, font: &mut Type) {
        let picker = self.picker.as_mut().expect("visible picker");
        let save = picker.save.is_some();
        self.hits.clear();
        let full = Rect::new(0, 0, canvas.width, canvas.height);
        canvas.fill(full, 0xff263b34);
        let pane = full.inset(24);
        canvas.round(pane, 16, WHITE);
        let content = pane.inset(16);
        canvas.text(
            font,
            content.x,
            content.y + 3,
            20,
            if save {
                "Save attachment"
            } else {
                "Attach files"
            },
            INK,
        );
        let close = Rect::new(content.x + content.w - 70, content.y, 70, 30);
        button(canvas, font, close, "Cancel", false);
        self.hits.push((close, Click::PickerCancel));
        let up = Rect::new(content.x, content.y + 38, 72, 28);
        button(canvas, font, up, "Parent", false);
        self.hits.push((up, Click::PickerParent));
        let folder = font.ellipsis(&picker.directory.to_string_lossy(), content.w - 86, 13);
        canvas.text(font, up.x + up.w + 12, up.y + 7, 13, &folder, MUTED);
        let list_top = content.y + 74;
        let footer = content.y + content.h - 61;
        let rows = ((footer - list_top - 6) / 30).max(0) as usize;
        picker.offset = picker
            .offset
            .min(picker.entries.len().saturating_sub(rows.max(1)));
        for (index, (name, path, directory)) in picker
            .entries
            .iter()
            .enumerate()
            .skip(picker.offset)
            .take(rows)
        {
            let rect = Rect::new(
                content.x,
                list_top + (index - picker.offset) as i32 * 30,
                content.w,
                28,
            );
            let selected = picker.selected.contains(path);
            canvas.round(rect, 6, if selected { MINT } else { PAPER });
            let marker = if *directory {
                "›"
            } else if selected {
                "✓"
            } else {
                "·"
            };
            canvas.text(font, rect.x + 10, rect.y + 6, 14, marker, GREEN);
            let name = font.ellipsis(name, rect.w - 40, 14);
            canvas.text(font, rect.x + 29, rect.y + 6, 14, &name, INK);
            self.hits.push((rect, Click::PickerRow(index)));
        }
        let input = Rect::new(content.x, footer, content.w - 92, 34);
        canvas.round(input, 8, BORDER);
        canvas.round(input.inset(1), 7, WHITE);
        let text = if picker.input.text.is_empty() {
            "File or folder path…"
        } else {
            &picker.input.text
        };
        let text = font.ellipsis(text, input.w - 20, 14);
        canvas.text(font, input.x + 9, input.y + 8, 14, &text, INK);
        let confirm = Rect::new(input.x + input.w + 8, footer, 84, 34);
        button(
            canvas,
            font,
            confirm,
            if save { "Save" } else { "Attach" },
            true,
        );
        self.hits.push((confirm, Click::PickerConfirm));
        let status = picker.message.clone().unwrap_or_else(|| {
            if save {
                "Choose a folder and filename.".into()
            } else {
                format!("{} selected", picker.selected.len())
            }
        });
        let status = font.ellipsis(&status, content.w, 12);
        canvas.text(font, content.x, footer + 41, 12, &status, MUTED);
    }
}

fn read_draft(path: &Path) -> Result<DraftFile> {
    if !fs::metadata(path)?.is_file() {
        return Err("Attachment is not a regular file".into());
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > FILE_LIMIT {
        return Err("Attachment exceeds 32 MiB".into());
    }
    let name = path
        .file_name()
        .and_then(|part| part.to_str())
        .ok_or("Attachment name is not UTF-8")?
        .to_owned();
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    Attachment {
        name: name.clone(),
        size: bytes.len() as u64,
        digest: "0".repeat(64),
    }
    .validate()?;
    Ok(DraftFile {
        name,
        thumbnail: picture(&bytes, 128),
        bytes: bytes.into(),
    })
}

fn save_copy(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or("Attachment save location has no directory")?;
    let temporary = parent.join(format!(".boxen-{}.tmp", new_id()?));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn picture(data: &[u8], size: u32) -> Option<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    Some(reader.decode().ok()?.thumbnail(size, size).to_rgba8())
}

fn fit(rect: Rect, image: &RgbaImage) -> Rect {
    let scale = (rect.w as f32 / image.width() as f32).min(rect.h as f32 / image.height() as f32);
    let (w, h) = (
        (image.width() as f32 * scale) as i32,
        (image.height() as f32 * scale) as i32,
    );
    Rect::new(rect.x + (rect.w - w) / 2, rect.y + (rect.h - h) / 2, w, h)
}

fn clip(rect: Rect, limit: Rect) -> Rect {
    let (x, y) = (rect.x.max(limit.x), rect.y.max(limit.y));
    Rect::new(
        x,
        y,
        (rect.x + rect.w).min(limit.x + limit.w) - x,
        (rect.y + rect.h).min(limit.y + limit.h) - y,
    )
}

fn button(canvas: &mut Canvas, font: &mut Type, rect: Rect, label: &str, primary: bool) {
    canvas.round(rect, 9, if primary { GREEN } else { MINT });
    canvas.text(
        font,
        rect.x + (rect.w - font.width(label, 14)) / 2,
        rect.y + (rect.h - 16) / 2,
        14,
        label,
        if primary { WHITE } else { GREEN },
    );
}

fn attachment_card(
    canvas: &mut Canvas,
    font: &mut Type,
    card: Rect,
    name: &str,
    subtitle: &str,
    thumbnail: Option<&RgbaImage>,
) {
    canvas.round(card, 8, BORDER);
    canvas.round(card.inset(1), 7, PAPER);
    let icon = Rect::new(card.x + 5, card.y + 5, (card.h - 10).min(35), card.h - 10);
    canvas.round(icon, 5, MINT);
    if let Some(thumbnail) = thumbnail {
        canvas.image(fit(icon, thumbnail), thumbnail);
    } else {
        canvas.text(font, icon.x + 6, icon.y + (icon.h - 13) / 2, 13, "↓", GREEN);
    }
    let x = icon.x + icon.w + 7;
    let max = card.x + card.w - x - 28;
    let name = font.ellipsis(name, max, 12);
    canvas.text(font, x, card.y + 5, 12, &name, INK);
    canvas.text(font, x, card.y + 19, 11, subtitle, MUTED);
}

fn friendly_size(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{} KiB", size.div_ceil(1024))
    } else {
        format!("{size} B")
    }
}
