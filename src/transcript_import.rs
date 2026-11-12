//! Explicit text-only seeds for an empty built-in resident conversation.
//! Imported words retain source roles; no native session or effects are replayed.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub provider: String,
    pub title: String,
    pub representation: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub text: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub resident_id: String,
    pub expected_revision: u64,
    pub source: Source,
    pub messages: Vec<Message>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub request_sha256: String,
    pub source: Source,
    pub message_ids: Vec<String>,
    pub imported_at: u64,
}
fn bounded(s: &str, max: usize) -> Result<()> {
    if s.is_empty() || s.len() > max || s.chars().any(char::is_control) {
        return Err(
            "transcript metadata is nonempty bounded text without control characters".into(),
        );
    }
    Ok(())
}
impl Jobs {
    pub fn import_transcript(&self, owner: &str, request: Request) -> Result<Snapshot> {
        let result = (|| -> Result<Snapshot> {
            valid_id(&request.id)?;
            valid_id(&request.resident_id)?;
            let bytes = serde_json::to_vec(&request)?;
            if bytes.len() > 384 * 1024
                || request.messages.is_empty()
                || request.messages.len() > 1000
            {
                return Err("text import contains 1–1000 messages and at most 384 KiB".into());
            }
            bounded(&request.source.provider, 64)?;
            bounded(&request.source.title, 256)?;
            bounded(&request.source.representation, 1024)?;
            if let Some(s) = &request.source.url {
                bounded(s, 2048)?;
            }
            if let Some(s) = &request.source.model {
                bounded(s, 256)?;
            }
            let mut ids = HashSet::new();
            for m in &request.messages {
                bounded(&m.id, 256)?;
                if !ids.insert(&m.id)
                    || !matches!(m.role.as_str(), "user" | "assistant")
                    || m.text.is_empty()
                    || m.text.len() > 128 * 1024
                {
                    return Err("text import has unique source IDs, user/assistant roles and nonempty text of at most 128 KiB per message; native tool calls and system instructions are not importable".into());
                }
            }
            let digest = state::hex(&Sha256::digest(&bytes));
            let _transition = self.transitions.lock().unwrap();
            let mut runs = self.runs.lock().unwrap();
            if let Some(prior) = runs.get(&request.id) {
                let old = prior.snapshot();
                if old.record.owner == owner
                    && old
                        .record
                        .transcript_import
                        .as_ref()
                        .is_some_and(|p| p.request_sha256 == digest)
                {
                    return Ok(old);
                }
                return Err("import ID belongs to a different request or owner".into());
            }
            let resident = self.residents.get(owner, &request.resident_id)?;
            if resident.backend != "openrouter" || resident.revision != request.expected_revision {
                return Err(
                    "text import requires an OpenRouter resident at the expected revision".into(),
                );
            }
            if runs
                .values()
                .any(|r| r.snapshot().record.spec.thread_id == resident.id)
            {
                return Err("text import seeds an empty canonical conversation; existing history is not overwritten or appended".into());
            }
            let at = now().max(
                runs.values()
                    .map(|r| r.snapshot().record.created_at)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1),
            );
            let provenance = Provenance {
                request_sha256: digest,
                source: request.source.clone(),
                message_ids: request.messages.iter().map(|m| m.id.clone()).collect(),
                imported_at: at,
            };
            let record = Record {
                runtime_policy: Default::default(),
                imported: None,
                transcript_import: Some(provenance.clone()),
                observations: vec![],
                scheduled: None,
                resident: Some(resident.clone()),
                recovery: None,
                mailbox: None,
                spec: Spec {
                    attachments: vec![],
                    context: Some(resident.context.clone()),
                    resident_id: Some(resident.id.clone()),
                    id: request.id.clone(),
                    thread_id: resident.id,
                    backend: resident.backend,
                    cwd: resident.cwd,
                    model: resident.model,
                    prompt: String::new(),
                },
                owner: owner.into(),
                created_at: at,
            };
            let mut events = vec![Event {
                seq: 1,
                at,
                kind: "transcript_imported".into(),
                data: json!({"source":provenance.source,"message_count":request.messages.len(),"request_sha256":provenance.request_sha256,"execution_started":false}),
            }];
            for m in &request.messages {
                events.push(Event {
                    seq: events.len() as u64 + 1,
                    at,
                    kind: "inference_message".into(),
                    data: json!({"role":m.role,"content":m.text}),
                });
            }
            events.push(Event{seq:events.len() as u64+1,at,kind:"status".into(),data:json!({"status":"completed","detail":"Source-attributed text history imported; no inference, tools, mailbox delivery or historical effect was executed."})});
            self.publish_history(record, events, &mut runs)
        })();
        logging::record(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::Workload,
            "transcript_import.outcome",
            Context::run(&request.id, &request.resident_id, owner),
            || json!({"accepted":result.is_ok(),"messages":request.messages.len(),"error":result.as_ref().err().map(|e|e.to_string())}),
            || Value::Null,
        );
        result
    }
}
