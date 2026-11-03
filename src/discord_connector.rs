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

const API: &str = "https://discord.com/api/v10";
struct Control {
    agent: ureq::Agent,
    url: String,
    token: String,
    peer: String,
    address: String,
    instance: String,
}
impl Control {
    fn call(&self, mut args: Value) -> Result<Value> {
        args["instance"] = json!(self.instance);
        let mut r=self.agent.post(format!("{}/api/operation",self.url)).header("Authorization",format!("Bearer {}",self.token)).send_json(json!({"peer":self.peer,"address":self.address,"request":{"op":"messaging","args":args}}))?;
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
}
enum Reply {
    Ok(Value),
    Rejected(u16),
}
impl Discord {
    fn call(&mut self, method: &str, path: &str, body: Option<Value>) -> Result<Reply> {
        for _ in 0..3 {
            while Instant::now() < self.next {
                std::thread::sleep(
                    self.next
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(1)),
                );
            }
            let url = format!("{}{path}", self.base);
            let response = if method == "POST" {
                self.agent
                    .post(url)
                    .header("Authorization", format!("Bot {}", self.key))
                    .header(
                        "User-Agent",
                        "DiscordBot (https://github.com/x3haloed/boxen, 0.1)",
                    )
                    .send_json(body.as_ref().ok_or("POST body required")?)
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
            return if (200..300).contains(&status) {
                Ok(Reply::Ok(value))
            } else {
                Ok(Reply::Rejected(status))
            };
        }
        Err("Discord remains rate limited; no unconfirmed send is automatically replayed".into())
    }
    fn get(&mut self, path: &str) -> Result<Value> {
        match self.call("GET",path,None)?{Reply::Ok(v)=>Ok(v),Reply::Rejected(s)=>Err(format!("Discord read rejected (HTTP {s}); channel access and message-content availability are independent requirements").into())}
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
    };
    let mut channel_failures: HashMap<String, (u64, Instant)> = HashMap::new();
    let mut failures = 0u64;
    let mut backoff = 2u64;
    loop {
        let result = (|| -> Result<bool> {
            let current: Instance = serde_json::from_value(control.call(json!({"action":"get"}))?)?;
            if !current.definition.enabled {
                return Ok(false);
            }
            let credential_file =
                current.definition.credential_file.as_ref().ok_or(
                    "Discord connector configuration includes a credential-file reference",
                )?;
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
            let conversations: HashSet<_> = current
                .definition
                .bindings
                .iter()
                .filter(|b| b.receive)
                .map(|b| b.conversation.as_str())
                .collect();
            for conversation in conversations {
                if channel_failures
                    .get(conversation)
                    .is_some_and(|(_, next)| Instant::now() < *next)
                {
                    continue;
                }
                match poll(
                    &control,
                    &mut discord,
                    conversation,
                    &current.definition.account,
                ) {
                    Ok(()) => {
                        if let Some((count, _)) = channel_failures.remove(conversation) {
                            notices(
                                "discord.channel_recovered",
                                &instance,
                                json!({"conversation":conversation,"failures":count}),
                            );
                        }
                    }
                    Err(error) => {
                        let count = channel_failures
                            .get(conversation)
                            .map(|(n, _)| n + 1)
                            .unwrap_or(1);
                        let wait = (1u64 << count.min(6)).min(60);
                        channel_failures.insert(
                            conversation.into(),
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
        std::thread::sleep(Duration::from_secs(2));
    }
}
fn poll(control: &Control, discord: &mut Discord, conversation: &str, account: &str) -> Result<()> {
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
        let claimed =
            control.call(json!({"action":"claim","id":message["id"],"claim_id":claim_id}))?;
        if claimed["claimed"] != true {
            continue;
        }
        let (status, external_id, reason) = if !valid_conversation {
            ("rejected", None, Some("invalid_native_conversation".into()))
        } else if body.is_empty() {
            ("rejected", None, Some("empty_body".into()))
        } else {
            let parts = text_parts(body);
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
                let (part_status, part_id, part_reason) = match discord.call(
                    "POST",
                    &format!("/channels/{conversation}/messages"),
                    Some(payload),
                ) {
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
                    Ok(Reply::Rejected(code)) => (
                        if (400..500).contains(&code) {
                            "rejected"
                        } else {
                            "uncertain"
                        },
                        None,
                        Some(format!("http_{code}")),
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
