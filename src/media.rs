//! Bounded file reads, immutable snapshots, and explicit OpenRouter wire projection.
use crate::{Result, state};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};
const MAX_FILE: usize = 2 * 1024 * 1024;
const MAX_RUN: u64 = 16 * 1024 * 1024;
pub const REQUEST_LIMIT: usize = 24 * 1024 * 1024;

pub fn tool() -> Value {
    json!({"type":"function","function":{"name":"boxen_read","description":"This tool reads a file on the executing host, relative to the working directory or by absolute path. UTF-8 text returns up to 65536 bytes, with byte offset and limit. PNG/JPEG/GIF/WebP images, WAV/MP3/FLAC/Ogg audio, MP4/MOV/WebM video and PDFs return immutable original-file snapshots as native model input, with explicit tool provenance. After a successful read, the image, audible content, video, or PDF pages appear directly in the next model input for perception; a separate analysis, OCR, or transcription tool is not needed on a compatible model. Model/provider support is required; PDFs use native processing without OCR fallback. Binary files are sent whole, at most 2 MiB each and 16 MiB per run; offsets/limits on media are rejected. There is no automatic transcription, conversion, frame extraction, or summarization. The file must be regular; unknown binary formats are rejected. Media bytes are retained independently of the source path.","parameters":{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}},"required":["path"],"additionalProperties":false}}})
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn kind(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image", "image/png"))
    } else if b.starts_with(b"\xff\xd8\xff") {
        Some(("image", "image/jpeg"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(("image", "image/gif"))
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        Some(("image", "image/webp"))
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WAVE") {
        Some(("audio", "audio/wav"))
    } else if b.starts_with(b"ID3") || (b.len() > 1 && b[0] == 255 && b[1] & 0xe0 == 0xe0) {
        Some(("audio", "audio/mp3"))
    } else if b.starts_with(b"fLaC") {
        Some(("audio", "audio/flac"))
    } else if b.starts_with(b"OggS")
        && (b.windows(8).take(4096).any(|w| w == b"OpusHead")
            || b.windows(6).take(4096).any(|w| w == b"vorbis"))
    {
        Some(("audio", "audio/ogg"))
    } else if b.get(4..8) == Some(b"ftyp") && b.get(8..12) == Some(b"qt  ") {
        Some(("video", "video/mov"))
    } else if b.get(4..8) == Some(b"ftyp") && b.get(8..12) != Some(b"M4A ") {
        Some(("video", "video/mp4"))
    } else if b.starts_with(b"\x1a\x45\xdf\xa3") && b.windows(4).take(4096).any(|w| w == b"webm") {
        Some(("video", "video/webm"))
    } else if b.starts_with(b"%PDF-") {
        Some(("file", "application/pdf"))
    } else {
        None
    }
}

fn file_bytes(path: &Path) -> Result<Vec<u8>> {
    // Nonblocking open prevents a raced FIFO replacement from hanging the daemon.
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err("read accepts regular files only".into());
    }
    if metadata.len() > MAX_FILE as u64 {
        return Err("file exceeds 2 MiB; an explicit smaller artifact can be read".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_FILE + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE {
        return Err("file grew beyond 2 MiB while reading".into());
    }
    Ok(bytes)
}

pub fn read(root: &Path, cwd: &str, run: &str, args: Value) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        path: String,
        offset: Option<usize>,
        limit: Option<usize>,
    }
    let input: Input = serde_json::from_value(args)?;
    if input.path.is_empty() || input.path.len() > 4096 {
        return Err("file path contains 1–4096 bytes".into());
    }
    let path = Path::new(cwd).join(&input.path);
    let bytes = file_bytes(&path)?;
    if let Some((modality, mime)) = kind(&bytes) {
        if input.offset.is_some() || input.limit.is_some() {
            return Err("media reads send the original whole file; text offset/limit do not select pages or time ranges".into());
        }
        let digest = hash(&bytes);
        let directory = root.join(run).join("artifacts");
        state::private_dir(&directory)?;
        let dest = directory.join(&digest);
        if !dest.exists() {
            let used = fs::read_dir(&directory)?
                .try_fold(0u64, |n, e| -> Result<u64> { Ok(n + e?.metadata()?.len()) })?;
            if used + bytes.len() as u64 > MAX_RUN {
                return Err("run artifact quota of 16 MiB is full".into());
            }
            state::publish_new(&dest, &bytes)?;
            #[cfg(unix)]
            fs::File::open(&directory)?.sync_all()?;
        } else if file_bytes(&dest)? != bytes {
            return Err("retained artifact failed integrity validation".into());
        }
        Ok(
            json!({"ok":true,"path":path,"modality":modality,"mime":mime,"bytes":bytes.len(),"sha256":digest,"artifact":{"run":run,"sha256":digest,"mime":mime,"modality":modality},"projection":"native whole-file input; no local conversion"}),
        )
    } else {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "file format is unsupported; bytes have not been substituted with text")?;
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("unknown binary format is not UTF-8 text".into());
        }
        let start = input.offset.unwrap_or(0);
        let limit = input.limit.unwrap_or(65536);
        if !(1..=65536).contains(&limit) || start > bytes.len() || !text.is_char_boundary(start) {
            return Err(
                "text limit is 1–65536 and offset is a UTF-8 byte boundary within the file".into(),
            );
        }
        let mut end = start.saturating_add(limit).min(bytes.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(
            json!({"ok":true,"path":path,"modality":"text","bytes":bytes.len(),"sha256":hash(&bytes),"offset":start,"next_offset":end,"truncated":end<bytes.len(),"text":&text[start..end]}),
        )
    }
}

