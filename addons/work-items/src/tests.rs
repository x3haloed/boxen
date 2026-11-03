use super::*;
use anyhow::bail;
use std::{collections::BTreeMap, sync::Mutex};
use transport::{Boxen, dispatch, release};
struct Fixture {
    root: PathBuf,
    s: Store,
    cfg: Config,
}
impl Fixture {
    fn new() -> Self {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(id());
        let s = Store::open(root.join("service.sqlite3")).unwrap();
        let ado = ado::Config {
            enabled: true,
            organization: "https://dev.azure.com/example".into(),
            project: "Example".into(),
            project_id: "11111111-1111-4111-8111-111111111111".into(),
            assignee: "operator@example.invalid".into(),
            marker: "Implementation: V2".into(),
            board_column_field: "Example.Column".into(),
            board_done_field: "Example.Done".into(),
            interval_minutes: 15,
        };
        ado::configure(&s, &ado).unwrap();
        let cfg = Config {
            boxen_home: root.join("boxen"),
            url: "http://127.0.0.1:42425".into(),
            owner: hash("owner"),
            client_id: hash("workflow"),
            definition_id: hash("coder"),
            definition_revision: 1,
            pack_root: root.join("pack"),
            pack_hash: hash("pack"),
            capacity: 1,
            supervision: None,
        };
        let f = Self { root, s, cfg };
        for item in [1, 2, 3] {
            fs::create_dir_all(f.root.join(format!("worktree-{item}"))).unwrap();
            f.seed(item);
        }
        f
    }
    fn seed(&self, item: i64) {
        let cfg = ado::config(&self.s).unwrap();
        self.s
            .connect()
            .unwrap()
            .execute(
                "INSERT INTO ado_items VALUES(?1,?2,1,'Synthetic work','Doing',1,'Eligible',?3,?4)",
                params![
                    cfg.source(),
                    item,
                    json!({"id":item,"rev":1,"fields":{"System.Title":"Synthetic work"}})
                        .to_string(),
                    now()
                ],
            )
            .unwrap();
    }
    fn input(&self, item: i64) -> Value {
        let digest = self.s.packet(item).unwrap()["source_digest"].clone();
        json!({"item":item,"source_digest":digest,"idempotency_key":format!("operator-{item}"),"new_copy":true,"context":{"decision":"new_implementation","evidence":["Operator-assigned synthetic test"]},"assignment":{"job_id":format!("fixture-{item}"),"assignment_revision":"1","repository_root":self.root.join("repo"),"worktree":self.root.join(format!("worktree-{item}")),"branch":"fixture","base_commit":"1111111111111111111111111111111111111111","evidence_dir":self.root.join("evidence"),"project_pack":{"manifest_sha256":self.cfg.pack_hash},"work_item":{"id":item,"revision":1,"source_digest":digest},"task":{"request":"Read-only fixture","acceptance_criteria":["Explicit send"]},"delivery":{"push":false,"create_or_update_pr":false,"merge":false,"deploy":false,"autocomplete":false},"runtime":{"required":false}}})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let target = self.root.canonicalize().unwrap();
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .canonicalize()
            .unwrap();
        assert!(target.starts_with(&parent) && target != parent);
        fs::remove_dir_all(target).unwrap();
    }
}
struct Fake {
    cfg: Config,
    mail: Mutex<BTreeMap<String, Value>>,
    created: Mutex<BTreeMap<String, Value>>,
    lose_response: Mutex<bool>,
    active: bool,
}
impl Fake {
    fn new(cfg: &Config) -> Self {
        Self {
            cfg: cfg.clone(),
            mail: Mutex::new(BTreeMap::new()),
            created: Mutex::new(BTreeMap::new()),
            lose_response: Mutex::new(false),
            active: false,
        }
    }
}
impl Boxen for Fake {
    fn call(&self, request: Value) -> Result<Value> {
        match request["op"].as_str().unwrap() {
            "resident_definition" => {
                let spec = request["args"]["spec"].clone();
                let mut created = self.created.lock().unwrap();
                let id = spec["id"].as_str().unwrap().to_string();
                if let Some(old) = created.get(&id) {
                    ensure!(old == &spec, "Changed instantiate retry");
                } else {
                    created.insert(id, spec.clone());
                }
                Ok(
                    json!({"id":spec["id"],"owner":self.cfg.owner,"definition":{"id":self.cfg.definition_id,"instantiated_revision":1}}),
                )
            }
            "mailbox_send" => {
                let mut message = request["message"].clone();
                message["sender"] =
                    json!({"type":"client","machine":self.cfg.owner,"id":self.cfg.client_id});
                let mut mail = self.mail.lock().unwrap();
                let id = message["id"].as_str().unwrap().to_string();
                if let Some(old) = mail.get(&id) {
                    ensure!(old == &message, "Changed mail retry");
                } else {
                    mail.insert(id, message.clone());
                }
                let mut lose = self.lose_response.lock().unwrap();
                if *lose {
                    *lose = false;
                    bail!("Simulated lost acceptance response");
                }
                Ok(message)
            }
            "list" => Ok(if self.active {
                let coder = self.created.lock().unwrap().keys().next().unwrap().clone();
                json!([{"record":{"spec":{"thread_id":coder}},"status":"running"}])
            } else {
                json!([])
            }),
            "resident_inbox" => {
                let messages = self
                    .mail
                    .lock()
                    .unwrap()
                    .values()
                    .cloned()
                    .map(|mut m| {
                        m["delivery"] = json!("presented");
                        m
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"messages":messages,"next_after":0}))
            }
            _ => bail!("Unexpected fixture request"),
        }
    }
}
#[test]
fn operator_request_is_exact_and_same_item_cannot_spawn_twice() {
    let f = Fixture::new();
    let input = f.input(1);
    let first = f.s.assign(&f.cfg, &input).unwrap();
    assert_eq!(first["state"], "queued");
    assert_eq!(
        f.s.assign(&f.cfg, &input).unwrap()["job_id"],
        first["job_id"]
    );
    let mut changed = input.clone();
    changed["assignment"]["task"]["request"] = json!("changed");
    assert!(f.s.assign(&f.cfg, &changed).is_err());
    changed["idempotency_key"] = json!("another request");
    assert!(f.s.assign(&f.cfg, &changed).is_err());
}

