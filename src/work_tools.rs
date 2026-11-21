//! Agent-facing projections of existing machine, run, control and attachment authorities.
use crate::{
    Result,
    jobs::{Jobs, Run, terminal},
    state::State,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 48 * 1024;

#[derive(Clone)]
struct Machine {
    key: String,
    label: String,
    address: String,
    local: bool,
    network: Option<Arc<State>>,
}

impl Machine {
    fn identity(&self) -> Value {
        json!({"id":self.key,"name":self.label,"local":self.local})
    }

    fn request(&self, jobs: &Arc<Jobs>, owner: &str, request: Value) -> Result<Value> {
        if self.local {
            return crate::operator::operation(jobs, owner, &request);
        }
        let network = self
            .network
            .as_ref()
            .ok_or("The machine network is unavailable")?;
        network.require_trust(&self.key)?;
        if self.address.is_empty() {
            return Err("This member has no known reachable endpoint".into());
        }
        crate::protocol::request(network, &self.address, &self.key, request)
    }
}

fn machines(jobs: &Jobs) -> Result<Vec<Machine>> {
    let network = jobs.network.lock().unwrap().clone();
    let mut machines = vec![Machine {
        key: jobs.machine_id().into(),
        label: "This machine".into(),
        address: String::new(),
        local: true,
        network: network.clone(),
    }];
    if let Some(network) = &network {
        for peer in network.peers()? {
            machines.push(Machine {
                key: peer.key,
                label: peer.label,
                address: peer.address,
                local: false,
                network: Some(network.clone()),
            });
        }
    }
    Ok(machines)
}

fn resolve(jobs: &Jobs, selector: Option<&str>) -> Result<Machine> {
    let selector = selector.unwrap_or("local");
    let available = machines(jobs)?;
    if matches!(selector, "local" | "This machine") {
        return Ok(available[0].clone());
    }
    let found: Vec<_> = available
        .into_iter()
        .filter(|m| {
            m.key == selector
                || m.label.eq_ignore_ascii_case(selector)
                || (selector.len() >= 6 && m.key.starts_with(selector))
        })
        .collect();
    match found.as_slice() {
        [machine] => Ok(machine.clone()),
        [] => Err("The selector does not identify a known network member; boxen_machines returns current members".into()),
        _ => Err("The machine selector is ambiguous; a full machine id distinguishes these members".into()),
    }
}

// Destination inference is a projection of this conversation's retained requests,
// not a second placement registry. An explicit selector remains authoritative.
fn work_machine(jobs: &Arc<Jobs>, run: &Run, args: &Value) -> Result<Machine> {
    if let Some(machine) = args["machine"].as_str() {
        return resolve(jobs, Some(machine));
    }
    let ids: Vec<String> = ["request_id", "run_id", "after_request_id"]
        .iter()
        .filter_map(|field| args[field].as_str())
        .map(|value| identifier(Some(value), run, ""))
        .collect::<Result<_>>()?;
    let thread = args["thread_id"].as_str();
    if ids.is_empty() && thread.is_none() {
        return resolve(jobs, None);
    }
    let parent = run.snapshot().record.spec.thread_id;
    let mut found = None;
    for previous in jobs
        .operator_list()
        .into_iter()
        .filter(|s| s.record.spec.thread_id == parent)
    {
        for event in jobs
            .operator_run(&previous.record.spec.id)?
            .retained_events("work_request")?
        {
            let rank = ids
                .iter()
                .position(|id| event.data["request_id"] == *id)
                .or_else(|| {
                    thread
                        .filter(|thread| event.data["request"]["spec"]["thread_id"] == *thread)
                        .map(|_| ids.len())
                });
            if let Some(rank) = rank
                && found
                    .as_ref()
                    .is_none_or(|(best, at, _)| rank < *best || (rank == *best && event.at >= *at))
            {
                found = Some((
                    rank,
                    event.at,
                    event.data["machine"]
                        .as_str()
                        .ok_or("The retained work request has no destination")?
                        .to_owned(),
                ));
            }
        }
    }
    resolve(jobs, found.as_ref().map(|(_, _, machine)| machine.as_str()))
}

fn inventory(jobs: &Arc<Jobs>, owner: &str, args: &Value) -> Result<Value> {
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let available = if let Some(selector) = args["machine"].as_str() {
        vec![resolve(jobs, Some(selector))?]
    } else {
        machines(jobs)?
    };
    let total = available.len();
    let page: Vec<_> = available.into_iter().skip(offset).take(8).collect();
    let rows = std::thread::scope(|scope| {
        let workers: Vec<_> = page
            .into_iter()
            .map(|machine| {
                scope.spawn(move || {
                    let mut row = machine.identity();
                    row["checked_at_ms"] = json!(crate::jobs::now());
                    match machine.request(jobs, owner, json!({"op":"runtime_info"})) {
                        Ok(runtime) => {
                            row["reachable"] = json!(true);
                            row["runtime"] = runtime;
                        }
                        Err(error) => {
                            row["reachable"] = json!(false);
                            row["error"] = json!(error.to_string());
                        }
                    }
                    row
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("machine query thread"))
            .collect::<Vec<_>>()
    });
    let next = offset.saturating_add(rows.len());
    Ok(
        json!({"machines":rows,"returned":rows.len(),"has_more":next<total,"next_offset":(next<total).then_some(next),"total":total}),
    )
}

fn identifier(value: Option<&str>, run: &Run, call_id: &str) -> Result<String> {
    if let Some(value) = value {
        if crate::jobs::valid_id(value).is_ok() {
            return Ok(value.into());
        }
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(
                "A request label contains 1–128 characters without control characters".into(),
            );
        }
        return Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&json!([
                run.snapshot().record.spec.thread_id,
                value
            ]))?)
        ));
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([
            run.snapshot().record.spec.id,
            call_id
        ]))?)
    ))
}

