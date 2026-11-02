use super::*;
struct Fixture {
    root: PathBuf,
    cfg: Config,
    store: Store,
    owner: String,
    peer: String,
}
impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
        private_dir(&parent).unwrap();
        let root = parent.join(random());
        private_dir(&root).unwrap();
        let cfg = Config {
            version: 1,
            label: "Scoped fixture".into(),
            thread_id: uuid::Uuid::new_v4().to_string(),
            client_id: String::new(),
            codex_binary: root.join("fake-codex"),
            boxen_home: root.join("boxen"),
            url: "http://127.0.0.1:42425".into(),
            peer: None,
            address: String::new(),
        };
        let mut cfg = cfg;
        cfg.client_id = hash(&format!("boxen-codex-desktop:{}", cfg.thread_id));
        let store = Store::open(&root.join("config.json"), &cfg).unwrap();
        let owner = random();
        let peer = owner.clone();
        store.pin(&owner, &peer).unwrap();
        Self {
            root,
            cfg,
            store,
            owner,
            peer,
        }
    }
    fn page(&self, n: i64) -> Value {
        json!({"messages":[{"id":hash(&format!("message-{n}")),"seq":n,"owner":self.owner,"accepted_on":self.peer,"to":{"type":"client","machine":self.owner,"id":self.cfg.client_id},"sender":{"type":"resident","id":hash("sender")},"reply_to":{"type":"resident","id":hash("sender")},"subject":"Support input","body":"Attributed peer text, not human authority","delivery":"queued","presented_in":null}],"next_after":n})
    }
    fn ingest(&self, n: i64) -> String {
        self.store
            .ingest(&self.cfg, &self.owner, &self.peer, &self.page(n))
            .unwrap();
        hash(&format!(
            "boxen:{}:{}",
            self.peer,
            hash(&format!("message-{n}"))
        ))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .canonicalize()
            .unwrap();
        let target = self.root.canonicalize().unwrap();
        assert!(target.starts_with(&parent) && target != parent);
        fs::remove_dir_all(target).unwrap();
    }
}
#[test]
fn intake_retries_are_immutable_and_cursor_gaps_are_valid() {
    let f = Fixture::new();
    let id = f.ingest(12);
    assert_eq!(f.store.cursor().unwrap(), 12);
    assert_eq!(
        f.store
            .ingest(&f.cfg, &f.owner, &f.peer, &f.page(12))
            .unwrap(),
        0
    );
    let mut conflict = f.page(12);
    conflict["messages"][0]["body"] = json!("changed");
    assert!(
        f.store
            .ingest(&f.cfg, &f.owner, &f.peer, &conflict)
            .is_err()
    );
    assert_eq!(
        f.store.inspect(&id).unwrap()["message"]["body"],
        "Attributed peer text, not human authority"
    );
    assert_eq!(f.store.cursor().unwrap(), 12);
}
#[test]
fn failed_page_is_atomic_and_never_advances_past_retention() {
    let f = Fixture::new();
    let mut page = f.page(4);
    let mut bad = f.page(7)["messages"][0].clone();
    bad["to"]["id"] = json!(random());
    page["messages"].as_array_mut().unwrap().push(bad);
    page["next_after"] = json!(7);
    assert!(f.store.ingest(&f.cfg, &f.owner, &f.peer, &page).is_err());
    assert_eq!(f.store.cursor().unwrap(), 0);
    assert_eq!(f.store.status(&f.cfg).unwrap()["retained_deliveries"], 0);
}
#[test]
fn offline_reply_is_idempotent_and_preserves_destination_and_cause() {
    let f = Fixture::new();
    let incoming = f.ingest(5);
    let args =
        json!({"message_id":incoming,"body":"Offline reply","idempotency_key":"cause:reply"});
    let first = f.store.save_reply(&f.cfg, &args, true).unwrap();
    assert_eq!(first["state"], "saved_locally");
    assert_eq!(
        f.store.save_reply(&f.cfg, &args, true).unwrap()["id"],
        first["id"]
    );
    let mut conflict = args.clone();
    conflict["body"] = json!("different");
    assert!(f.store.save_reply(&f.cfg, &conflict, true).is_err());
    let p: String = f
        .store
        .connect()
        .unwrap()
        .query_row("SELECT payload FROM replies", [], |r| r.get(0))
        .unwrap();
    let p: Value = serde_json::from_str(&p).unwrap();
    assert_eq!(p["to"]["id"], hash("sender"));
    assert_eq!(p["in_reply_to"], hash("message-5"));
    assert!(p.get("sender").is_none());
}
#[test]
fn explicit_send_preserves_cause_and_observation_stops_uncertain_notice_retry() {
    let f = Fixture::new();
    let mid = f.ingest(6);
    let (notice, _) = f.store.next_notice().unwrap().unwrap();
    f.store.recover().unwrap();
    f.store.snapshot(0).unwrap();
    assert!(f.store.next_notice().unwrap().is_none());
    f.store.disposition(&mid, "resolved").unwrap();
    let cause = hash("earlier-message");
    let args = json!({"to":{"type":"resident","id":hash("recipient")},"subject":"Follow up","body":"Explicit send","idempotency_key":"follow-up","in_reply_to":cause});
    f.store.save_reply(&f.cfg, &args, false).unwrap();
    let payload: String = f
        .store
        .connect()
        .unwrap()
        .query_row("SELECT payload FROM replies", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["in_reply_to"],
        cause
    );
    assert!(
        f.store
            .connect()
            .unwrap()
            .query_row("SELECT observed FROM notices WHERE id=?1", [notice], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap()
            .is_some()
    );
}
#[test]
fn accepted_notices_coalesce_until_observation_without_ack_gate() {
    let f = Fixture::new();
    f.ingest(2);
    let (n, ids) = f.store.next_notice().unwrap().unwrap();
    assert_eq!(ids.len(), 1);
    f.store.record_notice(&n, true).unwrap();
    f.ingest(3);
    assert!(f.store.next_notice().unwrap().is_none());
    f.store.snapshot(0).unwrap();
    assert!(f.store.next_notice().unwrap().is_some());
}
#[test]
fn crash_recovery_reuses_notice_ids_and_final_dispositions_stay_final() {
    let f = Fixture::new();
    let mid = f.ingest(6);
    let (n, ids) = f.store.next_notice().unwrap().unwrap();
    f.store.recover().unwrap();
    assert_eq!(f.store.next_notice().unwrap().unwrap(), (n.clone(), ids));
    f.store.record_notice(&n, true).unwrap();
    f.store.disposition(&mid, "resolved").unwrap();
    assert!(f.store.disposition(&mid, "acknowledged").is_err());
    assert!(f.store.next_notice().unwrap().is_none());
}
#[test]
fn owner_pin_and_participant_config_cannot_drift() {
    let f = Fixture::new();
    assert!(f.store.pin(&random(), &f.peer).is_err());
    let mut changed = f.cfg.clone();
    changed.thread_id = uuid::Uuid::new_v4().to_string();
    assert!(Store::open(&f.root.join("config.json"), &changed).is_err());
}
#[test]
fn notice_contains_only_ids_and_attribution_not_peer_body() {
    let f = Fixture::new();
    let id = f.ingest(1);
    let text = notice(&f.cfg, &f.root.join("config.json"), &[id]).unwrap();
    assert!(!text.contains("Attributed peer text"));
    assert!(text.contains("NOT authored by the human"));
    assert!(text.contains("--rpc inspect"));
}
#[test]
fn control_origin_is_loopback_only() {
    let f = Fixture::new();
    let mut c = f.cfg.clone();
    c.url = "http://example.com:42425".into();
    assert!(c.validate().is_err());
    c.url = "http://127.0.0.1:42425/redirect?token=x".into();
    assert!(c.validate().is_err());
}
