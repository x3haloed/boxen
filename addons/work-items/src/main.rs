use anyhow::{Context, Result, bail, ensure};
use boxen_work_items::{
    transport::{Boxen, Control},
    *,
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

fn option(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    if let Some(i) = args.iter().position(|s| s == name) {
        ensure!(i + 1 < args.len(), "Option value required");
        args.remove(i);
        return Ok(Some(args.remove(i)));
    }
    Ok(None)
}
fn lock(home: &Path, name: &str) -> Result<fs::File> {
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(home.join(name))?;
    f.try_lock_exclusive()
        .context("Another worker owns this operation")?;
    Ok(f)
}
fn read_json(path: &str) -> Result<Value> {
    let p = Path::new(path);
    ensure!(
        p.is_absolute() && fs::metadata(p)?.len() < 512 * 1024,
        "Absolute JSON file under 512 KiB required"
    );
    Ok(serde_json::from_str(
        fs::read_to_string(p)?.trim_start_matches('\u{feff}'),
    )?)
}
fn poll(s: &Store, home: &Path, item: Option<i64>) -> Result<()> {
    let _lock = lock(home, "azure-read.lock")?;
    let cfg = ado::config(s)?;
    ensure!(cfg.enabled, "Mirror is disabled");
    let mut source = ado::AzureCli::new(cfg.clone(), s);
    if let Some(item) = item {
        return ado::sync_item(s, &cfg, &mut source, item);
    }
    s.connect()?.execute(
        "UPDATE ado_sync SET running=1,requested=0,last_started=?1,error=NULL,examined=0",
        [now()],
    )?;
    event("mirror.poll_started", json!({}));
    let result = ado::sync_once(s, &cfg, &mut source);
    match result {
        Ok(n) => {
            s.connect()?.execute("UPDATE ado_sync SET running=0,last_success=?1,next_due=?2,error=NULL,failures=0,examined=?3",rusqlite::params![now(),now()+cfg.interval_minutes*60_000,n as i64])?;
            event("mirror.poll_succeeded", json!({"examined":n}));
            Ok(())
        }
        Err(e) => {
            s.connect()?.execute("UPDATE ado_sync SET running=0,next_due=?1+MIN(900000,60000*(1 << MIN(failures,4))),error=?2,failures=failures+1",rusqlite::params![now(),e.to_string()])?;
            event("mirror.poll_failed", json!({"detail":e.to_string()}));
            Err(e)
        }
    }
}
fn inspect_assignment(s: &Store, cfg: &Config, input: &Value) -> Result<()> {
    let folder = s.path.parent().unwrap().join("inspections");
    fs::create_dir_all(&folder)?;
    let path = folder.join(format!("{}.json", hash(&input.to_string())));
    let text = input["assignment"].to_string();
    if path.exists() {
        ensure!(
            fs::read_to_string(&path)? == text,
            "Inspection request conflicts"
        );
    } else {
        fs::write(&path, text)?;
    }
    // This invokes the installed read-only helper, never an assignment-provided
    // executable. Full Access is the agreed local process trust boundary.
    let mut cmd = std::process::Command::new("pwsh");
    cmd.args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(cfg.pack_root.join("scripts/Inspect-Assignment.ps1"))
        .arg("-AssignmentPath")
        .arg(&path)
        .arg("-PackRoot")
        .arg(&cfg.pack_root);
    use std::{io::Read, process::Stdio, time::Instant};
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child =
        process::ManagedChild::spawn(&mut cmd).context("Assignment inspector could not start")?;
    let stdout = child
        .child
        .stdout
        .take()
        .context("Inspector stdout missing")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(256 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "Assignment inspector exceeded its deadline"
        );
        thread::sleep(Duration::from_millis(50));
    };
    drop(child);
    let stdout = reader
        .join()
        .map_err(|_| anyhow::anyhow!("Inspector reader failed"))??;
    ensure!(
        stdout.len() <= 256 * 1024,
        "Assignment inspector exceeded its output bound"
    );
    fs::write(path.with_extension("report.json"), &stdout)?;
    ensure!(
        status.success(),
        "Assignment inspector rejected inputs; the retained inspection request and helper report identify missing inputs"
    );
    let report: Value =
        serde_json::from_slice(&stdout).context("Assignment inspector did not return JSON")?;
    ensure!(
        report["assignment_inputs_valid"] == true
            && report["pack"]["manifest_sha256"] == cfg.pack_hash,
        "Assignment inspection/pinned pack failed"
    );
    Ok(())
}
fn execute() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|a| a == "--help") {
        println!(
            "boxen-work-items --home ABS init --config-file ABS --legacy-db ABS\nboxen-work-items --home ABS run\nboxen-work-items --home ABS status\nboxen-work-items --home ABS packet ITEM\nboxen-work-items --home ABS poll [--item ITEM]\nboxen-work-items --home ABS assign --request-file ABS\nboxen-work-items --home ABS release --request-file ABS\nboxen-work-items --home ABS messages [--after SEQ]\nExplicit local operator CLI. No automatic assignment, GUI or Azure writes. Ordinary mail is not parsed into operator commands."
        );
        return Ok(());
    }
    let home = PathBuf::from(option(&mut args, "--home")?.context("--home required")?);
    ensure!(home.is_absolute(), "Absolute home required");
    fs::create_dir_all(&home)?;
    let s = Store::open(home.join("work-items.sqlite3"))?;
    let action = args.first().context("Command required")?.clone();
    args.remove(0);
    if action == "init" {
        let config_file = option(&mut args, "--config-file")?.context("--config-file required")?;
        let legacy = option(&mut args, "--legacy-db")?.context("--legacy-db required")?;
        ensure!(args.is_empty(), "Unexpected init arguments");
        let cfg: Config = serde_json::from_value(read_json(&config_file)?)?;
        cfg.validate()?;
        let cfgpath = home.join("config.json");
        if cfgpath.exists() {
            ensure!(
                fs::read_to_string(&cfgpath)? == serde_json::to_string_pretty(&cfg)?,
                "Existing service configuration differs"
            );
            println!("{}", json!({"initialized":true,"idempotent":true}));
            return Ok(());
        }
        let c = s.connect()?;
        let imported: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE k='legacy_import')",
            [],
            |r| r.get(0),
        )?;
        if !imported {
            s.import_legacy(Path::new(&legacy))?;
        }
        let _control = Control::new(&cfg)?;
        let bytes = serde_json::to_vec_pretty(&cfg)?;
        let temporary = home.join(format!("config-{}.tmp", id()));
        fs::write(&temporary, bytes)?;
        fs::hard_link(&temporary, &cfgpath)?;
        fs::remove_file(temporary)?;
        println!(
            "{}",
            json!({"initialized":true,"automatic_assignment":false})
        );
        return Ok(());
    }
    let cfg = Config::read(&home)?;
    // Pin the destination/client/template, while allowing an operator to tune
    // capacity without rewriting accepted assignment versions.
    let pin=hash(&json!({"owner":cfg.owner,"client":cfg.client_id,"definition":cfg.definition_id,"revision":cfg.definition_revision,"pack":cfg.pack_hash}).to_string());
    let c = s.connect()?;
    c.execute("INSERT OR IGNORE INTO meta VALUES('binding',?1)", [&pin])?;
    ensure!(
        c.query_row("SELECT v FROM meta WHERE k='binding'", [], |r| r
            .get::<_, String>(0))?
            == pin,
        "Service configuration identity changed"
    );
    drop(c);
    match action.as_str() {
        "status" => {
            ensure!(args.is_empty(), "Unexpected status arguments");
            println!("{}", serde_json::to_string_pretty(&s.status()?)?);
        }
        "packet" => {
            ensure!(args.len() == 1, "packet ITEM required");
            println!(
                "{}",
                serde_json::to_string_pretty(&s.packet(args[0].parse()?)?)?
            );
        }
        "poll" => {
            let item = option(&mut args, "--item")?
                .map(|s| s.parse())
                .transpose()?;
            ensure!(args.is_empty(), "Unexpected poll arguments");
            poll(&s, &home, item)?;
        }
        "assign" => {
            let file = option(&mut args, "--request-file")?.context("--request-file required")?;
            ensure!(args.is_empty(), "Unexpected assign arguments");
            let input = read_json(&file)?;
            let known: bool = s.connect()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM assignment_requests WHERE k=?1)",
                [input["idempotency_key"].as_str().unwrap_or("")],
                |r| r.get(0),
            )?;
            if !known {
                inspect_assignment(&s, &cfg, &input)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&s.assign(&cfg, &input)?)?
            );
        }
        "release" => {
            let file = option(&mut args, "--request-file")?.context("--request-file required")?;
            ensure!(args.is_empty(), "Unexpected release arguments");
            let input = read_json(&file)?;
            let result = transport::release(
                &s,
                &Control::new(&cfg)?,
                input["job_id"].as_str().context("job_id required")?,
                input["outcome"].as_str().context("outcome required")?,
                input["resources_cleaned"].as_bool().unwrap_or(false),
                input["note"].as_str().unwrap_or(""),
            )?;
            println!("{result}");
        }
        "messages" => {
            let after = option(&mut args, "--after")?
                .unwrap_or("0".into())
                .parse::<u64>()?;
            ensure!(args.is_empty(), "Unexpected messages arguments");
            println!(
                "{}",
                Control::new(&cfg)?
                    .call(json!({"op":"client_inbox","client_id":cfg.client_id,"after":after}))?
            );
        }
        "run" => {
            ensure!(args.is_empty(), "Unexpected run arguments");
            let _worker = lock(&home, "worker.lock")?;
            let poll_store = s.clone();
            let poll_home = home.clone();
            s.connect()?.execute(
                "UPDATE ado_sync SET running=0,requested=1 WHERE running=1",
                [],
            )?;
            thread::spawn(move || {
                loop {
                    let tick = || -> Result<()> {
                        let c = poll_store.connect()?;
                        let due: bool = c.query_row(
                            "SELECT requested=1 OR next_due<=?1 FROM ado_sync",
                            [now()],
                            |r| r.get(0),
                        )?;
                        if due && ado::config(&poll_store)?.enabled {
                            let _ = poll(&poll_store, &poll_home, None);
                        }
                        Ok(())
                    };
                    if let Err(e) = tick() {
                        event("mirror.supervisor_error", json!({"detail":e.to_string()}));
                    }
                    thread::sleep(Duration::from_secs(5));
                }
            });
            event(
                "service.started",
                json!({"capacity":cfg.capacity,"automatic_assignment":false}),
            );
            loop {
                let tick = || -> Result<()> {
                    s.reserve(cfg.capacity)?;
                    let jobs=s.connect()?.prepare("SELECT id FROM work WHERE state='reserved' AND COALESCE(CAST((SELECT v FROM meta WHERE k='retry:'||work.id) AS INTEGER),0)<=?1 ORDER BY created LIMIT 16")?.query_map([now()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    if !jobs.is_empty() {
                        let api = Control::new(&cfg)?;
                        for job in jobs {
                            if let Err(e) = transport::dispatch(&s, &cfg, &api, &job) {
                                let c = s.connect()?;
                                let old: Option<String> = c.query_row(
                                    "SELECT error FROM work WHERE id=?1",
                                    [&job],
                                    |r| r.get(0),
                                )?;
                                if old.as_deref() != Some(&e.to_string()) {
                                    event(
                                        "assignment.dispatch_unavailable",
                                        json!({"job":job,"detail":e.to_string()}),
                                    );
                                }
                                c.execute(
                                    "UPDATE work SET error=?2 WHERE id=?1",
                                    rusqlite::params![job, e.to_string()],
                                )?;
                                c.execute("INSERT INTO meta VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",rusqlite::params![format!("retry:{job}"),(now()+15000).to_string()])?;
                            }
                        }
                    }
                    Ok(())
                };
                if let Err(e) = tick() {
                    event("service.dispatch_error", json!({"detail":e.to_string()}));
                    thread::sleep(Duration::from_secs(15));
                }
                thread::sleep(Duration::from_secs(2));
            }
        }
        _ => bail!("Unknown command"),
    }
    Ok(())
}
fn main() {
    if let Err(e) = execute() {
        eprintln!("boxen-work-items: {e}");
        std::process::exit(1)
    }
}
