use crate::{Connection, Control, Result, hash_id, hex};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::digest;
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

pub const FILE_LIMIT: u64 = 32 * 1024 * 1024;
pub const MAX_FILES: usize = 8;
const CHUNK_LIMIT: u64 = 1024 * 1024;
const STORE_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub name: String,
    pub digest: String,
    pub size: u64,
}

impl Attachment {
    pub fn validate(&self) -> Result<()> {
        if !valid_digest(&self.digest)
            || self.size > FILE_LIMIT
            || self.name.is_empty()
            || self.name.len() > 255
            || matches!(self.name.as_str(), "." | "..")
            || self
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err("invalid guest attachment".into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    digest: String,
    size: u64,
    offset: u64,
    data: String,
}

impl Chunk {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        if !valid_digest(&self.digest)
            || self.size > FILE_LIMIT
            || !self.offset.is_multiple_of(CHUNK_LIMIT)
            || self.offset >= self.size
            || self.data.len() > (CHUNK_LIMIT as usize).div_ceil(3) * 4
        {
            return Err("invalid guest attachment chunk".into());
        }
        let bytes = STANDARD.decode(&self.data)?;
        if bytes.len() as u64 != (self.size - self.offset).min(CHUNK_LIMIT) {
            return Err("invalid guest attachment chunk length".into());
        }
        Ok(bytes)
    }
}

pub struct BlobStore(PathBuf, Cell<Option<Instant>>);

impl BlobStore {
    pub fn new(path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(path, Cell::new(None)))
    }

    pub fn retain(&self, name: String, bytes: &[u8]) -> Result<Attachment> {
        let attachment = Attachment {
            name,
            digest: hash_id(bytes),
            size: bytes.len() as u64,
        };
        attachment.validate()?;
        if !self.0.join(&attachment.digest).is_file() {
            self.writer(&attachment.digest, bytes.len() as u64)?
                .write_all(bytes)?;
            self.commit(&attachment.digest)?;
        }
        Ok(attachment)
    }