fn reference(machine: &Machine, snapshot: &Value) -> Value {
    let record = &snapshot["record"];
    let spec = &record["spec"];
    json!({"machine":machine.key,"request_id":spec["id"],"thread_id":spec["thread_id"],"status":snapshot["status"],"backend":spec["backend"],"model":spec["model"],"cwd":spec["cwd"],"execution":record["execution"],"last_seq":snapshot["last_seq"]})
}

fn anonymous(snapshot: &Value) -> Result<()> {
    if !snapshot["record"]["resident"].is_null()
        || !snapshot["record"]["spec"]["resident_id"].is_null()
    {
        return Err("Identity-bearing conversations expose correspondence through mailbox tools; work requests address anonymous threads".into());
    }
    Ok(())
}

fn head(jobs: &Arc<Jobs>, owner: &str, machine: &Machine, thread: &str) -> Result<Value> {
    crate::jobs::valid_id(thread)?;
    let mut after = Value::Null;
    let mut latest = None;
    for _ in 0..128 {
        let page = machine.request(
            jobs,
            owner,
            json!({"op":"list","summary_page":{"after":after}}),
        )?;
        let rows = page
            .as_array()
            .or_else(|| page["summaries"].as_array())
            .ok_or("The peer returned an invalid run catalog")?;
        for row in rows {
            let spec = if row["record"].is_object() {
                &row["record"]["spec"]
            } else {
                row
            };
            if spec["thread_id"] == thread {
                latest = Some(
                    spec["id"]
                        .as_str()
                        .ok_or("The run summary has no id")?
                        .to_owned(),
                );
            }
        }
        after = page["next_after"].clone();
        if after.is_null() {
            let id = latest.ok_or("This anonymous thread was not found on the selected machine")?;
            let snapshot = machine.request(jobs, owner, json!({"op":"get","id":id}))?;
            anonymous(&snapshot)?;
            return Ok(snapshot);
        }
    }
    Err("The catalog exceeds this lookup's page bound; after_request_id provides a direct continuation handle".into())
}

