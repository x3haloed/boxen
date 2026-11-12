//! The client snapshots files before Send. Retried messages point to the same
//! local daemon originals even if the source files have changed or disappeared.
use crate::{
    Action, Desktop,
    client::{Client, Update},
    model::{Destination, ThreadKey},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const FILE_LIMIT: usize = 32 * 1024 * 1024;
const CHUNK: usize = 192 * 1024;
pub const MAX_FILES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub name: String,
    pub digest: String,
    pub size: u64,
}
impl FileRef {
    pub fn info(&self) -> String {
        let kind = Path::new(&self.name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("File");
        let size = if self.size >= 1024 * 1024 {
            format!("{:.1} MiB", self.size as f64 / (1024. * 1024.))
        } else if self.size >= 1024 {
            format!("{:.1} KiB", self.size as f64 / 1024.)
        } else {
            format!("{} B", self.size)
        };
        format!("{kind} · {size}")
    }
    pub fn parse(value: &Value) -> Vec<Self> {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    NewThread,
    Mail(String),
    Thread(ThreadKey),
}
pub enum FileAction {
    Add(Target, Option<Vec<PathBuf>>, usize),
    Open(Destination, FileRef, bool),
}
pub struct Preparation {
    pub working: bool,
    pub errors: Vec<String>,
}
pub enum Preview {
    Image(egui::ColorImage),
    Text(String),
    Unsupported,
}
pub enum FileUpdate {
    Staged(Target, Vec<FileRef>, Vec<String>),
    Preview(String, Result<Preview, String>),
    Saved(Result<(), String>),
}
impl Client {
    pub fn copy_files(&self, destination: &Destination, files: &Value) -> Result<(), String> {
        for file in FileRef::parse(files) {
            let local = self.local();
            let request = |action: &str| json!({"op":"attachment","action":action,"file":file});
            let mut status = self.operation(destination, request("status"))?;
            while status["complete"] != true {
                let offset = status["next"]
                    .as_u64()
                    .filter(|n| *n <= file.size)
                    .ok_or("Invalid attachment transfer position")?;
                let mut read = request("read");
                read["offset"] = json!(offset);
                let part = self.operation(&local, read)?;
                let mut write = request("write");
                write["offset"] = json!(offset);
                write["data"] = part["data"].clone();
                status = self.operation(destination, write)?;
                if status["next"] != part["next"]
                    || (status["complete"] != true && status["next"] == offset)
                {
                    return Err("Attachment transfer acknowledgment differs".into());
                }
            }
        }
        Ok(())
    }
    fn stage_file(&self, path: &Path) -> Result<FileRef, String> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Filename is not valid UTF-8")?;
        if !fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
            return Err("Only regular files can be attached".into());
        }
        let input = File::open(path).map_err(|e| e.to_string())?;
        if !input.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("Only regular files can be attached".into());
        }
        let mut bytes = Vec::new();
        input
            .take((FILE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > FILE_LIMIT {
            return Err("Files can be up to 32 MiB".into());
        }
        let file = FileRef {
            name: name.into(),
            digest: format!("{:x}", Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        };
        let local = self.local();
        let mut status = self.operation(
            &local,
            json!({"op":"attachment","action":"status","file":file}),
        )?;
        while status["complete"] != true {
            let offset = status["next"]
                .as_u64()
                .filter(|n| *n <= file.size)
                .ok_or("Invalid attachment transfer position")? as usize;
            let end = (offset + CHUNK).min(bytes.len());
            status = self.operation(&local, json!({"op":"attachment","action":"write","file":file,"offset":offset,"data":STANDARD.encode(&bytes[offset..end])}))?;
            if status["next"].as_u64() != Some(end as u64)
                || (offset == end && status["complete"] != true)
            {
                return Err("Attachment transfer acknowledgment differs".into());
            }
        }
        Ok(file)
    }
    fn download(&self, destination: &Destination, file: &FileRef) -> Result<Vec<u8>, String> {
        if file.size > FILE_LIMIT as u64 {
            return Err("File exceeds the download limit".into());
        }
        let mut bytes = Vec::new();
        loop {
            let offset = bytes.len() as u64;
            let part = self.operation(
                destination,
                json!({"op":"attachment","action":"read","file":file,"offset":offset}),
            )?;
            let chunk = STANDARD
                .decode(part["data"].as_str().ok_or("Attachment data missing")?)
                .map_err(|e| e.to_string())?;
            bytes.extend_from_slice(&chunk);
            if bytes.len() as u64 > file.size || part["next"].as_u64() != Some(bytes.len() as u64) {
                return Err("Attachment response differs from its reference".into());
            }
            if bytes.len() as u64 == file.size {
                break;
            }
            if chunk.is_empty() {
                return Err("Attachment download stopped early".into());
            }
        }
        if format!("{:x}", Sha256::digest(&bytes)) != file.digest {
            return Err("Attachment failed its integrity check".into());
        }
        Ok(bytes)
    }
}

fn preview(bytes: &[u8]) -> Result<Preview, String> {
    if let Ok(format) = image::guess_format(bytes)
        && matches!(
            format,
            image::ImageFormat::Png
                | image::ImageFormat::Jpeg
                | image::ImageFormat::Gif
                | image::ImageFormat::WebP
        )
    {
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|e| e.to_string())?
            .thumbnail(480, 300)
            .into_rgba8();
        return Ok(Preview::Image(egui::ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            &image,
        )));
    }
    if let Ok(text) = std::str::from_utf8(bytes)
        && text
            .chars()
            .take(8192)
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
    {
        let mut shown: String = text.chars().take(8192).collect();
        if shown.len() < text.len() {
            shown.push_str("\n…");
        }
        return Ok(Preview::Text(shown));
    }
    Ok(Preview::Unsupported)
}

