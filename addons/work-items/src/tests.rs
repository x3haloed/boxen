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
            "thread_activity" => Ok(json!({"thread_id":request["id"],"idle":!self.active})),
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
fn retained_continuation_preserves_lease_owner_history_and_report_versions() {
    let mut f = Fixture::new();
    f.cfg.supervision = Some(supervision::Settings {
        notify_to: json!({"type":"client","machine":f.cfg.owner,"id":hash("observer")}),
        start_timeout_seconds: 2,
        inactivity_seconds: 3,
        report_grace_seconds: 1,
    });
    let initial = f.s.assign(&f.cfg, &f.input(1)).unwrap();
    let job = f.s.reserve(1).unwrap().unwrap();
    let mut api = Fake::new(&f.cfg);
    dispatch(&f.s, &f.cfg, &api, &job).unwrap();
    let blocked =
        json!({"outcome":"blocked","summary":"Browser stopped; evidence retained"}).to_string();
    f.s.connect()
        .unwrap()
        .execute(
            "UPDATE supervision SET report=?1,phase='report_received' WHERE job=?2 AND version=1",
            params![blocked, job],
        )
        .unwrap();
    let mut next = f.input(1);
    next["idempotency_key"] = json!("continuation-one");
    next["new_copy"] = json!(false);
    next["coder_id"] = initial["coder_id"].clone();
    next["continuation"] = json!({"expected_version":1,"expected_coder":initial["coder_id"],"reason":"Runtime recovered; existing effects and blocked report remain retained"});
    api.active = true;
    assert!(transport::continue_assignment(&f.s, &f.cfg, &api, &next).is_err());
    api.active = false;
    struct Queued<'a>(&'a Fake);
    impl Boxen for Queued<'_> {
        fn call(&self, request: Value) -> Result<Value> {
            let queued = request["op"] == "resident_inbox";
            let mut value = self.0.call(request)?;
            if queued {
                for m in value["messages"].as_array_mut().unwrap() {
                    m["delivery"] = json!("queued");
                }
            }
            Ok(value)
        }
    }
    assert!(transport::continue_assignment(&f.s, &f.cfg, &Queued(&api), &next).is_err());
    let mut changed = next.clone();
    changed["assignment"]["worktree"] = json!(f.root.join("worktree-2"));
    assert!(transport::continue_assignment(&f.s, &f.cfg, &api, &changed).is_err());
    let mut stale = next.clone();
    stale["continuation"]["expected_version"] = json!(2);
    assert!(transport::continue_assignment(&f.s, &f.cfg, &api, &stale).is_err());
    let mut replacement = next.clone();
    replacement["new_copy"] = json!(true);
    assert!(transport::continue_assignment(&f.s, &f.cfg, &api, &replacement).is_err());
    let second = transport::continue_assignment(&f.s, &f.cfg, &api, &next).unwrap();
    assert_eq!(second["version"], 2);
    assert_eq!(second["state"], "reserved");
    assert_eq!(second["coder_id"], initial["coder_id"]);
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
    assert_eq!(f.s.assign(&f.cfg, &next).unwrap()["version"], 2);
    assert!(f.s.reserve(1).unwrap().is_none());
    dispatch(&f.s, &f.cfg, &api, &job).unwrap();
    assert_eq!(api.created.lock().unwrap().len(), 1);
    assert_eq!(api.mail.lock().unwrap().len(), 2);
    let c = f.s.connect().unwrap();
    let old: (String, i64, String) = c
        .query_row(
            "SELECT report,closed,phase FROM supervision WHERE job=?1 AND version=1",
            [&job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(old, (blocked, 1, "superseded".into()));
    assert!(
        c.query_row(
            "SELECT report IS NULL AND closed=0 FROM supervision WHERE job=?1 AND version=2",
            [&job],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM assignments WHERE job=?1",
            [&job],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM reservations WHERE job=?1",
            [&job],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
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
fn returned_work_references_full_prior_delivery_without_recursive_mail_growth() {
    let f = Fixture::new();
    let raw = json!({"job_id":hash("prior job"),"coder_id":hash("same coder"),"assignment_version":3,"pr_id":42,
        "candidate":{"commit":"1111111111111111111111111111111111111111","base":"2222222222222222222222222222222222222222","summary":"e".repeat(100000)},
        "operator_resume":{"prior_report":{"evidence":"h".repeat(100000)}}}).to_string();
    let first = workflow::retained_prior_delivery(&f.root, &raw).unwrap();
    assert!(first.to_string().len() < 2048);
    assert_eq!(first["coder_id"], hash("same coder"));
    assert_eq!(first["pr_id"], 42);
    assert_eq!(first["evidence_sha256"], hash(&raw));
    assert_eq!(
        fs::read_to_string(first["evidence_path"].as_str().unwrap()).unwrap(),
        raw
    );
    assert_eq!(
        workflow::retained_prior_delivery(&f.root, &raw).unwrap(),
        first
    );
    fs::write(first["evidence_path"].as_str().unwrap(), "changed").unwrap();
    assert!(workflow::retained_prior_delivery(&f.root, &raw).is_err());
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
            "thread_activity" => Ok(
                json!({"thread_id":r["id"],"idle":matches!(self.status.lock().unwrap().as_str(),"completed"|"failed"|"interrupted"|"cancelled")}),
            ),
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
fn ordinary_worker_progress_reaches_observer_without_completing_or_replaying() {
    let (f, api, a) = supervised_fixture();
    let mut progress = api.report(&a, "completed", true);
    progress["body"] = json!("Normal browser access is unavailable; validation remains pending.");
    api.reports.lock().unwrap().push(progress.clone());
    let t = now();
    supervision::tick(&f.s, &f.cfg, &api, t).unwrap();
    let mail = api.inner.mail.lock().unwrap();
    let notices = mail
        .values()
        .filter(|m| {
            m["body"]
                .as_str()
                .unwrap_or("")
                .contains("Attributed worker correspondence")
        })
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    let body: Value = serde_json::from_str(notices[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["detail"]["sender"], progress["sender"]);
    drop(mail);
    assert!(
        f.s.connect()
            .unwrap()
            .query_row("SELECT report IS NULL FROM supervision", [], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    assert_eq!(f.s.status().unwrap()["jobs"][0]["holds_slot"], true);
    let mut forged = progress;
    forged["seq"] = json!(2);
    forged["id"] = json!(hash("forged progress"));
    forged["sender"]["id"] = json!(hash("other resident"));
    api.reports.lock().unwrap().push(forged);
    let reopened = Store::open(&f.s.path).unwrap();
    supervision::tick(&reopened, &f.cfg, &api, t + 100).unwrap();
    assert_eq!(
        api.inner
            .mail
            .lock()
            .unwrap()
            .values()
            .filter(|m| m["body"]
                .as_str()
                .unwrap_or("")
                .contains("Attributed worker correspondence"))
            .count(),
        1
    );
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

// Synthetic boundary driver: project effects and model observations are fixtures, not runtime proof.
struct WorkflowDriver {
    input: Value,
    calls: Vec<String>,
    pr: bool,
    lose_create: bool,
    question_posted: bool,
    lose_question: bool,
    commit: String,
}
impl workflow::Driver for WorkflowDriver {
    fn call(&mut self, operation: &str, request: &Value) -> Result<Value> {
        self.calls.push(operation.into());
        Ok(match operation {
            "find_question" => {
                json!({"ok":true,"found":self.question_posted,"comment_id":8,"comment_version":1,"service_authored":true})
            }
            "post_question" => {
                self.question_posted = true;
                if self.lose_question {
                    self.lose_question = false;
                    bail!("Unknown comment-create outcome")
                };
                json!({"ok":true,"comment_id":8,"comment_version":1,"service_authored":true})
            }
            "plan" => json!({"ok":true,"assignment":self.input["assignment"]}),
            "prepare" => {
                fs::create_dir_all(request["assignment"]["worktree"].as_str().unwrap())?;
                json!({"ok":true,"prepared":true})
            }
            "verify_cleanup" => json!({"ok":true,"quiescent":true,"database_removed":true}),
            "inspect_candidate" => json!({"ok":true,"ready":true}),
            "push" => {
                self.commit = request["candidate"]["commit"].as_str().unwrap().into();
                json!({"ok":true})
            }
            "find_pr" => json!({"ok":true,"prs":if self.pr{vec![json!({"id":23})]}else{vec![]}}),
            "create_pr" => {
                self.pr = true;
                if self.lose_create {
                    self.lose_create = false;
                    bail!("Simulated unknown create outcome")
                };
                json!({"ok":true,"id":23})
            }
            "verify_pr" => {
                json!({"ok":true,"id":23,"url":"https://example.invalid/pr/23","is_draft":false,"active":true,"commit":self.commit,"work_item_id":1,"autocomplete":false})
            }
            _ => bail!("Unexpected project operation"),
        })
    }
}
struct WorkflowApi {
    cfg: Config,
    s: Store,
    mail: Mutex<BTreeMap<String, Value>>,
    incoming: Mutex<Vec<Value>>,
    actors: Mutex<BTreeMap<String, Value>>,
    correction: bool,
    auto_answer: bool,
}
impl WorkflowApi {
    fn answer(&self, message: &Value, mut body: Value, sender: Value) {
        let mut incoming = self.incoming.lock().unwrap();
        let seq = incoming.len() as i64 + 1;
        if body["protocol"] == "boxen.interpretation.v1" && body["decision"] == "ready" {
            if body["intended_outcome"].is_null() {
                body["intended_outcome"] = json!("Synthetic intent");
            }
            if let Some(scenarios) = body["scenarios"].as_array_mut() {
                for scenario in scenarios {
                    for field in ["role", "starting_state", "action", "expected_result"] {
                        if scenario[field].is_null() {
                            scenario[field] = json!("Synthetic fixture");
                        }
                    }
                }
            }
        }
        body["request_id"] = message["id"].clone();
        let request: Value = serde_json::from_str(message["body"].as_str().unwrap()).unwrap();
        for field in ["attempt_id", "source_digest", "work_item_id"] {
            body[field] = request[field].clone();
        }
        incoming.push(json!({"id":hash(&format!("fixture:{seq}")),"seq":seq,"sender":sender,"to":{"type":"client","machine":self.cfg.owner,"id":self.cfg.client_id},"in_reply_to":message["id"],"body":body.to_string()}));
    }
}
impl Boxen for WorkflowApi {
    fn call(&self, r: Value) -> Result<Value> {
        match r["op"].as_str().unwrap() {
            "thread_activity" => Ok(json!({"thread_id":r["id"],"idle":true})),
            "list" => Ok(json!([])),
            "resident_definition" => {
                let spec = &r["args"]["spec"];
                let id = spec["id"].as_str().unwrap();
                let mut actors = self.actors.lock().unwrap();
                if let Some(old) = actors.get(id) {
                    ensure!(old == spec, "Instantiation retry changed");
                } else {
                    actors.insert(id.into(), spec.clone());
                }
                Ok(
                    json!({"id":id,"owner":self.cfg.owner,"definition":{"id":spec["definition_id"],"instantiated_revision":spec["definition_revision"]}}),
                )
            }
            "resident_get" => Ok(
                json!({"id":r["id"],"owner":self.cfg.owner,"definition":{"id":self.cfg.definition_id}}),
            ),
            "client_inbox" => {
                let after = r["after"].as_i64().unwrap_or(0);
                let incoming = self.incoming.lock().unwrap();
                let rows: Vec<_> = incoming
                    .iter()
                    .filter(|v| v["seq"].as_i64().unwrap() > after)
                    .cloned()
                    .collect();
                Ok(
                    json!({"messages":rows,"next_after":rows.last().map_or(after,|r|r["seq"].as_i64().unwrap())}),
                )
            }
            "resident_inbox" => {
                let rows: Vec<_> = self
                    .mail
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|v| v["to"]["id"] == r["id"])
                    .cloned()
                    .map(|mut v| {
                        v["delivery"] = json!("presented");
                        v
                    })
                    .collect();
                Ok(json!({"messages":rows,"next_after":0}))
            }
            "mailbox_send" => {
                let mut m = r["message"].clone();
                m["sender"] =
                    json!({"type":"client","machine":self.cfg.owner,"id":self.cfg.client_id});
                let mid = m["id"].as_str().unwrap();
                let mut mail = self.mail.lock().unwrap();
                if let Some(old) = mail.get(mid) {
                    ensure!(old == &m, "Stable mail ID changed");
                    return Ok(old.clone());
                }
                mail.insert(mid.into(), m.clone());
                drop(mail);
                if let Ok(request) = serde_json::from_str::<Value>(m["body"].as_str().unwrap()) {
                    if self.auto_answer && request["protocol"] == "boxen.interpretation-request.v1"
                    {
                        self.answer(&m,json!({"protocol":"boxen.interpretation.v1","decision":"ready","intended_outcome":"Synthetic behavior","scenarios":[{"id":"s1","action":"Action","expected_result":"Result"}],"preservation_targets":[],"questions":[],"holds":[],"existing_work":{"unresolved_ownership":false},"evidence_refs":["Inspected fixture"]}),m["to"].clone());
                    }
                    if self.auto_answer && request["protocol"] == "boxen.review-request.v1" {
                        let changes = self.correction
                            && request["role"] == "technical"
                            && request["review_round"] == 1;
                        self.answer(&m,json!({"protocol":"boxen.review.v1","role":request["role"],"reviewed_commit":request["reviewed_commit"],"base_commit":request["base_commit"],"decision":if changes{"changes_required"}else{"pass"},"findings":if changes{vec![json!({"id":"wrong-state","blocking":true,"requested_change":"Exercise the correct state"})]}else{vec![]}}),m["to"].clone());
                    }
                } else if m["subject"].as_str().unwrap_or("").contains("assignment v") {
                    let c = self.s.connect()?;
                    let (job, version): (String, i64) = c.query_row(
                        "SELECT job,version FROM assignments WHERE json_extract(message,'$.id')=?1",
                        [mid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    let report = json!({"protocol":"boxen.work-report.v1","job_id":job,"assignment_version":version,"outcome":"completed","summary":"Synthetic boundary result, not actual application evidence","commit":if version==1{"1111111111111111111111111111111111111111"}else{"3333333333333333333333333333333333333333"},"base":"2222222222222222222222222222222222222222","evidence":["synthetic fixture"],"remaining":[],"validation":{"local_runtime_exercised":true,"acceptance":[{"id":"s1","status":"observed_pass","evidence":"Synthetic protocol fixture"}]},"cleanup":{"no_owned_runtime_resources":true}});
                    // The existing supervision suite separately verifies authenticated report acceptance.
                    c.execute("UPDATE supervision SET report=?3,phase='report_received' WHERE job=?1 AND version=?2",params![job,version,report.to_string()])?;
                }
                Ok(m)
            }
            _ => bail!("Unexpected Boxen fixture operation"),
        }
    }
}
fn workflow_fixture(
    correction: bool,
) -> (Fixture, WorkflowApi, workflow::Settings, WorkflowDriver) {
    let mut f = Fixture::new();
    f.cfg.supervision = Some(supervision::Settings {
        notify_to: json!({"type":"client","machine":f.cfg.owner,"id":hash("observer")}),
        start_timeout_seconds: 30,
        inactivity_seconds: 30,
        report_grace_seconds: 5,
    });
    fs::create_dir_all(&f.cfg.pack_root).unwrap();
    fs::write(
        f.cfg.pack_root.join("pack.manifest.json"),
        json!({"id":"synthetic","version":"1"}).to_string(),
    )
    .unwrap();
    let settings = workflow::Settings {
        enabled: true,
        allow_new_copies: true,
        publication: true,
        post_clarifications: false,
        authority: "Explicit synthetic test authority".into(),
        context_resident: hash("context"),
        review_definition: hash("reviewer"),
        review_revision: 1,
        notify_to: json!({"type":"client","machine":f.cfg.owner,"id":hash("observer")}),
        driver: f.root.join("driver"),
        driver_args: vec![],
        driver_script: f.root.join("script"),
        driver_hash: hash("script"),
        project: json!({"azure_organization":"https://dev.azure.com/example","azure_project":"Example"}),
        items: vec![1],
    };
    fs::remove_dir(f.root.join("worktree-1")).unwrap();
    let mut input = f.input(1);
    input["assignment"]["runtime"] = json!({"required":true,"reservation_id":"synthetic","allocation_source":"Fixture-owned allocation","frontend_port":4500,"backend_port":4501,"database_server":"local","database_name":"synthetic","browser_context":f.root.join("browser")});
    let api = WorkflowApi {
        cfg: f.cfg.clone(),
        s: f.s.clone(),
        mail: Mutex::new(BTreeMap::new()),
        incoming: Mutex::new(vec![]),
        actors: Mutex::new(BTreeMap::new()),
        correction,
        auto_answer: true,
    };
    let driver = WorkflowDriver {
        input,
        calls: vec![],
        pr: false,
        lose_create: false,
        question_posted: false,
        lose_question: false,
        commit: String::new(),
    };
    (f, api, settings, driver)
}
fn workflow_step(
    f: &Fixture,
    api: &WorkflowApi,
    settings: &workflow::Settings,
    driver: &mut WorkflowDriver,
) {
    workflow::tick(&f.s, &f.cfg, api, settings, driver).unwrap();
    if let Some(job) = f.s.reserve(1).unwrap() {
        transport::dispatch(&f.s, &f.cfg, api, &job).unwrap();
    }
    let jobs =
        f.s.connect()
            .unwrap()
            .prepare("SELECT id FROM work WHERE state='reserved'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
    for job in jobs {
        transport::dispatch(&f.s, &f.cfg, api, &job).unwrap();
    }
}
#[test]
fn operator_resume_keeps_coder_versions_assignment_and_deduplicates_after_dispatch() {
    let (f, api, settings, mut driver) = workflow_fixture(false);
    fs::write(
        f.s.path.parent().unwrap().join("workflow.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    for _ in 0..10 {
        workflow_step(&f, &api, &settings, &mut driver);
        if workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"] == "coding" {
            break;
        }
    }
    let c = f.s.connect().unwrap();
    let (job, coder): (String, String) = c
        .query_row("SELECT id,coder FROM work", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    c.execute(
        "UPDATE supervision SET report=json_set(report,'$.outcome','blocked')",
        [],
    )
    .unwrap();
    for _ in 0..3 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    let id = workflow::snapshot(&f.s).unwrap()["attempts"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "held"
    );
    let request = json!({"idempotency_key":"resume-browser-v1","attempt_id":id,"expected_version":1,"expected_coder":coder,"reason":"Human approved the supported browser fallback; resume normal required validation"});
    let mut wrong = request.clone();
    wrong["expected_version"] = json!(2);
    assert!(workflow::resume(&f.s, &api, &wrong).is_err());
    let receipt = workflow::resume(&f.s, &api, &request).unwrap();
    for _ in 0..10 {
        workflow_step(&f, &api, &settings, &mut driver);
        if workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"] == "coding" {
            break;
        }
    }
    c.execute("UPDATE supervision SET report=json_set(report,'$.outcome','blocked','$.summary','A different blocked result') WHERE version=2",[]).unwrap();
    for _ in 0..3 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    let blocked = workflow::snapshot(&f.s).unwrap();
    assert_eq!(blocked["attempts"][0]["stage"], "held");
    assert!(blocked["attempts"][0]["error"].is_null(), "{blocked}");
    let mut third = request.clone();
    third["idempotency_key"] = json!("resume-second-blocker");
    third["expected_version"] = json!(2);
    workflow::resume(&f.s, &api, &third).unwrap();
    for _ in 0..25 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "published"
    );
    assert_eq!(
        workflow::resume(&Store::open(&f.s.path).unwrap(), &api, &request).unwrap(),
        receipt
    );
    let state: (String, i64) = c
        .query_row("SELECT coder,version FROM work WHERE id=?1", [job], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(state, (coder, 3));
    assert_eq!(
        api.actors
            .lock()
            .unwrap()
            .values()
            .filter(|v| v["definition_id"] == f.cfg.definition_id)
            .count(),
        1
    );
    let mut conflict = request;
    conflict["reason"] = json!("Different authority");
    assert!(workflow::resume(&f.s, &api, &conflict).is_err());
}
#[test]
fn automatic_loop_returns_findings_to_same_coder_and_publishes_exact_full_pr() {
    let (f, api, mut settings, mut driver) = workflow_fixture(true);
    settings.project["browser_workflow"] =
        json!("Assigned browser profile with native computer access");
    for _ in 0..35 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    let state = workflow::snapshot(&f.s).unwrap();
    assert_eq!(state["attempts"][0]["stage"], "published", "{state}");
    let (version, held): (i64, i64) =
        f.s.connect()
            .unwrap()
            .query_row("SELECT version,holds_slot FROM work", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
    assert_eq!((version, held), (2, 0));
    let retained: Vec<String> = f.s.connect().unwrap().prepare(
        "SELECT json_extract(input,'$.assignment.task.browser_workflow') FROM assignments ORDER BY version"
    ).unwrap().query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        retained,
        vec!["Assigned browser profile with native computer access"; 2]
    );
    let actors = api.actors.lock().unwrap();
    assert_eq!(
        actors
            .values()
            .filter(|v| v["definition_id"] == f.cfg.definition_id)
            .count(),
        1
    );
    assert_eq!(
        actors.len(),
        2,
        "One coder and one distinct technical reviewer"
    );
    assert_eq!(driver.calls.iter().filter(|v| *v == "create_pr").count(), 1);
    assert_eq!(driver.commit, "3333333333333333333333333333333333333333");
    let reopened = Store::open(f.s.path.clone()).unwrap();
    workflow::tick(&reopened, &f.cfg, &api, &settings, &mut driver).unwrap();
    assert_eq!(driver.calls.iter().filter(|v| *v == "create_pr").count(), 1);
}
#[test]
fn uncertain_pr_creation_is_reconciled_without_a_second_create() {
    let (f, api, settings, mut driver) = workflow_fixture(false);
    driver.lose_create = true;
    for _ in 0..25 {
        workflow_step(&f, &api, &settings, &mut driver);
        f.s.connect()
            .unwrap()
            .execute("UPDATE workflow_attempts SET next_due=0", [])
            .unwrap();
    }
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "published"
    );
    assert_eq!(driver.calls.iter().filter(|v| *v == "create_pr").count(), 1);
}
#[test]
fn changed_source_supersedes_an_interpretation_without_creating_a_coder() {
    let (f, mut api, settings, mut driver) = workflow_fixture(false);
    api.auto_answer = false;
    workflow_step(&f, &api, &settings, &mut driver);
    f.s.connect()
        .unwrap()
        .execute(
            "UPDATE ado_items SET raw=json_set(raw,'$.rev',2),revision=2 WHERE id=1",
            [],
        )
        .unwrap();
    workflow_step(&f, &api, &settings, &mut driver);
    assert!(api.actors.lock().unwrap().is_empty());
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "superseded"
    );
}
#[test]
fn forged_and_superseded_review_replies_cannot_advance_publication() {
    let (f, mut api, settings, mut driver) = workflow_fixture(false);
    api.auto_answer = false;
    workflow_step(&f, &api, &settings, &mut driver);
    let request = api
        .mail
        .lock()
        .unwrap()
        .values()
        .find(|m| m["subject"].as_str().unwrap().contains("interpretation"))
        .unwrap()
        .clone();
    let answer = json!({"protocol":"boxen.interpretation.v1","decision":"ready","scenarios":[{"id":"s1"}],"preservation_targets":[],"holds":[],"questions":[],"existing_work":{"unresolved_ownership":false},"evidence_refs":["fixture"]});
    api.answer(
        &request,
        answer.clone(),
        json!({"type":"resident","id":hash("forged sender")}),
    );
    workflow_step(&f, &api, &settings, &mut driver);
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "interpreting"
    );
    let accepted: bool =
        f.s.connect()
            .unwrap()
            .query_row("SELECT accepted FROM workflow_receipts LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert!(!accepted);
    api.answer(&request, answer, request["to"].clone());
    workflow_step(&f, &api, &settings, &mut driver);
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "ready"
    );
}
#[test]
fn no_copy_grant_cannot_instantiate_a_worker() {
    let (f, api, mut settings, mut driver) = workflow_fixture(false);
    settings.allow_new_copies = false;
    for _ in 0..6 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "held"
    );
    assert!(api.actors.lock().unwrap().is_empty());
    assert!(driver.calls.is_empty());
}
#[test]
fn stale_reserved_allocation_releases_capacity_before_new_dispatch() {
    let (f, api, settings, mut driver) = workflow_fixture(false);
    for _ in 0..3 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "awaiting_allocation"
    );
    let held: i64 =
        f.s.connect()
            .unwrap()
            .query_row("SELECT holds_slot FROM work", [], |r| r.get(0))
            .unwrap();
    assert_eq!(held, 1);
    f.s.connect()
        .unwrap()
        .execute(
            "UPDATE ado_items SET raw=json_set(raw,'$.rev',2),revision=2 WHERE id=1",
            [],
        )
        .unwrap();
    for _ in 0..2 {
        workflow_step(&f, &api, &settings, &mut driver);
    }
    let held: i64 =
        f.s.connect()
            .unwrap()
            .query_row("SELECT holds_slot FROM work", [], |r| r.get(0))
            .unwrap();
    assert_eq!(held, 0);
    assert!(api.actors.lock().unwrap().is_empty());
}
#[test]
fn stale_observations_wait_and_resume_without_another_interpretation() {
    let (f, mut api, settings, mut driver) = workflow_fixture(false);
    api.auto_answer = false;
    workflow_step(&f, &api, &settings, &mut driver);
    let request = api
        .mail
        .lock()
        .unwrap()
        .values()
        .find(|m| m["subject"].as_str().unwrap().contains("interpretation"))
        .unwrap()
        .clone();
    f.s.connect()
        .unwrap()
        .execute(
            "UPDATE ado_items SET observed=?1 WHERE id=1",
            [now() - 31 * 60_000],
        )
        .unwrap();
    workflow_step(&f, &api, &settings, &mut driver);
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "interpreting"
    );
    f.s.connect()
        .unwrap()
        .execute("UPDATE ado_items SET observed=?1 WHERE id=1", [now()])
        .unwrap();
    api.answer(&request,json!({"protocol":"boxen.interpretation.v1","decision":"ready","scenarios":[{"id":"s1"}],"questions":[],"holds":[],"existing_work":{"unresolved_ownership":false},"evidence_refs":["fixture"]}),request["to"].clone());
    workflow_step(&f, &api, &settings, &mut driver);
    assert_eq!(
        workflow::snapshot(&f.s).unwrap()["attempts"][0]["stage"],
        "ready"
    );
    assert!(api.actors.lock().unwrap().is_empty());
}
#[test]
fn uncertain_clarification_comment_creation_reconciles_without_duplicate() {
    let (f, mut api, mut settings, mut driver) = workflow_fixture(false);
    api.auto_answer = false;
    settings.post_clarifications = true;
    driver.lose_question = true;
    workflow_step(&f, &api, &settings, &mut driver);
    let request = api
        .mail
        .lock()
        .unwrap()
        .values()
        .find(|m| m["subject"].as_str().unwrap().contains("interpretation"))
        .unwrap()
        .clone();
    api.answer(&request,json!({"protocol":"boxen.interpretation.v1","decision":"clarification_required","questions":[{"id":"which-field","question":"Which field is intended?","route":"work_item"}],"holds":[]}),request["to"].clone());
    for _ in 0..5 {
        workflow_step(&f, &api, &settings, &mut driver);
        f.s.connect()
            .unwrap()
            .execute("UPDATE workflow_questions SET next_due=0", [])
            .unwrap();
    }
    assert_eq!(
        driver
            .calls
            .iter()
            .filter(|v| *v == "post_question")
            .count(),
        1
    );
    let state: String =
        f.s.connect()
            .unwrap()
            .query_row("SELECT state FROM workflow_questions", [], |r| r.get(0))
            .unwrap();
    assert_eq!(state, "posted");
    assert!(api.actors.lock().unwrap().is_empty());
}

struct ApprovalApi {
    snapshot: Value,
    controls: Mutex<Vec<Value>>,
    lose: bool,
}
impl Boxen for ApprovalApi {
    fn call(&self, r: Value) -> Result<Value> {
        match r["op"].as_str().unwrap_or("") {
            "get" => Ok(self.snapshot.clone()),
            "control" => {
                self.controls.lock().unwrap().push(r);
                if self.lose {
                    bail!("Lost control response")
                }
                Ok(json!({"accepted":true}))
            }
            _ => bail!("Unexpected approval fixture operation"),
        }
    }
}
fn approval_fixture() -> (Fixture, ApprovalApi, PathBuf, Vec<Value>, Value) {
    let mut f = Fixture::new();
    fs::create_dir_all(&f.cfg.pack_root).unwrap();
    f.cfg.supervision = Some(supervision::Settings {
        notify_to: json!({"type":"client","machine":f.cfg.owner,"id":hash("observer")}),
        start_timeout_seconds: 30,
        inactivity_seconds: 60,
        report_grace_seconds: 10,
    });
    let browser = f.root.join("msedge.exe");
    let shell = f.root.join("powershell.exe");
    let profile = f.root.join("browser");
    fs::write(&browser, "native fixture, never executed").unwrap();
    fs::write(&shell, "never executed").unwrap();
    fs::create_dir_all(&profile).unwrap();
    let policy = approvals::Policy {
        enabled: true,
        rule_id: "assigned-browser".into(),
        native_edge_app_access: false,
        revision: 1,
        authority: "Explicit operator policy fixture".into(),
        browser_executable: browser,
        shells: vec![shell.clone()],
    };
    fs::write(
        f.root.join("approval-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let mut input = f.input(1);
    input["assignment"]["runtime"] = json!({"required":true,"reservation_id":"fixture","allocation_source":"Fixture allocation","frontend_port":4500,"backend_port":4501,"database_server":"local","database_name":"synthetic","browser_context":profile,"synthetic_tenant_id":100});
    let script = approvals::launch_script(&policy, &input["assignment"]).unwrap();
    let command = shlex::try_join([shell.to_str().unwrap(), "-Command", script.as_str()]).unwrap();
    let fake = Fake::new(&f.cfg);
    let result = f.s.assign(&f.cfg, &input).unwrap();
    f.s.reserve(1).unwrap();
    dispatch(&f.s, &f.cfg, &fake, result["job_id"].as_str().unwrap()).unwrap();
    let run = hash("approval run");
    f.s.connect()
        .unwrap()
        .execute("UPDATE supervision SET run=?1", [&run])
        .unwrap();
    let request = json!({"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"threadId":"native-fixture","turnId":"turn-fixture","itemId":"item-fixture","command":command,"cwd":input["assignment"]["worktree"]}});
    let events = vec![
        json!({"kind":"backend","data":{"method":"turn/started","params":{"turn":{"id":"turn-fixture"}}}}),
        json!({"kind":"backend","data":{"method":"item/started","params":{"item":{"id":"item-fixture","type":"commandExecution","command":command}}}}),
        json!({"kind":"backend","data":request}),
    ];
    let folder = f.cfg.boxen_home.join("runs").join(&run);
    fs::create_dir_all(&folder).unwrap();
    let journal = folder.join("events.jsonl");
    write_approval_events(&journal, &events);
    let snapshot = json!({"status":"waiting","backend_thread":"native-fixture","record":{"owner":f.cfg.owner,"spec":{"id":run,"thread_id":result["coder_id"]}}});
    (
        f,
        ApprovalApi {
            snapshot,
            controls: Mutex::new(vec![]),
            lose: false,
        },
        journal,
        events,
        input,
    )
}
fn write_approval_events(path: &Path, events: &[Value]) {
    fs::write(
        path,
        events.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
}
#[test]
fn assigned_browser_approval_is_once_bound_to_current_lease_and_restart_safe() {
    let (f, api, _, _, _) = approval_fixture();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
    let control = api.controls.lock().unwrap()[0].clone();
    assert_eq!(control["action"]["result"], json!({"decision":"accept"}));
    approvals::tick(&Store::open(&f.s.path).unwrap(), &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
    assert_eq!(
        approvals::snapshot(&f.s).unwrap()["decisions"][0]["state"],
        "submitted"
    );
}
#[test]
fn unknown_approval_control_outcome_is_not_resubmitted() {
    let (f, mut api, _, _, _) = approval_fixture();
    api.lose = true;
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
    assert_eq!(
        approvals::snapshot(&f.s).unwrap()["decisions"][0]["state"],
        "uncertain"
    );
}
#[test]
fn browser_policy_does_not_approve_changed_commands_scope_or_permissions() {
    for case in [
        "compound",
        "profile",
        "url",
        "executable",
        "shell",
        "turn",
        "thread",
        "cwd",
        "permissions",
        "method",
        "manual",
    ] {
        let (f, api, journal, mut events, _) = approval_fixture();
        match case {
            "turn"=>events[2]["data"]["params"]["turnId"]=json!("other-turn"),
            "thread"=>events[2]["data"]["params"]["threadId"]=json!("other-thread"),
            "cwd"=>events[2]["data"]["params"]["cwd"]=json!(f.root),
            "permissions"=>events[2]["data"]["params"]["additionalPermissions"]=json!({"network":{"enabled":true}}),
            "method"=>events[2]["data"]["method"]=json!("item/permissions/requestApproval"),
            "manual"=>events.push(json!({"kind":"control","data":{"type":"respond","request_id":"approval-1","result":{"decision":"decline"}}})),
            _=>{
                let command=events[2]["data"]["params"]["command"].as_str().unwrap();let mut words=shlex::split(command).unwrap();
                let last=words.len()-1;
                match case {
                    "compound"=>words[last].push_str("; Write-Output additional-effect"),
                    "profile"=>words[last]=words[last].replace("browser","different-profile"),
                    "url"=>words[last]=words[last].replace("localhost:4500","example.invalid:4500"),
                    "executable"=>words[last]=words[last].replace("msedge.exe","another.exe"),
                    "shell"=>words[0]=f.root.join("different.exe").to_string_lossy().into_owned(),
                    _=>unreachable!()
                }
                let changed=shlex::try_join(words.iter().map(String::as_str)).unwrap();events[2]["data"]["params"]["command"]=json!(changed);events[1]["data"]["params"]["item"]["command"]=json!(changed);
            }
        }
        write_approval_events(&journal, &events);
        approvals::tick(&f.s, &f.cfg, &api).unwrap();
        assert!(api.controls.lock().unwrap().is_empty(), "{case}");
    }
}
#[test]
fn lost_or_paused_browser_reservations_cannot_auto_approve() {
    for state in [
        "paused",
        "needs_refresh",
        "changed-version",
        "missing-reservation",
        "disabled",
    ] {
        let (f, api, _, _, _) = approval_fixture();
        let c = f.s.connect().unwrap();
        match state {
            "changed-version" => {
                c.execute("UPDATE work SET version=2", []).unwrap();
            }
            "missing-reservation" => {
                c.execute(
                    "DELETE FROM reservations WHERE resource LIKE 'browser:%'",
                    [],
                )
                .unwrap();
            }
            "disabled" => {
                let p = f.root.join("approval-policy.json");
                let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                v["enabled"] = json!(false);
                fs::write(p, v.to_string()).unwrap();
            }
            _ => {
                c.execute("UPDATE work SET state=?1", [state]).unwrap();
            }
        }
        approvals::tick(&f.s, &f.cfg, &api).unwrap();
        assert!(api.controls.lock().unwrap().is_empty(), "{state}");
    }
}
#[test]
fn partial_approval_journal_record_waits_for_completion() {
    use std::io::Write;
    let (f, api, journal, events, _) = approval_fixture();
    let text = events.iter().map(|v| format!("{v}\n")).collect::<String>();
    fs::write(&journal, &text[..text.len() - 1]).unwrap();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert!(api.controls.lock().unwrap().is_empty());
    fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
}

fn native_edge_fixture() -> (Fixture, ApprovalApi, PathBuf, Vec<Value>) {
    let (f, api, journal, mut events, _) = approval_fixture();
    let policy_path = f.root.join("approval-policy.json");
    let mut policy: Value = serde_json::from_slice(&fs::read(&policy_path).unwrap()).unwrap();
    policy["native_edge_app_access"] = json!(true);
    policy["revision"] = json!(2);
    fs::write(policy_path, policy.to_string()).unwrap();
    events[1] = json!({"kind":"backend","data":{"method":"item/started","params":{"item":{"id":"native-call","type":"mcpToolCall","server":"cua_repl","tool":"js","pluginId":"unified-computer-use@openai-bundled"}}}});
    events[2] = json!({"kind":"backend","data":{"id":"native-app","method":"mcpServer/elicitation/request","params":{"threadId":"native-fixture","turnId":"turn-fixture","serverName":"cua_repl","mode":"form","requestedSchema":{"type":"object","properties":{}},"_meta":{"callId":"native-call","connector_id":"computer-use","codex_approval_kind":"mcp_tool_call","tool_params":{"app":"msedge.exe"},"x-codex-turn-metadata":{"thread_id":"native-fixture","turn_id":"turn-fixture"}}}}});
    write_approval_events(&journal, &events);
    (f, api, journal, events)
}
#[test]
fn native_edge_access_is_explicit_lease_bound_and_once_without_persistence() {
    let (f, api, _, _) = native_edge_fixture();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
    assert_eq!(
        api.controls.lock().unwrap()[0]["action"]["result"],
        json!({"action":"accept","content":{}})
    );
    approvals::tick(&Store::open(&f.s.path).unwrap(), &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
}
#[test]
fn native_app_policy_rejects_other_apps_connectors_forms_and_unassigned_work() {
    for case in [
        "disabled",
        "other-app",
        "extra-app-params",
        "connector",
        "server",
        "plugin",
        "schema",
        "call",
        "thread",
        "turn",
        "lease",
        "answered",
    ] {
        let (f, api, journal, mut e) = native_edge_fixture();
        match case {
            "disabled" => {
                let p = f.root.join("approval-policy.json");
                let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                v["native_edge_app_access"] = json!(false);
                fs::write(p, v.to_string()).unwrap();
            }
            "other-app" => {
                e[2]["data"]["params"]["_meta"]["tool_params"]["app"] = json!("other.exe")
            }
            "extra-app-params" => {
                e[2]["data"]["params"]["_meta"]["tool_params"]["other"] = json!(true)
            }
            "connector" => {
                e[2]["data"]["params"]["_meta"]["connector_id"] = json!("other-connector")
            }
            "server" => e[2]["data"]["params"]["serverName"] = json!("other-server"),
            "plugin" => e[1]["data"]["params"]["item"]["pluginId"] = json!("other-plugin"),
            "schema" => {
                e[2]["data"]["params"]["requestedSchema"]["properties"]["credential"] =
                    json!({"type":"string"})
            }
            "call" => e[2]["data"]["params"]["_meta"]["callId"] = json!("unknown-call"),
            "thread" => {
                e[2]["data"]["params"]["_meta"]["x-codex-turn-metadata"]["thread_id"] =
                    json!("other-thread")
            }
            "turn" => {
                e[2]["data"]["params"]["_meta"]["x-codex-turn-metadata"]["turn_id"] =
                    json!("other-turn")
            }
            "lease" => {
                f.s.connect()
                    .unwrap()
                    .execute(
                        "DELETE FROM reservations WHERE resource LIKE 'browser:%'",
                        [],
                    )
                    .unwrap();
            }
            "answered" => {
                e.push(json!({"kind":"request_answered","data":{"request_id":"native-app"}}))
            }
            _ => unreachable!(),
        }
        write_approval_events(&journal, &e);
        approvals::tick(&f.s, &f.cfg, &api).unwrap();
        assert!(api.controls.lock().unwrap().is_empty(), "{case}");
    }
}
#[test]
fn approval_projection_upgrade_finds_pending_native_requests_without_repeating_answers() {
    let (f, api, _, _) = native_edge_fixture();
    let run = api.snapshot["record"]["spec"]["id"].as_str().unwrap();
    f.s.connect().unwrap().execute("INSERT INTO approval_streams VALUES(?1,100,'{\"turn\":null,\"items\":{},\"pending\":{}}')",[run]).unwrap();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
    f.s.connect()
        .unwrap()
        .execute(
            "UPDATE approval_streams SET projection='{\"turn\":null,\"items\":{},\"pending\":{}}'",
            [],
        )
        .unwrap();
    approvals::tick(&f.s, &f.cfg, &api).unwrap();
    assert_eq!(api.controls.lock().unwrap().len(), 1);
}