fn catalog(jobs: &Arc<Jobs>, run: &Run, machine: &Machine, args: &Value) -> Result<Value> {
    let page = machine.request(
        jobs,
        &run.owner(),
        json!({"op":"list","summary_page":{"after":args["cursor"]}}),
    )?;
    let source = page["summaries"]
        .as_array()
        .ok_or("The peer returned an invalid run catalog")?;
    let mut rows = Vec::new();
    let mut bytes = 0;
    let mut next = page["next_after"].clone();
    let mut scanned = 0;
    let mut clipped = false;
    for (index, row) in source.iter().enumerate() {
        if row["record"]["spec"]["resident_id"].is_null()
            && args["thread_id"]
                .as_str()
                .is_none_or(|thread| row["record"]["spec"]["thread_id"] == thread)
        {
            let mut item = json!({"work":reference(machine,row),"title":row["title"],"placement":row["placement"]});
            if item.to_string().len() > OUTPUT_LIMIT / 2 {
                item = json!({"work":reference(machine,row),"title":row["title"],"placement_omitted":true});
            }
            let size = item.to_string().len();
            if bytes + size > OUTPUT_LIMIT - 512 && !rows.is_empty() {
                clipped = true;
                let previous = &source[index - 1]["record"];
                next = json!({"created_at":previous["created_at"],"id":previous["spec"]["id"]});
                break;
            }
            bytes += size;
            rows.push(item);
        }
        scanned += 1;
    }
    let has_more = !next.is_null();
    let mut result = json!({"requests":rows,"machine":machine.key,"scanned":scanned,
        "returned":rows.len(),"has_more":has_more,"next_cursor":next});
    if let Some(thread) = args["thread_id"].as_str() {
        result["thread_id"] = json!(thread);
    }
    if clipped {
        result["stopped_by"] = json!("bytes");
    }
    Ok(result)
}

fn active(run: &Run) -> Result<()> {
    if run.cancellation_requested() {
        return Err(
            "The coordinating run was cancelled; no further work operation was dispatched".into(),
        );
    }
    Ok(())
}

