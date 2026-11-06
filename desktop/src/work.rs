//! Optional host-local project UI. All service/HTTP work runs off the render thread.
use boxen_work_items::{Config, Store, operator, transport::Control};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    selected: Option<i64>,
    drafts: BTreeMap<i64, Draft>,
    pub pending: Option<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Draft {
    text: String,
    kind: String,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            text: String::new(),
            kind: "question".into(),
        }
    }
}
#[derive(Clone)]
enum Request {
    List,
    Detail(i64),
    Submit(Value),
}
struct Reply {
    request: Request,
    result: Result<Value, String>,
}
pub enum Action {
    Submit,
    Retry,
    Discard,
}

pub struct State {
    home: PathBuf,
    boxen_home: PathBuf,
    origin: String,
    rx: Option<mpsc::Receiver<Reply>>,
    items: Vec<Value>,
    detail: Value,
    detail_item: Option<i64>,
    error: String,
    next: Option<Instant>,
    refresh_detail: bool,
    filter: String,
    more: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            home: PathBuf::new(),
            boxen_home: PathBuf::new(),
            origin: String::new(),
            rx: None,
            items: vec![],
            detail: Value::Null,
            detail_item: None,
            error: String::new(),
            next: None,
            refresh_detail: false,
            filter: String::new(),
            more: false,
        }
    }
}
impl State {
    pub fn new(home: &std::path::Path, origin: &str) -> Self {
        Self {
            home: std::env::var_os("BOXEN_WORK_ITEMS_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.parent().unwrap_or(home).join(".boxen-work-items")),
            boxen_home: home.into(),
            origin: origin.into(),
            ..Default::default()
        }
    }
    fn begin(&mut self, request: Request, ctx: &egui::Context) {
        let (tx, rx) = mpsc::sync_channel(1);
        self.rx = Some(rx);
        let home = self.home.clone();
        let boxen_home = self.boxen_home.clone();
        let origin = self.origin.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Value, String> {
                let cfg = Config::read(&home)
                    .map_err(|e| format!("Work-item service unavailable: {e}"))?;
                if cfg.boxen_home.canonicalize().ok() != boxen_home.canonicalize().ok()
                    || cfg.url != origin
                {
                    return Err("Work-item service belongs to a different daemon. BOXEN_WORK_ITEMS_HOME selects the matching local service.".into());
                }
                let s = Store::open(home.join("work-items.sqlite3")).map_err(|e| e.to_string())?;
                match &request {
                    Request::List => operator::list(&s).map_err(|e| e.to_string()),
                    Request::Detail(item) => {
                        let mut d = operator::detail(&s, *item).map_err(|e| e.to_string())?;
                        if d["feedback"].as_array().is_some_and(|v| !v.is_empty()) {
                            let replies = Control::new(&cfg)
                                .and_then(|api| operator::replies(&api, &cfg, &mut d));
                            if let Err(e) = replies {
                                d["reply_error"] = json!(e.to_string());
                            }
                        }
                        Ok(d)
                    }
                    Request::Submit(input) => {
                        operator::locked_submit(&s, &cfg, input).map_err(|e| e.to_string())
                    }
                }
            })();
            let _ = tx.send(Reply { request, result });
            ctx.request_repaint();
        });
    }
    pub fn pump(&mut self, saved: &mut Saved, visible: bool, ctx: &egui::Context) -> bool {
        let received = self.rx.as_ref().map(|rx| rx.try_recv());
        if matches!(received, Some(Err(mpsc::TryRecvError::Disconnected))) {
            self.rx = None;
            self.error = "Work reader stopped; the next refresh retries observation only.".into();
            self.next = Some(Instant::now() + Duration::from_secs(5));
        }
        let reply = received.and_then(Result::ok);
        let mut changed = false;
        if let Some(reply) = reply {
            self.rx = None;
            match reply.result {
                Err(error) => {
                    self.error = error;
                    self.next = Some(Instant::now() + Duration::from_secs(5));
                }
                Ok(value) => {
                    self.error.clear();
                    match reply.request {
                        Request::List => {
                            self.items = value["items"].as_array().cloned().unwrap_or_default();
                            self.more = value["more"] == true;
                            if saved.selected.is_none() {
                                saved.selected =
                                    self.items.first().and_then(|v| v["item"].as_i64());
                            }
                            self.refresh_detail = true;
                            self.next = Some(Instant::now() + Duration::from_secs(5));
                        }
                        Request::Detail(item) => {
                            if saved.selected == Some(item) {
                                self.detail = value;
                                self.detail_item = Some(item);
                                self.refresh_detail = false;
                            }
                        }
                        Request::Submit(input) => {
                            if saved
                                .pending
                                .as_ref()
                                .is_some_and(|p| p["id"] == input["id"])
                            {
                                if let Some(d) = input["item"]
                                    .as_i64()
                                    .and_then(|i| saved.drafts.get_mut(&i))
                                    && d.text == input["text"].as_str().unwrap_or("")
                                {
                                    d.text.clear();
                                }
                                saved.pending = None;
                                changed = true;
                            }
                            self.refresh_detail = true;
                            self.next = None;
                        }
                    }
                }
            }
        }
        if visible
            && self.rx.is_none()
            && (self.error.is_empty() || self.next.is_none_or(|at| Instant::now() >= at))
        {
            if self.detail_item != saved.selected || self.refresh_detail {
                if let Some(item) = saved.selected {
                    self.begin(Request::Detail(item), ctx);
                } else {
                    self.refresh_detail = false;
                }
            } else if self.next.is_none_or(|at| Instant::now() >= at) {
                self.begin(Request::List, ctx);
            }
        }
        if visible
            && self.rx.is_none()
            && let Some(next) = self.next
        {
            ctx.request_repaint_after(next.saturating_duration_since(Instant::now()));
        }
        changed
    }
    pub fn prepare(&self, saved: &mut Saved) -> bool {
        let Some(item) = saved.selected.filter(|i| self.detail_item == Some(*i)) else {
            return false;
        };
        let Some(draft) = saved.drafts.get(&item) else {
            return false;
        };
        if saved.pending.is_some() || draft.text.trim().is_empty() {
            return false;
        }
        saved.pending = Some(
            json!({"id":crate::new_id(),"item":item,"kind":draft.kind,"text":draft.text,"basis":self.detail["basis"]}),
        );
        true
    }
    pub fn send(&mut self, saved: &Saved, ctx: &egui::Context) {
        if self.rx.is_none()
            && let Some(input) = saved.pending.clone()
        {
            self.begin(Request::Submit(input), ctx);
        }
    }
    pub fn panel(&mut self, ui: &mut egui::Ui, saved: &mut Saved) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.heading("Work");
            if ui
                .add_enabled(self.rx.is_none(), egui::Button::new("Refresh"))
                .clicked()
            {
                self.next = None;
                self.refresh_detail = true;
            }
            if self.rx.is_some() {
                ui.weak("Refreshing…");
            }
        });
        ui.small("Retained project status · PR delivery is separate from merge and deployment");
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
        }
        if let Some(pending) = &saved.pending {
            ui.group(|ui| {
                ui.label(format!("Unresolved feedback for #{}. Its exact request is retained; reopening does not resend it.",pending["item"]));
                ui.label(pending["text"].as_str().unwrap_or(""));
                if ui.add_enabled(self.rx.is_none(),egui::Button::new("Retry exact request")).clicked() {action=Some(Action::Retry);}
                if ui.button("Keep draft; stop retrying").clicked() {action=Some(Action::Discard);}
                ui.small("Stopping retries does not undo a request the service may already have accepted.");
            });
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter work items…")
                .desired_width(f32::INFINITY),
        );
        egui::ScrollArea::vertical().id_salt("work-list").max_height(170.).show(ui,|ui| {
            for item in &self.items {
                let id=item["item"].as_i64().unwrap_or(0);
                let label=format!("#{id}  {}  ·  {}",item["title"].as_str().unwrap_or(""),item["status"].as_str().unwrap_or(""));
                if label.to_lowercase().contains(&self.filter.to_lowercase()) && ui.selectable_label(saved.selected==Some(id),label).clicked() {saved.selected=Some(id);}
            }
            if self.more {ui.weak("Showing 200 most recent item IDs. The service retains the remaining records.");}
        });
        ui.separator();
        if self.items.is_empty() {
            ui.label("No project work is available. This panel connects to the optional local work-item service.");
        }
        let Some(item) = saved.selected.filter(|i| self.detail_item == Some(*i)) else {
            return action;
        };
        let d = &self.detail;
        egui::ScrollArea::vertical().id_salt(("work-detail",item)).show(ui,|ui| {
            ui.heading(format!("#{item} · {}",d["title"].as_str().unwrap_or("")));
            ui.strong(d["status"].as_str().unwrap_or("Unknown"));
            if d["observation_stale"]==true {ui.colored_label(egui::Color32::YELLOW,"Source observations are older than 30 minutes. Current remote state is unverified.");}
            if let Some(reason)=d["reason"].as_str() {ui.label(reason);}
            if let Some(url)=d["pr"]["url"].as_str() {ui.hyperlink_to("Open pull request",url);}
            if let Some(commit)=d["commit"].as_str() {ui.label(format!("Candidate: {commit}"));}
            let w=&d["workflow"];
            if let Some(summary)=w["candidate"]["summary"].as_str() {ui.label(summary);}
            ui.horizontal_wrapped(|ui| {
                for (key,name) in [("context_review","Context review"),("technical_review","Technical review")] {
                    ui.label(format!("{name}: {}",w[key]["decision"].as_str().unwrap_or("not recorded")));
                }
            });
            egui::CollapsingHeader::new("Evidence and limitations").show(ui,|ui| {
                for (name,v) in [("Acceptance",&w["candidate"]["validation"]["acceptance"]),("Limitations",&w["candidate"]["validation"]["gaps"]),("Remaining",&w["candidate"]["remaining"]),("Context findings",&w["context_review"]["findings"]),("Technical findings",&w["technical_review"]["findings"]),("Interpretation",&w["interpretation"])] {
                    if !v.is_null() {ui.strong(name);evidence(ui,v);}
                }
            });
            ui.separator();
            ui.strong("Your feedback");
            let draft=saved.drafts.entry(item).or_default();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.kind,"question".into(),"Ask a question");
                ui.add_enabled_ui(d["can_return"]==true,|ui| {ui.selectable_value(&mut draft.kind,"correction".into(),"Return for correction");});
            });
            if draft.kind=="correction" {ui.small("A return starts fresh interpretation and a new assignment for the existing coder. Your text and this candidate remain retained; required reviews run again.");}
            else {ui.small("The context resident receives your question. Its explicit reply appears below and in Mail.");}
            ui.add(egui::TextEdit::multiline(&mut draft.text).desired_rows(3).desired_width(f32::INFINITY).hint_text("What is unclear, or what behavior needs to change?"));
            let allowed=draft.kind!="correction" || d["can_return"]==true;
            if ui.add_enabled(allowed && self.rx.is_none() && saved.pending.is_none() && !draft.text.trim().is_empty() && draft.text.len()<=8192,egui::Button::new("Send feedback")).clicked() {action=Some(Action::Submit);}
            for f in d["feedback"].as_array().into_iter().flatten() {
                ui.separator();
                ui.label(format!("{} · {}",f["input"]["kind"].as_str().unwrap_or("Feedback"),f["state"].as_str().unwrap_or("")));
                ui.label(f["input"]["text"].as_str().unwrap_or(""));
                if let Some(error)=f["error"].as_str() {ui.colored_label(egui::Color32::YELLOW,error);}
                for reply in d["replies"].as_array().into_iter().flatten().filter(|m|m["in_reply_to"]==f["id"]) {
                    ui.group(|ui| {ui.strong("Resident reply");ui.label(reply["body"].as_str().unwrap_or(""));});
                }
            }
            if let Some(error)=d["reply_error"].as_str() {ui.label(format!("Replies unavailable: {error}"));}
            if d["replies_limited"]==true {ui.small("Reply lookup reached its bounded page limit. Mail retains the complete inbox.");}
        });
        action
    }
}

