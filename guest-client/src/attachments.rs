use boxen_guest_transport::{Attachment, FILE_LIMIT, Result, new_id};
use image::{ImageReader, RgbaImage};
use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::Path,
};

pub fn read_attachment(path: &Path) -> Result<(String, Vec<u8>)> {
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
    Ok((name, bytes))
}

pub fn save_copy(path: &Path, bytes: &[u8]) -> Result<()> {
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

pub fn picture(data: &[u8], size: u32) -> Option<RgbaImage> {
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