fn request(jobs: &Arc<Jobs>, run: &Arc<Run>, call_id: &str, args: &Value) -> Result<Value> {
    let record = run.snapshot().record;
    let machine = work_machine(jobs, run, args)?;
    let id = identifier(args["request_id"].as_str(), run, call_id)?;
    let prompt = args["prompt"]
        .as_str()
        .ok_or("A work request contains a prompt")?;
    let mut intent = args.clone();
    intent
        .as_object_mut()
        .ok_or("Work arguments are an object")?
        .remove("request_id");
    intent["machine"] = json!(machine.key);
    let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&intent)?));
    let mut receipts = run.retained_events("work_request")?;
    if args["request_id"].is_string() {
        for previous in jobs.operator_list().into_iter().filter(|s| {
            s.record.spec.thread_id == record.spec.thread_id && s.record.spec.id != record.spec.id
        }) {
            receipts.extend(
                jobs.operator_run(&previous.record.spec.id)?
                    .retained_events("work_request")?,
            );
        }
    }
    let existing = receipts
        .into_iter()
        .find(|event| event.data["request_id"] == id);
    let mut repeated = existing.is_some();
    let wire = if let Some(existing) = existing {
        if existing.data["fingerprint"] != fingerprint {
            return Err("This work request id already identifies different input".into());
        }
        existing.data["request"].clone()
    } else {
        let old = match machine.request(jobs, &record.owner, json!({"op":"get","id":id})) {
            Ok(snapshot) => {
                anonymous(&snapshot)?;
                repeated = true;
                Some(snapshot)
            }
            Err(error) if machine.local || error.is::<crate::protocol::Rejected>() => None,
            Err(error) => {
                return Ok(
                    json!({"machine":machine.key,"request_id":id,"acceptance":"not_dispatched","error":error.to_string()}),
                );
            }
        };
        let previous = if let Some(old) = &old {
            Some(old.clone())
        } else if let Some(prior) = args["after_request_id"].as_str() {
            let prior = identifier(Some(prior), run, call_id)?;
            let previous = machine.request(jobs, &record.owner, json!({"op":"get","id":prior}))?;
            anonymous(&previous)?;
            Some(previous)
        } else if let Some(thread) = args["thread_id"].as_str() {
            Some(head(jobs, &record.owner, &machine, thread)?)
        } else {
            None
        };
        let base = previous.as_ref().map(|s| &s["record"]["spec"]);
        let thread = args["thread_id"]
            .as_str()
            .or_else(|| base.and_then(|s| s["thread_id"].as_str()))
            .unwrap_or(&id);
        let backend = args["backend"]
            .as_str()
            .or_else(|| base.and_then(|s| s["backend"].as_str()))
            .unwrap_or(&record.spec.backend);
        let default_cwd = if machine.local {
            record.spec.cwd.as_str()
        } else {
            ""
        };
        let cwd = args["cwd"].as_str().unwrap_or_else(|| {
            if previous
                .as_ref()
                .is_some_and(|s| s["record"]["automatic_cwd"] == true)
            {
                ""
            } else {
                base.and_then(|s| s["cwd"].as_str()).unwrap_or(default_cwd)
            }
        });
        let model = args["model"]
            .as_str()
            .or_else(|| base.and_then(|s| s["model"].as_str()))
            .unwrap_or("");
        let files = if let Some(old) = &old {
            if args["attachments"]
                .as_array()
                .is_some_and(|files| !files.is_empty())
            {
                return Err("An existing destination request without a source receipt cannot validate attachment intent; its current result remains readable".into());
            }
            if old["record"]["spec"]["prompt"] != prompt {
                return Err("This work request id already identifies a different prompt".into());
            }
            serde_json::from_value(old["record"]["spec"]["attachments"].clone())
                .unwrap_or(Vec::new())
        } else {
            let paths = args["attachments"].as_array().cloned().unwrap_or_default();
            if paths.len() > 8 {
                return Err("A work request retains at most eight attachments".into());
            }
            let mut files = Vec::new();
            for path in paths {
                active(run)?;
                let path = Path::new(path.as_str().ok_or("Attachment paths are strings")?);
                let path = if path.is_absolute() {
                    path.to_owned()
                } else {
                    Path::new(&record.spec.cwd).join(path)
                };
                files.push(jobs.attachments.capture_local(&path)?);
            }
            files
        };
        let mut wire = json!({"op":"start","spec":{"id":id,"thread_id":thread,"backend":backend,"cwd":cwd,"model":model,"prompt":prompt,"attachments":files}});
        if let Some(old) = &old {
            if !old["record"]["after"].is_null() {
                wire["after"] = old["record"]["after"].clone();
            }
        } else if let Some(previous) = &previous {
            wire["after"] = previous["record"]["spec"]["id"].clone();
        }
        run.append("work_request", json!({"call_id":call_id,"request_id":id,"machine":machine.key,"fingerprint":fingerprint,"request":wire}))?;
        wire
    };
    let files: Vec<crate::attachments::Attachment> =
        serde_json::from_value(wire["spec"]["attachments"].clone())?;
    if !machine.local
        && let Err(error) = jobs.attachments.transfer(&files, |request| {
            active(run)?;
            machine.request(jobs, &record.owner, request)
        })
    {
        return Ok(
            json!({"machine":machine.key,"request_id":id,"thread_id":wire["spec"]["thread_id"],"acceptance":if repeated {"unknown"} else {"not_dispatched"},"dispatch":"not_dispatched","error":error.to_string()}),
        );
    }
    active(run)?;
    match machine.request(jobs, &record.owner, wire) {
        Ok(snapshot) => {
            let work = reference(&machine, &snapshot);
            run.append("work_accepted", json!({"call_id":call_id,"work":work}))?;
            Ok(json!({"acceptance":"accepted","work":work,"next_after":0}))
        }
        Err(error) => Ok(
            json!({"machine":machine.key,"request_id":id,"acceptance":if machine.local || error.is::<crate::protocol::Rejected>() {"rejected"}else{"unknown"},"error":error.to_string(),"retry":"An exact request_id retry reuses the destination's accepted run; a read inspects its current state"}),
        ),
    }
}

fn clip(text: &str, limit: usize) -> (String, bool) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].into(), end < text.len())
}

fn text_page(text: &str, offset: u64) -> Result<(String, Option<usize>)> {
    let start = usize::try_from(offset)?;
    if start > text.len() || !text.is_char_boundary(start) {
        return Err("The text offset is a UTF-8 byte boundary within the retained text".into());
    }
    let (page, truncated) = clip(&text[start..], 16384);
    let next = start + page.len();
    Ok((page, truncated.then_some(next)))
}

