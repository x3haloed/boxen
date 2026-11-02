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