impl Desktop {
    pub fn file_action(&mut self, action: FileAction) {
        let Some(client) = self.client.clone() else {
            self.data.notice =
                "The daemon is unavailable. Attachments could not be opened or added.".into();
            return;
        };
        let publisher = self.bus.publisher();
        match action {
            FileAction::Add(target, paths, available) => {
                if self.data.preparing.get(&target).is_some_and(|p| p.working) {
                    return;
                }
                if let Target::Thread(key) = &target {
                    let run = crate::thread_view::active(
                        crate::find_thread(&self.data, key).and_then(|t| t.runs.last()),
                    )
                    .map(str::to_owned);
                    crate::thread_view::capture(&mut self.data, key, run.as_deref());
                }
                self.data.preparing.insert(
                    target.clone(),
                    Preparation {
                        working: true,
                        errors: vec![],
                    },
                );
                std::thread::spawn(move || {
                    let mut errors = Vec::new();
                    let mut files = Vec::new();
                    let paths = paths
                        .or_else(|| {
                            rfd::FileDialog::new()
                                .set_title("Attach files")
                                .pick_files()
                        })
                        .unwrap_or_default();
                    if paths.len() > available {
                        errors.push("A message can carry at most eight attachments.".into());
                    }
                    for path in paths.into_iter().take(available) {
                        match client.stage_file(&path) {
                            Ok(file) => files.push(file),
                            Err(e) => errors.push(format!(
                                "{}: {e}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            )),
                        }
                    }
                    publisher.send(Update::Files(FileUpdate::Staged(target, files, errors)));
                });
            }
            FileAction::Open(destination, file, save) => {
                let key = format!("{}:{}", destination.key, file.digest);
                if !save {
                    self.data.file_previews.insert(key.clone(), None);
                }
                std::thread::spawn(move || {
                    if save {
                        let result = (|| {
                            let Some(path) = rfd::FileDialog::new()
                                .set_title("Save attachment")
                                .set_file_name(&file.name)
                                .save_file()
                            else {
                                return Ok(());
                            };
                            let bytes = client.download(&destination, &file)?;
                            let mut output = fs::OpenOptions::new()
                                .create_new(true)
                                .write(true)
                                .open(&path)
                                .map_err(|e| e.to_string())?;
                            if let Err(error) =
                                output.write_all(&bytes).and_then(|_| output.sync_all())
                            {
                                drop(output);
                                let _ = fs::remove_file(path);
                                return Err(error.to_string());
                            }
                            Ok(())
                        })();
                        publisher.send(Update::Files(FileUpdate::Saved(result)));
                    } else {
                        let result = client
                            .download(&destination, &file)
                            .and_then(|bytes| preview(&bytes));
                        publisher.send(Update::Files(FileUpdate::Preview(key, result)));
                    }
                });
            }
        }
    }
}

pub fn add_files(
    ui: &mut egui::Ui,
    response: &egui::Response,
    target: Target,
    files: usize,
    enabled: bool,
    click: bool,
    actions: &mut Vec<Action>,
) {
    let enabled = enabled && files < MAX_FILES;
    let drop = if enabled {
        ui.input(|input| {
            if input
                .pointer
                .latest_pos()
                .is_some_and(|p| response.rect.contains(p))
            {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>()
            } else {
                vec![]
            }
        })
    } else {
        vec![]
    };
    if click || !drop.is_empty() {
        actions.push(Action::File(FileAction::Add(
            target,
            (!drop.is_empty()).then_some(drop),
            MAX_FILES - files,
        )));
    }
}

impl Desktop {
    pub fn file_update(&mut self, update: FileUpdate) {
        match update {
            FileUpdate::Staged(target, mut files, errors) => {
                self.data.preparing.insert(
                    target.clone(),
                    Preparation {
                        working: false,
                        errors,
                    },
                );
                if files.is_empty() {
                    return;
                }
                match target {
                    Target::NewThread => self.data.saved.new_thread_files.append(&mut files),
                    Target::Mail(id) => {
                        if let Some(draft) =
                            self.data.saved.mail.drafts.iter_mut().find(|d| d.id == id)
                        {
                            draft.files.append(&mut files);
                            if let Some(key) = &draft.recipient {
                                self.data
                                    .saved
                                    .workspace
                                    .pin_pane(&crate::model::Pane::Chat(key.clone()));
                            }
                        }
                    }
                    Target::Thread(key) => {
                        if let Some((_, existing)) = self
                            .data
                            .saved
                            .thread_files
                            .iter_mut()
                            .find(|(k, _)| k == &key)
                        {
                            existing.append(&mut files);
                        } else {
                            self.data.saved.thread_files.push((key.clone(), files));
                        }
                        self.data.saved.workspace.pin(&key);
                    }
                }
                self.persist();
            }
            FileUpdate::Preview(key, result) => {
                if self.data.file_previews.len() > 16 {
                    self.data
                        .file_previews
                        .retain(|k, v| k == &key || v.is_none());
                }
                self.data.file_previews.insert(key, Some(result));
            }
            FileUpdate::Saved(Err(error)) => {
                self.data.notice = format!("Attachment was not saved: {error}");
            }
            FileUpdate::Saved(Ok(())) => {}
        }
    }
}
