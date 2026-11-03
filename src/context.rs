//! Context views are projections of accepted journal entries, never replacement history.
use crate::{
    Result,
    jobs::{self, Run},
    logging::Level,
    state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Summary,
    Verbatim,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub budget_bytes: usize,
    pub pin_quota_bytes: usize,
    pub mask_after: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Summary,
            budget_bytes: 196608,
            pin_quota_bytes: 32768,
            mask_after: 12,
        }
    }
}
impl Settings {
    pub fn resident() -> Self {
        Self {
            mode: Mode::Verbatim,
            ..Self::default()
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !(16384..=524288).contains(&self.budget_bytes)
            || self.pin_quota_bytes > self.budget_bytes / 2
            || self.mask_after > 256
        {
            return Err("context budget is 16384–524288 serialized bytes, pin quota is at most half that budget, and mask_after is at most 256 entries".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub at: u64,
    pub message: Value,
    pub provenance: Value,
}
impl Entry {
    pub fn size(&self) -> usize {
        self.message.to_string().len()
    }
}
pub struct Source {
    pub entries: Vec<Entry>,
    pub events: Vec<Value>,
}
// Bounded client/model previews point to exact paged journal records.
fn preview(record: &Value) -> Value {
    let mut p = record.clone();
    for field in [
        "selected",
        "masked",
        "excluded_bundles",
        "pin_warnings",
        "excluded_notes",
        "notes",
        "sources",
        "entries",
    ] {
        if let Some(items) = record[field].as_array() {
            let limit = if matches!(field, "selected" | "masked") {
                64
            } else {
                4
            };
            let values: Vec<_> = items
                .iter()
                .take(limit)
                .map(|v| if v.is_object() { preview(v) } else { v.clone() })
                .collect();
            p[field] = json!(values);
            p[format!("{field}_total")] = json!(items.len());
        }
    }
    if let Some(t) = record["text"].as_str()
        && t.len() > 512
    {
        p["text"] = json!(t.chars().take(128).collect::<String>());
        p["text_preview"] = json!(true);
    }
    if let Some(summary) = record.get("summary")
        && !summary.is_null()
    {
        p["summary"] = preview(summary);
    }
    p["preview"] = json!(true);
    p
}
impl Source {
    pub fn from_runs(runs: &[Arc<Run>]) -> Self {
        let mut entries = Vec::new();
        let mut events = Vec::new();
        for run in runs {
            let snapshot = run.snapshot();
            for event in run.context_events() {
                if event.kind == "inference_message" {
                    entries.push(Entry {
                        id: format!("{}:{}", snapshot.record.spec.id, event.seq),
                        at: event.at,
                        message: event.data,
                        provenance: if let Some(import)=&snapshot.record.transcript_import {
                            json!({"run_id":snapshot.record.spec.id,"thread_id":snapshot.record.spec.thread_id,"backend":"transcript_import","source":import.source,"source_message_id":event.seq.checked_sub(2).and_then(|i|import.message_ids.get(i as usize)),"model":import.source.model,"resident_id":snapshot.record.spec.resident_id,"imported_at":import.imported_at})
                        } else {json!({"run_id":snapshot.record.spec.id,"thread_id":snapshot.record.spec.thread_id,"backend":snapshot.record.spec.backend,"model":if snapshot.record.spec.model.is_empty(){crate::inference::DEFAULT_MODEL}else{&snapshot.record.spec.model},"resident_id":snapshot.record.spec.resident_id,"scheduled":snapshot.record.scheduled})},
                    });
                } else {
                    events.push(json!({"id":format!("{}:{}",snapshot.record.spec.id,event.seq),"at":event.at,"kind":event.kind,"data":event.data}));
                }
            }
        }
        Self { entries, events }
    }
    pub fn boundary(&self) -> Option<&Value> {
        self.events
            .iter()
            .rev()
            .find(|e| e["kind"] == "context_boundary")
            .map(|e| &e["data"])
    }
    fn approved_notes(&self) -> Vec<Value> {
        let mut seen = HashSet::new();
        let mut notes = Vec::new();
        for event in self
            .events
            .iter()
            .rev()
            .filter(|e| e["kind"] == "context_note" && e["data"]["approved"] == true)
        {
            if let Some(id) = event["data"]["id"].as_str()
                && seen.insert(id.to_owned())
            {
                notes.push(event["data"].clone());
            }
        }
        notes.reverse();
        notes
    }
    pub fn objects(&self, kind: &str) -> Vec<Value> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for e in self.events.iter().rev().filter(|e| e["kind"] == kind) {
            let d = &e["data"];
            if let Some(id) = d["id"].as_str()
                && seen.insert(id.to_owned())
            {
                result.push(d.clone());
            }
        }
        result.reverse();
        result
    }
    fn page(&self, args: &Value, reply_limit: usize) -> Result<Value> {
        let mut offset = args["offset"].as_u64().unwrap_or(0) as usize;
        if let Some(id) = args["id"].as_str() {
            offset = self
                .entries
                .iter()
                .position(|e| e.id == id)
                .ok_or("history entry is unavailable in this thread")?
                .saturating_sub(args["before"].as_u64().unwrap_or(0).min(10) as usize);
        }
        let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
        let query = args["query"].as_str();
        let mut entries = Vec::new();
        let mut bytes = 0;
        let mut next = offset;
        for (index, e) in self.entries.iter().enumerate().skip(offset) {
            next = index + 1;
            if query.is_some_and(|q| !e.message.to_string().contains(q)) {
                continue;
            }
            let text = e.message.to_string();
            let byte_offset = args["byte_offset"]
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or_else(|| {
                    query
                        .and_then(|q| text.find(q))
                        .unwrap_or(0)
                        .saturating_sub(256)
                });
            let mut begin = byte_offset.min(text.len());
            while !text.is_char_boundary(begin) {
                begin -= 1;
            }
            let mut end = (begin + (reply_limit / 4).clamp(128, 16384)).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let item = loop {
                let candidate = if begin == 0 && end == text.len() {
                    serde_json::to_value(e)?
                } else {
                    json!({"id":e.id,"at":e.at,"role":e.message["role"],"provenance":e.provenance,"serialized_message_excerpt":&text[begin..end],"byte_offset":begin,"next_byte_offset":if end<text.len(){Some(end)}else{None},"total_bytes":text.len()})
                };
                if candidate.to_string().len() <= reply_limit.saturating_sub(512)
                    || end <= begin + 4
                {
                    break candidate;
                }
                end = begin + (end - begin) / 2;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
            };
            let size = item.to_string().len();
            if bytes + size > reply_limit.saturating_sub(512) {
                next = index;
                break;
            }
            bytes += size;
            entries.push(item);
            if entries.len() >= limit {
                break;
            }
        }
        Ok(
            json!({"entries":entries,"total":self.entries.len(),"next_offset":if next<self.entries.len(){Some(next)}else{None},"detail":"Original accepted journal entries, not reexecuted observations. Excerpts paginate serialized message JSON at UTF-8 byte boundaries."}),
        )
    }
    pub fn operation(
        &self,
        run: &Run,
        args: Value,
        mutating: bool,
        actor: &str,
        reply_limit: usize,
    ) -> Result<Value> {
        let result = self.operation_raw(run, args.clone(), mutating, actor, reply_limit)?;
        if result.to_string().len() <= reply_limit {
            return Ok(result);
        }
        Ok(
            json!({"bounded":true,"operation_completed":true,"action":args["action"],"record_id":result.get("id").or_else(||result.get("boundary_id")).or_else(||result.get("boundary").and_then(|b|b.get("id"))),"source_entries":self.entries.len(),"detail":"The operation completed, but its full result exceeds this input's reply allowance. Exact context records remain available through record with byte_offset; read returns bounded original entries, and coverage uses source-offset pages. This notice is not the original record."}),
        )
    }
    fn operation_raw(
        &self,
        run: &Run,
        args: Value,
        mutating: bool,
        actor: &str,
        reply_limit: usize,
    ) -> Result<Value> {
        let action = args["action"].as_str().unwrap_or("status");
        if action == "read" || action == "search" {
            return self.page(&args, reply_limit);
        }
        if action == "record" {
            let id = args["id"].as_str().ok_or("record requires an id")?;
            let e = self
                .events
                .iter()
                .rev()
                .find(|e| {
                    (e["id"] == id || e["data"]["id"] == id)
                        && args["revision"]
                            .as_u64()
                            .is_none_or(|r| e["data"]["revision"].as_u64() == Some(r))
                })
                .ok_or("context record or revision is unavailable")?;
            let text = e.to_string();
            let mut begin = (args["byte_offset"].as_u64().unwrap_or(0) as usize).min(text.len());
            while !text.is_char_boundary(begin) {
                begin -= 1;
            }
            let mut end = (begin + (reply_limit / 4).clamp(128, 16384)).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            return if begin == 0 && end == text.len() {
                Ok(json!({"record":e}))
            } else {
                Ok(
                    json!({"event_id":e["id"],"serialized_record_excerpt":&text[begin..end],"byte_offset":begin,"next_byte_offset":if end<text.len(){Some(end)}else{None},"total_bytes":text.len()}),
                )
            };
        }
        if action == "status" {
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;
            let bundles = self.objects("context_bundle");
            let notes = self.objects("context_note");
            return Ok(
                json!({"boundary":self.boundary().map(preview),"entries":self.entries.len(),"bundles":bundles.iter().skip(offset).take(8).map(preview).collect::<Vec<_>>(),"bundle_count":bundles.len(),"notes":notes.iter().skip(offset).take(4).map(preview).collect::<Vec<_>>(),"note_count":notes.len(),"next_bundle_offset":if offset+8<bundles.len(){Some(offset+8)}else{None},"next_note_offset":if offset+4<notes.len(){Some(offset+4)}else{None}}),
            );
        }
        if action == "coverage" {
            let b = if let Some(id) = args["boundary_id"].as_str() {
                self.events
                    .iter()
                    .find(|e| e["kind"] == "context_boundary" && e["data"]["id"] == id)
                    .map(|e| &e["data"])
            } else {
                self.boundary()
            }
            .ok_or("no installed context boundary")?;
            let selected: HashSet<_> = b["selected"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let masked: HashSet<_> = b["masked"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;
            let count = b["source_count"].as_u64().unwrap_or(0) as usize;
            let page:Vec<_>=self.entries.iter().take(count).skip(offset).take((reply_limit/512).clamp(1,50)).map(|e|json!({"id":e.id,"at":e.at,"role":e.message["role"],"state":if masked.contains(e.id.as_str()){"observation_reference"}else if selected.contains(e.id.as_str()){"verbatim"}else{"omitted"},"reason":if masked.contains(e.id.as_str()){"older_tool_body"}else if !selected.contains(e.id.as_str()){"prompt_budget"}else{"selected"}})).collect();
            return Ok(
                json!({"boundary_id":b["id"],"source_count":count,"next_offset":if offset+page.len()<count{Some(offset+page.len())}else{None},"entries":page,"boundary_record_id":b["id"],"excluded_bundles":preview(b)["excluded_bundles"],"excluded_bundle_count":b["excluded_bundles"].as_array().map_or(0,Vec::len),"notes":preview(b)["notes"]}),
            );
        }
        if !mutating {
            return Err("context changes are accepted at a tool boundary in the executing run or after execution ends".into());
        }
        if action == "compact" {
            run.append("context_compact_requested", json!({}))?;
            return Ok(
                json!({"accepted":true,"effect":"The configured policy is evaluated before the next inference request, after this tool batch completes."}),
            );
        }
        let id = args["id"]
            .as_str()
            .ok_or("context changes require a stable id")?;
        jobs::valid_id(id)?;
        if action == "pin" || action == "unpin" {
            let objects = self.objects("context_bundle");
            if !objects.iter().any(|b| b["id"] == id) && objects.len() >= 128 {
                return Err("this thread supports 128 bundle records; existing bundle ids remain updateable".into());
            }
            let ids = args["entries"].as_array().cloned().unwrap_or_default();
            if action == "pin"
                && (ids.is_empty()
                    || ids.len() > 64
                    || ids.iter().any(|id| {
                        !self
                            .entries
                            .iter()
                            .any(|e| Some(e.id.as_str()) == id.as_str())
                    }))
            {
                return Err(
                    "a source bundle contains 1–64 exact entry ids from this thread".into(),
                );
            }
            let value = json!({"id":id,"active":action=="pin","entries":ids,"at":jobs::now()});
            run.append("context_bundle", value.clone())?;
            return Ok(value);
        }
        if action == "note" || action == "draft" || action == "approve" {
            let objects = self.objects("context_note");
            if args["author_label"].as_str().is_some_and(|s| s.len() > 256) {
                return Err("author_label is at most 256 bytes".into());
            }
            if !objects.iter().any(|n| n["id"] == id) && objects.len() >= 32 {
                return Err("this thread supports 32 authored note/signpost ids; existing ids remain updateable".into());
            }
            let prior = self
                .objects("context_note")
                .into_iter()
                .find(|n| n["id"] == id);
            let value = if action == "approve" {
                let mut n = prior.ok_or("note draft does not exist")?;
                if args["expected_revision"].as_u64() != n["revision"].as_u64() {
                    return Err("note review revision conflict; the draft remains unchanged".into());
                }
                let visible = self
                    .events
                    .iter()
                    .rev()
                    .find(|e| e["kind"] == "context_exposure")
                    .map(|e| &e["data"])
                    .ok_or("note review requires an exposed source view")?;
                for id in n["sources"].as_array().into_iter().flatten() {
                    if !visible["selected"]
                        .as_array()
                        .is_some_and(|a| a.contains(id))
                        || visible["masked"].as_array().is_some_and(|a| a.contains(id))
                    {
                        return Err("note source is absent or masked in the current view; pinning its source and a new input can expose originals before review".into());
                    }
                }
                n["source_exposure"] = json!({"boundary_id":visible["boundary_id"],"input_at":visible["at"],"source_ids":n["sources"],"review_at":jobs::now(),"detail":"Listed source entries were selected verbatim in the review view; this is not evidence of attention or comprehension."});
                n["approved"] = json!(true);
                n["reviewed_at"] = json!(jobs::now());
                n["review_run"] = json!(run.snapshot().record.spec.id);
                n["reviewer"] = json!({"kind":actor,"peer_id":run.snapshot().record.owner,"resident_id":run.snapshot().record.resident.map(|r|r.id)});
                n
            } else {
                let text = args["text"].as_str().ok_or("note text is required")?;
                if text.is_empty() || text.len() > 8192 {
                    return Err("context note contains 1–8192 bytes".into());
                }
                let refs = args["entries"].as_array().cloned().unwrap_or_default();
                if refs.len() > 64
                    || refs.iter().any(|id| {
                        !self
                            .entries
                            .iter()
                            .any(|e| Some(e.id.as_str()) == id.as_str())
                    })
                {
                    return Err("note sources are at most 64 exact entries from this thread".into());
                }
                if action == "draft" && refs.is_empty() {
                    return Err(
                        "a helper draft requires original source pointers for review".into(),
                    );
                }
                if let Some(old) = &prior
                    && args["expected_revision"].as_u64() != old["revision"].as_u64()
                {
                    return Err("note revision conflict; the prior record remains unchanged".into());
                }
                let parent = args["parent_note"].as_str();
                let ancestor = parent
                    .map(|p| {
                        self.objects("context_note")
                            .into_iter()
                            .find(|n| n["id"] == p)
                            .ok_or("parent note is unavailable")
                    })
                    .transpose()?;
                let revision = prior
                    .as_ref()
                    .and_then(|n| n["revision"].as_u64())
                    .unwrap_or(0)
                    + 1;
                json!({"id":id,"revision":revision,"kind":if args["kind"]=="semantic_signpost"{"semantic_signpost"}else{"continuity_note"},"text":text,"sources":refs,"author":{"kind":if action=="draft"{"helper"}else{actor},"submitted_by":actor,"peer_id":run.snapshot().record.owner,"label":args["author_label"],"run_id":run.snapshot().record.spec.id,"model":run.snapshot().record.spec.model},"approved":action=="note","parent":ancestor.as_ref().map(|n|json!({"id":n["id"],"revision":n["revision"]})),"derivation_depth":ancestor.as_ref().and_then(|n|n["derivation_depth"].as_u64()).unwrap_or(0)+1,"created_at":jobs::now(),"source_exposure":{"boundary_id":self.boundary().map(|b|b["id"].clone()),"source_ids":refs,"detail":"Source pointers and prior input selection establish availability only, not reading or understanding."}})
            };
            if value.to_string().len() > 16384 {
                return Err(
                    "note provenance exceeds 16 KiB; exact journal lineage remains available"
                        .into(),
                );
            }
            run.append("context_note", value.clone())?;
            return Ok(value);
        }
        Err("context actions are status, record, read, search, coverage, pin, unpin, note, draft, approve, and compact".into())
    }
}

/// One runtime-local projection builder; durable facts stay in the existing journals.
pub struct View {
    pub settings: Settings,
    pub boundary: Option<Value>,
    pub force: bool,
}
pub struct Plan {
    pub messages: Vec<Value>,
    pub selected: Vec<String>,
    pub masked: Vec<String>,
    pub excluded: Vec<Value>,
    pub pin_warnings: Vec<Value>,
    pub excluded_notes: Vec<Value>,
    pub notes: Vec<Value>,
    pub pin_used_bytes: usize,
    pub source_count: usize,
    pub summary_input: Vec<Value>,
}
fn masked(e: &Entry) -> Value {
    json!({"role":"tool","tool_call_id":e.message["tool_call_id"],"content":format!("Historical observation {} is omitted by the older-tool-body policy. boxen_history read returns the original without repeating its effects.",e.id)})
}
impl View {
    pub fn new(settings: Settings, source: &Source) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            settings,
            boundary: source.boundary().cloned(),
            force: source
                .events
                .iter()
                .rposition(|e| e["kind"] == "context_compact_requested")
                .is_some_and(|r| {
                    source
                        .events
                        .iter()
                        .rposition(|e| e["kind"] == "context_boundary")
                        .is_none_or(|b| r > b)
                }),
        })
    }
    pub fn plan(&self, source: &Source) -> Result<Plan> {
        let entries = &source.entries;
        let mut units: Vec<Vec<usize>> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if e.message["role"] == "tool" && !units.is_empty() {
                units.last_mut().unwrap().push(i);
            } else {
                units.push(vec![i]);
            }
        }
        let candidates: Vec<_> = source.approved_notes().into_iter().rev().collect();
        let mut excluded_notes = Vec::new();
        let mut notes = Vec::new();
        let mut note_bytes = 0;
        for note in candidates {
            let n = note.to_string().len();
            if note_bytes + n <= self.settings.budget_bytes / 8 {
                note_bytes += n;
                notes.push(note);
            } else {
                excluded_notes.push(
                    json!({"id":note["id"],"revision":note["revision"],"reason":"note_budget"}),
                );
            }
        }
        // Summary and explanatory metadata have separate bounded headroom.
        let budget = self.settings.budget_bytes.saturating_sub(
            note_bytes
                + if self.settings.mode == Mode::Summary {
                    14336
                } else {
                    6144
                },
        );
        let threshold = entries.len().saturating_sub(self.settings.mask_after);
        let pinned_ids: HashSet<String> = source
            .objects("context_bundle")
            .iter()
            .filter(|b| b["active"] == true)
            .flat_map(|b| b["entries"].as_array().cloned().unwrap_or_default())
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let pinned_units: HashSet<usize> = units
            .iter()
            .enumerate()
            .filter(|(_, u)| u.iter().any(|i| pinned_ids.contains(&entries[*i].id)))
            .map(|(u, _)| u)
            .collect();
        let recall_outputs: HashSet<usize> = units
            .iter()
            .filter(|u| {
                u.iter().any(|i| {
                    entries[*i].message["tool_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|c| c["function"]["name"] == "boxen_history")
                })
            })
            .flat_map(|u| u.iter().copied())
            .collect();
        let exact_pins: HashSet<usize> = pinned_units
            .iter()
            .flat_map(|u| units[*u].iter().copied())
            .collect();
        let mut projected: Vec<Value> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if i < threshold
                    && e.message["role"] == "tool"
                    && e.size() > 1024
                    && !exact_pins.contains(&i)
                    && !recall_outputs.contains(&i)
                    && !units.last().is_some_and(|u| u.contains(&i))
                {
                    masked(e)
                } else {
                    e.message.clone()
                }
            })
            .collect();
        let mut included = HashSet::new();
        let mut used = 0;
        let mut pins = 0;
        let mut excluded = Vec::new();
        // Neither the current request nor the latest exchange can disappear and
        // induce a model to repeat an already completed effect.
        let newest_user = units
            .iter()
            .rposition(|u| u.iter().any(|i| entries[*i].message["role"] == "user"));
        for i in newest_user.into_iter().chain(units.len().checked_sub(1)) {
            if !included.insert(i) {
                continue;
            }
            let mut n: usize = units[i]
                .iter()
                .map(|j| projected[*j].to_string().len() + 512)
                .sum();
            if used + n > budget {
                for j in &units[i] {
                    if entries[*j].message["role"] == "tool"
                        && !exact_pins.contains(j)
                        && !units[i].iter().any(|a| {
                            entries[*a].message["tool_calls"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|c| {
                                    c["function"]["name"] == "boxen_history"
                                        && c["id"] == entries[*j].message["tool_call_id"]
                                })
                        })
                    {
                        projected[*j] = masked(&entries[*j]);
                    }
                }
                n = units[i]
                    .iter()
                    .map(|j| projected[*j].to_string().len() + 512)
                    .sum();
            }
            if used + n > budget {
                return Err("the newest request and exchange exceed this context budget; original input/effects remain retained, and a larger context setting can admit them without replay".into());
            }
            used += n;
        }
        let cost = |u: &Vec<usize>| {
            u.iter()
                .map(|i| projected[*i].to_string().len() + 512)
                .sum::<usize>()
        };
        let mut bundles = source.objects("context_bundle");
        bundles.sort_by_key(|b| b["id"].as_str().unwrap_or("").to_owned());
        for b in bundles.iter().filter(|b| b["active"] == true) {
            let requested: HashSet<_> = b["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let indexes: Vec<_> = units
                .iter()
                .enumerate()
                .filter(|(_, u)| {
                    u.iter()
                        .any(|i| requested.contains(entries[*i].id.as_str()))
                })
                .map(|(i, _)| i)
                .collect();
            let extra: usize = indexes
                .iter()
                .filter(|i| !included.contains(*i))
                .map(|i| cost(&units[*i]))
                .sum();
            if extra + pins > self.settings.pin_quota_bytes || extra + used > budget {
                excluded.push(json!({"id":b["id"],"entries":b["entries"],"reason":"pin_quota_or_prompt_budget"}));
            } else {
                pins += extra;
                used += extra;
                included.extend(indexes);
            }
        }
        for i in (0..units.len()).rev() {
            if included.contains(&i) {
                continue;
            }
            let c = cost(&units[i]);
            if used + c <= budget {
                included.insert(i);
                used += c;
            } else {
                break;
            }
        }
        let mut selected = Vec::new();
        let mut masks = Vec::new();
        let mut messages = Vec::new();
        let mut summary_input = Vec::new();
        let mut summary_bytes = 0;
        let through = self
            .boundary
            .as_ref()
            .filter(|b| {
                !self.force
                    && self.settings.mode == Mode::Summary
                    && b["settings"]["mode"] == "summary"
            })
            .and_then(|b| {
                b["summary_attempted_through"]
                    .as_str()
                    .or_else(|| b["summary"]["through_id"].as_str())
            })
            .and_then(|id| entries.iter().position(|e| e.id == id));
        for (u, unit) in units.iter().enumerate() {
            if included.contains(&u) {
                for i in unit {
                    selected.push(entries[*i].id.clone());
                    if projected[*i] != entries[*i].message {
                        masks.push(entries[*i].id.clone());
                    }
                    if *i > 0
                        && selected.len() > 1
                        && selected
                            .iter()
                            .rev()
                            .nth(1)
                            .is_none_or(|id| id != &entries[*i - 1].id)
                    {
                        messages.push(json!({"role":"system","content":format!("Boxen history selection omitted intervening source entries before {}. The following original entry keeps its original role and chronology; omitted neighbors are available through boxen_history.",entries[*i].id)}));
                    }
                    messages.push(projected[*i].clone());
                }
            } else if through.is_none_or(|t| unit.last().is_some_and(|i| *i > t)) {
                let results: HashSet<_> = unit
                    .iter()
                    .filter_map(|i| entries[*i].message["tool_call_id"].as_str())
                    .collect();
                let group:Vec<Value>=unit.iter().map(|i|{
                    let unknown:Vec<_>=entries[*i].message["tool_calls"].as_array().into_iter().flatten().filter_map(|c|c["id"].as_str()).filter(|id|!results.contains(id)).collect();
                    json!({"source_id":entries[*i].id,"at":entries[*i].at,"message":projected[*i],"observation_masked":projected[*i]!=entries[*i].message,"missing_tool_results":unknown})
                }).collect();
                let n = serde_json::to_vec(&group)?.len();
                if summary_bytes + n < self.settings.budget_bytes / 2 {
                    summary_bytes += n;
                    summary_input.extend(group);
                }
            }
        }
        let pin_warnings = excluded.clone();
        excluded.retain(|b| {
            b["entries"].as_array().into_iter().flatten().any(|id| {
                !selected.iter().any(|s| Some(s.as_str()) == id.as_str())
                    || masks.iter().any(|s| Some(s.as_str()) == id.as_str())
            })
        });
        Ok(Plan {
            messages,
            selected,
            masked: masks,
            excluded,
            pin_warnings,
            excluded_notes,
            notes,
            pin_used_bytes: pins,
            source_count: entries.len(),
            summary_input,
        })
    }
    pub fn needs_boundary(&self, p: &Plan) -> bool {
        if self.force {
            return true;
        }
        let Some(b) = &self.boundary else {
            return true;
        };
        let prefix = b["source_count"].as_u64().unwrap_or(0) as usize;
        let old_selected = b["selected"].as_array().cloned().unwrap_or_default();
        // Appending an unchanged live tail is not itself a new cut/audit notice.
        let common = p
            .selected
            .iter()
            .filter(|id| old_selected.contains(&json!(id)))
            .count();
        let omitted_total = p.source_count.saturating_sub(p.selected.len());
        let old_omitted = prefix.saturating_sub(old_selected.len());
        b["settings"] != json!(self.settings)
            || common != old_selected.len()
            || omitted_total != old_omitted
            || b["masked"] != json!(p.masked)
            || b["excluded_bundles"] != json!(p.excluded)
            || b["notes"] != json!(p.notes)
            || b["pin_warnings"] != json!(p.pin_warnings)
            || b["excluded_notes"] != json!(p.excluded_notes)
    }
    pub fn install(
        &mut self,
        run: &Run,
        source: &Source,
        p: &Plan,
        summary: Option<Value>,
        failure: Option<String>,
    ) -> Result<()> {
        let id = state::random_hex()?;
        let b = json!({"schema":"boxen.context_boundary.v1","id":id,"parent":self.boundary.as_ref().map(|b|b["id"].clone()),"thread_id":run.snapshot().record.spec.thread_id,"at":jobs::now(),"source_count":p.source_count,"watermark":source.entries.last().map(|e|json!({"id":e.id,"at":e.at})),"settings":self.settings,"selected":p.selected,"masked":p.masked,"excluded_bundles":p.excluded,"pin_used_bytes":p.pin_used_bytes,"pin_warnings":p.pin_warnings,"excluded_notes":p.excluded_notes,"notes":p.notes,"summary":summary,"summary_attempted_through":if self.settings.mode==Mode::Summary {p.summary_input.last().map(|e|e["source_id"].clone()).or_else(||self.boundary.as_ref().filter(|b|b["settings"]["mode"]=="summary").map(|b|b["summary_attempted_through"].clone()))} else {None},"fallback_reason":failure,"detail":"Source journals are unchanged. Coverage and exact history are available through boxen_history; semantic completeness is not certified."});
        run.append("context_boundary", b.clone())?;
        run.diagnostic(Level::Info,"context.boundary.installed",||json!({"boundary_id":id,"mode":self.settings.mode,"source_count":p.source_count,"included":p.selected.len(),"masked":p.masked.len(),"excluded_bundles":p.excluded.len(),"fallback":b["fallback_reason"]}),||Value::Null);
        self.boundary = Some(b);
        self.force = false;
        Ok(())
    }
    pub fn framing(&self, source: &Source, first: bool) -> String {
        let Some(b) = &self.boundary else {
            return String::new();
        };
        let count = b["source_count"].as_u64().unwrap_or(0) as usize;
        let included = b["selected"].as_array().map_or(0, Vec::len);
        let masked = b["masked"].as_array().map_or(0, Vec::len);
        let excluded = b["excluded_bundles"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let gaps: Vec<_> = source
            .entries
            .iter()
            .take(count)
            .rev()
            .filter(|e| {
                !b["selected"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(e.id)))
            })
            .take(4)
            .map(|e| json!({"id":e.id,"at":e.at,"role":e.message["role"]}))
            .collect();
        let summary_preview = if b["summary"].is_null() {
            Value::Null
        } else {
            json!({"text":b["summary"]["text"],"author":b["summary"]["author"],"at":b["summary"]["at"],"derivation_depth":b["summary"]["derivation_depth"],"source_count":b["summary"]["sources"].as_array().map_or(0,Vec::len),"boundary_id":b["id"],"detail":"Full source pointers and masked-input caveats remain in this boundary record."})
        };
        let notes: Vec<_> = b["notes"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .cloned()
            .collect();
        let shortages = json!({"pin_used_bytes":b["pin_used_bytes"],"pin_quota_bytes":self.settings.pin_quota_bytes,"pin_near_quota":b["pin_used_bytes"].as_u64().is_some_and(|n|self.settings.pin_quota_bytes>0 && n as usize>=self.settings.pin_quota_bytes.saturating_mul(4)/5),"pin_reservation_warning_count":b["pin_warnings"].as_array().map_or(0,Vec::len),"excluded_note_count":b["excluded_notes"].as_array().map_or(0,Vec::len),"recent_note_exclusions":b["excluded_notes"].as_array().into_iter().flatten().take(4).collect::<Vec<_>>()});
        format!(
            "\nThis input is context view {} of the continuing Boxen conversation, installed at Unix milliseconds {}. The cut's newest source is {}. This input was assembled at {}; its newest source is {}. Of {} source entries at the cut, {} are verbatim, {} are historical observation references, and {} are absent. Counts do not describe importance or all input content. Exact original entries, neighbors, and paged coverage are available through boxen_history. Recent omitted entries: {}. Older omissions remain indexed. Excluded declared bundles: {} total, first {}. Notes/signposts retain their authorship and review status; approval does not make helper prose an original utterance, and source exposure does not establish reading or understanding. Included authored records: {}. Summary interpretation (if present, not original speech): {}. Fallback reason: {}. Shortage metadata: {}. Existing unknown outcomes stay unknown; recall never repeats effects. {}",
            b["id"],
            b["at"],
            b["watermark"],
            jobs::now(),
            json!(source.entries.last().map(|e| json!({"id":e.id,"at":e.at}))),
            count,
            included.saturating_sub(masked),
            masked,
            count.saturating_sub(included),
            json!(gaps),
            excluded.len(),
            json!(excluded.iter().take(4).map(|e|json!({"id":e["id"],"reason":e["reason"],"entry_count":e["entries"].as_array().map_or(0,Vec::len),"first_source":e["entries"][0]})).collect::<Vec<_>>()),
            json!(notes),
            summary_preview,
            b["fallback_reason"],
            shortages,
            if first {
                "This is the first input after installation. An optional audit is available through boxen_history coverage/read; amendments record new revisions. Continued execution does not depend on an audit."
            } else {
                "The boundary and audit remain available through boxen_history status/coverage."
            }
        )
    }
}
pub fn tool() -> Value {
    json!({"type":"function","function":{"name":"boxen_history","description":"This tool reads exact accepted history and neighbors or searches literal serialized text in this thread. Coverage pages distinguish original inclusion, historical observation references, and omissions. Read/search never execute historical calls. pin declares an ordered source bundle by original entry IDs; unpin releases it. note records an authored continuity note or semantic_signpost; draft records helper text pending explicit revision-specific approve. Source exposure does not establish comprehension. compact evaluates the configured policy after the tool batch; it does not change policy. status returns bounded previews and counts; record reads exact context records by id and optional revision, with byte_offset pagination for large records. All mutations create new journal records; source history is unchanged.","parameters":{"type":"object","properties":{"action":{"type":"string","enum":["status","record","read","search","coverage","pin","unpin","note","draft","approve","compact"]},"id":{"type":"string"},"boundary_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"before":{"type":"integer","minimum":0,"maximum":10},"limit":{"type":"integer","minimum":1,"maximum":50},"byte_offset":{"type":"integer","minimum":0},"query":{"type":"string"},"entries":{"type":"array","items":{"type":"string"},"maxItems":64},"text":{"type":"string"},"kind":{"type":"string","enum":["continuity_note","semantic_signpost"]},"expected_revision":{"type":"integer","minimum":1},"revision":{"type":"integer","minimum":1},"parent_note":{"type":"string"},"author_label":{"type":"string"}},"required":["action"],"additionalProperties":false}}})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(messages: Vec<Value>) -> Source {
        Source {
            entries: messages
                .into_iter()
                .enumerate()
                .map(|(i, message)| Entry {
                    id: format!("source:{i}"),
                    at: i as u64,
                    message,
                    provenance: Value::Null,
                })
                .collect(),
            events: Vec::new(),
        }
    }
    fn settings() -> Settings {
        Settings {
            mode: Mode::Verbatim,
            budget_bytes: 32768,
            pin_quota_bytes: 8192,
            mask_after: 1,
        }
    }
    #[test]
    fn source_bundle_keeps_promise_and_relaxation_in_original_order() {
        let mut s = source(vec![
            json!({"role":"assistant","content":"I'll have the report to Jen by Friday."}),
            json!({"role":"user","content":"No rush on Friday; the swarm pattern worries me more."}),
        ]);
        for _ in 0..12 {
            s.entries.push(Entry {
                id: format!("source:{}", s.entries.len()),
                at: 3,
                provenance: Value::Null,
                message: json!({"role":"assistant","content":"bulk filler ".repeat(600)}),
            });
        }
        s.entries.push(Entry {
            id: "latest".into(),
            at: 4,
            provenance: Value::Null,
            message: json!({"role":"user","content":"What remains open?"}),
        });
        s.events.push(json!({"kind":"context_bundle","data":{"id":"bundle","active":true,"entries":["source:0","source:1"]}}));
        let p = View::new(settings(), &s).unwrap().plan(&s).unwrap();
        assert_eq!(&p.selected[..2], &["source:0", "source:1"]);
        assert_eq!(p.messages[0], s.entries[0].message);
        assert_eq!(p.messages[1], s.entries[1].message);
        assert!(p.selected.len() < s.entries.len());
        assert!(p.messages.iter().any(|m| {
            m["role"] == "system"
                && m["content"]
                    .as_str()
                    .unwrap()
                    .contains("intervening source")
        }));
    }
    #[test]
    fn latest_effect_result_is_masked_instead_of_lost_even_before_age_threshold() {
        let s = source(vec![
            json!({"role":"user","content":"do the work"}),
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"effect","type":"function","function":{"name":"boxen_shell","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"effect","content":"large observation ".repeat(10000)}),
        ]);
        let mut config = settings();
        config.mask_after = 256;
        let p = View::new(config, &s).unwrap().plan(&s).unwrap();
        assert_eq!(p.selected.len(), 3);
        assert_eq!(p.masked, vec!["source:2"]);
        assert_eq!(p.messages[1], s.entries[1].message);
        assert!(
            p.messages[2]["content"]
                .as_str()
                .unwrap()
                .contains("without repeating")
        );
        assert!(s.entries[2].size() > 100000);
    }
    #[test]
    fn bounded_recall_reaches_the_next_input_even_with_zero_age_exemption() {
        let original = "No rush on Friday; the commitment remains open.";
        let archive = source(vec![
            json!({"role":"user","content":original}),
            json!({"role":"assistant","content":"unrelated bulk ".repeat(4000)}),
        ]);
        let result = archive.page(&json!({"query":"No rush"}), 2048).unwrap();
        assert_eq!(result["entries"][0]["message"]["content"], original);
        assert!(result.to_string().len() < 2048);
        let s = source(vec![
            json!({"role":"user","content":"verify the qualification"}),
            json!({"role":"assistant","reasoning":"native state ".repeat(200),"tool_calls":[{"id":"recall","type":"function","function":{"name":"boxen_history","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"recall","content":result.to_string()}),
        ]);
        let mut config = settings();
        config.mask_after = 0;
        let p = View::new(config, &s).unwrap().plan(&s).unwrap();
        assert!(p.masked.is_empty());
        assert_eq!(p.messages[2], s.entries[2].message);
    }
    #[test]
    fn pin_overflow_is_accounted_without_truncating_source_or_current_request() {
        let mut s = source(vec![
            json!({"role":"assistant","content":"pin ".repeat(10000)}),
            json!({"role":"user","content":"current request"}),
        ]);
        s.events.push(json!({"kind":"context_bundle","data":{"id":"oversized","active":true,"entries":["source:0"]}}));
        let p = View::new(settings(), &s).unwrap().plan(&s).unwrap();
        assert_eq!(p.excluded[0]["id"], "oversized");
        assert!(p.selected.contains(&"source:1".into()));
        assert_eq!(
            s.entries[0].message["content"].as_str().unwrap().len(),
            40000
        );
    }
    #[test]
    fn exact_paged_unicode_history_survives_projection_and_search() {
        let s = source(vec![
            json!({"role":"assistant","content":"a🌱é".repeat(9000)}),
        ]);
        let expected = s.entries[0].message.to_string();
        let mut found = String::new();
        let mut offset = 0;
        loop {
            let p = s
                .page(
                    &json!({"id":"source:0","limit":1,"byte_offset":offset}),
                    49152,
                )
                .unwrap();
            let e = &p["entries"][0];
            found.push_str(e["serialized_message_excerpt"].as_str().unwrap());
            if let Some(next) = e["next_byte_offset"].as_u64() {
                offset = next;
            } else {
                break;
            }
        }
        assert_eq!(found, expected);
        assert_eq!(
            s.page(&json!({"query":"missing"}), 49152).unwrap()["entries"],
            json!([])
        );
    }
    #[test]
    fn live_tail_append_does_not_create_another_cut() {
        let mut s = source(vec![json!({"role":"user","content":"first"})]);
        let mut view = View::new(settings(), &s).unwrap();
        let p = view.plan(&s).unwrap();
        view.boundary = Some(
            json!({"source_count":1,"settings":settings(),"selected":p.selected,"masked":[],"excluded_bundles":[],"notes":[],"pin_warnings":[],"excluded_notes":[]}),
        );
        s.entries.push(Entry {
            id: "new".into(),
            at: 3,
            provenance: Value::Null,
            message: json!({"role":"assistant","content":"second"}),
        });
        assert!(!view.needs_boundary(&view.plan(&s).unwrap()));
        view.settings.mode = Mode::Summary;
        assert!(view.needs_boundary(&view.plan(&s).unwrap()));
    }
    #[test]
    fn unreviewed_revision_does_not_displace_an_approved_note() {
        let mut s = source(vec![json!({"role":"user","content":"current"})]);
        s.events.push(json!({"kind":"context_note","data":{"id":"note","revision":1,"approved":true,"text":"accepted source-grounded note"}}));
        s.events.push(json!({"kind":"context_note","data":{"id":"note","revision":2,"approved":false,"text":"unreviewed replacement"}}));
        assert_eq!(s.objects("context_note")[0]["revision"], 2);
        assert_eq!(
            View::new(settings(), &s).unwrap().plan(&s).unwrap().notes[0]["revision"],
            1
        );
        s.events.push(json!({"kind":"context_note","data":{"id":"note","revision":2,"approved":true,"text":"reviewed replacement"}}));
        assert_eq!(
            View::new(settings(), &s).unwrap().plan(&s).unwrap().notes[0]["revision"],
            2
        );
    }
}