#[test]
fn explicit_eligibility_exception_is_frozen_and_does_not_bypass_source_freshness() {
    let f = Fixture::new();
    let c = f.s.connect().unwrap();
    let raw = json!({"id":1,"rev":1,"fields":{"System.TeamProject":"Example","System.Title":"Returned work"}});
    c.execute(
        "UPDATE ado_items SET eligible=0,reason='Assigned elsewhere',raw=?1 WHERE id=1",
        [raw.to_string()],
    )
    .unwrap();
    let mut input = f.input(1);
    assert!(f.s.assign(&f.cfg, &input).is_err());
    for invalid in [
        json!({"reason":"Return"}),
        json!({"reason":"Return","authority":" "}),
        json!({"reason":"Return","authority":"Operator","extra":true}),
    ] {
        input["eligibility_override"] = invalid;
        assert!(f.s.assign(&f.cfg, &input).is_err());
    }
    input["eligibility_override"] =
        json!({"reason":"Follow-up on reviewed work","authority":"Explicit operator selection"});
    let accepted = f.s.assign(&f.cfg, &input).unwrap();
    assert_eq!(f.s.packet(1).unwrap()["eligible"], false);
    assert_eq!(
        f.s.assign(&f.cfg, &input).unwrap()["job_id"],
        accepted["job_id"]
    );
    let job = f.s.reserve(1).unwrap().unwrap();
    let api = Fake::new(&f.cfg);
    dispatch(&f.s, &f.cfg, &api, &job).unwrap();
    assert_eq!(api.mail.lock().unwrap().len(), 1);
    let frozen: String = c
        .query_row("SELECT packet FROM assignments WHERE job=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&frozen).unwrap()["eligibility_override"],
        input["eligibility_override"]
    );
    c.execute("UPDATE work SET state='reserved' WHERE id=?1", [&job])
        .unwrap();
    c.execute(
        "UPDATE ado_items SET raw=?1 WHERE id=1",
        [json!({"id":1,"rev":2,"fields":{"System.TeamProject":"Example"}}).to_string()],
    )
    .unwrap();
    dispatch(&f.s, &f.cfg, &api, &job).unwrap();
    assert_eq!(f.s.status().unwrap()["jobs"][0]["state"], "needs_refresh");
    assert_eq!(api.mail.lock().unwrap().len(), 1);
    c.execute(
        "UPDATE ado_items SET eligible=0,raw=?1 WHERE id=2",
        [json!({"id":2,"rev":1,"fields":{"System.TeamProject":"Another project"}}).to_string()],
    )
    .unwrap();
    let mut other = f.input(2);
    other["eligibility_override"] = input["eligibility_override"].clone();
    assert!(f.s.assign(&f.cfg, &other).is_err());
}
#[test]
fn capacity_and_conflicting_worktree_claims_are_atomic() {
    let f = Fixture::new();
    let a = f.input(1);
    let mut b = f.input(2);
    b["assignment"]["worktree"] = a["assignment"]["worktree"].clone();
    f.s.assign(&f.cfg, &a).unwrap();
    f.s.assign(&f.cfg, &b).unwrap();
    assert!(f.s.reserve(2).unwrap().is_some());
    assert!(f.s.reserve(2).unwrap().is_none());
    let c = f.input(3);
    f.s.assign(&f.cfg, &c).unwrap();
    assert!(f.s.reserve(1).unwrap().is_none());
    assert!(f.s.reserve(2).unwrap().is_some());
}
#[test]
fn lost_mail_response_and_restart_do_not_duplicate_copy_or_assignment() {
    let f = Fixture::new();
    let r = f.s.assign(&f.cfg, &f.input(1)).unwrap();
    let job = r["job_id"].as_str().unwrap();
    f.s.reserve(1).unwrap();
    let api = Fake::new(&f.cfg);
    *api.lose_response.lock().unwrap() = true;
    assert!(dispatch(&f.s, &f.cfg, &api, job).is_err());
    let reopened = Store::open(&f.s.path).unwrap();
    dispatch(&reopened, &f.cfg, &api, job).unwrap();
    assert_eq!(api.mail.lock().unwrap().len(), 1);
    assert_eq!(api.created.lock().unwrap().len(), 1);
    assert_eq!(reopened.status().unwrap()["jobs"][0]["state"], "delivered");
}
#[test]
fn source_change_invalidates_without_releasing_capacity_or_creating_new_work() {
    let f = Fixture::new();
    f.s.assign(&f.cfg, &f.input(1)).unwrap();
    f.s.reserve(1).unwrap();
    let c = f.s.connect().unwrap();
    c.execute(
        "INSERT INTO ado_entities VALUES(?1,1,'comment','1',?2)",
        params![
            ado::config(&f.s).unwrap().source(),
            json!({"text":"Ordinary reviewer revision"}).to_string()
        ],
    )
    .unwrap();
    source_changed(&c, &ado::config(&f.s).unwrap().source(), 1).unwrap();
    let state = f.s.status().unwrap();
    assert_eq!(state["jobs"][0]["state"], "needs_refresh");
    assert_eq!(state["jobs"][0]["holds_slot"], true);
    assert_eq!(state["jobs"].as_array().unwrap().len(), 1);
}
#[test]
fn cleanup_release_refuses_active_coder_and_continuation_retains_identity() {
    let f = Fixture::new();
    let r = f.s.assign(&f.cfg, &f.input(1)).unwrap();
    let job = r["job_id"].as_str().unwrap();
    f.s.reserve(1).unwrap();
    let mut api = Fake::new(&f.cfg);
    dispatch(&f.s, &f.cfg, &api, job).unwrap();
    api.active = true;
    assert!(release(&f.s, &api, job, "finished", true, "Confirmed cleanup").is_err());
    api.active = false;
    release(&f.s, &api, job, "finished", true, "Confirmed cleanup").unwrap();
    let mut input = f.input(1);
    input["new_copy"] = json!(false);
    input["idempotency_key"] = json!("continuation");
    let next = f.s.assign(&f.cfg, &input).unwrap();
    assert_eq!(next["coder_id"], r["coder_id"]);
    assert_eq!(next["version"], 2);
}
#[test]
fn unresolved_legacy_ownership_and_holds_are_not_fresh_copy_permission() {
    let f = Fixture::new();
    let c = f.s.connect().unwrap();
    let source = ado::config(&f.s).unwrap().source();
    c.execute("INSERT INTO legacy_records VALUES(?1,1,'ownership',?2)",params![source,json!({"lifecycle":"unfinished","coder_thread_id":"11111111-1111-4111-8111-111111111111"}).to_string()]).unwrap();
    let mut input = f.input(1);
    input["legacy_reconciliation"] = json!("Old coder still owns this work");
    assert!(f.s.assign(&f.cfg, &input).is_err());
}
#[test]
fn large_source_retained_in_immutable_file_without_silent_mail_truncation() {
    let f = Fixture::new();
    let c = f.s.connect().unwrap();
    let raw = json!({"id":1,"rev":1,"fields":{"description":"e".repeat(100000)}}).to_string();
    c.execute("UPDATE ado_items SET raw=?1 WHERE id=1", [raw])
        .unwrap();
    let r = f.s.assign(&f.cfg, &f.input(1)).unwrap();
    let job = r["job_id"].as_str().unwrap();
    f.s.reserve(1).unwrap();
    let api = Fake::new(&f.cfg);
    dispatch(&f.s, &f.cfg, &api, job).unwrap();
    let text = fs::read_to_string(f.root.join("evidence").join(job).join("1/packet.json")).unwrap();
    assert!(text.len() > 100000);
    let mail = api.mail.lock().unwrap();
    assert!(
        mail.values().next().unwrap()["body"]
            .as_str()
            .unwrap()
            .len()
            < 32768
    );
}

