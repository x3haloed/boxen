//! Read-only discovery and turn-oriented transcript projection for non-resident threads.
//! Resident conversations are strictly excluded from listing, search, and reads.
use crate::{
    Result,
    jobs::{Jobs, Run, Snapshot},
    logging::{self, Context, Level, Target},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

const READ_BUDGET_BYTES: usize = 24 * 1024;
const SEARCH_BUDGET_BYTES: usize = 24 * 1024;
const LIST_BUDGET_BYTES: usize = 24 * 1024;

pub(crate) fn agent_continuity_enabled() -> bool {
    std::env::var_os("BOXEN_EXPERIMENTAL_THREAD_CONTINUITY").is_some_and(|value| value == "1")
}

fn clip(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

fn clip_preview(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let (s, truncated) = clip(trimmed, limit);
    if truncated { format!("{s}…") } else { s }
}

/// Convert Unix epoch milliseconds to an ISO-8601 UTC timestamp (`YYYY-MM-DD HH:MM UTC`)
/// without external date crates (portable across native and APE builds).
fn format_utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    // Howard Hinnant's civil-from-days algorithm
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {hour:02}:{minute:02} UTC")
}

fn format_age(now_ms: u64, ts_ms: u64) -> String {
    let diff_s = now_ms.saturating_sub(ts_ms) / 1000;
    if diff_s < 60 {
        return "just now".into();
    }
    let mins = diff_s / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 21 {
        return format!("{days}d ago");
    }
    let weeks = days / 7;
    format!("{weeks}w ago")
}

/// Returns true if `thread_id` is registered as a resident OR if any retained run
/// in that thread carries a resident attachment or resident_id.
pub fn is_resident_thread(jobs: &Jobs, thread_id: &str) -> bool {
    if jobs.residents.all().iter().any(|r| r.id == thread_id) {
        return true;
    }
    jobs.operator_list().into_iter().any(|s| {
        s.record.spec.thread_id == thread_id
            && (s.record.resident.is_some() || s.record.spec.resident_id.is_some())
    })
}

fn excluded_resident_threads(jobs: &Jobs, snapshots: &[Snapshot]) -> HashSet<String> {
    let mut excluded: HashSet<String> = jobs.residents.all().into_iter().map(|r| r.id).collect();
    for s in snapshots {
        if s.record.resident.is_some() || s.record.spec.resident_id.is_some() {
            excluded.insert(s.record.spec.thread_id.clone());
        }
    }
    excluded
}

#[derive(Clone, Debug)]
struct ExtractedTool {
    name: String,
    command: Option<String>,
    status: Option<String>,
    output: String,
}

#[derive(Clone, Debug)]
struct ExtractedTurn {
    turn_index: usize,
    run_id: String,
    created_at: u64,
    backend: String,
    model: String,
    status: String,
    status_detail: Option<String>,
    recovery: Option<Value>,
    user: String,
    steers: Vec<String>,
    attachments: Vec<String>,
    assistant: String,
    tools: Vec<ExtractedTool>,
}

fn extract_run_turns(run: &Arc<Run>, start_index: usize) -> Result<Vec<ExtractedTurn>> {
    let snapshot = run.snapshot();
    let record = &snapshot.record;
    let spec = &record.spec;
    let (events, _) = run.batch(0, false)?;

    // Imported Codex threads store multiple historical turns inside a single run.
    if record.imported.is_some() {
        let mut turns = Vec::new();
        let mut cur_user = String::new();
        let mut cur_assistant = Vec::new();
        let mut cur_tools = Vec::new();
        let mut has_any = false;
        let flush = |turns: &mut Vec<ExtractedTurn>,
                     cur_user: &mut String,
                     cur_assistant: &mut Vec<String>,
                     cur_tools: &mut Vec<ExtractedTool>| {
            if cur_user.is_empty() && cur_assistant.is_empty() && cur_tools.is_empty() {
                return;
            }
            let idx = start_index + turns.len();
            turns.push(ExtractedTurn {
                turn_index: idx,
                run_id: spec.id.clone(),
                created_at: record.created_at,
                backend: spec.backend.clone(),
                model: spec.model.clone(),
                status: snapshot.status.clone(),
                status_detail: None,
                recovery: record.recovery.clone(),
                user: std::mem::take(cur_user),
                steers: Vec::new(),
                attachments: Vec::new(),
                assistant: std::mem::take(cur_assistant).join("\n\n"),
                tools: std::mem::take(cur_tools),
            });
        };
        for ev in &events {
            if ev.kind == "user_message" {
                if has_any {
                    flush(
                        &mut turns,
                        &mut cur_user,
                        &mut cur_assistant,
                        &mut cur_tools,
                    );
                }
                has_any = true;
                if let Some(t) = ev.data["text"].as_str() {
                    cur_user = t.to_owned();
                }
            } else if ev.kind == "backend" && ev.data["method"] == "item/completed" {
                let item = &ev.data["params"]["item"];
                match item["type"].as_str() {
                    Some("agentMessage") => {
                        if let Some(t) = item["text"].as_str().filter(|s| !s.trim().is_empty()) {
                            cur_assistant.push(t.to_owned());
                        }
                    }
                    Some("commandExecution") => {
                        cur_tools.push(ExtractedTool {
                            name: "commandExecution".into(),
                            command: item["command"].as_str().map(str::to_owned),
                            status: item["status"].as_str().map(str::to_owned),
                            output: item["aggregatedOutput"].as_str().unwrap_or("").to_owned(),
                        });
                    }
                    _ => {}
                }
            }
        }
        flush(
            &mut turns,
            &mut cur_user,
            &mut cur_assistant,
            &mut cur_tools,
        );
        if !turns.is_empty() {
            return Ok(turns);
        }
    }

    let mut steers = Vec::new();
    let mut final_answers = Vec::new();
    let mut commentary = Vec::new();
    let mut tools = Vec::new();
    let mut pending_tools: HashMap<String, usize> = HashMap::new();
    let mut status_detail = None;

    for ev in &events {
        match ev.kind.as_str() {
            "status" => {
                if let Some(detail) = ev.data["detail"].as_str() {
                    status_detail = Some(detail.to_owned());
                }
            }
            "control" => {
                let kind = ev.data["type"].as_str().unwrap_or("");
                if matches!(kind, "steer" | "steer_queued")
                    && let Some(t) = ev.data["text"]
                        .as_str()
                        .or_else(|| ev.data["message"].as_str())
                        .filter(|s| !s.trim().is_empty())
                {
                    steers.push(t.to_owned());
                }
            }
            "assistant_message" => {
                if let Some(t) = ev.data["text"].as_str().filter(|s| !s.trim().is_empty()) {
                    final_answers.push(t.to_owned());
                }
            }
            "tool_started" => {
                let name = ev.data["name"].as_str().unwrap_or("tool").to_owned();
                let command = ev.data["input"]["command"]
                    .as_str()
                    .or_else(|| ev.data["command"].as_str())
                    .map(str::to_owned);
                let idx = tools.len();
                tools.push(ExtractedTool {
                    name,
                    command,
                    status: None,
                    output: String::new(),
                });
                if let Some(cid) = ev.data["call_id"].as_str() {
                    pending_tools.insert(cid.to_owned(), idx);
                }
            }
            "tool_result" => {
                let text = ev.data["text"]
                    .as_str()
                    .or_else(|| ev.data["output"].as_str())
                    .unwrap_or("")
                    .to_owned();
                if let Some(cid) = ev.data["call_id"].as_str()
                    && let Some(&idx) = pending_tools.get(cid)
                {
                    tools[idx].output = text;
                    tools[idx].status = ev.data["status"].as_str().map(str::to_owned);
                } else {
                    tools.push(ExtractedTool {
                        name: ev.data["name"].as_str().unwrap_or("tool").to_owned(),
                        command: None,
                        status: ev.data["status"].as_str().map(str::to_owned),
                        output: text,
                    });
                }
            }
            "backend" if ev.data["method"] == "item/completed" => {
                let item = &ev.data["params"]["item"];
                match item["type"].as_str() {
                    Some("agentMessage") => {
                        if let Some(t) = item["text"].as_str().filter(|s| !s.trim().is_empty()) {
                            let phase = item["phase"].as_str();
                            if phase.is_none() || phase == Some("final_answer") {
                                final_answers.push(t.to_owned());
                            } else {
                                commentary.push(t.to_owned());
                            }
                        }
                    }
                    Some("commandExecution") => {
                        tools.push(ExtractedTool {
                            name: "commandExecution".into(),
                            command: item["command"].as_str().map(str::to_owned),
                            status: item["status"].as_str().map(str::to_owned),
                            output: item["aggregatedOutput"].as_str().unwrap_or("").to_owned(),
                        });
                    }
                    Some("fileChange") => {
                        let summary = item["changes"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| c["path"].as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        tools.push(ExtractedTool {
                            name: "fileChange".into(),
                            command: (!summary.is_empty()).then_some(summary),
                            status: item["status"].as_str().map(str::to_owned),
                            output: String::new(),
                        });
                    }
                    Some("mcpToolCall" | "dynamicToolCall") => {
                        let tname = item["tool"]
                            .as_str()
                            .or_else(|| item["name"].as_str())
                            .unwrap_or("mcpToolCall")
                            .to_owned();
                        // Avoid echoing recursive boxen_threads reads into noise
                        if tname != "boxen_threads" {
                            tools.push(ExtractedTool {
                                name: tname,
                                command: None,
                                status: item["status"].as_str().map(str::to_owned),
                                output: item["result"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_default(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let assistant = if !final_answers.is_empty() {
        final_answers.join("\n\n")
    } else {
        commentary.join("\n\n")
    };
    let attachments = spec.attachments.iter().map(|a| a.name.clone()).collect();
    let keep_detail = snapshot.status != "completed";

    Ok(vec![ExtractedTurn {
        turn_index: start_index,
        run_id: spec.id.clone(),
        created_at: record.created_at,
        backend: spec.backend.clone(),
        model: spec.model.clone(),
        status: snapshot.status.clone(),
        status_detail: if keep_detail { status_detail } else { None },
        recovery: record.recovery.clone(),
        user: spec.prompt.clone(),
        steers,
        attachments,
        assistant,
        tools,
    }])
}

struct ThreadGroup {
    thread_id: String,
    runs: Vec<Snapshot>,
    started_at: u64,
    updated_at: u64,
    cwd: String,
    backends: Vec<String>,
    latest_status: String,
}

fn collect_eligible_threads(
    jobs: &Jobs,
    exclude_thread: Option<&str>,
    since_hours: Option<f64>,
    until_hours: Option<f64>,
    cwd_filter: Option<&str>,
    now_ms: u64,
) -> Vec<ThreadGroup> {
    let all = jobs.operator_list();
    let excluded = excluded_resident_threads(jobs, &all);
    let mut grouped: HashMap<String, Vec<Snapshot>> = HashMap::new();
    for snap in all {
        let tid = &snap.record.spec.thread_id;
        if excluded.contains(tid) {
            continue;
        }
        if exclude_thread == Some(tid.as_str()) {
            continue;
        }
        grouped.entry(tid.clone()).or_default().push(snap);
    }

    let min_ts = since_hours.map(|h| now_ms.saturating_sub((h.max(0.0) * 3_600_000.0) as u64));
    let max_ts = until_hours.map(|h| now_ms.saturating_sub((h.max(0.0) * 3_600_000.0) as u64));
    let cwd_low = cwd_filter.map(|s| s.to_ascii_lowercase());

    let mut threads = Vec::new();
    for (thread_id, mut runs) in grouped {
        runs.sort_by_key(|s| (s.record.created_at, s.record.spec.id.clone()));
        let started_at = runs.first().map_or(0, |s| s.record.created_at);
        let updated_at = runs.last().map_or(0, |s| s.record.created_at);
        if let Some(min) = min_ts
            && updated_at < min
        {
            continue;
        }
        if let Some(max) = max_ts
            && started_at > max
        {
            continue;
        }
        let cwd = runs
            .last()
            .map(|s| s.record.spec.cwd.clone())
            .unwrap_or_default();
        if let Some(filter) = &cwd_low
            && !cwd.to_ascii_lowercase().contains(filter)
        {
            continue;
        }
        let mut backends = Vec::new();
        for r in &runs {
            if !backends.contains(&r.record.spec.backend) {
                backends.push(r.record.spec.backend.clone());
            }
        }
        let latest_status = runs
            .last()
            .map(|s| s.status.clone())
            .unwrap_or_else(|| "unknown".into());
        threads.push(ThreadGroup {
            thread_id,
            runs,
            started_at,
            updated_at,
            cwd,
            backends,
            latest_status,
        });
    }
    // Newest activity first
    threads.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.thread_id.cmp(&b.thread_id))
    });
    threads
}

fn load_thread_turns(jobs: &Jobs, group: &ThreadGroup) -> Vec<ExtractedTurn> {
    let mut turns = Vec::new();
    for snap in &group.runs {
        if let Ok(run) = jobs.operator_run(&snap.record.spec.id)
            && let Ok(mut extracted) = extract_run_turns(&run, turns.len())
        {
            turns.append(&mut extracted);
        }
    }
    turns
}

fn stem_token(token: &str) -> String {
    let t = token.to_ascii_lowercase();
    if t.len() <= 3 {
        return t;
    }
    for suffix in ["ing", "ied", "ies", "ed", "es", "s"] {
        if let Some(stem) = t.strip_suffix(suffix)
            && stem.len() >= 3
        {
            if suffix == "ies" || suffix == "ied" {
                return format!("{stem}y");
            }
            return stem.to_owned();
        }
    }
    t
}

fn is_stop_word(w: &str) -> bool {
    matches!(
        w,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "been"
            | "but"
            | "by"
            | "can"
            | "could"
            | "did"
            | "do"
            | "does"
            | "for"
            | "from"
            | "had"
            | "has"
            | "have"
            | "how"
            | "i"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "just"
            | "me"
            | "my"
            | "of"
            | "oh"
            | "on"
            | "or"
            | "our"
            | "project"
            | "remind"
            | "right"
            | "should"
            | "so"
            | "some"
            | "something"
            | "talking"
            | "that"
            | "the"
            | "their"
            | "them"
            | "there"
            | "these"
            | "they"
            | "thing"
            | "this"
            | "those"
            | "thread"
            | "to"
            | "us"
            | "was"
            | "we"
            | "were"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "with"
            | "would"
            | "you"
            | "your"
            | "about"
            | "discussed"
    )
}

/// Tokenize text into normalized stems and exact compound identifiers (preserving hyphens/underscores/dots).
fn tokenize(text: &str, filter_stops: bool) -> Vec<String> {
    let mut raw_tokens = Vec::new();
    let lower = text.to_ascii_lowercase();
    for piece in lower.split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
    {
        let trimmed = piece.trim_matches(|c: char| matches!(c, '-' | '_' | '.'));
        if trimmed.is_empty() {
            continue;
        }
        raw_tokens.push(trimmed.to_owned());
        if trimmed.contains(['-', '_', '.']) {
            for sub in trimmed.split(['-', '_', '.']) {
                if !sub.is_empty() {
                    raw_tokens.push(sub.to_owned());
                }
            }
        }
    }
    let filtered: Vec<String> = raw_tokens
        .iter()
        .filter(|t| !filter_stops || !is_stop_word(t))
        .map(|t| stem_token(t))
        .collect();
    if filtered.is_empty() && filter_stops {
        raw_tokens.into_iter().map(|t| stem_token(&t)).collect()
    } else {
        filtered
    }
}

fn excerpt_around(text: &str, query_terms: &[String], max_bytes: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_owned();
    }
    let lower = trimmed.to_ascii_lowercase();
    let mut best_pos = None;
    for term in query_terms {
        if let Some(pos) = lower.find(term.as_str()) {
            best_pos = Some(best_pos.map_or(pos, |old: usize| old.min(pos)));
        }
    }
    let anchor = best_pos.unwrap_or(0);
    let mut start = anchor.saturating_sub(max_bytes / 3);
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    // Snap to a word/line boundary if reasonable
    if start > 0
        && let Some(ws) = trimmed[start..anchor.min(trimmed.len())].find(char::is_whitespace)
    {
        start += ws + 1;
    }
    let mut end = (start + max_bytes).min(trimmed.len());
    while end > start && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < trimmed.len() { "…" } else { "" };
    format!("{prefix}{}{suffix}", trimmed[start..end].trim())
}

fn list_action(jobs: &Jobs, args: &Value, exclude_thread: Option<&str>) -> Result<Value> {
    let now_ms = crate::jobs::now();
    let since_hours = args["since_hours"].as_f64();
    let until_hours = args["until_hours"].as_f64();
    let cwd_filter = args["cwd"].as_str().filter(|s| !s.is_empty());
    let limit = args["limit"].as_u64().unwrap_or(15).clamp(1, 50) as usize;
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let budget = args["max_bytes"]
        .as_u64()
        .map(|v| (v as usize).clamp(4096, LIST_BUDGET_BYTES))
        .unwrap_or(LIST_BUDGET_BYTES);

    let threads = collect_eligible_threads(
        jobs,
        exclude_thread,
        since_hours,
        until_hours,
        cwd_filter,
        now_ms,
    );
    let total = threads.len();
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut clipped = false;

    for group in threads.iter().skip(offset).take(limit) {
        let first_prompt = group
            .runs
            .first()
            .map(|s| clip_preview(&s.record.spec.prompt, 200))
            .unwrap_or_default();
        let latest_prompt = (group.runs.len() > 1)
            .then(|| {
                group
                    .runs
                    .last()
                    .map(|s| clip_preview(&s.record.spec.prompt, 200))
            })
            .flatten();
        let latest_response = group
            .runs
            .last()
            .and_then(|snap| jobs.operator_run(&snap.record.spec.id).ok())
            .and_then(|run| extract_run_turns(&run, 0).ok())
            .and_then(|turns| turns.into_iter().last())
            .map(|t| clip_preview(&t.assistant, 240))
            .filter(|s| !s.is_empty());

        let mut item = json!({
            "thread_id": group.thread_id,
            "run_count": group.runs.len(),
            "latest_status": group.latest_status,
            "backends": group.backends,
            "cwd": group.cwd,
            "started_at": format_utc(group.started_at),
            "updated_at": format_utc(group.updated_at),
            "age_human": format_age(now_ms, group.updated_at),
            "opening_prompt": first_prompt,
        });
        if let Some(lp) = latest_prompt {
            item["latest_prompt"] = json!(lp);
        }
        if let Some(lr) = latest_response {
            item["latest_response_preview"] = json!(lr);
        }
        let size = item.to_string().len();
        if bytes + size > budget && !rows.is_empty() {
            clipped = true;
            break;
        }
        bytes += size;
        rows.push(item);
    }

    let next_offset = offset + rows.len();
    let has_more = next_offset < total;
    Ok(json!({
        "action": "list",
        "scope": "non_resident_threads",
        "now_utc": format_utc(now_ms),
        "total": total,
        "offset": offset,
        "returned": rows.len(),
        "has_more": has_more,
        "next_offset": has_more.then_some(next_offset),
        "clipped_by_budget": clipped,
        "threads": rows,
    }))
}

fn search_action(jobs: &Jobs, args: &Value, exclude_thread: Option<&str>) -> Result<Value> {
    let query = args["query"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Search requires a non-empty query string")?;
    if query.len() > 2048 {
        return Err("Search query exceeds 2048 bytes".into());
    }
    let now_ms = crate::jobs::now();
    let since_hours = args["since_hours"].as_f64();
    let until_hours = args["until_hours"].as_f64();
    let cwd_filter = args["cwd"].as_str().filter(|s| !s.is_empty());
    let limit = args["limit"].as_u64().unwrap_or(8).clamp(1, 20) as usize;
    let budget = args["max_bytes"]
        .as_u64()
        .map(|v| (v as usize).clamp(4096, SEARCH_BUDGET_BYTES))
        .unwrap_or(SEARCH_BUDGET_BYTES);

    let query_terms_vec = tokenize(query, true);
    let mut query_terms: Vec<String> = Vec::new();
    for t in query_terms_vec {
        if !query_terms.contains(&t) {
            query_terms.push(t);
        }
    }
    if query_terms.is_empty() {
        return Err("Search query contains no searchable terms".into());
    }
    let phrase_low = query.to_ascii_lowercase();

    let threads = collect_eligible_threads(
        jobs,
        exclude_thread,
        since_hours,
        until_hours,
        cwd_filter,
        now_ms,
    );
    let total_corpus = threads.len();

    // Load turns and compute document frequency per query term
    struct Doc<'a> {
        group: &'a ThreadGroup,
        turns: Vec<ExtractedTurn>,
        conv_tf: HashMap<String, usize>,
        tool_tf: HashMap<String, usize>,
        conv_len: usize,
        has_phrase: bool,
    }

    let mut docs = Vec::with_capacity(threads.len());
    let mut df: HashMap<String, usize> = HashMap::new();

    for group in &threads {
        let turns = load_thread_turns(jobs, group);
        let mut conv_tf: HashMap<String, usize> = HashMap::new();
        let mut tool_tf: HashMap<String, usize> = HashMap::new();
        let mut conv_len = 0usize;
        let mut has_phrase = false;
        let mut seen_in_doc: HashSet<String> = HashSet::new();

        for turn in &turns {
            if !has_phrase
                && (turn.user.to_ascii_lowercase().contains(&phrase_low)
                    || turn.assistant.to_ascii_lowercase().contains(&phrase_low))
            {
                has_phrase = true;
            }
            for text in std::iter::once(turn.user.as_str())
                .chain(turn.steers.iter().map(String::as_str))
                .chain(std::iter::once(turn.assistant.as_str()))
                .chain(turn.attachments.iter().map(String::as_str))
            {
                let toks = tokenize(text, false);
                conv_len += toks.len();
                for tok in toks {
                    if query_terms.contains(&tok) {
                        *conv_tf.entry(tok.clone()).or_default() += 1;
                        seen_in_doc.insert(tok);
                    }
                }
            }
            for tool in &turn.tools {
                let mut tool_text = tool.name.clone();
                if let Some(cmd) = &tool.command {
                    tool_text.push(' ');
                    tool_text.push_str(cmd);
                }
                // Cap tool output scanned per call so bulky logs don't dominate
                let (preview, _) = clip(&tool.output, 2048);
                tool_text.push(' ');
                tool_text.push_str(&preview);
                for tok in tokenize(&tool_text, false) {
                    if query_terms.contains(&tok) {
                        *tool_tf.entry(tok.clone()).or_default() += 1;
                        seen_in_doc.insert(tok);
                    }
                }
            }
        }
        for tok in seen_in_doc {
            *df.entry(tok).or_default() += 1;
        }
        docs.push(Doc {
            group,
            turns,
            conv_tf,
            tool_tf,
            conv_len,
            has_phrase,
        });
    }

    let n_docs = (docs.len() as f64).max(1.0);
    let avg_len = (docs.iter().map(|d| d.conv_len).sum::<usize>() as f64 / n_docs).max(20.0);

    let mut scored: Vec<(f64, Vec<String>, &Doc<'_>)> = Vec::new();
    for doc in &docs {
        let mut score = 0.0_f64;
        let mut matched_terms = Vec::new();
        for term in &query_terms {
            let tf_conv = *doc.conv_tf.get(term).unwrap_or(&0) as f64;
            let tf_tool = (*doc.tool_tf.get(term).unwrap_or(&0) as f64).min(3.0) * 0.35;
            let tf = tf_conv + tf_tool;
            if tf <= 0.0 {
                continue;
            }
            matched_terms.push(term.clone());
            let doc_freq = *df.get(term).unwrap_or(&1) as f64;
            let idf = ((n_docs - doc_freq + 0.5) / (doc_freq + 0.5) + 1.0)
                .ln()
                .max(0.2);
            let k1 = 1.2;
            let b = 0.5;
            let denom = tf + k1 * (1.0 - b + b * (doc.conv_len as f64 / avg_len));
            score += idf * ((tf * (k1 + 1.0)) / denom);
        }
        if matched_terms.is_empty() {
            continue;
        }
        // Strong multi-term coverage bonus so specific matches outrank partial near-misses
        let coverage = matched_terms.len() as f64 / query_terms.len() as f64;
        score *= 0.6 + 0.9 * coverage;
        if doc.has_phrase {
            score += 2.0;
        }
        // Mild recency tie-breaker (bounded within +0.25 so topical precision always dominates)
        let age_days = (now_ms.saturating_sub(doc.group.updated_at) as f64) / 86_400_000.0;
        score += 0.25 / (1.0 + age_days / 7.0);
        scored.push((score, matched_terms, doc));
    }

    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| b.2.group.updated_at.cmp(&a.2.group.updated_at))
    });

    let total_matches = scored.len();
    let mut rows = Vec::new();
    let mut bytes = 0usize;

    for (score, matched_terms, doc) in scored.into_iter().take(limit) {
        // Pick up to 3 best-matching turn excerpts (favoring later turns on ties)
        let mut turn_hits: Vec<(usize, usize, String, String, u64)> = Vec::new();
        for turn in &doc.turns {
            for (speaker, text) in [
                ("user", turn.user.as_str()),
                ("assistant", turn.assistant.as_str()),
            ] {
                if text.trim().is_empty() {
                    continue;
                }
                let toks: HashSet<String> = tokenize(text, false).into_iter().collect();
                let hits = query_terms.iter().filter(|q| toks.contains(*q)).count();
                if hits > 0 {
                    turn_hits.push((
                        hits,
                        turn.turn_index,
                        speaker.into(),
                        excerpt_around(text, &matched_terms, 320),
                        turn.created_at,
                    ));
                }
            }
            if let Some(tool) = turn.tools.iter().find(|t| {
                let s = format!("{} {}", t.command.as_deref().unwrap_or(""), t.output);
                let toks: HashSet<String> = tokenize(&s, false).into_iter().collect();
                query_terms.iter().any(|q| toks.contains(q))
            }) {
                let s = format!(
                    "{}: {}",
                    tool.command.as_deref().unwrap_or(&tool.name),
                    tool.output
                );
                turn_hits.push((
                    1,
                    turn.turn_index,
                    "tool".into(),
                    excerpt_around(&s, &matched_terms, 240),
                    turn.created_at,
                ));
            }
        }
        turn_hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        let snippets: Vec<Value> = turn_hits
            .into_iter()
            .take(3)
            .map(|(_, idx, speaker, excerpt, ts)| {
                json!({
                    "turn_index": idx,
                    "speaker": speaker,
                    "age_human": format_age(now_ms, ts),
                    "excerpt": excerpt,
                })
            })
            .collect();

        let latest_preview = doc
            .turns
            .last()
            .map(|t| {
                if !t.assistant.trim().is_empty() {
                    clip_preview(&t.assistant, 240)
                } else if let Some(detail) = &t.status_detail {
                    format!("[status: {} — {}]", t.status, clip_preview(detail, 180))
                } else {
                    clip_preview(&t.user, 200)
                }
            })
            .unwrap_or_default();

        let item = json!({
            "thread_id": doc.group.thread_id,
            "score": (score * 100.0).round() / 100.0,
            "matched_terms": matched_terms,
            "run_count": doc.group.runs.len(),
            "turn_count": doc.turns.len(),
            "latest_status": doc.group.latest_status,
            "started_at": format_utc(doc.group.started_at),
            "updated_at": format_utc(doc.group.updated_at),
            "age_human": format_age(now_ms, doc.group.updated_at),
            "cwd": doc.group.cwd,
            "opening_prompt": doc.turns.first().map(|t| clip_preview(&t.user, 160)).unwrap_or_default(),
            "latest_turn_preview": latest_preview,
            "matches": snippets,
        });
        let size = item.to_string().len();
        if bytes + size > budget && !rows.is_empty() {
            break;
        }
        bytes += size;
        rows.push(item);
    }

    Ok(json!({
        "action": "search",
        "scope": "non_resident_threads",
        "now_utc": format_utc(now_ms),
        "query": query,
        "searched_threads": total_corpus,
        "total_matches": total_matches,
        "returned": rows.len(),
        "results": rows,
    }))
}

fn render_turn(turn: &ExtractedTurn, now_ms: u64, include_tool_output: bool) -> Value {
    let tool_limit = if include_tool_output { 4096 } else { 280 };
    let mut obj = json!({
        "turn_index": turn.turn_index,
        "run_id": turn.run_id,
        "created_at": format_utc(turn.created_at),
        "age_human": format_age(now_ms, turn.created_at),
        "backend": turn.backend,
        "status": turn.status,
        "user": turn.user,
    });
    if !turn.model.is_empty() {
        obj["model"] = json!(turn.model);
    }
    if let Some(detail) = &turn.status_detail {
        obj["status_detail"] = json!(detail);
    }
    if let Some(recovery) = &turn.recovery {
        obj["recovery_notice"] = recovery.clone();
    }
    if !turn.steers.is_empty() {
        obj["steering"] = json!(turn.steers);
    }
    if !turn.attachments.is_empty() {
        obj["attachments"] = json!(turn.attachments);
    }
    if !turn.assistant.trim().is_empty() {
        obj["assistant"] = json!(turn.assistant);
    }
    if !turn.tools.is_empty() {
        let tools: Vec<Value> = turn
            .tools
            .iter()
            .take(16)
            .map(|t| {
                let mut row = json!({"name": t.name});
                if let Some(cmd) = &t.command {
                    row["command"] = json!(clip_preview(cmd, 240));
                }
                if let Some(st) = &t.status {
                    row["status"] = json!(st);
                }
                if !t.output.trim().is_empty() {
                    let (out, truncated) = clip(t.output.trim(), tool_limit);
                    row["output"] = json!(out);
                    if truncated {
                        row["output_truncated"] = json!(true);
                        row["output_total_bytes"] = json!(t.output.len());
                    }
                }
                row
            })
            .collect();
        obj["tools"] = json!(tools);
        if turn.tools.len() > 16 {
            obj["omitted_tool_count"] = json!(turn.tools.len() - 16);
        }
    }
    obj
}

fn read_action(jobs: &Jobs, args: &Value) -> Result<Value> {
    let thread_id = args["thread_id"]
        .as_str()
        .ok_or("Read requires a non-resident thread_id")?;
    crate::jobs::valid_id(thread_id)?;

    if is_resident_thread(jobs, thread_id) {
        return Err(
            "Resident conversations are excluded from boxen_threads; only non-resident threads are readable through this tool"
                .into(),
        );
    }

    let now_ms = crate::jobs::now();
    let include_tool_output = args["include_tool_output"].as_bool().unwrap_or(false);
    let order = args["order"].as_str().unwrap_or("asc");
    if !matches!(order, "asc" | "desc") {
        return Err("order accepts asc or desc".into());
    }
    let explicit_start = args["start_turn"].as_u64().map(|v| v as usize);
    let limit = args["limit"].as_u64().unwrap_or(40).clamp(1, 100) as usize;
    let budget = args["max_bytes"]
        .as_u64()
        .map(|v| (v as usize).clamp(4096, READ_BUDGET_BYTES))
        .unwrap_or(READ_BUDGET_BYTES);

    let mut runs: Vec<Snapshot> = jobs
        .operator_list()
        .into_iter()
        .filter(|s| s.record.spec.thread_id == thread_id)
        .collect();
    if runs.is_empty() {
        return Err("This non-resident thread was not found on the selected machine".into());
    }
    runs.sort_by_key(|s| (s.record.created_at, s.record.spec.id.clone()));
    let started_at = runs.first().map_or(0, |s| s.record.created_at);
    let updated_at = runs.last().map_or(0, |s| s.record.created_at);
    let cwd = runs
        .last()
        .map(|s| s.record.spec.cwd.clone())
        .unwrap_or_default();
    let latest_status = runs
        .last()
        .map(|s| s.status.clone())
        .unwrap_or_else(|| "unknown".into());
    let group = ThreadGroup {
        thread_id: thread_id.to_owned(),
        runs,
        started_at,
        updated_at,
        cwd: cwd.clone(),
        backends: Vec::new(),
        latest_status: latest_status.clone(),
    };
    let all_turns = load_thread_turns(jobs, &group);
    let total_turns = all_turns.len();
    let rendered: Vec<Value> = all_turns
        .iter()
        .map(|t| render_turn(t, now_ms, include_tool_output))
        .collect();

    let mut selected_indices: Vec<usize> = Vec::new();
    let mut omitted_middle: Option<Value> = None;

    if explicit_start.is_none() && order == "asc" {
        // Default whole-thread view: try to fit all turns; if over budget, preserve
        // turn 0 (opening intent) plus as many latest turns as fit so late reversals
        // and final statuses are always visible on the first read.
        let mut total_bytes = 0usize;
        let fits_all = rendered.len() <= limit
            && rendered.iter().all(|v| {
                total_bytes += v.to_string().len();
                total_bytes <= budget
            });
        if fits_all {
            selected_indices = (0..rendered.len()).collect();
        } else if !rendered.is_empty() {
            let mut used = rendered[0].to_string().len();
            let mut tail = Vec::new();
            for i in (1..rendered.len()).rev() {
                if 1 + tail.len() >= limit {
                    break;
                }
                let sz = rendered[i].to_string().len();
                if used + sz > budget && !tail.is_empty() {
                    break;
                }
                used += sz;
                tail.push(i);
            }
            tail.reverse();
            selected_indices.push(0);
            if let Some(&first_tail) = tail.first()
                && first_tail > 1
            {
                omitted_middle = Some(json!({
                    "from_turn": 1,
                    "to_turn_exclusive": first_tail,
                    "count": first_tail - 1,
                    "detail": "Middle turns were omitted to fit the response budget while preserving the opening turn and latest turns; start_turn and limit read omitted ranges."
                }));
            }
            selected_indices.extend(tail);
        }
    } else {
        let mut order_indices: Vec<usize> = (0..rendered.len()).collect();
        if order == "desc" {
            order_indices.reverse();
        }
        let start = explicit_start.unwrap_or(0);
        let mut used = 0usize;
        for idx in order_indices.into_iter().skip(start).take(limit) {
            let sz = rendered[idx].to_string().len();
            if used + sz > budget && !selected_indices.is_empty() {
                break;
            }
            used += sz;
            selected_indices.push(idx);
        }
    }

    let turns_out: Vec<Value> = selected_indices
        .iter()
        .map(|&i| rendered[i].clone())
        .collect();
    let has_more = turns_out.len() < total_turns;

    let mut result = json!({
        "action": "read",
        "scope": "non_resident_threads",
        "now_utc": format_utc(now_ms),
        "thread_id": thread_id,
        "cwd": cwd,
        "started_at": format_utc(started_at),
        "updated_at": format_utc(updated_at),
        "age_human": format_age(now_ms, updated_at),
        "latest_status": latest_status,
        "total_turns": total_turns,
        "returned_turns": turns_out.len(),
        "has_more": has_more,
        "turns": turns_out,
    });
    if let Some(gap) = omitted_middle {
        result["omitted_middle"] = gap;
    }
    if let Some(placement) = jobs.placement_summary(thread_id) {
        result["placement"] = placement;
    }
    Ok(result)
}

/// Authenticated operator entry point (`op: "thread_history"`).
pub fn operation(jobs: &Jobs, actor: &str, request: &Value) -> Result<Value> {
    let args = request.get("args").unwrap_or(request);
    let exclude = args["exclude_thread_id"].as_str();
    let action = args["action"].as_str().unwrap_or("");
    let result = match action {
        "list" => list_action(jobs, args, exclude),
        "search" => search_action(jobs, args, exclude),
        "read" => read_action(jobs, args),
        _ => Err("thread_history actions are list, search, or read".into()),
    };
    logging::record(
        if result.is_ok() {
            Level::Debug
        } else {
            Level::Warn
        },
        Target::Workload,
        "thread_history.operation",
        Context::peer(actor),
        || {
            json!({
                "action": action,
                "ok": result.is_ok(),
                "thread_id": args["thread_id"],
                "returned": result.as_ref().ok().and_then(|v| v.get("returned").or_else(|| v.get("returned_turns"))),
            })
        },
        || {
            result
                .as_ref()
                .map_or_else(|e| json!({"error": e.to_string()}), |_| Value::Null)
        },
    );
    result
}

/// Compact one-time session-start orientation listing recent non-resident threads.
pub fn orientation_summary(jobs: &Jobs, current_thread_id: &str, limit: usize) -> Option<String> {
    if limit == 0 {
        return None;
    }
    let now_ms = crate::jobs::now();
    let threads = collect_eligible_threads(jobs, Some(current_thread_id), None, None, None, now_ms);
    if threads.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for g in threads.iter().take(limit) {
        let prompt = g
            .runs
            .first()
            .map(|s| clip_preview(&s.record.spec.prompt.replace('\n', " "), 110))
            .unwrap_or_default();
        lines.push(format!(
            "- {} ({}, {} runs, {}): {}",
            g.thread_id,
            format_age(now_ms, g.updated_at),
            g.runs.len(),
            g.latest_status,
            prompt
        ));
    }
    Some(format!(
        "\nRecent non-resident threads on this machine (showing {} of {}, newest first; older or detailed threads are searchable and readable via boxen_threads):\n{}",
        lines.len(),
        threads.len(),
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{jobs::Spec, residents::Create};
    use std::fs;

    fn hex_id(seed: u8) -> String {
        format!("{seed:02x}").repeat(32)
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_run(
        root: &std::path::Path,
        owner: &str,
        id: &str,
        thread_id: &str,
        prompt: &str,
        assistant: &str,
        created_at: u64,
        status: &str,
        resident: Option<crate::residents::Resident>,
    ) {
        let dir = root.join("runs").join(id);
        fs::create_dir_all(&dir).unwrap();
        let mut spec = Spec {
            completion_handoff: None,
            required_capabilities: Default::default(),
            reasoning_effort: None,
            attachments: Vec::new(),
            context: None,
            resident_id: resident.as_ref().map(|r| r.id.clone()),
            id: id.into(),
            thread_id: thread_id.into(),
            backend: "codex".into(),
            cwd: "/tmp/example".into(),
            model: "gpt-6.1-sol".into(),
            prompt: prompt.into(),
        };
        if resident.is_some() {
            spec.resident_id = Some(thread_id.into());
        }
        let record = json!({
            "spec": spec,
            "owner": owner,
            "created_at": created_at,
            "resident": resident,
        });
        fs::write(
            dir.join("record.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let events = vec![
            json!({"seq":1,"at":created_at,"kind":"status","data":{"status":"starting","detail":"Accepted"}}),
            json!({"seq":2,"at":created_at+10,"kind":"user_message","data":{"text":prompt,"attachments":[]}}),
            json!({"seq":3,"at":created_at+20,"kind":"backend","data":{"method":"item/completed","params":{"item":{"id":"m1","type":"agentMessage","phase":"final_answer","text":assistant}}}}),
            json!({"seq":4,"at":created_at+30,"kind":"status","data":{"status":status,"detail":if status=="interrupted"{"Daemon restarted; outcome uncertain"}else{"Done"}}}),
        ];
        let mut body = String::new();
        for ev in events {
            body.push_str(&serde_json::to_string(&ev).unwrap());
            body.push('\n');
        }
        fs::write(dir.join("events.jsonl"), body).unwrap();
    }

    #[test]
    fn resident_and_promoted_threads_are_strictly_excluded() {
        let root = std::env::temp_dir().join(format!(
            "boxen-thread-history-test-{}",
            crate::state::random_hex().unwrap()
        ));
        let owner = hex_id(1);
        let t_anon = hex_id(2);
        let t_res = hex_id(3);
        let t_promoted = hex_id(4);
        let r_promoted_2 = hex_id(5);

        let res_direct = crate::residents::Resident::prototype(
            &owner,
            &owner,
            &Create {
                reasoning_effort: None,
                context: Default::default(),
                id: t_res.clone(),
                name: "Private Resident".into(),
                identity: "Secret".into(),
                memory: "CANARY_DIRECT".into(),
                backend: "codex".into(),
                cwd: "/tmp/example".into(),
                model: "gpt-6.1-sol".into(),
                quota_bytes: 65536,
                auto_activate: false,
            },
        );
        let res_promoted = crate::residents::Resident::prototype(
            &owner,
            &owner,
            &Create {
                reasoning_effort: None,
                context: Default::default(),
                id: t_promoted.clone(),
                name: "Promoted Resident".into(),
                identity: "Secret".into(),
                memory: "CANARY_PROMOTED".into(),
                backend: "codex".into(),
                cwd: "/tmp/example".into(),
                model: "gpt-6.1-sol".into(),
                quota_bytes: 65536,
                auto_activate: false,
            },
        );

        let now = crate::jobs::now();
        seed_run(
            &root,
            &owner,
            &t_anon,
            &t_anon,
            "Merchant payout webhook rotation",
            "Use header X-Payout-Sig-V2 with 18h grace window.",
            now - 10_000,
            "completed",
            None,
        );
        seed_run(
            &root,
            &owner,
            &t_res,
            &t_res,
            "Zephyr secret planning",
            "Codename CANARY_DIRECT",
            now - 5_000,
            "completed",
            Some(res_direct),
        );
        // Promoted thread: run 1 anonymous, run 2 resident
        seed_run(
            &root,
            &owner,
            &t_promoted,
            &t_promoted,
            "Zephyr early brainstorm before promotion",
            "Interim CANARY_PROMOTED",
            now - 8_000,
            "completed",
            None,
        );
        seed_run(
            &root,
            &owner,
            &r_promoted_2,
            &t_promoted,
            "Zephyr promoted follow-up",
            "Locked CANARY_PROMOTED",
            now - 4_000,
            "completed",
            Some(res_promoted),
        );

        let jobs = Jobs::open(root.join("runs"), "missing-codex".into(), owner.clone()).unwrap();

        let listed = operation(&jobs, &owner, &json!({"action":"list"})).unwrap();
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["threads"][0]["thread_id"], t_anon);

        let search = operation(
            &jobs,
            &owner,
            &json!({"action":"search","query":"Zephyr CANARY"}),
        )
        .unwrap();
        assert_eq!(search["total_matches"], 0);

        assert!(operation(&jobs, &owner, &json!({"action":"read","thread_id":t_res})).is_err());
        assert!(
            operation(
                &jobs,
                &owner,
                &json!({"action":"read","thread_id":t_promoted})
            )
            .is_err()
        );

        let read_ok =
            operation(&jobs, &owner, &json!({"action":"read","thread_id":t_anon})).unwrap();
        assert_eq!(read_ok["returned_turns"], 1);
        assert!(
            read_ok["turns"][0]["assistant"]
                .as_str()
                .unwrap()
                .contains("X-Payout-Sig-V2")
        );

        drop(jobs);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_ranks_specific_match_over_near_miss_and_preserves_interrupted_status() {
        let root = std::env::temp_dir().join(format!(
            "boxen-thread-search-test-{}",
            crate::state::random_hex().unwrap()
        ));
        let owner = hex_id(10);
        let t_payout = hex_id(11);
        let t_sandbox = hex_id(12);
        let t_interrupted = hex_id(13);
        let now = crate::jobs::now();

        seed_run(
            &root,
            &owner,
            &t_payout,
            &t_payout,
            "Webhook HMAC rotation for merchant payouts",
            "Header X-Payout-Sig-V2, grace window 18h.",
            now - 60 * 3_600_000,
            "completed",
            None,
        );
        seed_run(
            &root,
            &owner,
            &t_sandbox,
            &t_sandbox,
            "Webhook HMAC rotation for sandbox staging",
            "Header X-Sandbox-Sig, grace window 2h.",
            now - 12 * 3_600_000,
            "completed",
            None,
        );
        seed_run(
            &root,
            &owner,
            &t_interrupted,
            &t_interrupted,
            "Run backfill_v9.sql on prod replica",
            "",
            now - 5 * 3_600_000,
            "interrupted",
            None,
        );

        let jobs = Jobs::open(root.join("runs"), "missing-codex".into(), owner.clone()).unwrap();
        let search = operation(
            &jobs,
            &owner,
            &json!({"action":"search","query":"merchant payout webhook HMAC rotation"}),
        )
        .unwrap();
        assert_eq!(search["results"][0]["thread_id"], t_payout);

        let read_int = operation(
            &jobs,
            &owner,
            &json!({"action":"read","thread_id":t_interrupted}),
        )
        .unwrap();
        assert_eq!(read_int["latest_status"], "interrupted");
        assert!(
            read_int["turns"][0]["status_detail"]
                .as_str()
                .unwrap()
                .contains("uncertain")
        );

        drop(jobs);
        let _ = fs::remove_dir_all(root);
    }
}
