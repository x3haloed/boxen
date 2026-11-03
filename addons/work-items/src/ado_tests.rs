use super::*;

fn fixture() -> Result<(Store, Config, Fake)> {
    let base = std::env::var_os("BOXEN_WORK_ITEMS_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let s = Store::open(
        base.join(format!("ado-{}", id()))
            .join("work-items.sqlite3"),
    )?;
    let cfg = Config {
        enabled: true,
        organization: "https://dev.azure.com/example".into(),
        project: "Example".into(),
        project_id: "11111111-1111-4111-8111-111111111111".into(),
        assignee: "operator@example.invalid".into(),
        marker: "Implementation: V2".into(),
        board_column_field: "Example.Column".into(),
        board_done_field: "Example.Done".into(),
        ..Config::default()
    };
    configure(&s, &cfg)?;
    let wi = json!({"id":1,"rev":1,"fields":{"System.TeamProject":cfg.project,"System.Title":"Synthetic ticket","System.State":"Doing","System.AssignedTo":{"uniqueName":cfg.assignee},"System.Description":"<p><b>Implementation:</b> V2</p>",cfg.board_column_field.clone():"Doing",cfg.board_done_field.clone():false},"relations":[{"rel":"ArtifactLink","url":"vstfs:///Git/PullRequestId/11111111-1111-4111-8111-111111111111%2F22222222-2222-4222-8222-222222222222%2F10"},{"rel":"AttachedFile","url":"https://dev.azure.com/example/attachment","attributes":{"name":"screen.png"}}]});
    let fake = Fake {
        wi,
        discover: vec![1],
        comment_version: 1,
        discussion_version: 1,
        pr_status: "active".into(),
        fail_comment: false,
        repeat_token: false,
        omit_revision: None,
        read_items: vec![],
        fail_first: false,
        incomplete_comments: false,
    };
    Ok((s, cfg, fake))
}
struct Fake {
    wi: Value,
    discover: Vec<i64>,
    comment_version: i64,
    discussion_version: i64,
    pr_status: String,
    fail_comment: bool,
    repeat_token: bool,
    omit_revision: Option<i64>,
    read_items: Vec<i64>,
    fail_first: bool,
    incomplete_comments: bool,
}
impl Source for Fake {
    fn read(&mut self, r: Request) -> Result<Value> {
        Ok(match r {
            Request::Discover(after) => {
                json!({"queryType":"flat","workItems":self.discover.iter().filter(|n|**n>after).take(100).map(|n|json!({"id":n})).collect::<Vec<_>>()})
            }
            Request::Item(n) => {
                self.read_items.push(n);
                if n == 1 && self.fail_first {
                    bail!("synthetic item unavailable");
                }
                let mut wi = self.wi.clone();
                wi["id"] = json!(n);
                wi
            }
            Request::Updates(_, skip) => {
                let n = self.wi["rev"].as_i64().unwrap();
                json!({"value":(1..=n).skip(skip).take(100).filter(|n|Some(*n)!=self.omit_revision).map(|n|json!({"rev":n,"id":n,"fields":{"System.Title":{"oldValue":"Before","newValue":format!("Revision {n}")}}})).collect::<Vec<_>>()})
            }
            Request::Comments(_, None) => {
                if self.incomplete_comments {
                    return Ok(
                        json!({"totalCount":2,"comments":[{"id":100,"version":1,"text":"Only one returned"}]}),
                    );
                }
                json!({"comments":[{"id":100,"version":self.comment_version,"text":format!("Comment version {}",self.comment_version),"createdBy":{"displayName":"Synthetic reviewer"}}],"continuationToken":"next"})
            }
            Request::Comments(_, Some(_)) => {
                if self.fail_comment {
                    bail!("synthetic second comments page unavailable");
                }
                json!({"comments":[{"id":101,"version":1,"text":"Second page"}],"continuationToken":if self.repeat_token {json!("next")}else{Value::Null}})
            }
            Request::PullRequest(_, n) => {
                json!({"pullRequestId":n,"title":"Synthetic fix","status":self.pr_status,"isDraft":true,"lastMergeSourceCommit":{"commitId":"abc"},"discussionThreads":[{"id":1,"status":"active","comments":[{"id":1,"author":{"displayName":"Synthetic reviewer"},"content":format!("Review feedback {}",self.discussion_version)}]}]})
            }
        })
    }
}
fn event_count(s: &Store) -> Result<i64> {
    Ok(s.connect()?
        .query_row("SELECT count(*) FROM ado_events", [], |r| r.get(0))?)
}
fn revision(s: &Store) -> Result<i64> {
    Ok(s.connect()?
        .query_row("SELECT revision FROM ado_items WHERE id=1", [], |r| {
            r.get(0)
        })?)
}

