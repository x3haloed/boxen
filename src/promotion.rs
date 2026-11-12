//! Promotion attaches identity to an existing conversation; it starts no execution.
use super::*;
use crate::residents::{Promote, Promotion};

impl Jobs {
    pub fn promote(&self, owner: &str, request: Promote) -> Result<Resident> {
        valid_id(&request.thread_id)?;
        let _transition = self.transitions.lock().unwrap();
        if self
            .placement(&request.thread_id)?
            .is_some_and(|m| m.target != self.machine_id())
        {
            return Err("conversation execution moved to another machine".into());
        }
        let outcome = (|| -> Result<Resident> {
            if let Some(existing) = self.residents.existing_promotion(owner, &request)? {
                self.mailboxes.register(owner, &existing.id)?;
                return Ok(existing);
            }
            self.mailboxes.check()?;
            if self.schedules.list(owner).iter().any(|j|j.enabled && !j.deleted && matches!(&j.definition.target,crate::scheduler::Handler::Thread{id} if id==&request.thread_id)) {
                return Err("existing anonymous-thread schedules require an explicit pause or replacement with resident schedules before promotion".into());
            }
            if self.observations.list(owner)["subscriptions"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|s| {
                    s["spec"]["target"]["type"] == "thread"
                        && s["spec"]["target"]["id"] == request.thread_id
                        && s["enabled"] == true
                        && s["ended"] != true
                        && s["spec"]["expires_at_ms"]
                            .as_u64()
                            .is_none_or(|t| t > now())
                })
            {
                return Err("existing anonymous-thread observation subscriptions require an explicit pause or replacement with resident subscriptions before promotion".into());
            }
            let mut history = self
                .runs
                .lock()
                .unwrap()
                .values()
                .map(|r| r.snapshot())
                .filter(|s| s.record.spec.thread_id == request.thread_id)
                .collect::<Vec<_>>();
            if history.is_empty() {
                return Err("promotion addresses an existing Boxen thread".into());
            }
            if history.iter().any(|s| s.record.owner != owner) {
                return Err("thread belongs to a different peer".into());
            }
            if history.iter().any(|s| !terminal(&s.status)) {
                return Err(
                    "promotion becomes available after the thread's active run ends".into(),
                );
            }
            history.sort_by_key(|s| s.record.created_at);
            let last = history.last().unwrap();
            if last.record.spec.backend != "codex" {
                return Err("this promotion slice supports native Codex threads".into());
            }
            let native=history.iter().rev().find_map(|s|s.backend_thread.clone()).ok_or("thread has no accepted native session; promotion does not create a replacement")?;
            let initial = Create {
                id: request.thread_id.clone(),
                name: request.name.clone(),
                identity: request.identity.clone(),
                memory: request.memory.clone(),
                quota_bytes: request.quota_bytes,
                auto_activate: request.auto_activate,
                backend: last.record.spec.backend.clone(),
                cwd: last.record.spec.cwd.clone(),
                model: last.record.spec.model.clone(),
                context: crate::context::Settings::resident(),
            };
            let promotion = Promotion {
                request,
                backend_thread_id: native,
                source_run_id: last.record.spec.id.clone(),
            };
            let resident =
                self.residents
                    .create_with_origin(owner, initial, None, None, Some(promotion))?;
            self.mailboxes.register(owner, &resident.id)?;
            Ok(resident)
        })();
        logging::record(
            if outcome.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::State,
            "resident.promotion",
            Context::peer(owner),
            || json!({"resident_id":outcome.as_ref().ok().map(|r|&r.id),"accepted":outcome.is_ok(),"error":outcome.as_ref().err().map(|e|e.to_string())}),
            || Value::Null,
        );
        outcome
    }
}
