//! Shared daemon-owned resident operations, independent of runtime.
use crate::{
    Result,
    jobs::{Jobs, Spec},
    mailbox::{Address, Send as MailSend},
    residents::Resident,
};
use serde_json::{Value, json};
use std::sync::Arc;
pub fn resident_tool(
    jobs: &Arc<Jobs>,
    resident: &Resident,
    run_id: &str,
    call_id: &str,
    tool: &str,
    args: Value,
) -> Result<Value> {
    let owner = &resident.owner;
    match tool {
        "boxen_continuity" => jobs.residents.tool(owner, &resident.id, args),
        "boxen_mailbox" => jobs.inbox(
            owner,
            Address::Resident {
                id: resident.id.clone(),
            },
            args["after"].as_u64().unwrap_or(0),
        ),
        "boxen_send_message" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                id: Option<String>,
                to: Address,
                subject: String,
                body: String,
                reply_to: Option<Address>,
                in_reply_to: Option<String>,
            }
            let input: Input = serde_json::from_value(args)?;
            let source = format!("run:{run_id}:{call_id}");
            let id = input
                .id
                .or_else(|| jobs.mailboxes.source_id(&source))
                .unwrap_or(crate::state::random_hex()?);
            let message = jobs.send_mail(
                owner,
                Address::Resident {
                    id: resident.id.clone(),
                },
                MailSend {
                    id,
                    to: input.to,
                    subject: input.subject,
                    body: input.body,
                    reply_to: input.reply_to,
                    in_reply_to: input.in_reply_to,
                },
                source,
            )?;
            Ok(serde_json::to_value(message)?)
        }
        "boxen_thread_request" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                request_id: String,
                thread_id: Option<String>,
                prompt: String,
                cwd: Option<String>,
                model: Option<String>,
            }
            let input: Input = serde_json::from_value(args)?;
            Ok(serde_json::to_value(jobs.start(
                owner,
                Spec {
                    resident_id: None,
                    id: input.request_id.clone(),
                    thread_id: input.thread_id.unwrap_or(input.request_id),
                    backend: resident.backend.clone(),
                    cwd: input.cwd.unwrap_or(resident.cwd.clone()),
                    model: input.model.unwrap_or(resident.model.clone()),
                    prompt: input.prompt,
                },
            )?)?)
        }
        "boxen_thread_result" => jobs.thread_result(
            owner,
            args["request_id"]
                .as_str()
                .ok_or("thread result requires request_id")?,
        ),
        _ => Err("unknown resident tool".into()),
    }
}
pub fn resident_tools() -> Vec<Value> {
    let address = json!({"oneOf":[{"type":"object","properties":{"type":{"type":"string","enum":["resident"]},"id":{"type":"string"}},"required":["type","id"],"additionalProperties":false},{"type":"object","properties":{"type":{"type":"string","enum":["client"]},"machine":{"type":"string"},"id":{"type":"string"}},"required":["type","machine","id"],"additionalProperties":false}]});
    vec![
        crate::residents::tool_spec(),
        json!({"type":"function","name":"boxen_mailbox","description":"This tool reads this resident's durable inbox after an optional sequence cursor. Queued messages have not been included in an activation; presented_in identifies the run whose recorded input contains them. Presentation does not prove understanding or require a reply. Assistant text outside sending tools is private activity, not delivered mail.","inputSchema":{"type":"object","properties":{"after":{"type":"integer","minimum":0}},"additionalProperties":false}}),
        json!({"type":"function","name":"boxen_send_message","description":"This tool explicitly sends a message from this resident. Success means the destination queue on this host durably accepted it, not that its recipient read or answered it. A nonblank subject and body are conditions of acceptance. An optional 64-character lowercase hex id makes retries across different calls idempotent. in_reply_to links an optional reply to an incoming message; the incoming reply_to address identifies its return destination. Finishing an activation does not require sending. This first implementation routes within the owner's host-local mailboxes.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"to":address,"subject":{"type":"string"},"body":{"type":"string"},"reply_to":address,"in_reply_to":{"type":"string"}},"required":["to","subject","body"],"additionalProperties":false}}),
        json!({"type":"function","name":"boxen_thread_request","description":"This tool submits an ordinary request to an identity-less workload thread. request_id is a new 64-character lowercase hex id retained across retries of the exact request. thread_id identifies an existing anonymous conversation; omission starts a new thread. The request returns run metadata immediately; boxen_thread_result retrieves its eventual conventional response. This channel is distinct from sending mailbox messages. Working directory and model default to this resident's configured values.","inputSchema":{"type":"object","properties":{"request_id":{"type":"string"},"thread_id":{"type":"string"},"prompt":{"type":"string"},"cwd":{"type":"string"},"model":{"type":"string"}},"required":["request_id","prompt"],"additionalProperties":false}}),
        json!({"type":"function","name":"boxen_thread_result","description":"This tool returns an identity-less request's current status and, when finished, its conventional assistant response. request_id is the run id returned by boxen_thread_request. Reading a pending result does not create another request or send mail. Resident narration is not available through this request/response tool.","inputSchema":{"type":"object","properties":{"request_id":{"type":"string"}},"required":["request_id"],"additionalProperties":false}}),
    ]
}
