use crate::{
    attachments::{picture, read_attachment, save_copy},
    session::Session,
    setup::Installer,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use boxen_guest_transport::{Attachment, MAX_FILES, Result};
use image::{DynamicImage, ImageFormat, guess_format};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    io::Cursor,
    path::PathBuf,
};

unsafe extern "C" {
    fn guest_chat_run(state: *mut c_void, setup: bool);
}

// AppKit owns the UI on the main thread. The pointer is borrowed only for the
// duration of its event loop. Session and Installer own the journal and workers.
pub fn run(session: Option<Session>, installer: Option<Installer>) -> Result<()> {
    let setup = installer.is_some();
    let mut state = State {
        session,
        installer,
        revision: 0,
        shown: 0,
        draft: Vec::new(),
    };
    unsafe { guest_chat_run((&mut state as *mut State).cast(), setup) };
    Ok(())
}

struct State {
    session: Option<Session>,
    installer: Option<Installer>,
    revision: u64,
    shown: usize,
    draft: Vec<DraftAttachment>,
}

struct DraftAttachment {
    name: String,
    bytes: Vec<u8>,
    thumbnail: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Request {
    Poll,
    Connect {
        details: String,
        path: PathBuf,
    },
    Attach {
        paths: Vec<PathBuf>,
    },
    Remove {
        index: usize,
    },
    PreviewDraft {
        index: usize,
    },
    Send {
        body: String,
    },
    Preview {
        message: usize,
        index: usize,
    },
    Save {
        message: usize,
        index: usize,
        path: PathBuf,
    },
}

impl State {
    fn session(&self) -> Result<&Session> {
        self.session
            .as_ref()
            .ok_or_else(|| "Chat is still setting up.".into())
    }

    fn poll(&mut self) -> Result<Value> {
        if let Some(installer) = &self.installer {
            let Some(outcome) = installer.result() else {
                return Ok(Value::Null);
            };
            self.session = Some(outcome?);
            self.installer = None;
        }
        let session = self.session()?;
        let Some(view) = session.view_since(self.revision) else {
            return Ok(Value::Null);
        };
        // Bound decode and import work for long histories to one batch per UI tick.
        let end = (self.shown + 16).min(view.history.len());
        let entries: Vec<_> = view.history[self.shown..end]
            .iter()
            .map(|entry| {
                let message = &entry.message;
                let files: Vec<_> = message
                    .files
                    .iter()
                    .map(|file| {
                        let thumbnail = session
                            .file_prefix(file)
                            .ok()
                            .filter(|prefix| guess_format(prefix).is_ok())
                            .and_then(|_| session.read_file(file).ok())
                            .and_then(|bytes| png(&bytes, 96));
                        json!({"name": file.name, "size": file.size, "thumbnail": thumbnail})
                    })
                    .collect();
                json!({
                    "sent": entry.sent, "body": message.body, "html": markdown(&message.body),
                    "files": files, "unavailable": message.attachment_names,
                })
            })
            .collect();
        self.shown = end;
        if end == view.history.len() {
            self.revision = view.revision;
        }
        Ok(json!({
            "entries": entries,
            "resident": view.resident,
            "status": view.status,
            "presence": view.presence,
            "pending": view.pending,
        }))
    }

    fn draft_view(&self, error: Option<String>) -> Value {
        let files: Vec<_> = self
            .draft
            .iter()
            .map(|file| json!({"name": file.name, "thumbnail": file.thumbnail}))
            .collect();
        json!({"files": files, "notice": error})
    }

    fn attachment(&self, message: usize, index: usize) -> Result<Attachment> {
        self.session()?
            .view_since(0)
            .and_then(|view| view.history.get(message)?.message.files.get(index).cloned())
            .ok_or_else(|| "Attachment unavailable.".into())
    }