fn projected(event: &Value) -> Value {
    if event["kind"] != "backend" && event.to_string().len() <= 8192 {
        return event.clone();
    }
    let data = &event["data"];
    let params = &data["params"];
    let item = &params["item"];
    let mut out =
        json!({"seq":event["seq"],"at":event["at"],"kind":event["kind"],"method":data["method"]});
    if let Some(id) = data.get("id") {
        out["request_id"] = id.clone();
    }
    for (name, value) in [("delta", &params["delta"]), ("item", item), ("data", data)] {
        if name == "data" && event["kind"] == "backend" {
            continue;
        }
        if value.is_null() {
            continue;
        }
        if let Some(text) = value.as_str() {
            let (text, truncated) = clip(text, 4096);
            out[name] = json!(text);
            if truncated {
                out["truncated"] = json!(true);
            }
        } else if value.to_string().len() <= 8192 {
            out[name] = value.clone();
        } else {
            out["retained_bytes"] = json!(event.to_string().len());
            out["truncated"] = json!(true);
            for field in [
                "id",
                "type",
                "status",
                "command",
                "text",
                "aggregatedOutput",
            ] {
                if let Some(text) = value[field].as_str() {
                    out[field] = json!(clip(text, 4096).0);
                }
            }
        }
    }
    if data.get("id").is_some() {
        out["params"] = params.clone();
    }
    if serde_json::to_vec(&out).map_or(true, |bytes| bytes.len() > OUTPUT_LIMIT / 2) {
        return json!({"seq":event["seq"],"at":event["at"],"kind":event["kind"],"method":data["method"],"request_id":data["id"],"truncated":true,"retained_bytes":event.to_string().len(),"detail":"The original event remains in the run journal; this projection exceeds its response bound"});
    }
    out
}

fn read(jobs: &Arc<Jobs>, run: &Arc<Run>, machine: &Machine, args: &Value) -> Result<Value> {
    let owner = run.owner();
    let id = identifier(
        Some(
            args["request_id"]
                .as_str()
                .ok_or("A work read identifies its request_id")?,
        ),
        run,
        "",
    )?;
    let id = id.as_str();
    let snapshot = machine.request(jobs, &owner, json!({"op":"get","id":id}))?;
    anonymous(&snapshot)?;
    let event_seq = args["event_seq"].as_u64();
    if event_seq.is_some_and(|seq| seq == 0 || seq > snapshot["last_seq"].as_u64().unwrap_or(0)) {
        return Err("The selected event sequence is outside the retained run".into());
    }
    let after = event_seq
        .map(|seq| seq - 1)
        .unwrap_or_else(|| args["after"].as_u64().unwrap_or(0));
    let wait = args["wait_ms"].as_u64().unwrap_or(0).min(20000);
    let deadline = Instant::now() + Duration::from_millis(wait);
    let mut connection = if machine.local {
        None
    } else {
        let mut session = crate::protocol::connect(
            machine
                .network
                .as_ref()
                .ok_or("Machine network unavailable")?,
            &machine.address,
            &machine.key,
        )?;
        session.send(&json!({"op":"watch","id":id,"after":after}))?;
        Some(session)
    };
    let batch = loop {
        let batch = if let Some(session) = &mut connection {
            session.receive()?
        } else {
            let target = crate::operator::run(jobs, id)?;
            let (events, snapshot) = target.batch(after, wait > 0)?;
            json!({"events":events,"snapshot":snapshot})
        };
        if batch.get("error").is_some() {
            return Err(batch["error"].to_string().into());
        }
        if batch["events"].as_array().is_some_and(|e| !e.is_empty())
            || terminal(batch["snapshot"]["status"].as_str().unwrap_or(""))
            || Instant::now() >= deadline
            || run.cancellation_requested()
        {
            break batch;
        }
    };
    if let Some(seq) = event_seq {
        let event = batch["events"]
            .as_array()
            .and_then(|events| events.first())
            .filter(|event| event["seq"] == seq)
            .ok_or("The selected retained event is unavailable")?;
        let encoded = serde_json::to_string(event)?;
        let (text, next_offset) = text_page(&encoded, args["offset"].as_u64().unwrap_or(0))?;
        return Ok(
            json!({"work":reference(machine,&batch["snapshot"]),"event_seq":seq,"encoding":"json_utf8","text":text,"offset":args["offset"].as_u64().unwrap_or(0),"next_offset":next_offset,"total_bytes":encoded.len()}),
        );
    }
    let mut rows = Vec::new();
    let mut bytes = 0;
    let mut next = after;
    for event in batch["events"]
        .as_array()
        .ok_or("The peer returned an invalid event batch")?
    {
        let row = projected(event);
        let size = serde_json::to_vec(&row)?.len();
        if bytes + size > OUTPUT_LIMIT && !rows.is_empty() {
            break;
        }
        rows.push(row);
        bytes += size;
        next = event["seq"].as_u64().ok_or("The event has no sequence")?;
    }
    let snapshot = &batch["snapshot"];
    let done = terminal(snapshot["status"].as_str().unwrap_or(""));
    let result = if done {
        machine.request(jobs, &owner, json!({"op":"thread_result","id":id}))?
    } else {
        Value::Null
    };
    let response_offset = args["response_offset"].as_u64().unwrap_or(0);
    let response = result["response"]
        .as_str()
        .map(|s| text_page(s, response_offset))
        .transpose()?;
    Ok(
        json!({"work":reference(machine,snapshot),"events":rows,"next_after":next,"has_more":next<snapshot["last_seq"].as_u64().unwrap_or(0),"done":done,"response":response.as_ref().map(|r|&r.0),"response_offset":response_offset,"next_response_offset":response.as_ref().and_then(|r|r.1),"response_truncated":response.is_some_and(|r|r.1.is_some())}),
    )
}

