//! The guest correspondence binding and the Pub/Sub runner's local control client.
use crate::{
    Result, jobs,
    messaging::{Binding, Definition},
    state,
};
use serde_json::{Value, json};
use std::{fs, net::SocketAddr, path::Path, time::Duration};

pub const PROVIDER: &str = "guest_chat";

pub fn binding(definition: &Definition) -> Result<&Binding> {
    if definition.provider != PROVIDER
        || !definition.tools.is_empty()
        || definition.credential_file.is_some()
        || !definition.capabilities.iter().any(|c| c == "receive")
        || !definition.capabilities.iter().any(|c| c == "send")
    {
        return Err("guest chat instances support correspondence with a single binding".into());
    }
    match definition.bindings.as_slice() {
        [binding] if binding.receive && binding.send && !binding.accept_bots => Ok(binding),
        _ => Err("guest chat requires one two-way binding for human messages".into()),
    }
}

pub(crate) struct Control {
    agent: ureq::Agent,
    url: String,
    peer: String,
    token: String,
}

impl Control {
    pub(crate) fn open(home: &Path, args: &mut Vec<String>) -> Result<Self> {
        let url =
            crate::take_option(args, "--url")?.unwrap_or_else(|| "http://127.0.0.1:42425".into());
        let origin = url
            .strip_prefix("http://")
            .ok_or("control URL uses loopback HTTP")?;
        let address: SocketAddr = origin.parse()?;
        if !address.ip().is_loopback() {
            return Err("control URL uses loopback HTTP".into());
        }
        let url = format!("http://{address}");
        let token = fs::read_to_string(home.join("control-token"))?;
        let token = token.trim();
        state::key(token)?;
        let token = format!("Bearer {token}");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let status: Value = agent
            .get(format!("{url}/api/status"))
            .header("Authorization", &token)
            .call()?
            .body_mut()
            .with_config()
            .limit(512 * 1024)
            .read_json()?;
        let peer = status["identity"]
            .as_str()
            .ok_or("local daemon identity unavailable")?
            .to_owned();
        state::key(&peer)?;
        Ok(Self {
            agent,
            url,
            peer,
            token,
        })
    }

    pub(crate) fn call(&self, instance: Option<&str>, args: Value) -> Result<Value> {
        let mut response = self
            .agent
            .post(format!("{}/api/operation", self.url))
            .header("Authorization", &self.token)
            .send_json(json!({
                "peer":self.peer, "connector_instance":instance,
                "request":{"op":"messaging","args":args},
            }))?;
        Ok(response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_json()?)
    }
}

pub fn create(home: &Path, mut args: Vec<String>) -> Result<()> {
    let name = crate::take_option(&mut args, "--name")?.unwrap_or_else(|| "Guest chat".into());
    let control = Control::open(home, &mut args)?;
    let [resident] = args.as_slice() else {
        return Err("guest-chat-create takes one resident ID".into());
    };
    jobs::valid_id(resident)?;
    let instance = state::random_hex()?;
    let value = control.call(
        None,
        json!({
            "action":"create", "definition":{
                "id":instance, "name":name, "provider":PROVIDER, "account":"guest",
                "capabilities":["receive","send","replies"],
                "bindings":[{"conversation":"chat","resident":resident}],
            },
        }),
    )?;
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}
