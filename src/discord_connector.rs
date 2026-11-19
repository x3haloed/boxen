//! A bundled addon using only the public messaging API. The runner owns one bot
//! credential, while the daemon owns routing, accepted intake and send receipts.
use crate::{Result, messaging::Instance, state};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    time::{Duration, Instant},
};

#[path = "discord_gateway.rs"]
pub(crate) mod gateway;
const API: &str = "https://discord.com/api/v10";
struct Control {
    agent: ureq::Agent,
    url: String,
    token: String,
    peer: String,
    address: String,
    instance: String,
    local_instance: bool,
}
impl Control {
    fn call(&self, mut args: Value) -> Result<Value> {
        args["instance"] = json!(self.instance);
        let mut r=self.agent.post(format!("{}/api/operation",self.url)).header("Authorization",format!("Bearer {}",self.token)).send_json(json!({"peer":self.peer,"address":self.address,"connector_instance":if self.local_instance {Some(&self.instance)}else{None},"request":{"op":"messaging","args":args}}))?;
        if r.status().as_u16() != 200 {
            return Err(
                format!("local messaging API rejected request (HTTP {})", r.status()).into(),
            );
        }
        Ok(r.body_mut().with_config().limit(512 * 1024).read_json()?)
    }
}
struct Discord {
    agent: ureq::Agent,
    key: String,
    base: String,
    next: Instant,
    channels: HashMap<String, Value>,
}
enum Reply {
    Ok(Value),
    Rejected(HttpFailure),
}
#[derive(Debug)]
struct HttpFailure {
    status: u16,
    code: Option<u64>,
}
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Discord rejected the request (HTTP {})", self.status)
    }
}
impl std::error::Error for HttpFailure {}

