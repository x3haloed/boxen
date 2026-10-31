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

#[derive(Serialize, Deserialize)]
pub struct Peer {
    pub key: String,
    pub label: String,
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

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
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
    Ok(())
}

impl State {
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
    pub fn public_key(&self) -> &str {
        &self.identity.public
    }
    pub fn private_key(&self) -> Result<Vec<u8>> {
        key(&self.identity.private)
    }
    pub fn trust(&self, public: &str, label: &str) -> Result<()> {
        key(public)?;
        if public == self.public_key() {
            return Err("cannot pair this machine with itself".into());
        }
        if label.is_empty() || label.len() > 100 {
            return Err("label must contain 1–100 bytes".into());
        }
        let peer = Peer {
            key: public.into(),
            label: label.into(),
        };
        let path = self.directory.join("peers").join(format!("{public}.json"));
        let temporary = self
            .directory
            .join("peers")
            .join(format!(".pending-{}", random_hex()?));
        write_new(&temporary, &serde_json::to_vec_pretty(&peer)?)?;
        // Existing entries are immutable; revoke before changing their labels.
        let result = fs::hard_link(&temporary, &path);
        fs::remove_file(temporary)?;
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn untrust(&self, public: &str) -> Result<()> {
        key(public)?;
        fs::remove_file(self.directory.join("peers").join(format!("{public}.json")))?;
        Ok(())
    }
    pub fn require_trust(&self, public: &str) -> Result<()> {
        key(public)?;
        let peer: Peer = serde_json::from_slice(
            &fs::read(self.directory.join("peers").join(format!("{public}.json"))).map_err(
                |_| "peer is not trusted; exchange identity keys and run trust on both machines",
            )?,
        )?;
        if peer.key != public {
            return Err("peer record key mismatch".into());
        }
        Ok(())
    }
    pub fn peers(&self) -> Result<Vec<Peer>> {
        let mut peers = Vec::new();
        for entry in fs::read_dir(self.directory.join("peers"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "json") {
                peers.push(serde_json::from_slice::<Peer>(&fs::read(path)?)?);
            }
        }
        peers.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(peers)
    }
}
