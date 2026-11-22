//! A revision-checked Claude follow-up owned by its predecessor's run journal.
use super::{Jobs, Run, Spec, terminal, valid_id};
use crate::{Result, attachments::Attachment, logging::Level, state};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Held,
    Claimed,
    Started,
    Returned,
    Uncertain,
    Released,
}
impl Phase {
    fn pending(self) -> bool {
        matches!(self, Self::Held | Self::Claimed)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    id: String,
    text: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Contribution {
    machine: String,
    input: Input,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hold {
    revision: u64,
    phase: Phase,
    run_id: String,
    contributions: Vec<Contribution>,
    reason: Option<String>,
}

impl Run {
    fn hold(&self) -> Result<Option<Hold>> {
        self.inner
            .lock()
            .unwrap()
            .journal
            .latest("held_followup")?
            .map(|event| serde_json::from_value(event.data).map_err(Into::into))
            .transpose()
    }

    pub(super) fn has_pending_hold(&self) -> Result<bool> {
        Ok(self.hold()?.is_some_and(|hold| hold.phase.pending()))
    }

    fn record_hold(&self, hold: &Hold, transition: &'static str) -> Result<()> {
        self.append("held_followup", serde_json::to_value(hold)?)?;
        self.diagnostic(
            Level::Info,
            "followup.hold_changed",
            || {
                json!({"transition":transition,"revision":hold.revision,"phase":hold.phase,
                "followup_run":hold.run_id,"contribution_count":hold.contributions.len()})
            },
            || Value::Null,
        );
        Ok(())
    }
}

impl Jobs {
    pub(crate) fn held_followup(self: &Arc<Self>, actor: &str, request: &Value) -> Result<Value> {
        let id = request["id"]
            .as_str()
            .ok_or("Predecessor run ID required")?;
        let run = self.operator_run(id)?;
        let _transition = self.transitions.lock().unwrap();
        let source = run.snapshot();
        if source.record.resident.is_some() || source.record.spec.backend != "claude" {
            return Err("Held follow-ups apply to anonymous Claude Code runs".into());
        }
        let prior = run.hold()?;
        let action = request["action"].as_str().unwrap_or("get");
        if action == "get" {
            return Ok(json!({"after":id,"hold":prior}));
        }
        let revision = request["expected_revision"]
            .as_u64()
            .ok_or("Expected hold revision required")?;
        let proposed = matches!(action, "append" | "edit")
            .then(|| serde_json::from_value::<Input>(request["contribution"].clone()))
            .transpose()?;
        if action == "append"
            && let Some(input) = &proposed
            && let Some(previous) = prior.as_ref()
            && let Some(accepted) = previous
                .contributions
                .iter()
                .find(|entry| entry.input.id == input.id)
        {
            if accepted.machine != actor || accepted.input != *input {
                return Err("Contribution ID already identifies different held text".into());
            }
            return Ok(json!({"after":id,"hold":previous}));
        }
        if action == "append" && terminal(&source.status) {
            return Err("The predecessor ended before this contribution was accepted".into());
        }
        if prior.as_ref().map_or(0, |hold| hold.revision) != revision {
            return Err("Held follow-up revision changed".into());
        }
        let mut hold = if let Some(previous) = prior {
            previous
        } else if action == "append" && !terminal(&source.status) {
            Hold {
                revision: 0,
                phase: Phase::Held,
                run_id: state::random_hex()?,
                contributions: Vec::new(),
                reason: None,
            }
        } else {
            return Err("Only an active predecessor can first receive a hold".into());
        };
        if hold.phase != Phase::Held
            && !(hold.phase == Phase::Released && action == "append" && !terminal(&source.status))
        {
            return Err("The hold is no longer editable".into());
        }

        match (action, proposed) {
            ("append", Some(input)) => {
                hold.contributions.push(Contribution {
                    machine: actor.into(),
                    input,
                });
            }
            ("edit", Some(input)) => {
                let contribution = hold
                    .contributions
                    .iter_mut()
                    .find(|entry| entry.input.id == input.id && entry.machine == actor)
                    .ok_or("This machine has no matching hold contribution")?;
                contribution.input = input;
            }
            ("release", _) => {
                let contribution = request["contribution_id"]
                    .as_str()
                    .ok_or("Contribution ID required")?;
                let index = hold
                    .contributions
                    .iter()
                    .position(|entry| entry.input.id == contribution && entry.machine == actor)
                    .ok_or("This machine has no matching hold contribution")?;
                hold.contributions.remove(index);
            }
            _ => return Err("Hold action must be get, append, edit, or release".into()),
        }
        let (_, attachments) = payload(&hold)?;
        self.attachments.require(&attachments)?;
        hold.revision += 1;
        hold.phase = if hold.contributions.is_empty() {
            Phase::Released
        } else {
            Phase::Held
        };
        run.record_hold(&hold, "operator_update")?;
        if hold.phase.pending() {
            self.held_pending.lock().unwrap().insert(id.into());
        } else {
            self.held_pending.lock().unwrap().remove(id);
        }
        self.notify();
        Ok(json!({"after":id,"hold":hold}))
    }

    pub(super) fn dispatch_holds(self: &Arc<Self>) {
        let pending = self.held_pending.lock().unwrap().clone();
        for id in pending {
            let Ok(run) = self.operator_run(&id) else {
                continue;
            };
            if let Err(error) = self.dispatch_hold(&run, &id) {
                run.diagnostic(
                    Level::Warn,
                    "followup.hold_dispatch_failed",
                    || json!({"error":error.to_string()}),
                    || Value::Null,
                );
            }
        }
    }

    fn dispatch_hold(self: &Arc<Self>, run: &Run, predecessor: &str) -> Result<()> {
        let _transition = self.transitions.lock().unwrap();
        let Some(mut hold) = run.hold()? else {
            return Ok(());
        };
        if !hold.phase.pending() {
            self.held_pending.lock().unwrap().remove(predecessor);
            return Ok(());
        }
        let source = run.snapshot();
        if !terminal(&source.status) {
            return Ok(());
        }
        if source.status != "completed" {
            hold.phase = Phase::Returned;
            hold.reason = Some(format!("predecessor_{}", source.status));
        } else {
            let (prompt, attachments) = payload(&hold)?;
            let spec = Spec {
                id: hold.run_id.clone(),
                prompt,
                attachments,
                cwd: if source.record.automatic_cwd {
                    String::new()
                } else {
                    source.record.spec.cwd.clone()
                },
                completion_handoff: None,
                ..source.record.spec.clone()
            };
            if hold.phase == Phase::Held {
                hold.phase = Phase::Claimed;
                hold.revision += 1;
                run.record_hold(&hold, "claimed")?;
            }
            match self.start_locked(
                &source.record.owner,
                spec,
                None,
                None,
                Some(predecessor),
                None,
            ) {
                Ok(_) => hold.phase = Phase::Started,
                Err(error)
                    if self
                        .directory
                        .join(&hold.run_id)
                        .join("record.json")
                        .exists() =>
                {
                    hold.phase = Phase::Uncertain;
                    hold.reason = Some("retained_run_unavailable".into());
                    run.diagnostic(
                        Level::Warn,
                        "followup.hold_acceptance_uncertain",
                        || json!({"error":error.to_string(),"followup_run":hold.run_id}),
                        || Value::Null,
                    );
                }
                Err(error) => {
                    hold.phase = Phase::Returned;
                    hold.reason = Some("admission_refused".into());
                    run.diagnostic(
                        Level::Info,
                        "followup.hold_admission_refused",
                        || json!({"error":error.to_string(),"followup_run":hold.run_id}),
                        || Value::Null,
                    );
                }
            }
        }
        hold.revision += 1;
        run.record_hold(&hold, "settled")?;
        self.held_pending.lock().unwrap().remove(predecessor);
        Ok(())
    }
}

fn payload(hold: &Hold) -> Result<(String, Vec<Attachment>)> {
    if hold.contributions.len() > 16 {
        return Err("A held follow-up allows up to 16 contributions".into());
    }
    let mut attachments = Vec::new();
    let mut authored = Vec::new();
    for contribution in &hold.contributions {
        let input = &contribution.input;
        valid_id(&input.id)?;
        if input.text.trim().is_empty() && input.attachments.is_empty() {
            return Err("A hold contribution requires text or attachments".into());
        }
        if input.text.len() > 32 * 1024 {
            return Err("A hold contribution allows up to 32 KiB of text".into());
        }
        attachments.extend(input.attachments.iter().cloned());
        authored.push(json!({"machine":contribution.machine,"text":input.text}));
    }
    if attachments.len() > 8 {
        return Err("A held follow-up allows up to eight attachments".into());
    }
    let prompt = match hold.contributions.as_slice() {
        [] => String::new(),
        [entry] => entry.input.text.clone(),
        _ => format!(
            "This follow-up combines operator messages held while the preceding run was active. Entries retain acceptance order and machine attribution. {}",
            serde_json::to_string(&authored)?
        ),
    };
    if prompt.len() > 256 * 1024 {
        return Err("Combined held text exceeds 256 KiB".into());
    }
    Ok((prompt, attachments))
}
