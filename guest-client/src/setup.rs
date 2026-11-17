use crate::session::Session;
use boxen_guest_transport::{Config, Connection, Result, hash_id, host_path, lock_channel, new_id};
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

const KEY_LIMIT: u64 = 64 * 1024;

pub struct Installer {
    send: Sender<Request>,
    receive: Receiver<Result<Session>>,
}

struct Request {
    details: String,
    credential: PathBuf,
}

impl Installer {
    pub fn start(config: PathBuf, data: PathBuf) -> Result<Self> {
        let (send, requests) = mpsc::channel::<Request>();
        let (outcome, receive) = mpsc::channel();
        // AppKit takes over the main thread. This worker also becomes the
        // chat's network worker after a successful first-run setup.
        thread::Builder::new()
            .name("guest-setup".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    match install(&config, &data, request) {
                        Ok((session, sync)) => {
                            if outcome.send(Ok(session)).is_ok() {
                                sync();
                            }
                            break;
                        }
                        Err(error) => {
                            if outcome.send(Err(error)).is_err() {
                                break;
                            }
                        }
                    }
                }
            })?;
        Ok(Self { send, receive })
    }

    pub fn connect(&self, details: String, credential: PathBuf) -> Result<()> {
        self.send
            .send(Request {
                details,
                credential,
            })
            .map_err(|_| "Setup stopped. Open the app again to retry.".into())
    }

    pub fn result(&self) -> Option<Result<Session>> {
        match self.receive.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("Setup stopped. Open the app again to retry.".into()))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Details {
    project_id: String,
    publish_topic: String,
    receive_subscription: String,
}

impl Details {
    fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        let mut details: Self = if input.starts_with('{') {
            serde_json::from_str(input)
                .map_err(|_| "Paste the three connection details from your invitation.")?
        } else {
            let mut fields = serde_json::Map::new();
            for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let (key, value) = line
                    .split_once('=')
                    .ok_or("Put each connection detail on its own name=value line.")?;
                if fields
                    .insert(key.trim().to_owned(), value.trim().into())
                    .is_some()
                {
                    return Err("Each connection detail should appear once.".into());
                }
            }
            serde_json::from_value(fields.into())
                .map_err(|_| "Paste the project ID, outgoing topic, and incoming subscription.")?
        };
        details.project_id = details.project_id.trim().to_owned();
        details.publish_topic = short_id(&details.project_id, "topics", &details.publish_topic)?;
        details.receive_subscription = short_id(
            &details.project_id,
            "subscriptions",
            &details.receive_subscription,
        )?;
        Ok(details)
    }

    fn settings(self, credentials_file: PathBuf) -> Config {
        Config {
            project_id: self.project_id,
            service_account_email: String::new(),
            credentials_file,
            publish_topic: self.publish_topic,
            receive_subscription: self.receive_subscription,
        }
    }
}

fn short_id(project: &str, collection: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.starts_with("projects/") {
        return value.strip_prefix(&format!("projects/{project}/{collection}/"))
            .filter(|id| !id.contains('/') && !id.is_empty())
            .map(str::to_owned)
            .ok_or("The full resource names must match the project and the correct topic or subscription.".into());
    }
    Ok(value.to_owned())
}

fn install(
    config: &Path,
    data: &Path,
    request: Request,
) -> Result<(Session, impl FnOnce() + use<>)> {
    let details = Details::parse(&request.details)?;
    let credential = read_credential(&request.credential)?;
    let relative = PathBuf::from("credentials").join(format!("{}.json", hash_id(&credential)));
    let mut settings = details.settings(relative);
    let mut connection = Connection::for_setup(&mut settings, &credential)?;
    connection
        .check_permissions()
        .map_err(|error| format!("Could not verify access. {error}"))?;
    let session = Session::prepare(connection, data, true).map_err(
        |_| "Could not open local chat history. Close any other copy of this chat and retry.",
    )?;
    save(config, &settings, &credential)
        .map_err(|_| "Access verified, but the app could not save its local setup. Check that your settings folder is writable.")?;
    Ok(session)
}

