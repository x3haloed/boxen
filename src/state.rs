use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct Identity {
    private: String,
    public: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub removed: bool,
    #[serde(default)]
    pub endpoint_revision: u64,
}

pub struct State {
    directory: PathBuf,
    identity: Identity,
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn key(value: &str) -> Result<Vec<u8>> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err("expected a 64-character lowercase hexadecimal public key".into());
    }
    (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(Into::into))
        .collect()
}

pub fn random_hex() -> Result<String> {
    Ok(hex(&snow::Builder::new(crate::protocol::params())
        .generate_keypair()?
        .private))
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    crate::logging::record(
        crate::logging::Level::Trace,
        crate::logging::Target::State,
        "file.created",
        crate::logging::Context::default(),
        || serde_json::json!({"path":path,"bytes":bytes.len()}),
        || serde_json::Value::Null,
    );
    Ok(())
}

pub(crate) fn publish_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("pending-{}", random_hex()?));
    write_new(&temporary, bytes)?;
    let published = fs::hard_link(&temporary, path);
    fs::remove_file(temporary)?;
    published?;
    Ok(())
}

impl State {
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn open(directory: PathBuf) -> Result<Self> {
        private_dir(&directory)?;
        private_dir(&directory.join("peers"))?;
        let path = directory.join("identity.json");
        if !path.exists() {
            let pair = snow::Builder::new(crate::protocol::params()).generate_keypair()?;
            let identity = Identity {
                private: hex(&pair.private),
                public: hex(&pair.public),
            };
            let temporary = directory.join(format!(".identity-{}", random_hex()?));
            write_new(&temporary, &serde_json::to_vec_pretty(&identity)?)?;
            // Publish a complete identity without overwriting another initializer.
            let published = fs::hard_link(&temporary, &path);
            fs::remove_file(&temporary)?;
            if let Err(error) = published
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error.into());
            }
        }
        let identity: Identity = serde_json::from_slice(&fs::read(path)?)?;
        key(&identity.private)?;
        key(&identity.public)?;
        Ok(Self {
            directory,
            identity,
        })
    }
    /// A local client's authorization survives a daemon restart. Delete this file
    /// while stopped to revoke local clients; never expose its contents in logs.
    pub fn control_token(&self) -> Result<String> {
        let path = self.directory.join("control-token");
        if !path.exists() {
            match publish_new(&path, random_hex()?.as_bytes()) {
                Ok(()) => {}
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) => {}
                Err(error) => return Err(error),
            }
        }
        let token = fs::read_to_string(path)?;
        key(&token)?;
        Ok(token)
    }
    pub fn public_key(&self) -> &str {
        &self.identity.public
    }
    pub fn private_key(&self) -> Result<Vec<u8>> {
        key(&self.identity.private)
    }
    pub fn trust(&self, public: &str, label: &str) -> Result<()> {
        crate::network::admit(self, public, label)
    }
    pub fn untrust(&self, public: &str) -> Result<()> {
        crate::network::remove(self, public)
    }
    pub fn require_trust(&self, public: &str) -> Result<()> {
        key(public)?;
        let own = self
            .directory
            .join("peers")
            .join(format!("{}.json", self.public_key()));
        if own.exists() && crate::network::member(self, self.public_key())?.removed {
            return Err("this machine has been removed from the network".into());
        }
        if crate::network::member(self, public)?.removed {
            return Err("peer is not an active network member".into());
        }
        Ok(())
    }
    pub fn peers(&self) -> Result<Vec<Peer>> {
        Ok(crate::network::members(self)?
            .into_iter()
            .filter(|p| p.key != self.public_key() && !p.removed)
            .collect())
    }
}