    pub fn read(&self, file: &Attachment) -> Result<Vec<u8>> {
        file.validate()?;
        self.complete(file)?.ok_or("attachment is still arriving")?;
        let mut bytes = Vec::with_capacity(file.size as usize);
        File::open(self.0.join(&file.digest))?
            .take(file.size + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != file.size || hash_id(&bytes) != file.digest {
            return Err("guest attachment failed its integrity check".into());
        }
        Ok(bytes)
    }

    pub fn prefix(&self, file: &Attachment) -> Result<Vec<u8>> {
        file.validate()?;
        let mut bytes = Vec::new();
        File::open(self.0.join(&file.digest))?
            .take(32)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn insert(&self, chunk: &Chunk, bytes: &[u8]) -> Result<()> {
        if self.0.join(&chunk.digest).is_file() {
            return Ok(());
        }
        let name = chunk_name(&chunk.digest, chunk.size, chunk.offset);
        match fs::read(self.0.join(&name)) {
            Ok(old) if old == bytes => return Ok(()),
            Ok(_) => return Err("conflicting guest attachment chunk".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.writer(&name, bytes.len() as u64)?.write_all(bytes)?;
        self.commit(&name)
    }

    pub fn complete(&self, file: &Attachment) -> Result<Option<()>> {
        file.validate()?;
        if self.0.join(&file.digest).is_file() {
            return Ok(Some(()));
        }
        let parts: Vec<String> = (0..file.size)
            .step_by(CHUNK_LIMIT as usize)
            .map(|offset| chunk_name(&file.digest, file.size, offset))
            .collect();
        if parts.iter().any(|part| !self.0.join(part).is_file()) {
            return Ok(None);
        }
        let mut hash = digest::Context::new(&digest::SHA256);
        let mut merged = self.writer(&file.digest, file.size)?;
        let mut size = 0;
        for part in &parts {
            let bytes = fs::read(self.0.join(part))?;
            size += bytes.len() as u64;
            hash.update(&bytes);
            merged.write_all(&bytes)?;
        }
        drop(merged);
        if size != file.size || hex(hash.finish().as_ref()) != file.digest {
            fs::remove_file(self.0.join(format!("{}.tmp", file.digest)))?;
            return Err("guest attachment failed its integrity check".into());
        }
        self.commit(&file.digest)?;
        for part in parts {
            fs::remove_file(self.0.join(part))?;
        }
        Ok(Some(()))
    }

    // Each endpoint serializes calls to its cache. A staged file is published
    // via rename only after sync, so interrupted writes are never acknowledged.
    fn writer(&self, name: &str, size: u64) -> Result<File> {
        self.expire_staging()?;
        let temp = self.0.join(format!("{name}.tmp"));
        let mut total = size;
        for file in fs::read_dir(&self.0)? {
            let file = file?;
            if file.path() != temp {
                total = total.saturating_add(file.metadata()?.len());
            }
            if total > STORE_LIMIT {
                return Err("the guest attachment cache has reached its disk limit".into());
            }
        }
        let mut opts = OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        Ok(opts.open(temp)?)
    }

    fn expire_staging(&self) -> Result<()> {
        if self
            .1
            .get()
            .is_some_and(|last| last.elapsed() < Duration::from_secs(3600))
        {
            return Ok(());
        }
        let retention = Duration::from_secs(31 * 24 * 60 * 60);
        let now = SystemTime::now();
        for candidate in fs::read_dir(&self.0)? {
            let candidate = candidate?;
            let path = candidate.path();
            if matches!(
                path.extension().and_then(|part| part.to_str()),
                Some("part" | "tmp")
            ) && now
                .duration_since(candidate.metadata()?.modified()?)
                .is_ok_and(|age| age > retention)
            {
                fs::remove_file(path)?;
            }
        }
        self.1.set(Some(Instant::now()));
        Ok(())
    }

    fn commit(&self, name: &str) -> Result<()> {
        let temp = self.0.join(format!("{name}.tmp"));
        OpenOptions::new().write(true).open(&temp)?.sync_all()?;
        fs::rename(temp, self.0.join(name))?;
        #[cfg(all(unix, not(cosmo)))]
        File::open(&self.0)?.sync_all()?;
        Ok(())
    }
}

impl Connection {
    pub fn publish_file(&mut self, file: &Attachment, bytes: &[u8]) -> Result<()> {
        file.validate()?;
        if file.size != bytes.len() as u64 || file.digest != hash_id(bytes) {
            return Err("outgoing guest attachment failed its integrity check".into());
        }
        let mut offset = 0;
        for batch in bytes.chunks(4 * CHUNK_LIMIT as usize) {
            self.publish_frames(batch.chunks(CHUNK_LIMIT as usize).map(|bytes| {
                let part = Control::File(Chunk {
                    digest: file.digest.clone(),
                    size: file.size,
                    offset,
                    data: STANDARD.encode(bytes),
                });
                offset += bytes.len() as u64;
                part
            }))?;
        }
        Ok(())
    }
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn chunk_name(digest: &str, size: u64, offset: u64) -> String {
    format!("{digest}.{size}.{offset}.part")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_parts_survive_restart_and_preserve_bytes() -> Result<()> {
        let path = std::env::temp_dir().join(crate::new_id()?);
        let input = vec![42; CHUNK_LIMIT as usize + 8];
        let file = Attachment {
            name: "example.bin".into(),
            size: input.len() as u64,
            digest: hash_id(&input),
        };
        let packet = |offset: usize| Chunk {
            digest: file.digest.clone(),
            size: file.size,
            offset: offset as u64,
            data: STANDARD.encode(&input[offset..(offset + CHUNK_LIMIT as usize).min(input.len())]),
        };
        let late = packet(CHUNK_LIMIT as usize);
        let store = BlobStore::new(path.clone())?;
        store.insert(&late, &late.bytes()?)?;
        assert_eq!(store.complete(&file)?, None);
        let reopened = BlobStore::new(path.clone())?;
        reopened.insert(&late, &late.bytes()?)?;
        let first = packet(0);
        reopened.insert(&first, &first.bytes()?)?;
        assert_eq!(reopened.read(&file)?, input);
        reopened.insert(&late, &late.bytes()?)?;
        assert_eq!(fs::read_dir(&path)?.count(), 1);
        fs::remove_dir_all(path)?;
        Ok(())
    }
}