fn lookup_failure(error: &(dyn std::error::Error + 'static)) -> Value {
    let failure = error.downcast_ref::<HttpFailure>();
    let reason = match failure
        .filter(|f| (400..500).contains(&f.status))
        .and_then(|f| f.code)
    {
        Some(10003) => "provider_unknown_channel",
        Some(10008) => "provider_unknown_message",
        Some(50001 | 50013) => "provider_access_denied",
        _ => "native_lookup_unresolved",
    };
    let mut evidence = json!({"reason":reason});
    if let Some(failure) = failure {
        evidence["http_status"] = json!(failure.status);
        if let Some(code) = failure.code {
            evidence["provider_code"] = json!(code);
        }
    }
    evidence
}
pub(crate) fn lookup_reason(evidence: &Value) -> &'static str {
    match evidence["reason"].as_str() {
        Some("invalid_channel_id") => "invalid_channel_id",
        Some("provider_unknown_channel") if evidence["provider_code"] == 10003 => {
            "provider_unknown_channel"
        }
        Some("provider_access_denied")
            if evidence["provider_code"] == 50001 || evidence["provider_code"] == 50013 =>
        {
            "provider_access_denied"
        }
        _ => "native_lookup_unresolved",
    }
}
fn unavailable_context(error: &(dyn std::error::Error + 'static)) -> Value {
    json!({"unavailable":true,"lookup":lookup_failure(error)})
}

impl Discord {
    fn context(&mut self, id: &str) -> Result<Value> {
        snowflake(id)?;
        if let Some(value) = self.channels.get(id) {
            return Ok(value.clone());
        }
        let value = self.get(&format!("/channels/{id}"))?;
        if value["id"].as_str() != Some(id) || !value["type"].is_u64() {
            return Err("Discord returned invalid channel metadata".into());
        }
        let context = json!({"space":value["guild_id"],"parent":value["parent_id"],"direct":matches!(value["type"].as_u64(),Some(1|3)),"author":if value["type"]==1 {value["recipients"].as_array().and_then(|a|a.first()).cloned().unwrap_or(Value::Null)}else{Value::Null}});
        if self.channels.len() >= 512 {
            self.channels.clear();
        }
        self.channels.insert(id.into(), context.clone());
        Ok(context)
    }
    fn call(&mut self, method: &str, path: &str, body: Option<Value>) -> Result<Reply> {
        self.request(method, path, body, None)
    }
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<Value>,
        multipart: Option<(&str, &[u8])>,
    ) -> Result<Reply> {
        for _ in 0..3 {
            while Instant::now() < self.next {
                std::thread::sleep(
                    self.next
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(1)),
                );
            }
            let url = format!("{}{path}", self.base);
            let response = if let Some((content_type, bytes)) = multipart {
                self.agent
                    .post(url)
                    .header("Authorization", format!("Bot {}", self.key))
                    .header("Content-Type", content_type)
                    .send(bytes)
            } else if method == "POST" {
                self.agent
                    .post(url)
                    .header("Authorization", format!("Bot {}", self.key))
                    .header(
                        "User-Agent",
                        "DiscordBot (https://github.com/x3haloed/boxen, 0.1)",
                    )
                    .send_json(body.as_ref().ok_or("POST body required")?)
            } else if method == "PUT" {
                self.agent
                    .put(url)
                    .header("Authorization", format!("Bot {}", self.key))
                    .send_empty()
            } else {
                self.agent
                    .get(url)
                    .header("Authorization", format!("Bot {}", self.key))
                    .header(
                        "User-Agent",
                        "DiscordBot (https://github.com/x3haloed/boxen, 0.1)",
                    )
                    .call()
            };
            let mut response = response.map_err(|_| "Discord transport outcome unavailable")?;
            let status = response.status().as_u16();
            if status == 204 {
                return Ok(Reply::Ok(Value::Null));
            }
            if !(200..300).contains(&status) && status != 429 {
                let code = response
                    .body_mut()
                    .with_config()
                    .limit(4096)
                    .read_json::<Value>()
                    .ok()
                    .and_then(|body| body["code"].as_u64());
                return Ok(Reply::Rejected(HttpFailure { status, code }));
            }
            let remaining = response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok());
            let reset = response
                .headers()
                .get("x-ratelimit-reset-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok());
            if remaining == Some("0")
                && let Some(wait) = reset
            {
                self.next = Instant::now() + duration(wait)?;
            }
            let value: Value = response
                .body_mut()
                .with_config()
                .limit(2 * 1024 * 1024)
                .read_json()
                .map_err(|_| "Discord response JSON unavailable or exceeds 2 MiB")?;
            if status == 429 {
                let wait = value["retry_after"]
                    .as_f64()
                    .ok_or("Discord rate limit has no retry duration")?;
                self.next = Instant::now() + duration(wait)?;
                continue;
            }
            return Ok(Reply::Ok(value));
        }
        Err("Discord remains rate limited; no unconfirmed send is automatically replayed".into())
    }
    fn get(&mut self, path: &str) -> Result<Value> {
        match self.call("GET", path, None)? {
            Reply::Ok(value) => Ok(value),
            Reply::Rejected(error) => Err(error.into()),
        }
    }
}
fn duration(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || !(0. ..=86400.).contains(&seconds) {
        return Err("Discord retry duration outside supported one-day bound".into());
    }
    Ok(Duration::from_secs_f64(seconds) + Duration::from_millis(20))
}
fn snowflake(s: &str) -> Result<u64> {
    if s.len() > 20 || s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Discord identifiers are decimal snowflakes".into());
    }
    Ok(s.parse()?)
}
fn loopback(url: &str) -> bool {
    url.strip_prefix("http://")
        .and_then(|a| a.trim_end_matches('/').parse::<std::net::SocketAddr>().ok())
        .is_some_and(|a| a.ip().is_loopback())
}
fn notices(event: &str, instance: &str, fields: Value) {
    println!(
        "{}",
        json!({"at":crate::jobs::now(),"event":event,"instance_id":instance,"fields":fields})
    );
}
pub fn run(home: &Path, mut args: Vec<String>) -> Result<()> {
    let instance = crate::take_option(&mut args, "--instance")?
        .ok_or("--instance is a configured connector id")?;
    crate::jobs::valid_id(&instance)?;
    let url =
        crate::take_option(&mut args, "--url")?.unwrap_or_else(|| "http://127.0.0.1:42425".into());
    if !loopback(&url) {
        return Err(
            "connector control capabilities are sent only to a loopback HTTP origin".into(),
        );
    }
    let credential_override =
        crate::take_option(&mut args, "--credential-file")?.map(std::path::PathBuf::from);
    if credential_override
        .as_ref()
        .is_some_and(|p| !p.is_absolute())
    {
        return Err("runner credential override is an absolute local path".into());
    }
    let peer = crate::take_option(&mut args, "--peer")?;
    let address = crate::take_option(&mut args, "--address")?.unwrap_or_default();
    let api = crate::take_option(&mut args, "--api-base")?.unwrap_or_else(|| API.into());
    if api != API && !loopback(&api) {
        return Err("Discord uses its official API or an explicit loopback fixture origin".into());
    }
    let once = if let Some(p) = args.iter().position(|s| s == "--once") {
        args.remove(p);
        true
    } else {
        false
    };
    if !args.is_empty() {
        return Err("unexpected Discord connector arguments".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let token = fs::read_to_string(home.join("control-token"))?
        .trim()
        .to_owned();
    let local: Value = agent
        .get(format!("{url}/api/status"))
        .header("Authorization", format!("Bearer {token}"))
        .call()?
        .body_mut()
        .read_json()?;
    let control = Control {
        agent: agent.clone(),
        url,
        token,
        local_instance: peer.is_none(),
        peer: peer.unwrap_or_else(|| local["identity"].as_str().unwrap_or("").into()),
        address,
        instance: instance.clone(),
    };
    let first: Instance = serde_json::from_value(control.call(json!({"action":"get"}))?)?;
    if first.definition.provider != "discord" {
        return Err("Discord runner requires a discord connector instance".into());
    }
    let mut discord = Discord {
        agent,
        key: String::new(),
        base: api,
        next: Instant::now(),
        channels: HashMap::new(),
    };
    let mut gateway: Option<gateway::Gateway> = None;
    let mut pending = std::collections::VecDeque::<Value>::new();
    let mut gateway_next = Instant::now();
    let mut gateway_failures = 0u64;
    let mut gateway_health = (false, 0u64, 0u64);
    let mut channel_failures: HashMap<String, (u64, Instant)> = HashMap::new();
    let mut failures = 0u64;
    let mut backoff = 2u64;
    loop {
        let result = (|| -> Result<bool> {
            let current: Instance = serde_json::from_value(control.call(json!({"action":"get"}))?)?;
            if !current.definition.enabled {
                gateway = None;
                pending.clear();
                return Ok(false);
            }
            let credential_file = credential_override
                .as_ref()
                .or(current.definition.credential_file.as_ref())
                .ok_or("Discord connector configuration includes a credential-file reference")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(credential_file)?.permissions().mode() & 0o077 != 0 {
                    return Err("Discord credential file requires owner-only permissions".into());
                }
            }
            if fs::metadata(credential_file)?.len() > 4096 {
                return Err("Discord credential file exceeds 4096 bytes".into());
            }
            let key = fs::read_to_string(credential_file)?.trim().to_owned();
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err("Discord credential file is empty or has unexpected formatting".into());
            }
            if discord.key != key {
                gateway = None;
                pending.clear();
                gateway_next = Instant::now();
                discord.channels.clear();
                discord.key = key;
                let identity = match discord.get("/users/@me") {
                    Ok(i) => i,
                    Err(e) => {
                        discord.key.clear();
                        return Err(e);
                    }
                };
                if identity["id"] != current.definition.account || identity["bot"] != true {
                    discord.key.clear();
                    return Err("Discord credential account differs from this connector instance's configured bot".into());
                }
                notices(
                    "discord.connected",
                    &instance,
                    json!({"account":current.definition.account}),
                );
            }
            if current.gateway && !once {
                if gateway.is_none() && Instant::now() >= gateway_next {
                    let start = (|| -> Result<gateway::Gateway> {
                        let url = if let Some(url) =
                            current.definition.settings["gateway_url"].as_str()
                        {
                            if !url.starts_with("ws://127.0.0.1:") {
                                return Err("custom Gateway URLs are loopback fixtures".into());
                            }
                            url.to_owned()
                        } else {
                            discord.get("/gateway/bot")?["url"]
                                .as_str()
                                .ok_or("Gateway URL unavailable")?
                                .to_owned()
                        };
                        let saved = control.call(json!({"action":"gateway_state"}))?;
                        gateway::Gateway::start(
                            url,
                            discord.key.clone(),
                            serde_json::from_value(saved["session"].clone())?,
                        )
                    })();
                    match start {
                        Ok(g) => {
                            gateway = Some(g);
                            gateway_failures = 0;
                            channel_failures.remove("_gateway");
                        }
                        Err(_) => {
                            gateway_failures += 1;
                            let wait = (1u64 << gateway_failures.min(6)).min(60);
                            gateway_next = Instant::now() + Duration::from_secs(wait);
                            channel_failures
                                .insert("_gateway".into(), (gateway_failures, gateway_next));
                            if gateway_failures == 1 || gateway_failures.is_power_of_two() {
                                notices(
                                    "discord.gateway_unavailable",
                                    &instance,
                                    json!({"failures":gateway_failures,"retry_seconds":wait}),
                                );
                            }
                        }
                    }
                }
                if let Some(g) = &gateway {
                    if let Ok(presence) = control.call(json!({"action":"native_presence"})) {
                        g.presence(presence);
                    }
                    while pending.len() < 64 {
                        match g.events.try_recv() {
                            Ok(v) => pending.push_back(v),
                            Err(_) => break,
                        }
                    }
                    while let Some(event) = pending.front() {
                        let kind = event["type"].as_str().unwrap_or("");
                        let data = &event["data"];
                        if kind == "GUILD_CREATE" {
                            for c in data["channels"].as_array().into_iter().flatten() {
                                cache_channel(&mut discord, c);
                            }
                        } else if matches!(
                            kind,
                            "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE"
                        ) {
                            cache_channel(&mut discord, data);
                        } else if matches!(kind, "CHANNEL_DELETE" | "THREAD_DELETE")
                            && let Some(id) = data["id"].as_str()
                        {
                            discord.channels.remove(id);
                        }
                        let mut context = Value::Null;
                        if matches!(kind, "MESSAGE_CREATE" | "MESSAGE_REACTION_ADD")
                            && let Some(id) = data["channel_id"].as_str()
                            && current
                                .residents()
                                .iter()
                                .any(|r| current.policy().potential(r, "receive", id))
                        {
                            context = discord
                                .context(id)
                                .unwrap_or_else(|error| unavailable_context(error.as_ref()));
                        }
                        if kind == "MESSAGE_REACTION_ADD" && context.is_object() {
                            context["message_author_id"] = if data["message_author_id"].is_string()
                            {
                                data["message_author_id"].clone()
                            } else if let (Some(channel), Some(message)) =
                                (data["channel_id"].as_str(), data["message_id"].as_str())
                            {
                                if snowflake(channel).is_ok() && snowflake(message).is_ok() {
                                    discord
                                        .get(&format!("/channels/{channel}/messages/{message}"))
                                        .map(|v| v["author"]["id"].clone())
                                        .unwrap_or(Value::Null)
                                } else {
                                    Value::Null
                                }
                            } else {
                                Value::Null
                            };
                        }
                        let result=control.call(json!({"action":"gateway_event","event":event,"native_context":context}));
                        match result {
                            Ok(v) => {
                                g.acknowledge(&v["session"])?;
                                pending.pop_front();
                            }
                            Err(_) => {
                                channel_failures
                                    .insert("_gateway_intake".into(), (1, Instant::now()));
                                break;
                            }
                        }
                    }
                    if pending.is_empty() {
                        channel_failures.remove("_gateway_intake");
                    }
                    let health = (
                        g.connected.load(std::sync::atomic::Ordering::Relaxed),
                        g.failures.load(std::sync::atomic::Ordering::Relaxed),
                        g.pressure.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    if health != gateway_health
                        && (health.0 != gateway_health.0
                            || health.1.is_power_of_two()
                            || health.2.is_power_of_two())
                    {
                        notices(
                            "discord.gateway_health",
                            &instance,
                            json!({"connected":health.0,"failures":health.1,"backpressure_resumes":health.2,"reason":g.last_error.lock().unwrap().clone()}),
                        );
                    }
                    gateway_health = health;
                    if !health.0 {
                        channel_failures.insert("_gateway".into(), (health.1, Instant::now()));
                    } else {
                        channel_failures.remove("_gateway");
                    }
                }
            } else {
                gateway = None;
                pending.clear();
                channel_failures.remove("_gateway");
                channel_failures.remove("_gateway_intake");
            }
            let mut conversations: HashSet<String> = current
                .definition
                .bindings
                .iter()
                .filter(|b| b.receive)
                .map(|b| b.conversation.clone())
                .collect();
            for resident in current.residents() {
                for scope in current.attention(&resident).watched {
                    if scope.kind == "conversation"
                        && let Some(id) = scope.id
                    {
                        conversations.insert(id);
                    }
                }
            }
            for conversation in conversations {
                if current.gateway {
                    continue;
                }
                if !current
                    .residents()
                    .iter()
                    .any(|r| current.policy().potential(r, "receive", &conversation))
                {
                    continue;
                }
                if channel_failures
                    .get(&conversation)
                    .is_some_and(|(_, next)| Instant::now() < *next)
                {
                    continue;
                }
                let native_context = if current.policy.is_some() || !current.attention.is_empty() {
                    discord.context(&conversation)?
                } else {
                    Value::Null
                };
                match poll(
                    &control,
                    &mut discord,
                    &conversation,
                    &current.definition.account,
                    &native_context,
                ) {
                    Ok(()) => {
                        if let Some((count, _)) = channel_failures.remove(&conversation) {
                            notices(
                                "discord.channel_recovered",
                                &instance,
                                json!({"conversation":conversation,"failures":count}),
                            );
                        }
                    }
                    Err(error) => {
                        let count = channel_failures
                            .get(&conversation)
                            .map(|(n, _)| n + 1)
                            .unwrap_or(1);
                        let wait = (1u64 << count.min(6)).min(60);
                        channel_failures.insert(
                            conversation.clone(),
                            (count, Instant::now() + Duration::from_secs(wait)),
                        );
                        if count == 1 || count.is_multiple_of(32) {
                            notices(
                                "discord.channel_unavailable",
                                &instance,
                                json!({"conversation":conversation,"failures":count,"error":error.to_string(),"retry_seconds":wait}),
                            );
                        }
                    }
                }
            }
            deliver(&control, &mut discord)?;
            Ok(true)
        })();
        match result {
            Ok(false) => {
                let _ = control.call(json!({"action":"health","state":"disabled"}));
                notices("discord.disabled", &instance, json!({}));
                if once {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            Ok(true) => {
                let channels: Vec<_> = channel_failures.keys().cloned().collect();
                let _=control.call(json!({"action":"health","state":if channels.is_empty(){"ready"}else{"degraded"},"channels":channels}));
                if failures > 0 {
                    notices("discord.recovered", &instance, json!({"failures":failures}));
                }
                failures = 0;
                backoff = 2;
                if once {
                    return Ok(());
                }
            }
            Err(error) => {
                let _ = control.call(json!({"action":"health","state":"unavailable"}));
                failures += 1;
                if failures == 1 || failures.is_multiple_of(32) {
                    notices(
                        "discord.unavailable",
                        &instance,
                        json!({"error":error.to_string(),"failures":failures,"retry_seconds":backoff}),
                    );
                }
                if once {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(60);
                continue;
            }
        }
        if let Some(g) = &gateway {
            if let Ok(v) = g.events.recv_timeout(Duration::from_secs(2)) {
                pending.push_back(v);
            }
        } else {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}
fn poll(
    control: &Control,
    discord: &mut Discord,
    conversation: &str,
    account: &str,
    native_context: &Value,
) -> Result<()> {
    snowflake(conversation)?;
    let checkpoint = control.call(json!({"action":"cursor","conversation":conversation}))?;
    let after = checkpoint["checkpoint"]["cursor"].as_str();
    let mut page = discord.get(&format!("/channels/{conversation}/messages?limit=25"))?;
    let mut accumulated = vec![];
    let mut reached = false;
    for _ in 0..40 {
        let messages = page
            .as_array()
            .ok_or("Discord channel history is not an array")?;
        if messages.is_empty() {
            reached = true;
            break;
        }
        let mut oldest = u64::MAX;
        for message in messages {
            let mid = snowflake(message["id"].as_str().ok_or("Discord message has no id")?)?;
            oldest = oldest.min(mid);
            if after.is_none() {
                accumulated.push(message.clone());
                reached = true;
                continue;
            }
            if mid <= snowflake(after.unwrap())? {
                reached = true;
            } else {
                accumulated.push(message.clone());
            }
        }
        if reached || messages.len() < 25 {
            reached = true;
            break;
        }
        page = discord.get(&format!(
            "/channels/{conversation}/messages?limit=25&before={oldest}"
        ))?;
    }
    if !reached {
        return Err("Discord catch-up exceeds 1000 messages; cursor is preserved and no correspondence is silently skipped".into());
    }
    accumulated.sort_by_key(|v| {
        v["id"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    });
    if after.is_none() {
        // Registration starts at the current tail. No historical messages are invented as new arrivals.
        let cursor = accumulated
            .last()
            .and_then(|v| v["id"].as_str())
            .unwrap_or("0");
        control.call(json!({"action":"ingest","conversation":conversation,"after":null,"cursor":cursor,"messages":[]}))?;
        notices(
            "discord.receive_started",
            &control.instance,
            json!({"conversation":conversation,"cursor":cursor,"history":"from_now"}),
        );
        return Ok(());
    }
    let mut cursor = after.unwrap().to_owned();
    let normalized:Vec<_>=accumulated.iter().map(|v|crate::messaging::bounded_native(&json!({"id":v["id"],"author":v["author"],"body":v["content"].as_str().unwrap_or(""),"timestamp":v["timestamp"],"attachments":v["attachments"],"attachment_retention":"remote_references_only","native":v,"content_availability":if v["content"].as_str().is_some_and(|s|!s.is_empty())||v["attachments"].as_array().is_some_and(|a|!a.is_empty()){"present"}else{"empty_or_unavailable"}}))).collect();
    let normalized: Vec<_> = normalized
        .into_iter()
        .map(|mut v| {
            v["space"] = native_context["space"].clone();
            v["parent"] = native_context["parent"].clone();
            v["direct"] = native_context["direct"].clone();
            v["mention"] = json!(
                v["native"]["mentions"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|u| u["id"] == account))
            );
            v["reply"] = json!(v["native"]["referenced_message"]["author"]["id"] == account);
            v
        })
        .collect();
    let mut offset = 0;
    while offset < normalized.len() {
        let mut end = offset;
        let mut bytes = 0;
        while end < normalized.len() && end - offset < 8 {
            let next = serde_json::to_vec(&normalized[end])?.len();
            if end > offset && bytes + next > 128 * 1024 {
                break;
            }
            bytes += next;
            end += 1;
        }
        // The native cursor covers self posts too, so self-only pages advance
        // durably without being presented as new correspondence on every poll.
        let batch = &normalized[offset..end];
        let next = batch.last().unwrap()["id"].as_str().unwrap();
        let messages: Vec<_> = batch
            .iter()
            .filter(|v| v["author"]["id"].as_str() != Some(account))
            .collect();
        control.call(json!({"action":"ingest","conversation":conversation,"after":cursor,"cursor":next,"messages":messages}))?;
        if messages.len() != batch.len() {
            notices(
                "discord.self_echo_skipped",
                &control.instance,
                json!({"conversation":conversation,"count":batch.len()-messages.len(),"cursor":next}),
            );
        }
        cursor = next.into();
        offset = end;
    }
    Ok(())
}
fn text_parts(mut body: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    while !body.is_empty() {
        let mut units = 0;
        let mut end = body.len();
        let mut boundary = 0;
        for (i, c) in body.char_indices() {
            if units + c.len_utf16() > 2000 {
                end = i;
                break;
            }
            units += c.len_utf16();
            if units >= 1000 && c.is_whitespace() {
                boundary = i + c.len_utf8();
            }
        }
        if end < body.len() && boundary > 0 {
            end = boundary;
        }
        parts.push(&body[..end]);
        body = &body[end..];
    }
    parts
}
fn deliver(control: &Control, discord: &mut Discord) -> Result<()> {
    let outbox = control.call(json!({"action":"outbox"}))?;
    if outbox["enabled"] != true {
        return Ok(());
    }
    for message in outbox["messages"].as_array().into_iter().flatten().take(16) {
        if message["delivery"]["status"] != "queued" {
            continue;
        }
        let conversation = message["to"]["conversation"]
            .as_str()
            .ok_or("outgoing conversation unavailable")?;
        let valid_conversation = snowflake(conversation).is_ok();
        let body = message["body"]
            .as_str()
            .ok_or("outgoing text unavailable")?;
        // Rejection is recorded before any external effect if the adapter cannot represent the request.
        let claim_id = state::random_hex()?;
        let instance: Instance = serde_json::from_value(control.call(json!({"action":"get"}))?)?;
        let native_context = if !valid_conversation {
            json!({"unavailable":true,"lookup":{"reason":"invalid_channel_id"}})
        } else if instance.policy.is_some() {
            // Refresh authoritative native scope before dispatch; cached parent metadata can change.
            discord.channels.remove(conversation);
            match discord.context(conversation) {
                Ok(context) => context,
                Err(error) => unavailable_context(error.as_ref()),
            }
        } else {
            Value::Null
        };
        let claimed =
            control.call(json!({"action":"claim","id":message["id"],"claim_id":claim_id,"native_context":native_context}))?;
        if claimed["claimed"] != true {
            continue;
        }
        if native_context["unavailable"] == true {
            // No native effect was attempted. A terminal rejection prevents one
            // unavailable conversation from occupying the dispatch window forever.
            let reason = lookup_reason(&native_context["lookup"]);
            let mut result = native_context["lookup"].clone();
            result["conversation"] = json!(conversation);
            result["stage"] = json!("channel_lookup");
            result["dispatch_attempted"] = json!(false);
            control.call(json!({"action":"complete","id":message["id"],"claim_id":claim_id,"status":"rejected","reason":reason,"result":result}))?;
            notices(
                "discord.dispatch_context_unavailable",
                &control.instance,
                json!({"conversation":conversation,"message":message["id"],"reason":reason}),
            );
            continue;
        }
        if let Some(service) = message
            .get("service")
            .filter(|v| !v.is_null() && v["operation"] != "send_files")
        {
            let operation = service["operation"].as_str().unwrap_or("");
            let resident = claimed["message"]["sender"]["id"]
                .as_str()
                .ok_or("service resident unavailable")?;
            let result = service_call(
                discord,
                &instance,
                resident,
                conversation,
                operation,
                &service["arguments"],
                &native_context,
            );
            let (status, external_id, reason, result) = match result {
                Ok(Reply::Ok(value)) => (
                    "accepted",
                    if operation == "react" {
                        service["arguments"]["message_id"].as_str()
                    } else {
                        None
                    },
                    None,
                    Some(value),
                ),
                Ok(Reply::Rejected(error)) => {
                    let status =
                        if operation == "read_context" || (400..500).contains(&error.status) {
                            "rejected"
                        } else {
                            "uncertain"
                        };
                    let mut evidence = lookup_failure(&error);
                    evidence["conversation"] = json!(conversation);
                    evidence["stage"] = json!("service");
                    let reason = match evidence["reason"].as_str() {
                        Some("provider_unknown_channel") => "provider_unknown_channel",
                        Some("provider_unknown_message") => "provider_unknown_message",
                        Some("provider_access_denied") => "provider_access_denied",
                        _ => "service_rejected",
                    };
                    (status, None, Some(reason), Some(evidence))
                }
                Err(error) if operation == "read_context" => {
                    let mut evidence = lookup_failure(error.as_ref());
                    evidence["conversation"] = json!(conversation);
                    evidence["stage"] = json!("history_read");
                    let reason = match evidence["reason"].as_str() {
                        Some("provider_unknown_channel") => "provider_unknown_channel",
                        Some("provider_unknown_message") => "provider_unknown_message",
                        Some("provider_access_denied") => "provider_access_denied",
                        _ => "native_lookup_unresolved",
                    };
                    ("rejected", None, Some(reason), Some(evidence))
                }
                Err(_) => ("uncertain", None, Some("service_unavailable"), None),
            };
            control.call(json!({"action":"complete","id":message["id"],"claim_id":claim_id,"status":status,"external_id":external_id,"reason":reason,"result":result}))?;
            notices(
                "discord.service_outcome",
                &control.instance,
                json!({"message_id":message["id"],"operation":operation,"status":status,"reason":reason}),
            );
            continue;
        }
        let upload = message["service"]["operation"] == "send_files";
        let files = if upload {
            match load_uploads(control, message) {
                Ok(files) => files,
                Err(_) => {
                    control.call(json!({"action":"complete","id":message["id"],"claim_id":claim_id,"status":"rejected","reason":"attachment_unavailable"}))?;
                    notices(
                        "discord.upload_unavailable",
                        &control.instance,
                        json!({"message_id":message["id"]}),
                    );
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        let body = if upload {
            message["service"]["arguments"]["body"]
                .as_str()
                .unwrap_or("")
        } else {
            body
        };
        let (status, external_id, reason) = if !valid_conversation {
            ("rejected", None, Some("invalid_native_conversation".into()))
        } else if body.is_empty() && files.is_empty() {
            ("rejected", None, Some("empty_body".into()))
        } else {
            let mut parts = text_parts(body);
            if parts.is_empty() && !files.is_empty() {
                parts.push("");
            }
            let mut first_id = None;
            let mut aggregate = ("accepted", None);
            for (index, part) in parts.iter().enumerate() {
                control.call(json!({"action":"part","id":message["id"],"claim_id":claim_id,"index":index,"total":parts.len(),"status":"claimed"}))?;
                let nonce = format!(
                    "{:x}",
                    Sha256::digest(
                        format!("{}:{index}", message["id"].as_str().unwrap()).as_bytes()
                    )
                );
                let mut payload = json!({"content":part,"allowed_mentions":{"parse":[]},"nonce":&nonce[..25],"enforce_nonce":true});
                if index == 0
                    && let Some(reply) = claimed["native_reply"].as_str()
                {
                    payload["message_reference"] =
                        json!({"message_id":reply,"fail_if_not_exists":false});
                }
                let reply = if index == 0 && !files.is_empty() {
                    let (content_type, bytes) = multipart_upload(&payload, &files)?;
                    discord.request(
                        "POST",
                        &format!("/channels/{conversation}/messages"),
                        None,
                        Some((&content_type, &bytes)),
                    )
                } else {
                    discord.call(
                        "POST",
                        &format!("/channels/{conversation}/messages"),
                        Some(payload),
                    )
                };
                let (part_status, part_id, part_reason) = match reply {
                    Ok(Reply::Ok(value)) => {
                        if let Some(id) = value["id"].as_str() {
                            ("accepted", Some(id.to_owned()), None)
                        } else {
                            (
                                "uncertain",
                                None,
                                Some("service_message_id_unavailable".into()),
                            )
                        }
                    }
                    Ok(Reply::Rejected(error)) => (
                        if (400..500).contains(&error.status) {
                            "rejected"
                        } else {
                            "uncertain"
                        },
                        None,
                        Some(match lookup_failure(&error)["reason"].as_str() {
                            Some("native_lookup_unresolved") | None => {
                                format!("http_{}", error.status)
                            }
                            Some(reason) => reason.to_owned(),
                        }),
                    ),
                    Err(_) => (
                        "uncertain",
                        None,
                        Some("transport_or_response_unavailable".into()),
                    ),
                };
                control.call(json!({"action":"part","id":message["id"],"claim_id":claim_id,"index":index,"total":parts.len(),"status":part_status,"external_id":part_id,"reason":part_reason}))?;
                notices(
                    "discord.send_part_outcome",
                    &control.instance,
                    json!({"message_id":message["id"],"index":index,"total":parts.len(),"status":part_status,"external_id":part_id,"reason":part_reason}),
                );
                if first_id.is_none() && part_status == "accepted" {
                    first_id = part_id;
                }
                if part_status != "accepted" {
                    aggregate = (
                        if index > 0 { "uncertain" } else { part_status },
                        if index > 0 {
                            Some("partial_delivery".into())
                        } else {
                            part_reason
                        },
                    );
                    break;
                }
            }
            (aggregate.0, first_id, aggregate.1)
        };
        control.call(json!({"action":"complete","id":message["id"],"claim_id":claim_id,"status":status,"external_id":external_id,"reason":reason}))?;
        notices(
            "discord.send_outcome",
            &control.instance,
            json!({"message_id":message["id"],"status":status,"external_id":external_id,"reason":reason}),
        );
    }
    Ok(())
}

// Only a boundary behind a Boxen read filter requires an opaque cursor.
fn history_cursor_cipher(key: &str, salt: &[u8]) -> Result<Box<dyn snow::types::Cipher>> {
    use snow::resolvers::CryptoResolver;
    let mut cipher = snow::resolvers::DefaultResolver
        .resolve_cipher(&snow::params::CipherChoice::ChaChaPoly)
        .ok_or("Discord history cursor cipher unavailable")?;
    let seed: [u8; 32] = Sha256::new()
        .chain_update(b"boxen discord history cursor v1")
        .chain_update(salt)
        .chain_update(key.as_bytes())
        .finalize()
        .into();
    cipher.set(&seed);
    Ok(cipher)
}

fn history_cursor(key: &str, scope: &[u8], id: u64) -> Result<String> {
    use base64::Engine;
    let salt = state::key(&state::random_hex()?)?;
    let cipher = history_cursor_cipher(key, &salt)?;
    let mut bytes = [0; 56];
    bytes[..32].copy_from_slice(&salt);
    cipher.encrypt(0, scope, &id.to_be_bytes(), &mut bytes[32..]);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn history_cursor_id(key: &str, scope: &[u8], token: &str) -> Result<String> {
    use base64::Engine;
    if token.len() != 75 {
        return Err("Discord history cursor has invalid length".into());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token)?;
    if bytes.len() != 56 {
        return Err("Discord history cursor has invalid length".into());
    }
    let cipher = history_cursor_cipher(key, &bytes[..32])?;
    let mut id = [0; 8];
    cipher
        .decrypt(0, scope, &bytes[32..], &mut id)
        .map_err(|_| "Discord history cursor is unavailable in this scope")?;
    Ok(u64::from_be_bytes(id).to_string())
}

fn service_call(
    discord: &mut Discord,
    instance: &Instance,
    resident: &str,
    conversation: &str,
    operation: &str,
    args: &Value,
    context: &Value,
) -> Result<Reply> {
    snowflake(conversation)?;
    if operation == "react" {
        let Some(id) = args["message_id"].as_str().filter(|s| snowflake(s).is_ok()) else {
            return Ok(Reply::Rejected(HttpFailure {
                status: 400,
                code: None,
            }));
        };
        let Some(emoji) = args["emoji"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        else {
            return Ok(Reply::Rejected(HttpFailure {
                status: 400,
                code: None,
            }));
        };
        let encoded: String = emoji
            .as_bytes()
            .iter()
            .map(|b| format!("%{b:02X}"))
            .collect();
        return discord.call(
            "PUT",
            &format!("/channels/{conversation}/messages/{id}/reactions/{encoded}/@me"),
            None,
        );
    }
    if operation != "read_context" {
        return Ok(Reply::Rejected(HttpFailure {
            status: 400,
            code: None,
        }));
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default, deny_unknown_fields)]
    struct Input {
        message_id: Option<String>,
        before_id: Option<String>,
        after_id: Option<String>,
        before_cursor: Option<String>,
        after_cursor: Option<String>,
        limit: Option<usize>,
        before: Option<usize>,
        after: Option<usize>,
    }
    let input: Input = match serde_json::from_value(args.clone()) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Reply::Rejected(HttpFailure {
                status: 400,
                code: None,
            }));
        }
    };
    if [
        &input.before_id,
        &input.after_id,
        &input.message_id,
        &input.before_cursor,
        &input.after_cursor,
    ]
    .into_iter()
    .filter(|v| v.is_some())
    .count()
        > 1
    {
        return Ok(Reply::Rejected(HttpFailure {
            status: 400,
            code: None,
        }));
    }
    for id in [&input.message_id, &input.before_id, &input.after_id]
        .into_iter()
        .flatten()
    {
        snowflake(id)?;
    }
    let limit = input.limit.unwrap_or(25);
    let before = input.before.unwrap_or(20);
    let after = input.after.unwrap_or(5);
    if !(1..=50).contains(&limit) || before > 50 || after > 50 {
        return Ok(Reply::Rejected(HttpFailure {
            status: 400,
            code: None,
        }));
    }
    let cursor_scope = |direction: &str| {
        serde_json::to_vec(&(&instance.definition.id, resident, conversation, direction))
    };
    let before_id = if let Some(token) = &input.before_cursor {
        Some(history_cursor_id(
            &discord.key,
            &cursor_scope("older")?,
            token,
        )?)
    } else {
        input.before_id.clone()
    };
    let after_id = if let Some(token) = &input.after_cursor {
        Some(history_cursor_id(
            &discord.key,
            &cursor_scope("newer")?,
            token,
        )?)
    } else {
        input.after_id.clone()
    };
    let descending = after_id.is_none() && input.message_id.is_none();
    let selection = if let Some(id) = input.before_id.as_ref() {
        json!({"before_id":id,"limit":limit})
    } else if let Some(id) = input.after_id.as_ref() {
        json!({"after_id":id,"limit":limit})
    } else if let Some(id) = input.message_id.as_ref() {
        json!({"message_id":id,"before":before,"after":after})
    } else if let Some(cursor) = input.before_cursor.as_ref() {
        json!({"before_cursor":cursor,"limit":limit})
    } else if let Some(cursor) = input.after_cursor.as_ref() {
        json!({"after_cursor":cursor,"limit":limit})
    } else {
        json!({"latest":limit})
    };
    let mut originals = Vec::new();
    let base = format!("/channels/{conversation}/messages");
    let mut page = |path: &str| -> Result<()> {
        let v = discord.get(path)?;
        originals.extend(
            v.as_array()
                .ok_or("context page is not an array")?
                .iter()
                .cloned(),
        );
        Ok(())
    };
    if let Some(id) = before_id {
        page(&format!("{base}?limit={limit}&before={id}"))?;
    } else if let Some(id) = after_id {
        page(&format!("{base}?limit={limit}&after={id}"))?;
    } else if let Some(id) = input.message_id {
        if before > 0 {
            page(&format!("{base}?limit={before}&before={id}"))?;
        }
        if after > 0 {
            page(&format!("{base}?limit={after}&after={id}"))?;
        }
        originals.push(discord.get(&format!("{base}/{id}"))?);
    } else {
        page(&format!("{base}?limit={limit}"))?;
    }
    originals.sort_by_key(|v| {
        v["id"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    });
    originals.dedup_by(|a, b| a["id"] == b["id"]);
    let total = originals.len();
    if descending {
        originals.reverse();
    }
    let policy = instance.policy();
    let mut messages = Vec::new();
    let mut bytes = 0;
    let mut filtered = 0;
    let mut clipped = false;
    let mut first_observed = None;
    let mut last_observed = None;
    let mut first_consumed = None;
    let mut last_consumed = None;
    for original in originals {
        let id = original["id"]
            .as_str()
            .ok_or("Discord message ID unavailable")?;
        let mut native = context.clone();
        if !native.is_object() {
            native = json!({});
        }
        native["author"] = original["author"].clone();
        let visible = policy.permits(resident, "read", conversation, &native);
        let edge = (snowflake(id)?, id.to_owned(), visible);
        let bounds = visible.then(|| json!({"id":id,"at":original["timestamp"]}));
        if first_observed.is_none() {
            first_observed = Some(bounds.clone());
        }
        last_observed = Some(bounds);
        if visible && !clipped {
            let projected = crate::messaging::bounded_native(
                &json!({"id":original["id"],"author":original["author"],"body":original["content"].as_str().unwrap_or(""),"timestamp":original["timestamp"],"attachments":original["attachments"],"native":original}),
            );
            let size = serde_json::to_vec(&projected)?.len() + 1;
            if bytes + size > 60 * 1024 {
                clipped = true;
            } else {
                bytes += size;
                messages.push(projected);
            }
        }
        if !visible {
            filtered += 1;
        }
        if !clipped {
            if first_consumed.is_none() {
                first_consumed = Some(edge.clone());
            }
            last_consumed = Some(edge);
        }
    }
    if descending {
        messages.reverse();
        std::mem::swap(&mut first_observed, &mut last_observed);
        std::mem::swap(&mut first_consumed, &mut last_consumed);
    }
    let continuation = |edge: Option<(u64, String, bool)>,
                        direction: &str,
                        id_field: &str,
                        cursor_field: &str|
     -> Result<Value> {
        match edge {
            Some((_, text, true)) => Ok(json!({id_field:text,"limit":limit})),
            Some((id, _, false)) => Ok(
                json!({cursor_field:history_cursor(&discord.key,&cursor_scope(direction)?,id)?,"limit":limit}),
            ),
            None => Ok(Value::Null),
        }
    };
    let oldest = continuation(first_consumed, "older", "before_id", "before_cursor")?;
    let newest = continuation(last_consumed, "newer", "after_id", "after_cursor")?;
    let mut result = json!({"conversation":conversation,"selection":selection,"messages":messages,
        "ordering":"chronological","provider_window_count":total,"filtered_by_read_policy":filtered,
        "retained_count":messages.len(),"omitted_count":total-filtered-messages.len(),
        "observed":{"first":first_observed.flatten(),"last":last_observed.flatten()},
        "provider_exhaustion":"unknown",
        "next":{"older":oldest,"newer":newest}});
    if clipped {
        result["stopped_by"] = json!("bytes");
    }
    if total == 0 {
        result["detail"] = json!(
            "Discord returned no messages for this window; history permission and exhaustion are unconfirmed."
        );
    }
    Ok(Reply::Ok(result))
}

fn load_uploads(control: &Control, message: &Value) -> Result<Vec<(Value, Vec<u8>)>> {
    use base64::Engine;
    let files = message["service"]["arguments"]["files"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 10)
        .ok_or("upload has 1–10 retained files")?;
    let mut output = Vec::new();
    let mut total = 0;
    for (index, artifact) in files.iter().enumerate() {
        let expected = artifact["bytes"]
            .as_u64()
            .filter(|n| *n <= 2 * 1024 * 1024)
            .ok_or("upload file exceeds 2 MiB")? as usize;
        total += expected;
        if total > 16 * 1024 * 1024 {
            return Err("upload batch exceeds 16 MiB".into());
        }
        let mut bytes = Vec::new();
        loop {
            let part=control.call(json!({"action":"attachment","id":message["id"],"index":index,"offset":bytes.len(),"limit":262144}))?;
            let data = base64::engine::general_purpose::STANDARD.decode(
                part["base64"]
                    .as_str()
                    .ok_or("attachment bytes unavailable")?,
            )?;
            if bytes.len() + data.len() > expected
                || part["next_offset"].as_u64() != Some((bytes.len() + data.len()) as u64)
            {
                return Err("attachment chunk bounds failed".into());
            }
            bytes.extend(data);
            if part["done"] == true {
                break;
            }
            if bytes.len() == expected {
                return Err("attachment stream did not terminate".into());
            }
        }
        if bytes.len() != expected
            || state::hex(&Sha256::digest(&bytes)) != artifact["sha256"].as_str().unwrap_or("")
        {
            return Err("attachment integrity failed".into());
        }
        output.push((artifact.clone(), bytes));
    }
    Ok(output)
}
fn multipart_upload(payload: &Value, files: &[(Value, Vec<u8>)]) -> Result<(String, Vec<u8>)> {
    let boundary = format!("boxen-{}", state::random_hex()?);
    let mut data = Vec::new();
    let mut payload = payload.clone();
    payload["attachments"] = json!(
        files
            .iter()
            .enumerate()
            .map(|(i, (f, _))| json!({"id":i,"filename":f["filename"]}))
            .collect::<Vec<_>>()
    );
    data.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    data.extend(serde_json::to_vec(&payload)?);
    data.extend(b"\r\n");
    for (index, (artifact, bytes)) in files.iter().enumerate() {
        let filename = artifact["filename"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .ok_or("upload filename unavailable")?;
        let filename = filename.replace('\\', "\\\\").replace('"', "\\\"");
        let mime = artifact["mime"]
            .as_str()
            .filter(|s| s.len() <= 128 && !s.chars().any(char::is_control))
            .ok_or("upload MIME unavailable")?;
        data.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files[{index}]\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
        data.extend(bytes);
        data.extend(b"\r\n");
    }
    data.extend(format!("--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), data))
}

fn cache_channel(discord: &mut Discord, value: &Value) {
    if let Some(id) = value["id"].as_str() {
        if discord.channels.len() >= 512 {
            discord.channels.clear();
        }
        discord.channels.insert(id.into(),json!({"space":value["guild_id"],"parent":value["parent_id"],"direct":matches!(value["type"].as_u64(),Some(1|3)),"author":if value["type"]==1 {value["recipients"].as_array().and_then(|a|a.first()).cloned().unwrap_or(Value::Null)}else{Value::Null}}));
    }
}