fn control(
    jobs: &Arc<Jobs>,
    run: &Arc<Run>,
    call_id: &str,
    machine: &Machine,
    args: &Value,
) -> Result<Value> {
    let id = identifier(
        Some(
            args["request_id"]
                .as_str()
                .ok_or("A work control identifies its request_id")?,
        ),
        run,
        "",
    )?;
    let id = id.as_str();
    let snapshot = machine.request(jobs, &run.owner(), json!({"op":"get","id":id}))?;
    anonymous(&snapshot)?;
    if id == run.snapshot().record.spec.id {
        return Err("The coordinating run has its own ordinary control channel".into());
    }
    let action = match args["action"].as_str().unwrap_or("") {
        "steer" => {
            json!({"type":"steer","text":args["prompt"].as_str().ok_or("Steering contains a prompt")?})
        }
        "cancel" => json!({"type":"cancel"}),
        "respond" => {
            json!({"type":"respond","request_id":args["backend_request_id"],"result":args["result"]})
        }
        _ => return Err("Unknown work control".into()),
    };
    if let Some(old) = run
        .retained_events("work_control")?
        .into_iter()
        .find(|e| e.data["call_id"] == call_id)
    {
        if old.data["action"] != action
            || old.data["request_id"] != id
            || old.data["machine"] != machine.key
        {
            return Err("This call id already identifies another control".into());
        }
        if let Some(accepted) = run
            .retained_events("work_control_accepted")?
            .into_iter()
            .find(|e| e.data["call_id"] == call_id)
        {
            return Ok(
                json!({"control":accepted.data["response"],"work":reference(machine,&snapshot),"detail":"The retained accepted control is not repeated"}),
            );
        }
        return Ok(
            json!({"status":"unknown","request_id":id,"detail":"The retained control attempt is not automatically repeated"}),
        );
    }
    run.append(
        "work_control",
        json!({"call_id":call_id,"machine":machine.key,"request_id":id,"action":action}),
    )?;
    active(run)?;
    let response = machine.request(
        jobs,
        &run.owner(),
        json!({"op":"control","id":id,"action":action}),
    )?;
    run.append(
        "work_control_accepted",
        json!({"call_id":call_id,"machine":machine.key,"request_id":id,"response":response}),
    )?;
    Ok(
        json!({"control":response,"work":reference(machine,&snapshot),"detail":"Acceptance queues the control; later retained events report its outcome"}),
    )
}