#[test]
fn mirror_reopen_deduplicates_and_keeps_reassigned_closed_items() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    assert_eq!(cfg.interval_minutes, 15);
    sync_once(&s, &cfg, &mut api)?;
    let count = event_count(&s)?;
    let reopened = Store::open(&s.path)?;
    sync_once(&reopened, &cfg, &mut api)?;
    assert_eq!(event_count(&s)?, count);
    api.discover.clear();
    api.wi["rev"] = json!(3);
    api.wi["fields"]["System.AssignedTo"] = json!({"uniqueName":"reviewer@example.invalid"});
    api.wi["fields"]["System.State"] = json!("Closed");
    api.comment_version = 2;
    api.pr_status = "completed".into();
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 3);
    assert!(!snapshot(&s, 0)?["items"][0]["eligible"].as_bool().unwrap());
    assert_eq!(
        s.connect()?
            .query_row("SELECT count(*) FROM ado_revisions", [], |r| r
                .get::<_, i64>(0))?,
        3
    );
    assert!(event_count(&s)? > count);
    let after = event_count(&s)?;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(event_count(&s)?, after);
    assert_eq!(
        s.connect()?
            .query_row("SELECT count(*) FROM work", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(
        s.connect()?
            .query_row("SELECT count(*) FROM assignments", [], |r| r
                .get::<_, i64>(0))?,
        0
    );
    Ok(())
}
#[test]
fn comments_and_pr_changes_are_detected_without_a_ticket_revision() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    sync_once(&s, &cfg, &mut api)?;
    let before = event_count(&s)?;
    api.comment_version = 2;
    api.pr_status = "completed".into();
    // Removing the PR link does not remove it from our watch set.
    api.wi["relations"] = json!([]);
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 1);
    assert_eq!(event_count(&s)?, before + 3);
    let snapshot = snapshot(&s, 0)?;
    assert!(
        snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["summary"].as_str().unwrap().contains("completed"))
    );
    Ok(())
}
#[test]
fn partial_read_failure_does_not_advance_item_checkpoint_or_duplicate_replay() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    sync_once(&s, &cfg, &mut api)?;
    let count = event_count(&s)?;
    api.wi["rev"] = json!(2);
    api.fail_comment = true;
    assert!(sync_once(&s, &cfg, &mut api).is_err());
    assert_eq!(revision(&s)?, 1);
    assert_eq!(event_count(&s)?, count);
    api.fail_comment = false;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 2);
    let count = event_count(&s)?;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(event_count(&s)?, count);
    Ok(())
}
#[test]
fn missing_revisions_and_repeated_comment_tokens_fail_without_checkpoint_loss() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    api.wi["rev"] = json!(3);
    api.omit_revision = Some(2);
    assert!(sync_once(&s, &cfg, &mut api).is_err());
    assert_eq!(event_count(&s)?, 0);
    api.omit_revision = None;
    api.repeat_token = true;
    assert!(sync_once(&s, &cfg, &mut api).is_err());
    assert_eq!(event_count(&s)?, 0);
    api.repeat_token = false;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 3);
    Ok(())
}

#[test]
fn incomplete_comments_without_a_continuation_token_do_not_advance_checkpoint() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    api.incomplete_comments = true;
    assert!(sync_once(&s, &cfg, &mut api).is_err());
    assert_eq!(event_count(&s)?, 0);
    api.incomplete_comments = false;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 1);
    Ok(())
}
#[test]
fn discovery_and_revision_pagination_are_complete_and_one_failure_does_not_block_other_items()
-> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    api.discover = (1..=101).collect();
    api.wi["fields"]["System.Description"] = json!("outside scope");
    assert_eq!(sync_once(&s, &cfg, &mut api)?, 101);
    assert_eq!(api.read_items.len(), 101);
    api.discover = vec![1, 2];
    api.wi["fields"]["System.Description"] = json!(cfg.marker);
    api.wi["rev"] = json!(205);
    api.fail_first = true;
    assert!(sync_once(&s, &cfg, &mut api).is_err());
    assert_eq!(
        s.connect()?
            .query_row("SELECT revision FROM ado_items WHERE id=2", [], |r| r
                .get::<_, i64>(0))?,
        205
    );
    api.fail_first = false;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 205);
    Ok(())
}
#[test]
fn check_now_is_durable_across_reopen() -> Result<()> {
    let (s, _, _) = fixture()?;
    s.connect()?
        .execute("UPDATE ado_sync SET requested=0", [])?;
    request_check(&s)?;
    let reopened = Store::open(&s.path)?;
    assert_eq!(snapshot(&reopened, 0)?["status"]["requested"], true);
    Ok(())
}
#[test]
fn description_marker_ignores_script_and_style_text() -> Result<()> {
    let (_, cfg, mut api) = fixture()?;
    api.wi["fields"]["System.Description"] =
        json!("<script>Implementation: V2</script><style>Implementation: V2</style>");
    assert!(!eligibility(&cfg, &api.wi).0);
    api.wi["fields"]["System.Description"] = json!("<b>Implementation:</b>&nbsp;V2");
    assert!(eligibility(&cfg, &api.wi).0);
    Ok(())
}

#[test]
fn explicit_selection_retains_ineligible_source_without_changing_discovery_rules() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    api.discover.clear();
    api.wi["fields"]["System.AssignedTo"]["uniqueName"] = json!("reviewer@example.invalid");
    sync_once(&s, &cfg, &mut api)?;
    assert!(s.packet(1).is_err());
    sync_selected_item(&s, &cfg, &mut api, 1)?;
    assert_eq!(s.packet(1)?["eligible"], false);
    assert!(
        !s.packet(1)?["evidence"]["entities"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let before = event_count(&s)?;
    api.comment_version += 1;
    sync_once(&s, &cfg, &mut api)?;
    assert!(event_count(&s)? > before);
    api.wi["fields"]["System.TeamProject"] = json!("Another project");
    assert!(sync_selected_item(&s, &cfg, &mut api, 2).is_err());
    assert!(s.packet(2).is_err());
    Ok(())
}

#[test]
fn pr_discussion_text_changes_are_mirrored_without_ticket_revision_or_vote_changes() -> Result<()> {
    let (s, cfg, mut api) = fixture()?;
    sync_once(&s, &cfg, &mut api)?;
    let before = event_count(&s)?;
    api.discussion_version = 2;
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(revision(&s)?, 1);
    assert_eq!(event_count(&s)?, before + 1);
    let raw: String = s.connect()?.query_row(
        "SELECT raw FROM ado_entities WHERE kind='pull_request' LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    assert!(raw.contains("Review feedback 2"));
    sync_once(&s, &cfg, &mut api)?;
    assert_eq!(event_count(&s)?, before + 1);
    Ok(())
}