#[test]
fn investigation_preserves_open_questions_and_existing_owner_without_fake_answers() {
    let f = Fixture::new();
    let source = ado::config(&f.s).unwrap().source();
    let c = f.s.connect().unwrap();
    c.execute("INSERT INTO legacy_records VALUES(?1,1,'ownership',?2)",params![source,json!({"lifecycle":"unfinished","coder_thread_id":"11111111-1111-4111-8111-111111111111"}).to_string()]).unwrap();
    c.execute("INSERT INTO legacy_records VALUES(?1,1,'decision',?2)",params![source,json!({"kind":"wait_for_clarification","questions":["Original failure evidence missing"]}).to_string()]).unwrap();
    let mut input = f.input(1);
    input["new_copy"] = json!(false);
    input["coder_id"] = json!(hash("existing coder"));
    input["legacy_reconciliation"] = json!("Same prior owner; incident cause stays unknown");
    // An implementation continuation still cannot bypass the unresolved question.
    assert!(f.s.assign(&f.cfg, &input).is_err());
    input["context"]["decision"] = json!("investigate");
    input["context"]["investigation_rationale"] = json!(
        "Synthetic tests can measure the actual method without claiming incident attribution"
    );
    input["context"]["open_questions"] = json!(["Original failure evidence missing"]);
    input["assignment"]["task"]["mode"] = json!("investigation");
    input["assignment"]["task"]["validation"] = json!("isolated_synthetic");
    input["assignment"]["task"]["production_edits"] = json!(false);
    input["assignment"]["task"]["external_writes"] = json!(true);
    assert!(f.s.assign(&f.cfg, &input).is_err());
    input["assignment"]["task"]["external_writes"] = json!(false);
    let r = f.s.assign(&f.cfg, &input).unwrap();
    assert_eq!(r["coder_id"], input["coder_id"]);
    assert_eq!(
        f.s.packet(1).unwrap()["legacy"]["decision"]["kind"],
        "wait_for_clarification"
    );
    let mut duplicate = input;
    duplicate["idempotency_key"] = json!("duplicate new copy");
    duplicate["new_copy"] = json!(true);
    assert!(f.s.assign(&f.cfg, &duplicate).is_err());
}