fn files(jobs: &Arc<Jobs>, run: &Arc<Run>, args: &Value) -> Result<Value> {
    if args["machine"].is_null()
        && args["thread_id"].is_null()
        && (args["run_id"].is_null()
            || args["run_id"]
                .as_str()
                .and_then(|id| jobs.operator_run(id).ok())
                .is_some_and(|source| {
                    source.snapshot().record.spec.thread_id == run.snapshot().record.spec.thread_id
                }))
    {
        return jobs.workspace_files(&run.snapshot().record.spec.thread_id, args);
    }
    let machine = work_machine(jobs, run, args)?;
    let thread = if let Some(thread) = args["thread_id"].as_str() {
        thread.to_owned()
    } else if let Some(id) = args["run_id"].as_str() {
        let snapshot = machine.request(jobs, &run.owner(), json!({"op":"get","id":id}))?;
        anonymous(&snapshot)?;
        snapshot["record"]["spec"]["thread_id"]
            .as_str()
            .ok_or("The run has no thread id")?
            .into()
    } else {
        run.snapshot().record.spec.thread_id
    };
    if machine.local {
        return jobs.workspace_files(&thread, args);
    }
    if args["action"] == "list" {
        return machine.request(jobs,&run.owner(),json!({"op":"thread_workspace","thread_id":thread,"action":"list","offset":args["offset"],"through_run_id":args["through_run_id"]}))
            .map_err(|error| format!("Workspace listing for thread {thread} on {} is unavailable: {error}", machine.key).into());
    }
    let offer = if args["action"] == "attachment" {
        let result = machine.request(jobs,&run.owner(),json!({"op":"thread_workspace","thread_id":thread,"action":"attachment","run_id":args["run_id"],"digest":args["digest"]}))?;
        result["file"].clone()
    } else if args["action"] == "pull" {
        machine.request(jobs,&run.owner(),json!({"op":"thread_workspace_offer","thread_id":thread,"run_id":args["run_id"],"path":args["path"]}))?
    } else {
        return Err("File retrieval accepts list, pull or attachment".into());
    };
    let file = serde_json::from_value(offer)?;
    let path = jobs.attachments.receive(&file, |request| {
        active(run)?;
        machine.request(jobs, &run.owner(), request)
    })?;
    if let Some(execution) = &run.snapshot().record.execution {
        execution.own_managed_path(&path)?;
    }
    Ok(
        json!({"path":path,"file":file,"source":{"machine":machine.key,"thread_id":thread,"run_id":args["run_id"],"relative_path":args["path"]},"snapshot":if args["action"] == "attachment" {"retained_original"} else {"read_at_pull_time"}}),
    )
}

pub fn is_tool(name: &str) -> bool {
    matches!(name, "boxen_machines" | "boxen_work" | "boxen_thread_files")
}

pub fn call(
    jobs: &Arc<Jobs>,
    run: &Arc<Run>,
    call_id: &str,
    name: &str,
    args: Value,
) -> Result<Value> {
    run.diagnostic(crate::logging::Level::Info,"work.tool_started",||json!({"call_id":call_id,"tool":name,"action":args["action"],"machine":args["machine"],"request_id":args["request_id"]}),||Value::Null);
    let result = (|| -> Result<Value> {
        active(run)?;
        match name {
            "boxen_machines" => inventory(jobs, &run.owner(), &args),
            "boxen_thread_files" => files(jobs, run, &args),
            "boxen_work" => {
                let machine = work_machine(jobs, run, &args)?;
                match args["action"].as_str().unwrap_or("") {
                    "list" => catalog(jobs, run, &machine, &args),
                    "request" => request(jobs, run, call_id, &args),
                    "read" => read(jobs, run, &machine, &args),
                    "steer" | "cancel" | "respond" => control(jobs, run, call_id, &machine, &args),
                    _ => {
                        Err("Work actions are list, request, read, steer, cancel or respond".into())
                    }
                }
            }
            _ => Err("Unknown work tool".into()),
        }
    })();
    run.diagnostic(if result.is_ok(){crate::logging::Level::Info}else{crate::logging::Level::Warn},"work.tool_completed",||json!({"call_id":call_id,"tool":name,"action":args["action"],"machine":args["machine"],"success":result.is_ok(),"acceptance":result.as_ref().ok().map(|v|&v["acceptance"]),"done":result.as_ref().ok().map(|v|&v["done"]),"status":result.as_ref().ok().and_then(|v|v.get("status")),"has_more":result.as_ref().ok().and_then(|v|v.get("has_more"))}),||result.as_ref().map_or_else(|e|json!({"error":e.to_string()}),|v|v.clone()));
    result
}