fn read_credential(path: &Path) -> Result<Vec<u8>> {
    const TOO_LARGE: &str = "The credential file exceeds the 64 KiB limit.";
    let path = host_path(path);
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Cannot access the selected credential file. {error}"))?;
    if !metadata.is_file() {
        return Err("The selected credential path is not a regular file.".into());
    }
    if metadata.len() > KEY_LIMIT {
        return Err(TOO_LARGE.into());
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .map_err(|error| format!("Cannot open the selected credential file. {error}"))?
        .take(KEY_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read the selected credential file. {error}"))?;
    if bytes.len() as u64 > KEY_LIMIT {
        return Err(TOO_LARGE.into());
    }
    Ok(bytes)
}

fn save(config: &Path, settings: &Config, credential: &[u8]) -> Result<()> {
    let directory = config
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut dir = fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dir.mode(0o700);
    }
    let key = directory.join(&settings.credentials_file);
    let key_dir = key.parent().ok_or("credential folder unavailable")?;
    dir.create(key_dir)?;
    let _lock = lock_channel(&directory.join(".guest-setup.lock"))?;
    let temp = directory.join(format!(".guest-settings-{}.tmp", new_id()?));
    let created = !key.exists();
    if created {
        let pending = key.with_extension("new");
        match fs::remove_file(&pending) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if let Err(error) =
            write_private(&pending, credential).and_then(|()| Ok(fs::rename(&pending, &key)?))
        {
            let _ = fs::remove_file(&pending);
            return Err(error);
        }
    } else if read_credential(&key)? != credential {
        return Err("saved credential copy differs".into());
    }
    let result = (|| -> Result<()> {
        write_private(&temp, &serde_json::to_vec_pretty(settings)?)?;
        #[cfg(all(unix, not(cosmo)))]
        File::open(key_dir)?.sync_all()?;
        fs::rename(&temp, config)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
        if created {
            let _ = fs::remove_file(key);
        }
    }
    result
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ids_or_matching_resource_names() -> Result<()> {
        let input = " project_id = example-project\r\npublish_topic=projects/example-project/topics/outgoing\r\nreceive_subscription=incoming\r\n";
        let details = Details::parse(input)?;
        assert_eq!(details.project_id, "example-project");
        assert_eq!(details.publish_topic, "outgoing");
        assert_eq!(details.receive_subscription, "incoming");
        assert!(
            Details::parse(&input.replace("projects/example-project/", "projects/wrong-project/"))
                .is_err()
        );
        assert!(Details::parse(&format!("{input}project_id=another-project")).is_err());
        assert!(Details::parse(r#"{"type":"service_account","private_key":"test"}"#).is_err());
        Ok(())
    }

    #[test]
    fn saves_relative_key_copy_and_reuses_it() -> Result<()> {
        let path = std::env::temp_dir().join(format!("guest-setup-test-{}", new_id()?));
        let config = path.join("guest-pubsub.json");
        let mut settings = Details::parse(r#"{"project_id":"example-project","publish_topic":"outgoing","receive_subscription":"incoming"}"#)?
            .settings(PathBuf::from("credentials/test.json"));
        settings.service_account_email = "example@example-project.iam.gserviceaccount.com".into();
        let original = b"placeholder credential bytes";
        save(&config, &settings, original)?;
        settings.publish_topic = "other-outgoing".into();
        save(&config, &settings, original)?;
        let written: Config = serde_json::from_slice(&fs::read(config)?)?;
        assert_eq!(written.publish_topic, settings.publish_topic);
        assert_eq!(
            written.service_account_email,
            settings.service_account_email
        );
        assert_eq!(fs::read(path.join(written.credentials_file))?, original);
        assert_eq!(fs::read_dir(path.join("credentials"))?.count(), 1);
        fs::remove_dir_all(path)?;
        Ok(())
    }
}