/// Snapshot references remain in journals; only this projection creates native wire parts.
pub fn project(root: &Path, messages: &[Value]) -> Result<(Vec<Value>, bool, usize)> {
    let mut output = Vec::new();
    let mut pending = Vec::new();
    // Tool results own references atomically with the accepted result. Native
    // media follows the whole tool-result group, including repaired unknowns.
    for original in messages {
        if original["role"] != "tool" && !pending.is_empty() {
            output.push(json!({"role":"user","content":std::mem::take(&mut pending)}));
        }
        let mut message = original.clone();
        if let Some(artifact) = message
            .as_object_mut()
            .and_then(|m| m.remove("boxen_artifact"))
        {
            pending.push(json!({"type":"text","text":format!("Boxen tool output from boxen_read call {}: {}. The following native input is a retained file supplied by that tool, with no additional human instruction.",message["tool_call_id"],message["content"])}));
            pending.push(json!({"type":"boxen_artifact","artifact":artifact}));
        }
        output.push(message);
    }
    if !pending.is_empty() {
        output.push(json!({"role":"user","content":pending}));
    }
    let mut pdf = false;
    let mut missing = 0;
    let mut total = 0;
    for message in &mut output {
        if let Some(parts) = message["content"].as_array_mut() {
            for part in parts {
                if part["type"] != "boxen_artifact" {
                    continue;
                }
                let a = &part["artifact"];
                let load = (|| -> Result<Value> {
                    let run = a["run"]
                        .as_str()
                        .filter(|s| hex64(s))
                        .ok_or("invalid artifact run")?;
                    let digest = a["sha256"]
                        .as_str()
                        .filter(|s| hex64(s))
                        .ok_or("invalid artifact hash")?;
                    let bytes = file_bytes(&root.join(run).join("artifacts").join(digest))?;
                    total += bytes.len();
                    if bytes.len() > MAX_FILE || total > MAX_RUN as usize {
                        return Err("artifact projection exceeds limits".into());
                    }
                    if hash(&bytes) != digest {
                        return Err("artifact integrity mismatch".into());
                    }
                    let (modality, mime) =
                        kind(&bytes).ok_or("unknown retained artifact format")?;
                    if a["modality"] != modality || a["mime"] != mime {
                        return Err("artifact type mismatch".into());
                    }
                    let data = STANDARD.encode(&bytes);
                    let url = format!("data:{mime};base64,{data}");
                    Ok(match modality {
                        "image" => json!({"type":"image_url","image_url":{"url":url}}),
                        "audio" => {
                            json!({"type":"input_audio","input_audio":{"data":data,"format":mime.strip_prefix("audio/").unwrap()}})
                        }
                        "video" => json!({"type":"video_url","video_url":{"url":url}}),
                        "file" => {
                            pdf = true;
                            json!({"type":"file","file":{"filename":format!("{digest}.pdf"),"file_data":url}})
                        }
                        _ => unreachable!(),
                    })
                })();
                *part = match load {
                    Ok(p) => p,
                    Err(e) => {
                        missing += 1;
                        json!({"type":"text","text":format!("Boxen artifact unavailable: {e}. The original read remains recorded; this projection has not reread the source or repeated the tool call.")})
                    }
                };
            }
        }
    }
    Ok((output, pdf, missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_bounds_preserve_utf8_and_binary_selection_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "boxen-read-{}",
            crate::state::random_hex().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let cwd = root.to_str().unwrap();
        fs::write(root.join("text"), "a€z").unwrap();
        let v = read(
            &root,
            cwd,
            &"a".repeat(64),
            json!({"path":"text","limit":3}),
        )
        .unwrap();
        assert_eq!(v["text"], "a");
        assert_eq!(v["next_offset"], 1);
        assert!(
            read(
                &root,
                cwd,
                &"a".repeat(64),
                json!({"path":"text","offset":2})
            )
            .is_err()
        );
        fs::write(root.join("image"), b"\x89PNG\r\n\x1a\nfixture").unwrap();
        assert!(
            read(
                &root,
                cwd,
                &"a".repeat(64),
                json!({"path":"image","limit":10})
            )
            .is_err()
        );
        assert!(read(&root, cwd, &"a".repeat(64), json!({"path":"."})).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn invalid_reference_stays_unavailable_without_source_read() {
        let (m,pdf,missing)=project(Path::new("/does-not-exist"),&[json!({"role":"user","content":[{"type":"boxen_artifact","artifact":{"run":"../source","sha256":"x"}}]})]).unwrap();
        assert!(!pdf);
        assert_eq!(missing, 1);
        assert_eq!(m[0]["content"][0]["type"], "text");
        assert!(
            m[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not reread")
        );
    }
}
