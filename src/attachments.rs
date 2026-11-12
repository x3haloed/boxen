//! Original files are stored separately from messages and model input. Message
//! and run journals retain references; authenticated operators transfer bytes.
use crate::{Result, state};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const FILE_LIMIT: u64 = 32 * 1024 * 1024;
const CHUNK: usize = 192 * 1024;
const STORE_LIMIT: u64 = 1024 * 1024 * 1024;
const STORE_COUNT: usize = 8192;
const MESSAGE_LIMIT: usize = 8;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub name: String,
    pub digest: String,
    pub size: u64,
}
impl Attachment {
    fn validate(&self) -> Result<()> {
        if self.name.is_empty()
            || self.name.len() > 255
            || self
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
            || matches!(self.name.as_str(), "." | "..")
            || self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.size > FILE_LIMIT
        {
            return Err(
                "invalid attachment name, digest, or size (maximum 32 MiB per file)".into(),
            );
        }
        Ok(())
    }
}

pub struct Store {
    root: PathBuf,
    write: Mutex<()>,
}
impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            write: Mutex::new(()),
        }
    }
    fn path(&self, a: &Attachment) -> PathBuf {
        self.root.join(&a.digest)
    }
    fn digest(path: &Path, size: u64) -> Result<String> {
        let mut file = File::open(path)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() != size {
            return Err("attachment size differs from its reference".into());
        }
        let mut hash = Sha256::new();
        let read = std::io::copy(&mut (&mut file).take(size + 1), &mut hash)?;
        if read != size {
            return Err("attachment changed while being verified".into());
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    fn verify(&self, a: &Attachment) -> Result<PathBuf> {
        a.validate()?;
        let path = self.path(a);
        if !path.exists() {
            return Err("attachment original is unavailable on this machine".into());
        }
        if Self::digest(&path, a.size)? != a.digest {
            return Err("attachment original failed its integrity check".into());
        }
        Ok(path)
    }
    pub fn require(&self, files: &[Attachment]) -> Result<()> {
        if files.len() > MESSAGE_LIMIT {
            return Err("a message can carry at most eight attachments".into());
        }
        for a in files {
            self.verify(a)?;
        }
        Ok(())
    }
    pub fn context(&self, files: &[Attachment]) -> String {
        if files.is_empty() {
            return String::new();
        }
        let rows: Vec<_> = files
            .iter()
            .map(|a| match self.verify(a) {
                Ok(path) => json!({"name":a.name,"size":a.size,"path":path}),
                Err(_) => json!({"name":a.name,"size":a.size,"available":false}),
            })
            .collect();
        format!(
            "\nBoxen attachment metadata: these are original files retained on the executing machine. Contents are available through ordinary file tools at the listed paths and are not included in this input. {rows}",
            rows = Value::Array(rows)
        )
    }
    fn capacity(&self, extra: u64, creating: bool) -> Result<()> {
        let mut count = 0;
        let mut size = extra;
        for file in fs::read_dir(&self.root)? {
            let file = file?;
            count += 1;
            size = size.saturating_add(file.metadata()?.len());
            if size > STORE_LIMIT || count + usize::from(creating) > STORE_COUNT {
                return Err("the attachment store has reached its disk limit".into());
            }
        }
        Ok(())
    }
    pub fn operation(&self, request: &Value) -> Result<Value> {
        let a: Attachment = serde_json::from_value(request["file"].clone())?;
        a.validate()?;
        let path = self.path(&a);
        let part = path.with_extension("part");
        match request["action"].as_str().unwrap_or("") {
            "read" => {
                let offset = request["offset"]
                    .as_u64()
                    .ok_or("attachment offset required")?;
                if offset > a.size {
                    return Err("attachment offset is outside the file".into());
                }
                if offset == 0 {
                    self.verify(&a)?;
                }
                let mut file = File::open(path)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut data = vec![0; (a.size - offset).min(CHUNK as u64) as usize];
                file.read_exact(&mut data)?;
                Ok(
                    json!({"data":STANDARD.encode(data),"next":offset + (a.size - offset).min(CHUNK as u64)}),
                )
            }
            "status" | "write" => {
                let _guard = self.write.lock().unwrap();
                if path.exists() {
                    self.verify(&a)?;
                    return Ok(json!({"complete":true,"next":a.size}));
                }
                let next = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                if request["action"] == "status" {
                    return Ok(json!({"complete":false,"next":next}));
                }
                let offset = request["offset"]
                    .as_u64()
                    .ok_or("attachment offset required")?;
                let data = request["data"].as_str().ok_or("attachment data required")?;
                if data.len() > CHUNK.div_ceil(3) * 4 {
                    return Err("attachment chunk exceeds its bound".into());
                }
                let bytes = STANDARD.decode(data)?;
                let end = offset
                    .checked_add(bytes.len() as u64)
                    .ok_or("invalid attachment offset")?;
                if offset != next || end > a.size || (bytes.is_empty() && end != a.size) {
                    return Err("attachment chunk does not continue the retained transfer".into());
                }
                state::private_dir(&self.root)?;
                self.capacity(bytes.len() as u64, !part.exists())?;
                let mut options = OpenOptions::new();
                options.create(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&part)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                drop(file);
                if end == a.size {
                    if Self::digest(&part, a.size)? != a.digest {
                        fs::remove_file(&part)?;
                        return Err("attachment transfer failed its integrity check".into());
                    }
                    fs::rename(part, path)?;
                    #[cfg(unix)]
                    File::open(&self.root)?.sync_all()?;
                }
                Ok(json!({"complete":end == a.size,"next":end}))
            }
            _ => Err("unknown attachment operation".into()),
        }
    }
    pub fn transfer(
        &self,
        files: &[Attachment],
        mut send: impl FnMut(Value) -> Result<Value>,
    ) -> Result<()> {
        self.require(files)?;
        for a in files {
            let mut status = send(json!({"op":"attachment","action":"status","file":a}))?;
            while status["complete"] != true {
                let offset = status["next"]
                    .as_u64()
                    .filter(|n| *n <= a.size)
                    .ok_or("invalid attachment transfer position")?;
                let chunk = self.operation(&json!({"action":"read","file":a,"offset":offset}))?;
                status = send(
                    json!({"op":"attachment","action":"write","file":a,"offset":offset,"data":chunk["data"]}),
                )?;
                if status["next"] != chunk["next"]
                    || (status["complete"] != true && status["next"] == offset)
                {
                    return Err("attachment transfer acknowledgment differs".into());
                }
            }
        }
        Ok(())
    }
}