pub fn tools() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"boxen_machines","description":"This tool discovers the current machine and admitted network members, with observed reachability, runtime/backends and execution context. machine accepts an exact name, full id or unambiguous id prefix; omission reads eight members at a time. next_offset retrieves later members. A failed probe reports unavailable without declaring a machine stopped. Runtime declarations do not establish toolchain, application permissions or model credentials.","inputSchema":{"type":"object","properties":{"machine":{"type":"string"},"offset":{"type":"integer","minimum":0}},"additionalProperties":false}}),
        json!({"type":"function","name":"boxen_work","description":"This tool requests and follows ordinary work across admitted machines. list returns a bounded page of existing anonymous requests, optionally filtered by thread_id; next_cursor retrieves the following page. request starts an anonymous conversation or continues thread_id. after_request_id identifies an exact previous request instead of looking up a thread head; a stale predecessor is rejected by the destination. machine accepts a discovered name/id; omission follows a known request or thread handle in this conversation, otherwise local; new remote work gets an automatic destination workspace. request_id can be omitted or a short stable label; labels remain stable within this coordinating conversation, including reads and controls. Returned ids identify exact retries. Attachments are local file paths retained before transfer. read returns bounded retained progress, next_after, current status and a final response; after replays from that sequence, wait_ms waits up to 20000 for new activity. Large projections are labeled truncated; event_seq retrieves one original event as paged JSON text with offset/next_offset. A long final response has next_response_offset for a response_offset read. steer adds input to active work, cancel requests interruption, and respond answers a retained backend request. Controls can have unknown outcomes and are not automatically repeated. A host/network failure does not establish workload failure. Execution continues independently of this observer. Work requests are distinct from resident mailbox correspondence.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","request","read","steer","cancel","respond"]},"machine":{"type":"string"},"request_id":{"type":"string"},"thread_id":{"type":"string"},"after_request_id":{"type":"string"},"cursor":{"type":"object"},"prompt":{"type":"string"},"backend":{"type":"string","enum":["codex","openrouter","antigravity"]},"cwd":{"type":"string"},"model":{"type":"string"},"attachments":{"type":"array","items":{"type":"string"},"maxItems":8},"after":{"type":"integer","minimum":0},"event_seq":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0},"response_offset":{"type":"integer","minimum":0},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"backend_request_id":{},"result":{}},"required":["action"],"additionalProperties":false}}),
        crate::jobs::thread_workspace::tool(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_unicode_events_and_permission_schemas_remain_bounded_and_explicit() {
        let text = "🐈".repeat(50000);
        for event in [
            json!({"seq":17,"at":1,"kind":"backend","data":{"method":"item/commandExecution/outputDelta","params":{"delta":text}}}),
            json!({"seq":18,"at":2,"kind":"backend","data":{"id":0,"method":"mcpServer/elicitation/request","params":{"message":text,"requestedSchema":{"description":text}}}}),
            json!({"seq":19,"at":3,"kind":"tool_result","data":{"text":text}}),
        ] {
            let result = projected(&event);
            assert!(result.to_string().len() <= OUTPUT_LIMIT / 2);
            assert_eq!(result["seq"], event["seq"]);
            assert_eq!(result["truncated"], true);
        }
        assert_eq!(clip("a🐈b", 4), ("a".into(), true));
        assert!(text_page("a🐈b", 2).is_err());
        assert_eq!(text_page("a🐈b", 1).unwrap(), ("🐈b".into(), None));
    }
}