fn evidence(ui: &mut egui::Ui, value: &Value) {
    match value {
        Value::String(text) => {
            ui.label(text);
        }
        Value::Array(values) => {
            for value in values {
                evidence(ui, value);
                ui.add_space(4.);
            }
        }
        Value::Object(fields) => {
            for key in [
                "id",
                "status",
                "intended_outcome",
                "summary",
                "observed",
                "starting_state",
                "action",
                "expected_result",
                "requested_change",
                "evidence",
                "evidence_refs",
                "scenarios",
                "questions",
                "holds",
            ] {
                if let Some(v) = fields.get(key) {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(format!("{}:", key.replace('_', " ")));
                        evidence(ui, v);
                    });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_feedback_and_independent_drafts_survive_workspace_restart_without_submission() {
        let mut saved = Saved {
            selected: Some(7),
            ..Default::default()
        };
        saved.drafts.insert(
            7,
            Draft {
                text: "Exact original feedback".into(),
                kind: "correction".into(),
            },
        );
        saved.drafts.insert(
            8,
            Draft {
                text: "Other item draft".into(),
                kind: "question".into(),
            },
        );
        let state = State {
            detail_item: Some(7),
            detail: json!({"basis":"observed-basis"}),
            ..Default::default()
        };
        assert!(state.prepare(&mut saved));
        let restored: Saved = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(restored.pending, saved.pending);
        assert_eq!(
            restored.pending.as_ref().unwrap()["basis"],
            "observed-basis"
        );
        assert_eq!(restored.drafts[&8].text, "Other item draft");
        let mut runtime = State::default();
        let mut restored = restored;
        runtime.pump(&mut restored, false, &egui::Context::default());
        assert!(runtime.rx.is_none());
        assert!(restored.pending.is_some());
    }
}