    fn handle(&mut self, request: Request) -> Result<Value> {
        match request {
            Request::Poll => self.poll(),
            Request::Connect { details, path } => {
                self.installer
                    .as_ref()
                    .ok_or("Chat setup has already completed.")?
                    .connect(details, path)?;
                Ok(Value::Null)
            }
            Request::Attach { paths } => {
                let mut error = None;
                for path in paths {
                    if self.draft.len() >= MAX_FILES {
                        error = Some(format!("A message can include up to {MAX_FILES} files."));
                        break;
                    }
                    match read_attachment(&path) {
                        Ok((name, bytes)) => self.draft.push(DraftAttachment {
                            name,
                            thumbnail: png(&bytes, 24),
                            bytes,
                        }),
                        Err(e) => error = Some(format!("Could not attach a file. {e}")),
                    }
                }
                Ok(self.draft_view(error))
            }
            Request::Remove { index } => {
                if index < self.draft.len() {
                    self.draft.remove(index);
                }
                Ok(self.draft_view(None))
            }
            Request::PreviewDraft { index } => {
                let file = self
                    .draft
                    .get(index)
                    .ok_or("Draft attachment unavailable.")?;
                Ok(json!({"name": file.name, "image": png(&file.bytes, 1536)}))
            }
            Request::Send { body } => {
                if body.trim().is_empty() && self.draft.is_empty() {
                    return Ok(json!(false));
                }
                self.session()?
                    .send(
                        &body,
                        self.draft
                            .iter()
                            .map(|file| (file.name.clone(), file.bytes.as_slice())),
                    )
                    .map_err(|error| format!("Message kept in draft. {error}"))?;
                self.draft.clear();
                Ok(json!(true))
            }
            Request::Preview { message, index } => {
                let file = self.attachment(message, index)?;
                let bytes = self.session()?.read_file(&file)?;
                Ok(json!({"name": file.name, "image": png(&bytes, 1536)}))
            }
            Request::Save {
                message,
                index,
                path,
            } => {
                let file = self.attachment(message, index)?;
                save_copy(&path, &self.session()?.read_file(&file)?)?;
                Ok(Value::Null)
            }
        }
    }
}

fn png(bytes: &[u8], size: u32) -> Option<String> {
    let image = DynamicImage::ImageRgba8(picture(bytes, size)?);
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png).ok()?;
    Some(STANDARD.encode(encoded.into_inner()))
}

fn markdown(body: &str) -> String {
    let events = Parser::new_ext(
        body,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    )
    .map(|event| match event {
        // The HTML importer only receives generated markup. In particular,
        // neither raw HTML nor a Markdown image can introduce an external load.
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        Event::SoftBreak => Event::HardBreak,
        Event::Start(Tag::Image { .. }) => Event::Text("[Image: ".into()),
        Event::End(TagEnd::Image) => Event::Text("]".into()),
        Event::TaskListMarker(checked) => Event::Text(if checked { "☑ " } else { "☐ " }.into()),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let safe = dest_url.split_once("://").is_some_and(|(scheme, rest)| {
                matches!(scheme, "http" | "https")
                    && rest
                        .split(['/', '?', '#'])
                        .next()
                        .is_some_and(|host| !host.is_empty())
                    && !dest_url
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
            });
            Event::Start(Tag::Link {
                link_type,
                dest_url: if safe { dest_url } else { "".into() },
                title,
                id,
            })
        }
        other => other,
    });
    let mut output = String::new();
    html::push_html(&mut output, events);
    output
}

#[unsafe(no_mangle)]
unsafe extern "C" fn guest_chat_request(state: *mut c_void, request: *const c_char) -> *mut c_char {
    let state = unsafe { &mut *state.cast::<State>() };
    let request = unsafe { CStr::from_ptr(request) };
    let result = serde_json::from_slice::<Request>(request.to_bytes())
        .map_err(Into::into)
        .and_then(|request| state.handle(request));
    let response = match result {
        Ok(value) => json!({"value": value}),
        Err(error) => json!({"error": error.to_string()}),
    };
    CString::new(response.to_string())
        .expect("JSON escapes NUL bytes")
        .into_raw()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn guest_chat_free(response: *mut c_char) {
    drop(unsafe { CString::from_raw(response) });
}

#[cfg(test)]
mod tests {
    use super::markdown;

    #[test]
    fn uses_generated_markup_without_loading_message_resources() {
        let html = markdown(
            "**bold** [site](https://example.invalid) [file](file:///etc/test) ![alt](https://example.invalid/image) <img src=x>",
        );
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("href=\"https://example.invalid\""));
        assert!(html.contains("[Image: alt]"));
        assert!(!html.contains("file:///"));
        assert!(!html.contains("/image"));
        assert!(!html.contains("<img"));
    }
}