#[test]
fn explicit_replacement_preserves_prior_assignments_and_retries_without_resetting_owner() {
    let f = Fixture::new();
    let first = f.s.assign(&f.cfg, &f.input(1)).unwrap();
    let mut input = f.input(1);
    input["idempotency_key"] = json!("authorized-replacement");
    input["replacement"] = json!({"expected_coder":first["coder_id"],"expected_version":1,"reason":"Native child cannot accept direct input","authority":"Human explicitly approved a replacement","handoff":"Retained original assignment and historical evidence"});
    assert!(f.s.assign(&f.cfg, &input).is_err());
    release(
        &f.s,
        &Fake::new(&f.cfg),
        first["job_id"].as_str().unwrap(),
        "paused",
        true,
        "No dispatch or runtime; retained source checkout",
    )
    .unwrap();
    input["replacement"]["expected_version"] = json!(2);
    assert!(f.s.assign(&f.cfg, &input).is_err());
    input["replacement"]["expected_version"] = json!(1);
    input["replacement"]["authority"] = json!("");
    assert!(f.s.assign(&f.cfg, &input).is_err());
    input["replacement"]["authority"] = json!("Human approved the new copy");
    let replacement = f.s.assign(&f.cfg, &input).unwrap();
    assert_ne!(replacement["coder_id"], first["coder_id"]);
    assert_eq!(replacement["version"], 2);
    assert_eq!(replacement["job_id"], first["job_id"]);
    let c = f.s.connect().unwrap();
    let packets: i64 = c
        .query_row("SELECT count(*) FROM assignments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(packets, 2);
    let transfers: i64 = c
        .query_row(
            "SELECT count(*) FROM workflow_events WHERE kind='ownership.transferred'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(transfers, 1);
    f.s.reserve(1).unwrap();
    dispatch(
        &f.s,
        &f.cfg,
        &Fake::new(&f.cfg),
        first["job_id"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(f.s.assign(&f.cfg, &input).unwrap()["idempotent"], true);
    let coder: String = c
        .query_row("SELECT coder FROM work", [], |r| r.get(0))
        .unwrap();
    assert_eq!(replacement["coder_id"], coder);
    let old_input: String = c
        .query_row("SELECT input FROM assignments WHERE version=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&old_input).unwrap(),
        f.input(1)
    );
}

// A protocol fixture drives authoritative mail/run snapshots; no model or private text parsing.
struct Supervised {
    inner: Fake,
    reports: Mutex<Vec<Value>>,
    status: Mutex<String>,
    seq: Mutex<i64>,
    online: Mutex<bool>,
}
impl Supervised {
    fn new(cfg: &Config) -> Self {
        Self {
            inner: Fake::new(cfg),
            reports: Mutex::new(vec![]),
            status: Mutex::new("running".into()),
            seq: Mutex::new(1),
            online: Mutex::new(true),
        }
    }
    fn report(&self, assignment: &Value, outcome: &str, clean: bool) -> Value {
        let coder = &assignment["coder_id"];
        json!({"id":hash(&format!("report:{outcome}:{clean}")),"seq":1,"owner":self.inner.cfg.owner,"sender":{"type":"resident","id":coder},"to":{"type":"client","machine":self.inner.cfg.owner,"id":self.inner.cfg.client_id},"in_reply_to":assignment["message_id"],"body":json!({"protocol":"boxen.work-report.v1","job_id":assignment["job_id"],"assignment_version":assignment["version"],"outcome":outcome,"summary":"Explicit synthetic result","evidence":["local evidence"],"remaining":[],"cleanup":{"no_owned_runtime_resources":clean}}).to_string()})
    }
}
impl Boxen for Supervised {
    fn call(&self, r: Value) -> Result<Value> {
        ensure!(
            *self.online.lock().unwrap(),
            "Fixture transport unavailable"
        );
        match r["op"].as_str().unwrap() {
            "client_inbox" => {
                let after = r["after"].as_i64().unwrap_or(0);
                let rows = self
                    .reports
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|m| m["seq"].as_i64().unwrap() > after)
                    .cloned()
                    .collect::<Vec<_>>();
                Ok(
                    json!({"next_after":rows.last().map(|m|m["seq"].clone()).unwrap_or(json!(after)),"messages":rows}),
                )
            }
            "resident_inbox" => {
                let mut page = self.inner.call(r)?;
                for m in page["messages"].as_array_mut().unwrap() {
                    m["presented_in"] = json!(hash("observed run"));
                }
                Ok(page)
            }
            "get" => {
                let coder = self
                    .inner
                    .created
                    .lock()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap()
                    .clone();
                Ok(
                    json!({"record":{"owner":self.inner.cfg.owner,"spec":{"thread_id":coder,"id":hash("observed run")}},"status":*self.status.lock().unwrap(),"last_seq":*self.seq.lock().unwrap()}),
                )
            }
            "list" => {
                let coder = self
                    .inner
                    .created
                    .lock()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap()
                    .clone();
                Ok(
                    json!([{"record":{"spec":{"thread_id":coder}},"status":*self.status.lock().unwrap()}]),
                )
            }
            _ => self.inner.call(r),
        }
    }
}
fn supervised_fixture() -> (Fixture, Supervised, Value) {
    let mut f = Fixture::new();
    f.cfg.supervision = Some(supervision::Settings {
        notify_to: json!({"type":"client","machine":f.cfg.owner,"id":hash("observer")}),
        start_timeout_seconds: 2,
        inactivity_seconds: 3,
        report_grace_seconds: 1,
    });
    let api = Supervised::new(&f.cfg);
    let a = f.s.assign(&f.cfg, &f.input(1)).unwrap();
    f.s.reserve(1).unwrap();
    dispatch(&f.s, &f.cfg, &api, a["job_id"].as_str().unwrap()).unwrap();
    (f, api, a)
}
#[test]
fn authenticated_handoff_waits_for_idle_and_releases_without_closing_ticket() {
    let (f, api, a) = supervised_fixture();
    let t = now();
    api.reports
        .lock()
        .unwrap()
        .push(api.report(&a, "completed", true));
    supervision::tick(&f.s, &f.cfg, &api, t).unwrap();
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
    *api.status.lock().unwrap() = "completed".into();
    // Reopen represents supervisor process restart; report/cursor and outbox survive.
    let reopened = Store::open(&f.s.path).unwrap();
    supervision::tick(&reopened, &f.cfg, &api, t + 100).unwrap();
    assert_eq!(f.s.status().unwrap()["jobs"][0]["state"], "finished");
    let notices = api.inner.mail.lock().unwrap().len();
    supervision::tick(&reopened, &f.cfg, &api, t + 5000).unwrap();
    assert_eq!(api.inner.mail.lock().unwrap().len(), notices);
    assert_eq!(reopened.packet(1).unwrap()["eligible"], true);
}
#[test]
fn forged_stale_malformed_and_conflicting_reports_cannot_complete_work() {
    let (f, api, a) = supervised_fixture();
    let t = now();
    let mut forged = api.report(&a, "completed", true);
    forged["sender"]["id"] = json!(hash("wrong worker"));
    let mut stale = api.report(&a, "completed", true);
    stale["id"] = json!(hash("stale"));
    stale["seq"] = json!(2);
    let mut body: Value = serde_json::from_str(stale["body"].as_str().unwrap()).unwrap();
    body["assignment_version"] = json!(9);
    stale["body"] = json!(body.to_string());
    let mut malformed = api.report(&a, "completed", true);
    malformed["id"] = json!(hash("malformed"));
    malformed["seq"] = json!(3);
    body["assignment_version"] = json!(1);
    body["summary"] = json!("");
    malformed["body"] = json!(body.to_string());
    api.reports
        .lock()
        .unwrap()
        .extend([forged, stale, malformed]);
    *api.status.lock().unwrap() = "completed".into();
    supervision::tick(&f.s, &f.cfg, &api, t).unwrap();
    assert!(supervision::snapshot(&f.s).unwrap()["assignments"][0]["report"].is_null());
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
    let mut valid = api.report(&a, "blocked", false);
    valid["seq"] = json!(4);
    api.reports.lock().unwrap().push(valid);
    supervision::tick(&f.s, &f.cfg, &api, t + 100).unwrap();
    let mut conflict = api.report(&a, "completed", true);
    conflict["seq"] = json!(5);
    conflict["id"] = json!(hash("conflict"));
    api.reports.lock().unwrap().push(conflict);
    supervision::tick(&f.s, &f.cfg, &api, t + 200).unwrap();
    assert_eq!(
        supervision::snapshot(&f.s).unwrap()["assignments"][0]["report"]["outcome"],
        "blocked"
    );
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
}
#[test]
fn missing_handoff_has_one_report_only_recovery_and_restart_safe_notice_retries() {
    let (f, api, a) = supervised_fixture();
    let t = now();
    *api.status.lock().unwrap() = "completed".into();
    supervision::tick(&f.s, &f.cfg, &api, t).unwrap();
    *api.inner.lose_response.lock().unwrap() = true;
    supervision::tick(&f.s, &f.cfg, &api, t + 1001).unwrap();
    let reopened = Store::open(&f.s.path).unwrap();
    supervision::tick(&reopened, &f.cfg, &api, t + 1002).unwrap();
    supervision::tick(&reopened, &f.cfg, &api, t + 32_000).unwrap();
    supervision::tick(&reopened, &f.cfg, &api, t + 34_000).unwrap();
    let c = f.s.connect().unwrap();
    let follows: i64 = c
        .query_row(
            "SELECT count(*) FROM supervision_outbox WHERE purpose='report_request'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(follows, 1);
    assert_eq!(
        supervision::snapshot(&f.s).unwrap()["assignments"][0]["phase"],
        "unresolved"
    );
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
    let creates = api.inner.created.lock().unwrap().len();
    assert_eq!(creates, 1);
    let mut late = api.report(&a, "failed", true);
    late["seq"] = json!(10);
    api.reports.lock().unwrap().push(late);
    supervision::tick(&reopened, &f.cfg, &api, t + 35_000).unwrap();
    assert_eq!(f.s.status().unwrap()["jobs"][0]["state"], "paused");
}
#[test]
fn crash_and_inactivity_are_visible_without_replaying_or_declaring_failure_from_silence() {
    let (f, api, _) = supervised_fixture();
    let t = now();
    supervision::tick(&f.s, &f.cfg, &api, t).unwrap();
    supervision::tick(&f.s, &f.cfg, &api, t + 3100).unwrap();
    let c = f.s.connect().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM supervision_outbox WHERE purpose='activity_overdue'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    *api.status.lock().unwrap() = "failed".into();
    supervision::tick(&f.s, &f.cfg, &api, t + 3200).unwrap();
    supervision::tick(&f.s, &f.cfg, &api, t + 4300).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM supervision_outbox WHERE purpose='unresolved_outcome'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM supervision_outbox WHERE purpose='report_request'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
}
#[test]
fn report_page_failure_rolls_back_receipts_report_and_cursor_together() {
    let (f, api, a) = supervised_fixture();
    let t = now();
    let valid = api.report(&a, "completed", true);
    let mut broken = valid.clone();
    broken["seq"] = json!(2);
    broken["id"] = Value::Null;
    api.reports.lock().unwrap().extend([valid, broken]);
    assert!(supervision::tick(&f.s, &f.cfg, &api, t).is_err());
    let c = f.s.connect().unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM report_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(supervision::snapshot(&f.s).unwrap()["assignments"][0]["report"].is_null());
    api.reports.lock().unwrap().pop();
    supervision::tick(&f.s, &f.cfg, &api, t + 1).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM report_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn service_health_retains_outage_and_recovery_without_repeated_notification_flood() {
    let (f, api, _) = supervised_fixture();
    supervision::health(&f.s, &f.cfg, Some("Unavailable endpoint")).unwrap();
    supervision::health(&f.s, &f.cfg, Some("Still unavailable")).unwrap();
    let reopened = Store::open(&f.s.path).unwrap();
    assert_eq!(
        supervision::snapshot(&reopened).unwrap()["health"]["failures"],
        2
    );
    supervision::health(&reopened, &f.cfg, None).unwrap();
    supervision::tick(&reopened, &f.cfg, &api, now()).unwrap();
    let c = reopened.connect().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM supervision_outbox WHERE purpose IN ('service_unavailable','service_recovered')",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(
        supervision::snapshot(&reopened).unwrap()["health"]["available"],
        true
    );
}
#[test]
fn completed_report_followed_by_failed_run_is_paused_not_certified_successful() {
    let (f, api, a) = supervised_fixture();
    api.reports
        .lock()
        .unwrap()
        .push(api.report(&a, "completed", true));
    *api.status.lock().unwrap() = "failed".into();
    supervision::tick(&f.s, &f.cfg, &api, now()).unwrap();
    assert_eq!(f.s.status().unwrap()["jobs"][0]["state"], "paused");
    assert_eq!(
        f.s.connect()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM supervision_outbox WHERE purpose='report_run_conflict'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}
struct NotPresented<'a>(&'a Supervised);
impl Boxen for NotPresented<'_> {
    fn call(&self, r: Value) -> Result<Value> {
        let inbox = r["op"] == "resident_inbox";
        let mut result = self.0.call(r)?;
        if inbox {
            for m in result["messages"].as_array_mut().unwrap() {
                m["presented_in"] = Value::Null;
                m["delivery"] = json!("queued");
            }
        }
        Ok(result)
    }
}
#[test]
fn accepted_mail_without_activation_emits_one_start_warning_without_resubmission() {
    let (f, api, _) = supervised_fixture();
    let t = now();
    let pending = NotPresented(&api);
    supervision::tick(&f.s, &f.cfg, &pending, t).unwrap();
    supervision::tick(&f.s, &f.cfg, &pending, t + 2100).unwrap();
    supervision::tick(&f.s, &f.cfg, &pending, t + 5000).unwrap();
    assert_eq!(
        f.s.connect()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM supervision_outbox WHERE purpose='start_overdue'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(api.inner.created.lock().unwrap().len(), 1);
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
}
