//! Service-account authentication and versioned guest messages over Pub/Sub.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{digest, rand, signature};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const PUBSUB: &str = "https://pubsub.googleapis.com/v1";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/pubsub";
// One delivery at a time keeps the response bounded even for Pub/Sub's 10 MB
// maximum message, which expands during Base64 encoding.
const RESPONSE_LIMIT: u64 = 15 * 1024 * 1024;
const WIRE_LIMIT: usize = 256 * 1024;
pub const GUEST_BODY_LIMIT: usize = 8 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    project_id: String,
    service_account_email: String,
    credentials_file: PathBuf,
    publish_topic: String,
    receive_subscription: String,
}

#[derive(Deserialize)]
struct KeyFile {
    #[serde(rename = "type")]
    kind: String,
    client_email: String,
    private_key_id: String,
    private_key: String,
}

pub struct Connection {
    agent: ureq::Agent,
    account: String,
    key_id: String,
    key: signature::RsaKeyPair,
    scope_id: String,
    topic: String,
    subscription: String,
    token: Option<(String, Instant)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    version: u32,
    pub id: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_names: Vec<String>,
}

impl Message {
    pub fn new(id: String, body: String, attachment_names: Vec<String>) -> Self {
        Self {
            version: 1,
            id,
            body,
            attachment_names,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.id.len() != 64
            || !self
                .id
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
            || (self.body.trim().is_empty() && self.attachment_names.is_empty())
            || self.attachment_names.len() > 8
            || self
                .attachment_names
                .iter()
                .any(|name| name.is_empty() || name.len() > 1024)
        {
            return Err("invalid guest message envelope".into());
        }
        Ok(())
    }

    pub fn validate_guest_submission(&self) -> Result<()> {
        self.validate()?;
        if !self.attachment_names.is_empty() || self.body.len() > GUEST_BODY_LIMIT {
            return Err("guest transport accepts text messages up to 8 KiB".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub ack_id: String,
    message: PubSubMessage,
}

#[derive(Deserialize)]
struct PubSubMessage {
    #[serde(default)]
    data: String,
}

impl Delivery {
    pub fn decode(&self) -> Result<Message> {
        // Reject an oversized encoded body before allocating its decoded bytes.
        if self.message.data.len() > WIRE_LIMIT.div_ceil(3) * 4 {
            return Err("oversized guest message".into());
        }
        let bytes = STANDARD.decode(&self.message.data)?;
        if bytes.len() > WIRE_LIMIT {
            return Err("oversized guest message".into());
        }
        let message: Message = serde_json::from_slice(&bytes)?;
        message.validate()?;
        Ok(message)
    }
}

impl Connection {
    pub fn open(path: &Path) -> Result<Self> {
        let path = host_path(path);
        let config: Config =
            read_json(&path).map_err(|_| "could not read guest connection settings")?;
        if config.project_id.is_empty()
            || !config
                .project_id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !resource_id(&config.publish_topic)
            || !resource_id(&config.receive_subscription)
        {
            return Err("invalid guest Pub/Sub resource identifiers".into());
        }
        let path = resolve_key_path(&path, &config.credentials_file);
        let key: KeyFile =
            read_json(&path).map_err(|_| "could not read guest service account credentials")?;
        if key.kind != "service_account" || key.client_email != config.service_account_email {
            return Err("credential does not match the configured guest service account".into());
        }
        let pem = key
            .private_key
            .trim()
            .strip_prefix("-----BEGIN PRIVATE KEY-----")
            .and_then(|pem| pem.strip_suffix("-----END PRIVATE KEY-----"))
            .ok_or("guest service account key must use PKCS#8")?;
        let der = STANDARD
            .decode(pem.split_whitespace().collect::<String>())
            .map_err(|_| "could not decode the service account key")?;
        let rsa = signature::RsaKeyPair::from_pkcs8(&der)
            .map_err(|_| "invalid service account RSA key")?;
        let project = config.project_id;
        let topic = format!(
            "{PUBSUB}/projects/{project}/topics/{}",
            config.publish_topic.replace('%', "%25")
        );
        let subscription = format!(
            "{PUBSUB}/projects/{project}/subscriptions/{}",
            config.receive_subscription.replace('%', "%25")
        );
        let scope_id = hash_id(&serde_json::to_vec(&(
            &key.client_email,
            &topic,
            &subscription,
        ))?);
        Ok(Self {
            agent: ureq::Agent::config_builder()
                .proxy(None)
                .max_redirects(0)
                .timeout_global(Some(Duration::from_secs(12)))
                .build()
                .into(),
            account: key.client_email,
            key_id: key.private_key_id,
            key: rsa,
            scope_id,
            topic,
            subscription,
            token: None,
        })
    }

    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    pub fn authenticate(&mut self) -> Result<()> {
        if self
            .token
            .as_ref()
            .is_some_and(|(_, expires)| Instant::now() < *expires)
        {
            return Ok(());
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let header = json!({"alg":"RS256","typ":"JWT","kid":self.key_id});
        let claims =
            json!({"iss":self.account,"scope":SCOPE,"aud":TOKEN_URL,"iat":now,"exp":now + 300});
        let mut jwt = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?),
        );
        let mut bytes = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &signature::RSA_PKCS1_SHA256,
                &rand::SystemRandom::new(),
                jwt.as_bytes(),
                &mut bytes,
            )
            .map_err(|_| "could not sign guest OAuth assertion")?;
        jwt.push('.');
        URL_SAFE_NO_PAD.encode_string(bytes, &mut jwt);
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let response = self
            .agent
            .post(TOKEN_URL)
            .send_form([
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .map_err(|error| request_failure("guest OAuth exchange", &error))?;
        let token: Token = response
            .into_body()
            .with_config()
            .limit(16384)
            .read_json()
            .map_err(|_| "invalid guest OAuth response")?;
        if token.access_token.is_empty() || token.expires_in == 0 {
            return Err("guest OAuth returned an empty or expired token".into());
        }
        self.token = Some((
            token.access_token,
            Instant::now() + Duration::from_secs(token.expires_in.min(3600).saturating_sub(60)),
        ));
        Ok(())
    }

    pub fn publish(&mut self, message: &Message) -> Result<()> {
        message.validate()?;
        let bytes = serde_json::to_vec(message)?;
        if bytes.len() > WIRE_LIMIT {
            return Err("oversized guest message".into());
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Published {
            message_ids: Vec<String>,
        }
        let published: Published = self.post(
            "publish",
            format!("{}:publish", self.topic),
            json!({"messages":[{"data":STANDARD.encode(bytes)}]}),
        )?;
        if published.message_ids.len() != 1 || published.message_ids[0].is_empty() {
            return Err("guest publication outcome is uncertain".into());
        }
        Ok(())
    }

    pub fn check_permissions(&mut self) -> Result<()> {
        for (resource, permission) in [
            (self.topic.clone(), "pubsub.topics.publish"),
            (self.subscription.clone(), "pubsub.subscriptions.consume"),
        ] {
            let response: Value = self.post(
                "check permissions",
                format!("{resource}:testIamPermissions"),
                json!({"permissions":[permission]}),
            )?;
            if !response["permissions"]
                .as_array()
                .is_some_and(|grants| grants.iter().any(|grant| grant == permission))
            {
                return Err(format!("configured guest principal lacks {permission}").into());
            }
        }
        Ok(())
    }

    pub fn pull(&mut self) -> Result<Vec<Delivery>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Batch {
            #[serde(default)]
            received_messages: Vec<Delivery>,
        }
        match self.post::<Batch>(
            "pull",
            format!("{}:pull", self.subscription),
            json!({"maxMessages":1}),
        ) {
            Ok(response) => Ok(response.received_messages),
            Err(error) if error.is::<PullTimeout>() => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub fn ack(&mut self, ids: &[String]) -> Result<()> {
        if !ids.is_empty() {
            self.post::<Value>(
                "acknowledge",
                format!("{}:acknowledge", self.subscription),
                json!({"ackIds":ids}),
            )?;
        }
        Ok(())
    }

    fn post<T: DeserializeOwned>(
        &mut self,
        operation: &'static str,
        url: String,
        body: Value,
    ) -> Result<T> {
        self.authenticate()?;
        let token = &self
            .token
            .as_ref()
            .ok_or("guest OAuth token unavailable")?
            .0;
        let mut request = self
            .agent
            .post(url)
            .header("Authorization", &format!("Bearer {token}"));
        if operation == "pull" {
            request = request
                .config()
                .timeout_global(Some(Duration::from_secs(5)))
                .build();
        }
        let response = request.send_json(body);
        if matches!(response, Err(ureq::Error::StatusCode(401))) {
            self.token = None;
        }
        if operation == "pull" && matches!(response, Err(ureq::Error::Timeout(_))) {
            return Err(PullTimeout.into());
        }
        response
            .map_err(|error| request_failure(operation, &error))?
            .into_body()
            .with_config()
            .limit(RESPONSE_LIMIT)
            .read_json()
            .map_err(|_| format!("invalid Pub/Sub {operation} response").into())
    }
}

#[derive(Debug)]
struct PullTimeout;

impl std::fmt::Display for PullTimeout {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.write_str("Pub/Sub pull waited for messages")
    }
}

impl Error for PullTimeout {}

fn request_failure(operation: &str, error: &ureq::Error) -> Box<dyn Error + Send + Sync> {
    match error {
        ureq::Error::StatusCode(code) => format!("{operation} failed (HTTP {code})").into(),
        ureq::Error::Timeout(_) => format!("{operation} timed out").into(),
        _ => format!("{operation} network request failed").into(),
    }
}

fn resource_id(id: &str) -> bool {
    (3..=255).contains(&id.len())
        && id.as_bytes()[0].is_ascii_alphabetic()
        && !id.starts_with("goog")
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.~+%".contains(&c))
}

// Rust on APE uses a Unix path parser even when the loader selects Windows.
pub fn host_path(path: &Path) -> PathBuf {
    #[cfg(cosmo)]
    {
        unsafe extern "C" {
            static __hostos: std::ffi::c_int;
        }
        // Cosmopolitan sets this once at startup. Four is its Windows bit.
        if unsafe { __hostos } == 4 {
            return PathBuf::from(path.to_string_lossy().replace('\\', "/"));
        }
    }
    path.to_path_buf()
}

fn resolve_key_path(config: &Path, key: &Path) -> PathBuf {
    let key = host_path(key);
    if key.is_absolute() || drive_absolute(key.as_os_str().as_encoded_bytes()) {
        key
    } else {
        config.parent().unwrap_or_else(|| Path::new("")).join(key)
    }
}

fn drive_absolute(path: &[u8]) -> bool {
    matches!(path, [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
}

/// The caller retains this handle for the lifetime of the channel session.
pub fn lock_channel(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let lock = opts.open(path)?;
    // Cosmopolitan's Windows flock locks the current file length.
    if lock.metadata()?.len() == 0 {
        lock.set_len(1)?;
    }
    try_lock(&lock)?;
    Ok(lock)
}

#[cfg(not(cosmo))]
fn try_lock(file: &File) -> std::io::Result<()> {
    fs2::FileExt::try_lock_exclusive(file)
}

#[cfg(cosmo)]
fn try_lock(file: &File) -> std::io::Result<()> {
    use std::{ffi::c_int, os::fd::AsRawFd};
    unsafe extern "C" {
        fn flock(fd: c_int, op: c_int) -> c_int;
        static LOCK_NB: c_int;
    }
    // Linux Rust bakes in LOCK_NB=4, while Cosmopolitan's Windows
    // implementation reads the Win32 nonblocking flag. LOCK_EX is 2.
    if unsafe { flock(file.as_raw_fd(), 2 | LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err("guest configuration file is too large".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn hash_id(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

pub fn new_id() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0; 32];
    rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "could not generate a guest message ID")?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
