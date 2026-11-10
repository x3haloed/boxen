use crate::{
    Result,
    browser_assignment::{self, Backend, Grant},
    jobs::{Jobs, Record},
    mailbox::Address,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

pub struct Attachment {
    path: PathBuf,
    grant: Grant,
}

impl Attachment {
    pub fn config(&self) -> Value {
        json!({"command":self.grant.server,"args":["mcp-assignment",self.path],
            "enabled":true,"required":true,"startup_timeout_sec":15,"tool_timeout_sec":65,
            "default_tools_approval_mode":"approve"})
    }

    pub fn tools(&self) -> &[&str] {
        match self.grant.settings.backend {
            Backend::Chromium => &["browser_action", "browser_cdp", "browser_events"],
            Backend::Safari => &[
                "navigate_to_url",
                "get_page_content",
                "page_interactions",
                "screenshot",
            ],
        }
    }

    pub fn context(&self) -> &str {
        match self.grant.settings.backend {
            Backend::Chromium => {
                "The boxen_browser MCP connection operates the reserved local browser. It provides navigation, input, screenshots and native DevTools commands and events. The browser starts on first use. Screenshot paths identify files in the current assignment's evidence directory."
            }
            Backend::Safari => {
                "The boxen_browser MCP connection operates an isolated Safari automation window. It provides navigation, page inspection, input, console and network inspection, and screenshots. Screenshot paths identify files in the current assignment's evidence directory. The automation session starts without saved sign-in state."
            }
        }
    }

    pub fn identity(&self) -> Value {
        json!({"job":self.grant.job,"version":self.grant.version,"message":self.grant.message,
            "backend":self.grant.settings.backend})
    }
}

pub fn for_run(jobs: &Jobs, record: &Record) -> Result<Option<Attachment>> {
    let (Some(resident), Some(mail)) = (&record.resident, &record.mailbox) else {
        return Ok(None);
    };
    let Some(home) = jobs.runtime_home() else {
        return Ok(None);
    };
    find(home, &resident.id, &mail.message_ids, |id| {
        let message = jobs.mailboxes.get(&record.owner, id)?;
        if message.to
            != (Address::Resident {
                id: resident.id.clone(),
            })
        {
            return Err("browser assignment mailbox recipient differs from the activation".into());
        }
        Ok(message.sender)
    })
}

fn find(
    home: &Path,
    resident: &str,
    messages: &[String],
    sender: impl Fn(&str) -> Result<Address>,
) -> Result<Option<Attachment>> {
    let mut selected = None;
    for id in messages {
        crate::jobs::valid_id(id)?;
        let path = browser_assignment::grant_path(home, id);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(browser_assignment::MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > browser_assignment::MAX_BYTES {
            return Err("browser assignment record exceeds its size bound".into());
        }
        let grant: Grant =
            serde_json::from_slice(&bytes).map_err(|_| "browser assignment record is malformed")?;
        crate::jobs::valid_id(&grant.job)?;
        let expected = Address::Client {
            machine: grant.sender_machine.clone(),
            id: grant.sender_id.clone(),
        };
        if grant.message != *id || grant.resident != resident || sender(id)? != expected {
            return Err(
                "browser assignment record differs from the authenticated mailbox scope".into(),
            );
        }
        if grant.version == 0
            || !grant.server.is_absolute()
            || !grant.server.is_file()
            || !grant.settings.valid_configuration()
        {
            return Err("browser assignment record has invalid runtime paths or version".into());
        }
        if selected.is_some() {
            return Err("multiple browser assignments match one activation".into());
        }
        selected = Some(Attachment { path, grant });
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_assignment::Settings;

    #[test]
    fn grants_are_bound_to_the_exact_batch_resident_and_authenticated_sender() {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/browser-grant-tests");
        let root = parent.join(crate::state::random_hex().unwrap());
        std::fs::create_dir_all(root.join(browser_assignment::DIRECTORY)).unwrap();
        let resident = "a".repeat(64);
        let message = "b".repeat(64);
        let unrelated = "c".repeat(64);
        let grant = Grant {
            message: message.clone(),
            resident: resident.clone(),
            job: "d".repeat(64),
            version: 1,
            sender_machine: "e".repeat(64),
            sender_id: "f".repeat(64),
            server: std::env::current_exe().unwrap(),
            settings: Settings {
                executable: root.join("browser"),
                agent_browser: Some(root.join("helper")),
                backend: Backend::Chromium,
                profile: root.join("profile"),
                evidence: root.join("evidence"),
                headless: true,
            },
        };
        let path = browser_assignment::grant_path(&root, &message);
        std::fs::write(&path, serde_json::to_vec(&grant).unwrap()).unwrap();
        let sender = |_: &str| {
            Ok(Address::Client {
                machine: grant.sender_machine.clone(),
                id: grant.sender_id.clone(),
            })
        };
        assert!(
            find(&root, &resident, std::slice::from_ref(&unrelated), sender)
                .unwrap()
                .is_none()
        );
        let scope = find(&root, &resident, &[unrelated, message.clone()], sender)
            .unwrap()
            .unwrap();
        assert_eq!(scope.config()["args"][1], json!(path));
        assert!(
            find(
                &root,
                &"0".repeat(64),
                std::slice::from_ref(&message),
                sender
            )
            .is_err()
        );
        assert!(
            find(&root, &resident, std::slice::from_ref(&message), |_| Ok(
                Address::Resident {
                    id: resident.clone()
                }
            ))
            .is_err()
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root.join(browser_assignment::DIRECTORY)).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
