//! Read-only Azure DevOps observation. This module never dispatches agents.
use crate::{Store, id, now, process::ManagedChild};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "ado_tests.rs"]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub organization: String,
    pub project: String,
    pub project_id: String,
    pub assignee: String,
    pub marker: String,
    pub board_column_field: String,
    pub board_done_field: String,
    pub interval_minutes: i64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            organization: String::new(),
            project: String::new(),
            project_id: String::new(),
            assignee: String::new(),
            marker: String::new(),
            board_column_field: String::new(),
            board_done_field: String::new(),
            interval_minutes: 15,
        }
    }
}
impl Config {
    pub fn source(&self) -> String {
        format!(
            "{}|{}",
            self.organization.trim_end_matches('/'),
            self.project_id
        )
    }
    fn validate(&self) -> Result<()> {
        let org = self
            .organization
            .strip_prefix("https://dev.azure.com/")
            .context("Use https://dev.azure.com/ORGANIZATION")?;
        ensure!(
            !org.is_empty() && org.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid organization"
        );
        ensure!(
            uuid::Uuid::parse_str(&self.project_id).is_ok(),
            "Project ID must be a GUID"
        );
        ensure!(
            !self.project.trim().is_empty()
                && self.project.len() <= 200
                && !self.assignee.trim().is_empty()
                && self.assignee.len() <= 200,
            "Project and assignee required"
        );
        ensure!(
            !self.marker.is_empty() && self.marker.len() <= 200,
            "Description marker required"
        );
        ensure!(
            (1..=1440).contains(&self.interval_minutes),
            "Poll interval must be 1–1440 minutes"
        );
        Ok(())
    }
}

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS ado_config(id INTEGER PRIMARY KEY CHECK(id=1),json TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS ado_sync(source TEXT PRIMARY KEY,last_started INTEGER,last_success INTEGER,next_due INTEGER NOT NULL DEFAULT 0,running INTEGER NOT NULL DEFAULT 0,requested INTEGER NOT NULL DEFAULT 0,error TEXT,failures INTEGER NOT NULL DEFAULT 0,examined INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS ado_items(source TEXT NOT NULL,id INTEGER NOT NULL,revision INTEGER NOT NULL,title TEXT NOT NULL,state TEXT NOT NULL,eligible INTEGER NOT NULL,reason TEXT NOT NULL,raw TEXT NOT NULL,observed INTEGER NOT NULL,PRIMARY KEY(source,id));
    CREATE TABLE IF NOT EXISTS ado_revisions(source TEXT NOT NULL,item INTEGER NOT NULL,revision INTEGER NOT NULL,raw TEXT NOT NULL,PRIMARY KEY(source,item,revision));
    CREATE TABLE IF NOT EXISTS ado_entities(source TEXT NOT NULL,item INTEGER NOT NULL,kind TEXT NOT NULL,entity TEXT NOT NULL,raw TEXT NOT NULL,PRIMARY KEY(source,item,kind,entity));
    CREATE TABLE IF NOT EXISTS ado_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,item INTEGER NOT NULL,kind TEXT NOT NULL,event_key TEXT NOT NULL,summary TEXT NOT NULL,before_json TEXT,after_json TEXT NOT NULL,observed INTEGER NOT NULL,UNIQUE(source,item,kind,event_key));
    CREATE INDEX IF NOT EXISTS ado_events_source ON ado_events(source,seq DESC);")?;
    Ok(())
}
pub fn config(s: &Store) -> Result<Config> {
    let raw: Option<String> = s
        .connect()?
        .query_row("SELECT json FROM ado_config WHERE id=1", [], |r| r.get(0))
        .optional()?;
    raw.map(|v| serde_json::from_str(&v).map_err(Into::into))
        .unwrap_or_else(|| Ok(Config::default()))
}
pub fn configure(s: &Store, cfg: &Config) -> Result<()> {
    cfg.validate()?;
    let mut c = s.connect()?;
    let tx = c.transaction()?;
    tx.execute(
        "INSERT INTO ado_config VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json",
        [serde_json::to_string(cfg)?],
    )?;
    tx.execute("INSERT INTO ado_sync(source,requested) VALUES(?1,1) ON CONFLICT(source) DO UPDATE SET requested=1,next_due=0", [cfg.source()])?;
    tx.commit()?;
    Ok(())
}
pub fn request_check(s: &Store) -> Result<()> {
    let cfg = config(s)?;
    ensure!(cfg.enabled, "Enable work item synchronization first");
    s.connect()?.execute("INSERT INTO ado_sync(source,requested) VALUES(?1,1) ON CONFLICT(source) DO UPDATE SET requested=1", [cfg.source()])?;
    Ok(())
}
pub fn snapshot(s: &Store, before: i64) -> Result<Value> {
    let cfg = config(s)?;
    let c = s.connect()?;
    let source = cfg.source();
    let status=c.query_row("SELECT last_started,last_success,next_due,running,requested,error,failures,examined FROM ado_sync WHERE source=?1", [&source], |r|Ok(json!({"last_started":r.get::<_,Option<i64>>(0)?,"last_success":r.get::<_,Option<i64>>(1)?,"next_due":r.get::<_,i64>(2)?,"running":r.get::<_,bool>(3)?,"requested":r.get::<_,bool>(4)?,"error":r.get::<_,Option<String>>(5)?,"failures":r.get::<_,i64>(6)?,"examined":r.get::<_,i64>(7)?}))).optional()?.unwrap_or(json!({}));
    let mut q=c.prepare("SELECT id,revision,title,state,eligible,reason,observed FROM ado_items WHERE source=?1 ORDER BY id DESC LIMIT 200")?;
    let items=q.query_map([&source], |r|Ok(json!({"id":r.get::<_,i64>(0)?,"revision":r.get::<_,i64>(1)?,"title":preview(&r.get::<_,String>(2)?,512),"state":r.get::<_,String>(3)?,"eligible":r.get::<_,bool>(4)?,"reason":r.get::<_,String>(5)?,"observed":r.get::<_,i64>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let count: i64 = c.query_row(
        "SELECT count(*) FROM ado_items WHERE source=?1",
        [&source],
        |r| r.get(0),
    )?;
    let mut q=c.prepare("SELECT seq,item,kind,summary,observed FROM ado_events WHERE source=?1 AND (?2=0 OR seq<?2) ORDER BY seq DESC LIMIT 50")?;
    let events=q.query_map(params![source,before],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"item":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"summary":preview(&r.get::<_,String>(3)?,600),"observed":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        json!({"config":cfg,"status":status,"items":items,"item_count":count,"events":events,"read_only":true}),
    )
}
pub fn event(s: &Store, seq: i64) -> Result<Value> {
    let raw: (Option<String>, String) = s.connect()?.query_row(
        "SELECT before_json,after_json FROM ado_events WHERE source=?1 AND seq=?2",
        params![config(s)?.source(), seq],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    // Bounded preview; full originals remain in the local SQLite mirror.
    let pretty = |s: &str, n: usize| -> String {
        let value = serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| s.into());
        preview(&value, n)
    };
    Ok(
        json!({"before":pretty(raw.0.as_deref().unwrap_or("(first observation)"),12000),"after":pretty(&raw.1,24000)}),
    )
}
fn preview(s: &str, n: usize) -> String {
    let mut v: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        v.push_str("\n[Preview truncated; complete record retained in mirror]");
    }
    v
}

pub fn eligibility(cfg: &Config, wi: &Value) -> (bool, String) {
    let f = &wi["fields"];
    let description = scraper::Html::parse_fragment(f["System.Description"].as_str().unwrap_or(""));
    let mut text = String::new();
    for node in description.tree.nodes() {
        if let scraper::node::Node::Text(t) = node.value()
            && !node.ancestors().any(|a|matches!(a.value(),scraper::node::Node::Element(e) if matches!(e.name(),"script"|"style"|"template"))) {
                text.push_str(t);
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let reason = if f["System.TeamProject"].as_str() != Some(&cfg.project) {
        "Different project"
    } else if !f["System.AssignedTo"]["uniqueName"]
        .as_str()
        .is_some_and(|v| v.eq_ignore_ascii_case(&cfg.assignee))
    {
        "Assigned elsewhere"
    } else if !text.contains(&cfg.marker) {
        "Description marker absent"
    } else if matches!(
        f["System.State"].as_str(),
        Some("Done" | "Closed" | "Removed")
    ) {
        "Completed or removed"
    } else if !matches!(f[&cfg.board_column_field].as_str(), Some("To Do" | "Doing")) {
        "Outside To Do / Doing"
    } else if f[&cfg.board_done_field].as_bool() != Some(false) {
        "Board Done flag true or missing"
    } else {
        "Eligible"
    };
    (reason == "Eligible", reason.into())
}

#[derive(Debug, Clone)]
pub enum Request {
    Discover(i64),
    Item(i64),
    Updates(i64, usize),
    Comments(i64, Option<String>),
    PullRequest(String, i64),
}
pub trait Source {
    fn read(&mut self, request: Request) -> Result<Value>;
}
pub struct AzureCli {
    cfg: Config,
    scratch: PathBuf,
}
impl AzureCli {
    pub fn new(cfg: Config, s: &Store) -> Self {
        Self {
            cfg,
            scratch: s.path.parent().unwrap().join("azure-queries"),
        }
    }
    fn command(&self, args: Vec<String>) -> Result<Value> {
        let mut cmd = if let Some(bin) = std::env::var_os("BOXEN_WORK_ITEMS_AZURE_PYTHON") {
            let mut c = Command::new(bin);
            c.args(["-I", "-B", "-X", "utf8", "-m", "azure.cli"]);
            c
        } else if cfg!(windows) {
            let bin = PathBuf::from("C:/Program Files/Microsoft SDKs/Azure/CLI2/python.exe");
            ensure!(
                bin.is_file(),
                "Azure CLI not found; set BOXEN_WORK_ITEMS_AZURE_PYTHON to its Python executable"
            );
            let mut c = Command::new(bin);
            c.args(["-I", "-B", "-X", "utf8", "-m", "azure.cli"]);
            c
        } else {
            Command::new("az")
        };
        cmd.args(args)
            .args([
                "--org",
                &self.cfg.organization,
                "--only-show-errors",
                "--output",
                "json",
            ])
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = ManagedChild::spawn(&mut cmd)?;
        let stdout = child.child.stdout.take().context("CLI stdout")?;
        let stderr = child.child.stderr.take().context("CLI stderr")?;
        let out = thread::spawn(move || {
            let mut b = Vec::new();
            stdout
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut b)
                .map(|_| b)
        });
        let err = thread::spawn(move || {
            let mut b = Vec::new();
            stderr.take(65537).read_to_end(&mut b).map(|_| b)
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.child.try_wait()? {
                break status;
            }
            if start.elapsed() > Duration::from_secs(60) {
                let _ = child.child.kill();
                let _ = child.child.wait();
                bail!(
                    "Azure CLI read timed out after 60 seconds; sign-in or connectivity may need attention"
                );
            }
            thread::sleep(Duration::from_millis(50));
        };
        drop(child);
        let out = out
            .join()
            .map_err(|_| anyhow::anyhow!("CLI stdout reader failed"))??;
        let _err = err
            .join()
            .map_err(|_| anyhow::anyhow!("CLI stderr reader failed"))??;
        ensure!(
            status.success(),
            "Azure read failed (CLI exit {}); authentication/network may require attention",
            status.code().unwrap_or(-1)
        );
        ensure!(
            out.len() <= 16 * 1024 * 1024,
            "Azure response exceeds 16 MiB; checkpoint retained"
        );
        serde_json::from_slice(&out).context("Azure CLI returned invalid UTF-8 JSON")
    }
}

impl Source for AzureCli {
    fn read(&mut self, r: Request) -> Result<Value> {
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let invoke = |area: &str, resource: &str| {
            strings(&[
                "devops",
                "invoke",
                "--area",
                area,
                "--resource",
                resource,
                "--api-version",
                "7.1",
                "--route-parameters",
            ])
        };
        match r {
            Request::Discover(after) => {
                std::fs::create_dir_all(&self.scratch)?;
                let path = self.scratch.join(format!("{}.json", id()));
                let quote = |s: &str| s.replace('\'', "''");
                let query = format!(
                    "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = '{}' AND [System.AssignedTo] = '{}' AND [System.Id] > {} ORDER BY [System.Id] ASC",
                    quote(&self.cfg.project),
                    quote(&self.cfg.assignee),
                    after
                );
                std::fs::write(&path, serde_json::to_vec(&json!({"query":query}))?)?;
                let mut args = invoke("wit", "wiql");
                args.extend([
                    format!("project={}", self.cfg.project_id),
                    "--http-method".into(),
                    "POST".into(),
                    "--query-parameters".into(),
                    "$top=100".into(),
                    "--in-file".into(),
                    path.to_string_lossy().into_owned(),
                ]);
                let result = self.command(args);
                let _ = std::fs::remove_file(path);
                result
            }
            Request::Item(item) => self.command(strings(&[
                "boards",
                "work-item",
                "show",
                "--id",
                &item.to_string(),
                "--expand",
                "all",
            ])),
            Request::Updates(item, skip) => {
                let mut a = invoke("wit", "updates");
                a.extend([
                    format!("project={}", self.cfg.project_id),
                    format!("id={item}"),
                    "--query-parameters".into(),
                    format!("$skip={skip}"),
                    "$top=100".into(),
                ]);
                self.command(a)
            }
            Request::Comments(item, token) => {
                let mut a = strings(&[
                    "devops",
                    "invoke",
                    "--area",
                    "wit",
                    "--resource",
                    "comments",
                    "--api-version",
                    "7.1-preview",
                    "--route-parameters",
                ]);
                a.extend([
                    format!("project={}", self.cfg.project_id),
                    format!("workItemId={item}"),
                    "--query-parameters".into(),
                    "$top=100".into(),
                    "includeDeleted=true".into(),
                ]);
                if let Some(t) = token {
                    a.push(format!("continuationToken={t}"));
                }
                self.command(a)
            }
            Request::PullRequest(repo, pr) => {
                let mut a = invoke("git", "pullRequests");
                a.extend([
                    format!("project={}", self.cfg.project_id),
                    format!("repositoryId={repo}"),
                    format!("pullRequestId={pr}"),
                ]);
                let mut result = self.command(a)?;
                let mut threads = invoke("git", "pullRequestThreads");
                threads.extend([
                    format!("project={}", self.cfg.project_id),
                    format!("repositoryId={repo}"),
                    format!("pullRequestId={pr}"),
                ]);
                let response = self.command(threads)?;
                ensure!(
                    token(&response).is_none(),
                    "Unexpected PR-thread continuation; preserve prior checkpoint"
                );
                let values = response["value"]
                    .as_array()
                    .or_else(|| response.as_array())
                    .context("PR threads response missing array")?;
                if let Some(count) = response["count"].as_u64() {
                    ensure!(
                        count as usize == values.len(),
                        "Incomplete PR thread response"
                    );
                }
                let mut ids = BTreeSet::new();
                for t in values {
                    ensure!(
                        t["id"].as_i64().is_some_and(|id| ids.insert(id)),
                        "Duplicate/missing PR thread ID"
                    );
                }
                result["discussionThreads"] = json!(values);
                Ok(result)
            }
        }
    }
}

fn pull_refs(wi: &Value) -> Result<BTreeSet<(String, i64)>> {
    let mut result = BTreeSet::new();
    for r in wi["relations"].as_array().into_iter().flatten() {
        let url = r["url"].as_str().unwrap_or("");
        if r["rel"] == "ArtifactLink" && url.starts_with("vstfs:///Git/PullRequestId/") {
            let tail = url
                .trim_start_matches("vstfs:///Git/PullRequestId/")
                .replace("%2F", "/")
                .replace("%2f", "/");
            let parts = tail.split('/').collect::<Vec<_>>();
            ensure!(
                parts.len() == 3 && uuid::Uuid::parse_str(parts[1]).is_ok(),
                "Malformed linked PR reference"
            );
            result.insert((parts[1].to_string(), parts[2].parse()?));
        }
    }
    Ok(result)
}
fn normalized_pr(pr: &Value) -> Value {
    let mut result = json!({});
    for k in [
        "pullRequestId",
        "title",
        "description",
        "status",
        "isDraft",
        "sourceRefName",
        "targetRefName",
        "lastMergeSourceCommit",
        "lastMergeTargetCommit",
        "lastMergeCommit",
        "mergeStatus",
        "autoCompleteSetBy",
        "closedDate",
        "reviewers",
        "discussionThreads",
        "repository",
    ] {
        if let Some(v) = pr.get(k) {
            result[k] = v.clone();
        }
    }
    result
}
fn token(page: &Value) -> Option<String> {
    ["continuationToken", "continuation_token"]
        .iter()
        .filter_map(|k| page.get(k))
        .find_map(|v| {
            if v.is_null() {
                None
            } else {
                let s = v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string());
                if s.is_empty() { None } else { Some(s) }
            }
        })
}
pub fn sync_once(s: &Store, cfg: &Config, api: &mut impl Source) -> Result<usize> {
    cfg.validate()?;
    let source = cfg.source();
    let c = s.connect()?;
    let mut q = c.prepare("SELECT id FROM ado_items WHERE source=?1")?;
    let mut ids = q
        .query_map([&source], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    drop(q);
    drop(c);
    let mut after = 0;
    loop {
        let page = api.read(Request::Discover(after))?;
        ensure!(page["queryType"] == "flat", "Expected flat discovery query");
        let rows = page["workItems"]
            .as_array()
            .context("Missing discovery work items")?;
        ensure!(rows.len() <= 100, "Oversized discovery page");
        for row in rows {
            let n = row["id"].as_i64().context("Invalid work item ID")?;
            ensure!(n > after, "Discovery page did not advance monotonically");
            after = n;
            ids.insert(n);
        }
        ensure!(
            ids.len() <= 10000,
            "More than 10,000 watched/candidate items; scan incomplete"
        );
        if rows.len() < 100 {
            break;
        }
    }
    let start = Instant::now();
    let mut failures = Vec::new();
    for (index, item) in ids.iter().enumerate() {
        ensure!(
            config(s)? == *cfg,
            "Sync configuration changed; scan will restart"
        );
        ensure!(
            start.elapsed() < Duration::from_secs(1800),
            "Scan exceeded 30 minutes; per-item checkpoints retained"
        );
        if let Err(e) = sync_item(s, cfg, api, *item) {
            failures.push(format!("Work item {item}: {e:#}"));
        }
        s.connect()?.execute(
            "UPDATE ado_sync SET examined=?2 WHERE source=?1",
            params![source, index as i64 + 1],
        )?;
    }
    ensure!(
        failures.is_empty(),
        "{} item(s) failed; successful items retained. {}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
    Ok(ids.len())
}
pub fn sync_item(s: &Store, cfg: &Config, api: &mut impl Source, item: i64) -> Result<()> {
    sync_item_inner(s, cfg, api, item, false)
}
pub fn sync_selected_item(s: &Store, cfg: &Config, api: &mut impl Source, item: i64) -> Result<()> {
    sync_item_inner(s, cfg, api, item, true)
}
fn sync_item_inner(
    s: &Store,
    cfg: &Config,
    api: &mut impl Source,
    item: i64,
    selected: bool,
) -> Result<()> {
    let source = cfg.source();
    let old: Option<i64> = s
        .connect()?
        .query_row(
            "SELECT revision FROM ado_items WHERE source=?1 AND id=?2",
            params![source, item],
            |r| r.get(0),
        )
        .optional()?;
    let wi = api.read(Request::Item(item))?;
    ensure!(
        wi["id"].as_i64() == Some(item),
        "Work item identity mismatch"
    );
    let rev = wi["rev"]
        .as_i64()
        .filter(|n| *n > 0)
        .context("Invalid work item revision")?;
    ensure!(
        old.is_none_or(|n| rev >= n),
        "Source revision went backwards"
    );
    let (eligible, reason) = eligibility(cfg, &wi);
    if selected {
        ensure!(
            wi["fields"]["System.TeamProject"] == cfg.project,
            "Selected item belongs to another project"
        );
    }
    if old.is_none() && !eligible && !selected {
        return Ok(());
    }
    let mut revisions = Vec::new();
    if old != Some(rev) {
        for page in 0..500 {
            let result = api.read(Request::Updates(item, page * 100))?;
            let rows = result["value"]
                .as_array()
                .context("Missing revision updates")?;
            ensure!(rows.len() <= 100, "Oversized revision page");
            for u in rows {
                let r = u["rev"].as_i64().context("Missing update revision")?;
                if r > old.unwrap_or(0) && r <= rev {
                    revisions.push(u.clone());
                }
            }
            if rows.len() < 100 {
                break;
            }
            ensure!(page < 499, "Revision pagination limit reached");
        }
        let seen = revisions
            .iter()
            .filter_map(|r| r["rev"].as_i64())
            .collect::<BTreeSet<_>>();
        ensure!(
            (old.unwrap_or(0) + 1..=rev).all(|r| seen.contains(&r)),
            "Revision history incomplete; checkpoint retained"
        );
    }
    let mut comments = BTreeMap::new();
    let mut comment_total = None;
    let mut next = None;
    let mut tokens = BTreeSet::new();
    for page in 0..500 {
        let result = api.read(Request::Comments(item, next.clone()))?;
        if let Some(total) = result["totalCount"].as_u64() {
            ensure!(
                comment_total.is_none_or(|n| n == total),
                "Comment count changed during pagination; retry scan"
            );
            comment_total = Some(total);
        }
        let rows = result["comments"]
            .as_array()
            .context("Missing comments page")?;
        for comment in rows {
            let cid = comment["id"]
                .as_i64()
                .or_else(|| comment["commentId"].as_i64())
                .context("Missing comment ID")?;
            ensure!(
                comment["version"].as_i64().is_some_and(|n| n > 0),
                "Missing comment version"
            );
            ensure!(
                comments.insert(cid, comment.clone()).is_none(),
                "Duplicate comment across pages; retry scan"
            );
        }
        next = token(&result);
        if let Some(t) = &next {
            ensure!(
                tokens.insert(t.clone()) && page < 499,
                "Comment pagination failed to advance"
            );
        } else {
            break;
        }
    }
    ensure!(
        comment_total.is_none_or(|n| n == comments.len() as u64),
        "Comments incomplete; checkpoint retained"
    );
    // Previously linked PRs stay watched even if someone removes their WI relation.
    let mut refs = pull_refs(&wi)?;
    let c = s.connect()?;
    let mut q = c.prepare(
        "SELECT entity FROM ado_entities WHERE source=?1 AND item=?2 AND kind='pull_request'",
    )?;
    for key in q.query_map(params![source, item], |r| r.get::<_, String>(0))? {
        let key = key?;
        if let Some((repo, n)) = key.split_once(':') {
            refs.insert((repo.into(), n.parse()?));
        }
    }
    drop(q);
    drop(c);
    let mut prs = Vec::new();
    for (repo, n) in refs {
        let pr = api.read(Request::PullRequest(repo.clone(), n))?;
        ensure!(
            pr["pullRequestId"].as_i64() == Some(n),
            "PR identity mismatch"
        );
        prs.push((format!("{repo}:{n}"), normalized_pr(&pr)));
    }
    commit_item(
        s,
        cfg,
        &wi,
        (eligible, &reason),
        &revisions,
        &comments,
        &prs,
    )
}
fn commit_item(
    s: &Store,
    cfg: &Config,
    wi: &Value,
    eligibility: (bool, &str),
    revisions: &[Value],
    comments: &BTreeMap<i64, Value>,
    prs: &[(String, Value)],
) -> Result<()> {
    let (eligible, reason) = eligibility;
    let mut c = s.connect()?;
    let tx = c.transaction()?;
    // A configuration edit cannot let an in-flight scan publish under old rules.
    let current: Option<String> = tx
        .query_row("SELECT json FROM ado_config WHERE id=1", [], |r| r.get(0))
        .optional()?;
    if let Some(raw) = current {
        ensure!(
            serde_json::from_str::<Config>(&raw)? == *cfg,
            "Sync configuration changed; scan will restart"
        );
    }
    let source = cfg.source();
    let item = wi["id"].as_i64().context("Missing ID")?;
    let t = now();
    for revision in revisions {
        let rev = revision["rev"].as_i64().context("Missing revision")?;
        let raw = serde_json::to_string(revision)?;
        tx.execute(
            "INSERT OR IGNORE INTO ado_revisions VALUES(?1,?2,?3,?4)",
            params![source, item, rev, raw],
        )?;
        let fields = revision["fields"]
            .as_object()
            .map(|o| {
                o.keys()
                    .filter(|k| {
                        !matches!(
                            k.as_str(),
                            "System.Watermark"
                                | "System.Rev"
                                | "System.AuthorizedDate"
                                | "System.RevisedDate"
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let mut before = json!({"fields":{}});
        let mut after = json!({"revision":rev,"revisedBy":revision["revisedBy"],"revisedDate":revision["revisedDate"],"fields":{},"relations":revision["relations"]});
        if let Some(changes) = revision["fields"].as_object() {
            for (field, delta) in changes {
                before["fields"][field] = delta["oldValue"].clone();
                after["fields"][field] = delta["newValue"].clone();
            }
        }
        tx.execute("INSERT OR IGNORE INTO ado_events(source,item,kind,event_key,summary,before_json,after_json,observed) VALUES(?1,?2,'revision',?3,?4,?5,?6,?7)",params![source,item,rev.to_string(),format!("Revision {rev}: {fields}"),serde_json::to_string(&before)?,serde_json::to_string(&after)?,t])?;
    }
    for (cid, comment) in comments {
        record_entity(&tx, &source, item, "comment", &cid.to_string(), comment, t)?;
    }
    for (key, pr) in prs {
        record_entity(&tx, &source, item, "pull_request", key, pr, t)?;
    }
    let old: Option<String> = tx
        .query_row(
            "SELECT raw FROM ado_items WHERE source=?1 AND id=?2",
            params![source, item],
            |r| r.get(0),
        )
        .optional()?;
    let refs = json!({"relations":wi["relations"]});
    record_entity(&tx, &source, item, "relations", "relations", &refs, t)?;
    let title = wi["fields"]["System.Title"].as_str().unwrap_or("Untitled");
    let state = wi["fields"]["System.State"].as_str().unwrap_or("Unknown");
    tx.execute("INSERT INTO ado_items VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(source,id) DO UPDATE SET revision=excluded.revision,title=excluded.title,state=excluded.state,eligible=excluded.eligible,reason=excluded.reason,raw=excluded.raw,observed=excluded.observed",params![source,item,wi["rev"].as_i64(),title,state,eligible,reason,serde_json::to_string(wi)?,t])?;
    if old.is_none() {
        tx.execute("INSERT INTO ado_events(source,item,kind,event_key,summary,after_json,observed) VALUES(?1,?2,'discovered','initial',?3,?4,?5)",params![source,item,format!("Started watching: {title}"),serde_json::to_string(wi)?,t])?;
    }
    crate::source_changed(&tx, &source, item)?;
    tx.commit()?;
    Ok(())
}
fn record_entity(
    tx: &rusqlite::Transaction<'_>,
    source: &str,
    item: i64,
    kind: &str,
    key: &str,
    value: &Value,
    t: i64,
) -> Result<()> {
    let raw = serde_json::to_string(value)?;
    let old: Option<String> = tx
        .query_row(
            "SELECT raw FROM ado_entities WHERE source=?1 AND item=?2 AND kind=?3 AND entity=?4",
            params![source, item, kind, key],
            |r| r.get(0),
        )
        .optional()?;
    if old.as_deref() == Some(&raw) {
        return Ok(());
    }
    tx.execute("INSERT INTO ado_entities VALUES(?1,?2,?3,?4,?5) ON CONFLICT(source,item,kind,entity) DO UPDATE SET raw=excluded.raw",params![source,item,kind,key,raw])?;
    let summary = match kind {
        "comment" => format!(
            "Comment {key} v{}{}: {}",
            value["version"],
            if value["isDeleted"] == true {
                " (deleted)"
            } else {
                ""
            },
            preview(value["text"].as_str().unwrap_or(""), 180)
        ),
        "pull_request" => format!(
            "PR {}: {}{} · {}",
            value["pullRequestId"],
            value["status"].as_str().unwrap_or("unknown"),
            if value["isDraft"] == true {
                " (draft)"
            } else {
                ""
            },
            value["title"].as_str().unwrap_or("")
        ),
        _ => "Attachment / relationship references updated".into(),
    };
    tx.execute("INSERT INTO ado_events(source,item,kind,event_key,summary,before_json,after_json,observed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![source,item,kind,id(),summary,old,raw,t])?;
    Ok(())
}
