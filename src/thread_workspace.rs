//! Workspace references use accepted runs and the existing attachment store.
use super::*;
use crate::attachments::Attachment;
use std::io::Read;
use std::path::{Component, Path};

pub(crate) fn tool() -> Value {
    json!({
        "type":"function",
        "name":"boxen_thread_files",
        "description":"This tool lists working directories recorded by accepted runs in this anonymous conversation. Pull retrieves a named relative file from a recorded run's machine and returns a local path to retained bytes. The original is read at pull time. An unavailable host, directory, or file returns an error. List includes retained request attachments. Attachment takes a run_id and digest and returns a local path to the retained original.",
        "inputSchema":{"type":"object","properties":{
            "action":{"type":"string","enum":["list","pull","attachment"]},
            "run_id":{"type":"string"},
            "path":{"type":"string"},
            "digest":{"type":"string"},
            "offset":{"type":"integer","minimum":0}
        },"required":["action"],"additionalProperties":false}
    })
}

impl Jobs {
    pub(crate) fn workspace_files(&self, thread: &str, request: &Value) -> Result<Value> {
        valid_id(thread)?;
        let result = match request["action"].as_str() {
            Some("list") => {
                let offset = request["offset"].as_u64().unwrap_or(0);
                let mut snapshots = self.operator_list();
                snapshots.retain(|s| {
                    s.record.spec.thread_id == thread
                        && s.record.spec.resident_id.is_none()
                        && s.record.resident.is_none()
                });
                let count = snapshots.len() as u64;
                let mut directories = Vec::new();
                for s in snapshots
                    .into_iter()
                    .rev()
                    .skip(usize::try_from(offset)?)
                    .take(32)
                {
                    let id = &s.record.spec.id;
                    directories.push(json!({"run_id":id,"machine":self.run_machine(id)?,
                        "directory":s.record.spec.cwd,"automatic":s.record.automatic_cwd,"attachments":s.record.spec.attachments}));
                }
                let next = offset.saturating_add(directories.len() as u64);
                Ok(json!({"directories":directories,"next_offset":(next<count).then_some(next)}))
            }
            Some("pull") => self.pull_workspace_file(thread, request),
            Some("attachment") => self.workspace_attachment(thread, request),
            _ => Err("unknown thread file action".into()),
        };
        logging::record(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            Target::Workload,
            "thread_workspace.operation",
            Context {
                thread_id: Some(thread),
                ..Context::default()
            },
            || json!({"action":request["action"],"source_run":request["run_id"],"success":result.is_ok()}),
            || json!({"path":request["path"],"detail":result.as_ref().err().map(ToString::to_string)}),
        );
        result
    }

    fn workspace_run(&self, thread: &str, run: &str) -> Result<Snapshot> {
        let snapshot = self.operator_run(run)?.snapshot();
        if snapshot.record.spec.thread_id != thread
            || snapshot.record.resident.is_some()
            || snapshot.record.spec.resident_id.is_some()
            || !terminal(&snapshot.status)
        {
            return Err("the source must be a terminal run in this anonymous conversation".into());
        }
        Ok(snapshot)
    }

    fn workspace_attachment(&self, thread: &str, request: &Value) -> Result<Value> {
        let id = request["run_id"].as_str().ok_or("source run ID required")?;
        let run = self.workspace_run(thread, id)?;
        let digest = request["digest"]
            .as_str()
            .ok_or("attachment digest required")?;
        let file = run
            .record
            .spec
            .attachments
            .iter()
            .find(|file| file.digest == digest)
            .ok_or("this run has no request attachment with that digest")?;
        Ok(json!({"path":self.attachments.verify(file)?,"file":file,
            "source":{"machine":self.run_machine(id)?,"run_id":id},"snapshot":"retained_original"}))
    }

    pub(crate) fn offer_workspace_file(&self, request: &Value) -> Result<Value> {
        let thread = request["thread_id"].as_str().ok_or("thread ID required")?;
        let id = request["run_id"].as_str().ok_or("source run ID required")?;
        let run = self.workspace_run(thread, id)?;
        if self.run_machine(id)? != self.machine_id() {
            return Err("historical workspace belongs to another machine".into());
        }
        let relative = Path::new(request["path"].as_str().ok_or("relative path required")?);
        if relative.as_os_str().len() > 4096
            || relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("the file path must be relative to the recorded working directory".into());
        }
        let root = Path::new(&run.record.spec.cwd).canonicalize()?;
        let path = root.join(relative).canonicalize()?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(
                "the path does not name a regular file within the recorded working directory"
                    .into(),
            );
        }
        let file = File::open(path)?;
        let mut bytes = Vec::new();
        file.take(crate::attachments::FILE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        let name = relative
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("file name must be UTF-8")?;
        let attachment = self.attachments.capture(name, &bytes)?;
        logging::record(
            Level::Info,
            Target::Workload,
            "thread_workspace.file_captured",
            Context::run(id, thread, &run.record.owner),
            || json!({"bytes":attachment.size,"digest":attachment.digest}),
            || json!({"path":relative}),
        );
        Ok(serde_json::to_value(attachment)?)
    }

    fn pull_workspace_file(&self, thread: &str, request: &Value) -> Result<Value> {
        let run = request["run_id"].as_str().ok_or("source run ID required")?;
        self.workspace_run(thread, run)?;
        let machine = self.run_machine(run)?;
        let path = request["path"].as_str().ok_or("relative path required")?;
        let offer =
            json!({"op":"thread_workspace_offer","thread_id":thread,"run_id":run,"path":path});
        let file: Attachment = if machine == self.machine_id() {
            serde_json::from_value(self.offer_workspace_file(&offer)?)?
        } else {
            let network = self
                .network
                .lock()
                .unwrap()
                .clone()
                .ok_or("machine network unavailable")?;
            let address = network
                .peers()?
                .into_iter()
                .find(|p| p.key == machine && !p.address.is_empty())
                .ok_or("historical machine address is unavailable")?
                .address;
            let receive = |request| crate::protocol::request(&network, &address, &machine, request);
            let file: Attachment = serde_json::from_value(receive(offer)?)?;
            let mut status = self
                .attachments
                .operation(&json!({"action":"status","file":file}))?;
            while status["complete"] != true {
                let offset = status["next"]
                    .as_u64()
                    .filter(|n| *n <= file.size)
                    .ok_or("invalid workspace transfer position")?;
                let chunk = receive(
                    json!({"op":"attachment","action":"read","file":file,"offset":offset}),
                )?;
                status = self.attachments.operation(
                    &json!({"action":"write","file":file,"offset":offset,"data":chunk["data"]}),
                )?;
                if status["next"] != chunk["next"]
                    || (status["next"] == offset && status["complete"] != true)
                {
                    return Err("workspace transfer did not reach its retained offset".into());
                }
            }
            file
        };
        Ok(json!({"path":self.attachments.verify(&file)?,"file":file,
            "source":{"machine":machine,"run_id":run,"relative_path":path},"snapshot":"read_at_pull_time"}))
    }
}
