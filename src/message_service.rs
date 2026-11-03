//! Declared connector operations use the existing correspondence authority.
use crate::{Result, messaging::Instance};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub permission: String,
    #[serde(default)]
    pub parameters: Value,
}
pub fn tools(instance: &Instance) -> Vec<Tool> {
    if instance.definition.provider != "discord" {
        return instance.definition.tools.clone();
    }
    vec![Tool{name:"send_files".into(),kind:"action".into(),permission:"files".into(),description:"Sends text and 1–10 files from the executing host or HTTP(S) URLs. URL reads have a 15-second deadline, forward no service credentials and do not follow redirects. paths are snapshotted before queue acceptance, at most 2 MiB per file and 16 MiB per run. The remote connector receives retained bytes, not local paths. Text can be empty for an attachment-only post. Native file support does not imply model perception.".into(),parameters:json!({"type":"object","properties":{"body":{"type":"string"},"paths":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10}},"required":["paths"],"additionalProperties":false})},

        Tool{name:"read_context".into(),kind:"query".into(),permission:"read".into(),description:"Returns a bounded chronological native message window. message_id centers the window; before_id or after_id select directional pages; limit defaults to 25 and is at most 50. Continuations identify older/newer messages. Reading does not subscribe or send correspondence.".into(),parameters:json!({"type":"object","properties":{"message_id":{"type":"string"},"before_id":{"type":"string"},"after_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":50},"before":{"type":"integer","minimum":0,"maximum":50},"after":{"type":"integer","minimum":0,"maximum":50}},"additionalProperties":false})},
        Tool{name:"react".into(),kind:"action".into(),permission:"react".into(),description:"Adds this account's reaction to a native message. message_id and emoji identify the effect; a Unicode emoji or custom name:id is accepted. Attention does not grant reaction permission.".into(),parameters:json!({"type":"object","properties":{"message_id":{"type":"string"},"emoji":{"type":"string"}},"required":["message_id","emoji"],"additionalProperties":false})},
    ]
}
pub fn declared(instance: &Instance, name: &str) -> Result<Tool> {
    tools(instance)
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| "connector operation is not declared by this instance".into())
}
