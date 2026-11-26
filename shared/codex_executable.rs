//! Shared process-time Codex discovery for the daemon and desktop adapter.
//! Explicit paths are pins; `auto` and `codex` discover on every launch.
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::{Duration, Instant, SystemTime},
};

const DISCOVERY_BUDGET: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const SLOW_RETRY_BUDGET: Duration = Duration::from_secs(5);
const SLOW_RETRY_MAX: usize = 2;
const SLOW_RETRY_TIMEOUT: Duration = Duration::from_secs(3);
const SLOW_RETRY_WITH_FALLBACK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct Selection {
    pub path: PathBuf,
    pub version: Option<String>,
    pub automatic: bool,
    pub probe_fallback: Option<String>,
    pub skipped_candidates: Vec<String>,
}

pub fn automatic(configured: &str) -> bool {
    matches!(configured, "auto" | "codex")
}

// Re-resolution is safe only after spawn returned NotFound: no child accepted work.
// Errors after a successful spawn are outside this helper and are never retried.
pub fn launch<T>(
    configured: &str,
    os: &str,
    mut start: impl FnMut(&Selection) -> io::Result<T>,
    mut observe: impl FnMut(&Selection, bool),
) -> io::Result<T> {
    launch_with(|| resolve(configured, os), &mut start, &mut observe)
}
fn launch_with<T>(
    mut locate: impl FnMut() -> io::Result<Selection>,
    start: &mut impl FnMut(&Selection) -> io::Result<T>,
    observe: &mut impl FnMut(&Selection, bool),
) -> io::Result<T> {
    let first = locate()?;
    observe(&first, false);
    match start(&first) {
        Err(error)
            if first.automatic
                && error.kind() == io::ErrorKind::NotFound
                && !first.path.is_file() =>
        {
            let second = locate()?;
            observe(&second, true);
            start(&second)
        }
        result => result,
    }
}

pub fn resolve(configured: &str, os: &str) -> io::Result<Selection> {
    if !automatic(configured) {
        return Ok(Selection {
            path: configured.into(),
            version: None,
            automatic: false,
            probe_fallback: None,
            skipped_candidates: Vec::new(),
        });
    }
    let home = std::env::var_os(if os == "windows" {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(PathBuf::from);
    let desktop = if os == "windows" {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|p| p.join("AppData/Local")))
            .map(|p| p.join("OpenAI/Codex/bin"))
    } else {
        None
    };
    let candidates = fallback_candidates(
        os,
        std::env::consts::ARCH,
        home.as_deref(),
        std::env::var_os("PATH").as_deref(),
    );
    resolve_candidates(desktop.as_deref(), &candidates, probe)
}

#[derive(Clone, Debug, PartialEq)]
struct Fingerprint {
    target: PathBuf,
    bytes: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, u32),
}
fn fingerprint(path: &Path) -> io::Result<Fingerprint> {
    let target = fs::canonicalize(path)?;
    let metadata = fs::metadata(&target)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "candidate is not a file",
        ));
    }
    Ok(Fingerprint {
        target,
        bytes: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino(), metadata.mode())
        },
    })
}
#[derive(Clone)]
struct Verified {
    fingerprint: Fingerprint,
    version: String,
}
fn verified_cache() -> &'static Mutex<HashMap<PathBuf, Verified>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Verified>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
// Every launch still discovers and probes current candidates. Only a timeout on
// the identical previously verified file can use its cached version. Changed,
// deleted, nonzero-exit and malformed candidates never inherit that evidence.
fn checked_probe(
    path: &Path,
    timeout: Duration,
    probe: &mut impl FnMut(&Path, Duration) -> io::Result<String>,
) -> io::Result<(String, bool)> {
    let before = fingerprint(path)?;
    let result = probe(path, timeout).and_then(|version| {
        if Version::parse(&version).is_none() {
            return Err(io::Error::other("Codex version response invalid"));
        }
        if fingerprint(path)? != before {
            return Err(io::Error::other(
                "Codex executable changed during version probe",
            ));
        }
        Ok(version)
    });
    match result {
        Ok(version) => {
            let mut cache = verified_cache().lock().unwrap();
            if cache.len() >= 64 {
                cache.retain(|p, _| p == path);
            }
            cache.insert(
                path.to_owned(),
                Verified {
                    fingerprint: before,
                    version: version.clone(),
                },
            );
            Ok((version, false))
        }
        Err(error) => {
            let cached = verified_cache().lock().unwrap().get(path).cloned();
            if error.kind() == io::ErrorKind::TimedOut
                && fingerprint(path).ok().as_ref() == Some(&before)
                && let Some(saved) = cached.filter(|v| v.fingerprint == before)
            {
                return Ok((saved.version, true));
            }
            verified_cache().lock().unwrap().remove(path);
            Err(error)
        }
    }
}
enum ProbeOutcome {
    Verified { version: String, used_cache: bool },
    TimedOut,
    Failed,
}

fn desktop_installations(desktop: Option<&Path>) -> Vec<(SystemTime, PathBuf)> {
    let mut installations = Vec::new();
    if let Some(root) = desktop {
        for entry in fs::read_dir(root).into_iter().flatten().flatten().take(256) {
            let directory = entry.path();
            let path = directory.join("codex.exe");
            if directory.is_dir() && path.is_file() {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                installations.push((modified, path));
            }
        }
    }
    installations.sort_by(|a, b| b.cmp(a));
    installations.truncate(16);
    installations
}

fn dedup_fallback(fallback: &[PathBuf]) -> Vec<&Path> {
    let mut seen = HashSet::new();
    fallback
        .iter()
        .map(PathBuf::as_path)
        .filter(|p| p.is_file() && seen.insert(*p))
        .take(16)
        .collect()
}

fn resolve_candidates(
    desktop: Option<&Path>,
    fallback: &[PathBuf],
    mut probe: impl FnMut(&Path, Duration) -> io::Result<String>,
) -> io::Result<Selection> {
    let mut pass1_spent = Duration::ZERO;
    let mut retry_spent = Duration::ZERO;
    let mut retries_used = 0usize;
    let mut failures: Vec<(PathBuf, String)> = Vec::new();
    let skipped_for = |failures: &[(PathBuf, String)], chosen: &Path| -> Vec<String> {
        failures
            .iter()
            .filter(|(p, _)| p != chosen)
            .map(|(_, reason)| reason.clone())
            .collect()
    };
    let probe_fast = |path: &Path,
                      pass1_spent: &mut Duration,
                      failures: &mut Vec<(PathBuf, String)>,
                      probe: &mut dyn FnMut(&Path, Duration) -> io::Result<String>|
     -> ProbeOutcome {
        let remaining = DISCOVERY_BUDGET.saturating_sub(*pass1_spent);
        if remaining.is_zero() {
            failures.push((
                path.to_owned(),
                format!("{}: discovery deadline reached", path.display()),
            ));
            return ProbeOutcome::Failed;
        }
        let started = Instant::now();
        let result = checked_probe(path, remaining.min(PROBE_TIMEOUT), &mut |p, d| probe(p, d));
        *pass1_spent = pass1_spent.saturating_add(started.elapsed());
        match result {
            Ok((version, used_cache)) => ProbeOutcome::Verified {
                version,
                used_cache,
            },
            Err(error) => {
                failures.push((path.to_owned(), format!("{}: {}", path.display(), error)));
                if error.kind() == io::ErrorKind::TimedOut {
                    ProbeOutcome::TimedOut
                } else {
                    ProbeOutcome::Failed
                }
            }
        }
    };
    let probe_slow_retry = |path: &Path,
                            cap: Duration,
                            retries_used: &mut usize,
                            retry_spent: &mut Duration,
                            failures: &mut Vec<(PathBuf, String)>,
                            probe: &mut dyn FnMut(&Path, Duration) -> io::Result<String>|
     -> Option<String> {
        if *retries_used >= SLOW_RETRY_MAX {
            return None;
        }
        let remaining = SLOW_RETRY_BUDGET.saturating_sub(*retry_spent);
        if remaining.is_zero() {
            return None;
        }
        *retries_used += 1;
        let started = Instant::now();
        let result = checked_probe(path, remaining.min(cap), &mut |p, d| probe(p, d));
        *retry_spent = retry_spent.saturating_add(started.elapsed());
        match result {
            Ok((version, _)) => Some(version),
            Err(error) => {
                failures.push((
                    path.to_owned(),
                    format!("{}: slow retry: {}", path.display(), error),
                ));
                None
            }
        }
    };

    // Desktop installations (Windows): probe all current versions and give
    // timed-out installations a bounded slow retry before settling on an older
    // version, so a cold upgraded binary does not silently downgrade.
    let mut valid_desktop = Vec::new();
    let mut desktop_timed_out = Vec::new();
    for (modified, path) in desktop_installations(desktop) {
        match probe_fast(&path, &mut pass1_spent, &mut failures, &mut probe) {
            ProbeOutcome::Verified {
                version,
                used_cache,
            } => {
                if let Some(number) = Version::parse(&version) {
                    let note = used_cache.then(|| {
                        "unchanged previously verified executable; version probe timed out"
                            .to_owned()
                    });
                    valid_desktop.push((number, modified, path, version, note));
                }
            }
            ProbeOutcome::TimedOut => desktop_timed_out.push((modified, path)),
            ProbeOutcome::Failed => {}
        }
    }
    if !desktop_timed_out.is_empty() {
        let cap = if valid_desktop.is_empty() {
            SLOW_RETRY_TIMEOUT
        } else {
            SLOW_RETRY_WITH_FALLBACK_TIMEOUT
        };
        for (modified, path) in desktop_timed_out {
            if let Some(version) = probe_slow_retry(
                &path,
                cap,
                &mut retries_used,
                &mut retry_spent,
                &mut failures,
                &mut probe,
            ) && let Some(number) = Version::parse(&version)
            {
                valid_desktop.push((
                    number,
                    modified,
                    path,
                    version,
                    Some("version probe recovered on bounded slow retry".into()),
                ));
            }
        }
    }
    valid_desktop.sort_by(|a, b| (&b.0, &b.1, &b.2).cmp(&(&a.0, &a.1, &a.2)));
    if let Some((_, _, path, version, probe_fallback)) = valid_desktop.into_iter().next() {
        let skipped_candidates = skipped_for(&failures, &path);
        return Ok(Selection {
            path,
            version: Some(version),
            automatic: true,
            probe_fallback,
            skipped_candidates,
        });
    }

    // Fallback candidates (macOS/Linux and non-desktop CLI paths): probe in
    // priority order and retry a timed-out candidate before falling through to
    // lower-priority (potentially older) installations.
    for path in dedup_fallback(fallback) {
        match probe_fast(path, &mut pass1_spent, &mut failures, &mut probe) {
            ProbeOutcome::Verified {
                version,
                used_cache,
            } if Version::parse(&version).is_some() => {
                return Ok(Selection {
                    path: path.to_owned(),
                    version: Some(version),
                    automatic: true,
                    probe_fallback: used_cache.then(|| {
                        "unchanged previously verified executable; version probe timed out"
                            .to_owned()
                    }),
                    skipped_candidates: skipped_for(&failures, path),
                });
            }
            ProbeOutcome::TimedOut => {
                if let Some(version) = probe_slow_retry(
                    path,
                    SLOW_RETRY_TIMEOUT,
                    &mut retries_used,
                    &mut retry_spent,
                    &mut failures,
                    &mut probe,
                ) && Version::parse(&version).is_some()
                {
                    return Ok(Selection {
                        path: path.to_owned(),
                        version: Some(version),
                        automatic: true,
                        probe_fallback: Some(
                            "version probe recovered on bounded slow retry".into(),
                        ),
                        skipped_candidates: skipped_for(&failures, path),
                    });
                }
            }
            ProbeOutcome::Failed | ProbeOutcome::Verified { .. } => {}
        }
    }

    let reasons = if failures.is_empty() {
        "no candidate files found in current desktop or CLI locations".to_owned()
    } else {
        failures
            .into_iter()
            .map(|(_, reason)| reason)
            .collect::<Vec<_>>()
            .join("; ")
    };
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "Codex auto-discovery found no usable executable; candidate probes: {reasons}"
        ),
    ))
}

#[cfg(test)]
fn discover(
    desktop: Option<&Path>,
    fallback: &[PathBuf],
    mut version: impl FnMut(&Path) -> Option<String>,
) -> io::Result<Selection> {
    let mut valid = Vec::new();
    for (modified, path) in desktop_installations(desktop) {
        if let Some(text) = version(&path)
            && let Some(number) = Version::parse(&text)
        {
            valid.push((number, modified, path, text));
        }
    }
    valid.sort_by(|a, b| b.cmp(a));
    if let Some((_, _, path, text)) = valid.into_iter().next() {
        return Ok(Selection {
            path,
            version: Some(text),
            automatic: true,
            probe_fallback: None,
            skipped_candidates: Vec::new(),
        });
    }
    for path in dedup_fallback(fallback) {
        if let Some(text) = version(path)
            && Version::parse(&text).is_some()
        {
            return Ok(Selection {
                path: path.to_owned(),
                version: Some(text),
                automatic: true,
                probe_fallback: None,
                skipped_candidates: Vec::new(),
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Codex auto-discovery found no usable executable in the desktop installation or CLI locations; --codex PATH selects an explicit pin",
    ))
}

// Semver prerelease numbers compare numerically (alpha.10 > alpha.9); release > prerelease.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Version(Vec<u64>, Option<Vec<String>>);
impl Version {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim().strip_prefix("codex-cli ")?;
        let text = text.split('+').next()?;
        let (core, pre) = text
            .split_once('-')
            .map_or((text, None), |(a, b)| (a, Some(b)));
        let core = core
            .split('.')
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()?;
        if core.len() != 3 {
            return None;
        }
        let pre = pre.map(|p| p.split('.').map(str::to_owned).collect::<Vec<_>>());
        if pre.as_ref().is_some_and(|v| {
            v.iter()
                .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        }) {
            return None;
        }
        Some(Self(core, pre))
    }
}
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .cmp(&other.0)
            .then_with(|| match (&self.1, &other.1) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => {
                    for (a, b) in a.iter().zip(b) {
                        let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(a), Ok(b)) => a.cmp(&b),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            _ => a.cmp(b),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    a.len().cmp(&b.len())
                }
            })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn probe(path: &Path, timeout: Duration) -> io::Result<String> {
    // File-backed capture avoids blocked pipe readers and unjoined reader threads.
    // Both time and observed output are bounded, even for an invalid candidate.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = std::env::temp_dir().join(format!(
        "boxen-codex-version-{}-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, AtomicOrdering::Relaxed)
    ));
    struct Capture(PathBuf);
    impl Drop for Capture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _capture = Capture(name.clone());
    let output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(&name)?;
    let mut cmd = Command::new(path);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    struct ProbeChild(Child);
    impl Drop for ProbeChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ProbeChild(cmd.spawn()?);
    let deadline = Instant::now() + timeout;
    loop {
        if output.metadata()?.len() > 4096 {
            return Err(io::Error::other("Codex version output exceeded 4 KiB"));
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                return Err(io::Error::other(format!(
                    "Codex version probe failed: {status}"
                )));
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Codex version probe timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut text = String::new();
    fs::File::open(name)?.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096 || Version::parse(&text).is_none() {
        return Err(io::Error::other("Codex version response invalid"));
    }
    drop(output);
    Ok(text.trim().into())
}

fn fallback_candidates(
    os: &str,
    arch: &str,
    home: Option<&Path>,
    search: Option<&OsStr>,
) -> Vec<PathBuf> {
    let name = if os == "windows" {
        "codex.exe"
    } else {
        "codex"
    };
    let mut candidates = Vec::new();
    if os == "macos" {
        candidates.push(PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"));
    }
    let mut prefixes = Vec::new();
    if let Some(search) = search {
        for directory in std::env::split_paths(search).filter(|p| !p.as_os_str().is_empty()) {
            candidates.push(directory.join(name));
            if let Some(parent) = directory.parent() {
                prefixes.push(parent.to_owned());
            }
        }
    }
    if let Some(home) = home {
        candidates.extend([
            home.join(".local/bin").join(name),
            home.join(".cargo/bin").join(name),
        ]);
        prefixes.push(home.join(".npm-global"));
        if let Ok(entries) = fs::read_dir(home.join(".nvm/versions/node")) {
            let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            paths.reverse();
            prefixes.extend(paths);
        }
    }
    prefixes.extend([
        PathBuf::from("/usr/local"),
        PathBuf::from("/usr"),
        PathBuf::from("/opt/homebrew"),
    ]);
    let platform = match (os, arch) {
        ("linux", "x86_64") => Some(("linux-x64", "x86_64-unknown-linux-musl")),
        ("linux", "aarch64") => Some(("linux-arm64", "aarch64-unknown-linux-musl")),
        ("macos", "x86_64") => Some(("darwin-x64", "x86_64-apple-darwin")),
        ("macos", "aarch64") => Some(("darwin-arm64", "aarch64-apple-darwin")),
        ("windows", "x86_64") => Some(("win32-x64", "x86_64-pc-windows-msvc")),
        ("windows", "aarch64") => Some(("win32-arm64", "aarch64-pc-windows-msvc")),
        _ => None,
    };
    if let Some((package, target)) = platform {
        for prefix in prefixes {
            let modules = prefix.join("lib/node_modules/@openai");
            let codex = modules.join("codex");
            let relative = format!("vendor/{target}/bin/{name}");
            candidates.push(
                codex
                    .join(format!("node_modules/@openai/codex-{package}"))
                    .join(&relative),
            );
            candidates.push(modules.join(format!("codex-{package}")).join(&relative));
            candidates.push(codex.join(&relative));
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "boxen-discovery-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                N.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn file(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "fixture").unwrap();
            p
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn timeout() -> io::Error {
        io::Error::new(io::ErrorKind::TimedOut, "fixture probe timed out")
    }
    #[test]
    fn unchanged_verified_candidate_survives_timeout_but_not_invalid_output() {
        let f = Fixture::new();
        let path = f.file("candidate");
        let first = resolve_candidates(None, std::slice::from_ref(&path), |_, _| {
            Ok("codex-cli 1.0.0".into())
        })
        .unwrap();
        assert!(first.probe_fallback.is_none());
        let next =
            resolve_candidates(None, std::slice::from_ref(&path), |_, _| Err(timeout())).unwrap();
        assert_eq!(next.version.as_deref(), Some("codex-cli 1.0.0"));
        assert!(next.probe_fallback.is_some());
        let error = resolve_candidates(None, std::slice::from_ref(&path), |_, _| {
            Ok("other-cli 1.0.0".into())
        })
        .unwrap_err();
        assert!(error.to_string().contains("response invalid"));
        assert!(
            resolve_candidates(None, std::slice::from_ref(&path), |_, _| Err(timeout())).is_err()
        );
    }
    #[test]
    fn replaced_and_deleted_files_never_reuse_cached_versions() {
        let f = Fixture::new();
        let path = f.file("candidate");
        resolve_candidates(None, std::slice::from_ref(&path), |_, _| {
            Ok("codex-cli 1.0.0".into())
        })
        .unwrap();
        fs::write(&path, "replacement executable bytes").unwrap();
        assert!(
            resolve_candidates(None, std::slice::from_ref(&path), |_, _| Err(timeout())).is_err()
        );
        resolve_candidates(None, std::slice::from_ref(&path), |_, _| {
            Ok("codex-cli 2.0.0".into())
        })
        .unwrap();
        fs::remove_file(&path).unwrap();
        let new = f.file("new-location");
        let selected = resolve_candidates(None, &[path, new.clone()], |_, _| {
            Ok("codex-cli 3.0.0".into())
        })
        .unwrap();
        assert_eq!(selected.path, new);
        assert_eq!(selected.version.as_deref(), Some("codex-cli 3.0.0"));
    }
    #[test]
    fn cold_timeout_gets_one_bounded_slower_probe_and_keeps_failure_causes() {
        let f = Fixture::new();
        let path = f.file("candidate");
        let mut calls = Vec::new();
        let selected = resolve_candidates(None, std::slice::from_ref(&path), |_, duration| {
            calls.push(duration);
            if calls.len() == 1 {
                Err(timeout())
            } else {
                Ok("codex-cli 1.0.0".into())
            }
        })
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0] <= Duration::from_secs(1));
        assert!(calls[1] > Duration::from_secs(1) && calls[1] <= Duration::from_secs(3));
        assert!(selected.probe_fallback.unwrap().contains("slow retry"));
        let other = f.file("permanent-timeout");
        let mut attempts = 0;
        let error = resolve_candidates(None, std::slice::from_ref(&other), |_, _| {
            attempts += 1;
            Err(timeout())
        })
        .unwrap_err();
        assert_eq!(attempts, 2);
        assert!(error.to_string().contains("fixture probe timed out"));
        assert!(error.to_string().contains("slow retry"));
    }
    #[cfg(unix)]
    #[test]
    fn real_slow_cli_recovers_before_any_workload_is_started() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let path = f.file("slow-cli");
        fs::write(&path, "#!/bin/sh\nsleep 1.2\necho codex-cli 1.0.0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let selected = resolve_candidates(None, std::slice::from_ref(&path), probe).unwrap();
        assert_eq!(selected.version.as_deref(), Some("codex-cli 1.0.0"));
        assert!(selected.probe_fallback.unwrap().contains("slow retry"));
    }
    #[test]
    fn newer_installation_beats_unchanged_cached_candidate() {
        let f = Fixture::new();
        let old = f.file("old/codex.exe");
        resolve_candidates(Some(&f.0), &[], |_, _| Ok("codex-cli 1.0.0".into())).unwrap();
        let new = f.file("new/codex.exe");
        let selected = resolve_candidates(Some(&f.0), &[], |p, _| {
            if p == old {
                Err(timeout())
            } else {
                Ok("codex-cli 2.0.0".into())
            }
        })
        .unwrap();
        assert_eq!(selected.path, new);
        assert!(selected.probe_fallback.is_none());
    }
    #[cfg(unix)]
    #[test]
    fn retargeted_symlink_invalidates_cached_identity() {
        let f = Fixture::new();
        let a = f.file("a");
        let b = f.file("b");
        let link = f.0.join("codex");
        std::os::unix::fs::symlink(a, &link).unwrap();
        resolve_candidates(None, std::slice::from_ref(&link), |_, _| {
            Ok("codex-cli 1.0.0".into())
        })
        .unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(b, &link).unwrap();
        assert!(
            resolve_candidates(None, std::slice::from_ref(&link), |_, _| Err(timeout())).is_err()
        );
    }
    #[test]
    fn versions_compare_release_and_numeric_prerelease() {
        let parse = |s| Version::parse(&format!("codex-cli {s}")).unwrap();
        assert!(parse("0.166.0-alpha.10") > parse("0.166.0-alpha.9"));
        assert!(parse("0.166.0") > parse("0.166.0-alpha.10"));
        assert!(parse("0.167.0-alpha.1") > parse("0.166.0"));
        assert_eq!(parse("0.166.0+build1"), parse("0.166.0+build2"));
        for text in [
            "",
            "other-cli 99.0.0",
            "codex-cli 1.2",
            "codex-cli 1.2.3-",
            "codex-cli 1.2.3-invalid value",
        ] {
            assert!(Version::parse(text).is_none());
        }
    }
    #[test]
    fn complete_valid_versions_win_over_hash_order_and_partial_updates() {
        let f = Fixture::new();
        let old = f.file("zzzz/codex.exe");
        let new = f.file("aaaa/codex.exe");
        let invalid = f.file("yyyy/codex.exe");
        fs::create_dir_all(f.0.join("newest-incomplete")).unwrap();
        let fallback = f.file("fallback");
        let probe = |p: &Path| {
            if p == old {
                Some("codex-cli 0.166.0-alpha.9".into())
            } else if p == new {
                Some("codex-cli 0.166.0-alpha.10".into())
            } else if p == invalid {
                None
            } else {
                Some("codex-cli 1.0.0".into())
            }
        };
        assert_eq!(
            discover(Some(&f.0), std::slice::from_ref(&fallback), probe)
                .unwrap()
                .path,
            new
        );
        fs::remove_file(&new).unwrap();
        assert_eq!(
            discover(Some(&f.0), std::slice::from_ref(&fallback), probe)
                .unwrap()
                .path,
            old
        );
        fs::remove_file(&old).unwrap();
        assert_eq!(
            discover(Some(&f.0), std::slice::from_ref(&fallback), probe)
                .unwrap()
                .path,
            fallback
        );
    }
    #[test]
    fn explicit_pin_is_never_discovered_or_substituted() {
        let selected = resolve("/missing/pinned-codex", "windows").unwrap();
        assert!(!selected.automatic);
        assert!(selected.version.is_none());
        let mut locations = 0;
        let mut starts = 0;
        let result: io::Result<()> = launch_with(
            || {
                locations += 1;
                Ok(selected.clone())
            },
            &mut |_| {
                starts += 1;
                Err(io::ErrorKind::NotFound.into())
            },
            &mut |_, _| {},
        );
        assert!(result.is_err());
        assert_eq!((locations, starts), (1, 1));
    }
    #[test]
    fn missing_spawn_rediscovers_once_and_other_errors_do_not_replay() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Other,
        ] {
            let mut resolutions = 0;
            let mut starts = 0;
            let mut retried = Vec::new();
            let result: io::Result<()> = launch_with(
                || {
                    resolutions += 1;
                    Ok(Selection {
                        path: format!("candidate-{resolutions}").into(),
                        version: None,
                        automatic: true,
                        probe_fallback: None,
                        skipped_candidates: Vec::new(),
                    })
                },
                &mut |_| {
                    starts += 1;
                    Err(kind.into())
                },
                &mut |s, retry| retried.push((s.path.clone(), retry)),
            );
            assert!(result.is_err());
            let count = if kind == io::ErrorKind::NotFound {
                2
            } else {
                1
            };
            assert_eq!((resolutions, starts), (count, count));
            assert_eq!(retried.last().unwrap().1, count == 2);
        }
        let mut starts = 0;
        let result = launch_with(
            || {
                Ok(Selection {
                    path: "candidate".into(),
                    version: None,
                    automatic: true,
                    probe_fallback: None,
                    skipped_candidates: Vec::new(),
                })
            },
            &mut |_| {
                starts += 1;
                Ok(42)
            },
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!((starts, result), (1, 42));
    }
    #[test]
    fn missing_working_directory_does_not_trigger_executable_retry() {
        let f = Fixture::new();
        let executable = f.file("present-codex");
        let mut starts = 0;
        let result: io::Result<()> = launch_with(
            || {
                Ok(Selection {
                    path: executable.clone(),
                    version: None,
                    automatic: true,
                    probe_fallback: None,
                    skipped_candidates: Vec::new(),
                })
            },
            &mut |_| {
                starts += 1;
                Err(io::ErrorKind::NotFound.into())
            },
            &mut |_, _| {},
        );
        assert!(result.is_err());
        assert_eq!(starts, 1);
    }
    #[test]
    fn linux_cli_and_native_npm_locations_remain_available() {
        let f = Fixture::new();
        let standalone = f.file(".local/bin/codex");
        let candidates = fallback_candidates("linux", "x86_64", Some(&f.0), None);
        let probe = |_: &Path| Some("codex-cli 1.0.0".into());
        assert_eq!(discover(None, &candidates, probe).unwrap().path, standalone);
        fs::remove_file(standalone).unwrap();
        let npm=f.file(".nvm/versions/node/v22/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex");
        assert_eq!(
            discover(
                None,
                &fallback_candidates("linux", "x86_64", Some(&f.0), None),
                probe
            )
            .unwrap()
            .path,
            npm
        );
    }
    #[test]
    fn timed_out_higher_priority_fallback_retries_before_older_fallback() {
        let f = Fixture::new();
        let primary = f.file("primary/codex");
        let stale = f.file("stale/codex");
        let mut probed = Vec::new();
        let mut primary_attempts = 0;
        let selected = resolve_candidates(None, &[primary.clone(), stale.clone()], |p, duration| {
            probed.push((p.to_owned(), duration));
            if p == primary {
                primary_attempts += 1;
                if primary_attempts == 1 {
                    Err(timeout())
                } else {
                    Ok("codex-cli 0.171.0-alpha.13.2".into())
                }
            } else {
                Ok("codex-cli 0.149.0".into())
            }
        })
        .unwrap();
        assert_eq!(selected.path, primary);
        assert_eq!(
            selected.version.as_deref(),
            Some("codex-cli 0.171.0-alpha.13.2")
        );
        assert!(selected.probe_fallback.unwrap().contains("slow retry"));
        assert!(selected.skipped_candidates.is_empty());
        assert_eq!(probed.len(), 2);
        assert!(probed.iter().all(|(p, _)| p == &primary));
        assert!(probed[0].1 <= Duration::from_secs(1));
        assert!(probed[1].1 > Duration::from_secs(1) && probed[1].1 <= Duration::from_secs(3));
    }
    #[test]
    fn permanently_hung_higher_priority_fallback_falls_through_and_reports_skipped() {
        let f = Fixture::new();
        let hung = f.file("hung/codex");
        let working = f.file("working/codex");
        let mut hung_attempts = 0;
        let selected = resolve_candidates(None, &[hung.clone(), working.clone()], |p, _| {
            if p == hung {
                hung_attempts += 1;
                Err(timeout())
            } else {
                Ok("codex-cli 0.149.0".into())
            }
        })
        .unwrap();
        assert_eq!(selected.path, working);
        assert_eq!(selected.version.as_deref(), Some("codex-cli 0.149.0"));
        assert!(selected.probe_fallback.is_none());
        assert_eq!(hung_attempts, 2);
        assert_eq!(selected.skipped_candidates.len(), 2);
        assert!(selected.skipped_candidates[0].contains("fixture probe timed out"));
        assert!(selected.skipped_candidates[1].contains("slow retry"));
    }
    #[test]
    fn cold_newer_desktop_installation_retries_before_settling_on_older_desktop() {
        let f = Fixture::new();
        let _old = f.file("old/codex.exe");
        let new = f.file("new/codex.exe");
        let mut new_calls = Vec::new();
        let selected = resolve_candidates(Some(&f.0), &[], |p, duration| {
            if p == new {
                new_calls.push(duration);
                if new_calls.len() == 1 {
                    Err(timeout())
                } else {
                    Ok("codex-cli 0.171.0".into())
                }
            } else {
                Ok("codex-cli 0.149.0".into())
            }
        })
        .unwrap();
        assert_eq!(selected.path, new);
        assert_eq!(selected.version.as_deref(), Some("codex-cli 0.171.0"));
        assert!(selected.probe_fallback.unwrap().contains("slow retry"));
        assert!(selected.skipped_candidates.is_empty());
        assert_eq!(new_calls.len(), 2);
        assert!(new_calls[0] <= Duration::from_secs(1));
        assert!(new_calls[1] > Duration::from_secs(1) && new_calls[1] <= Duration::from_secs(2));

        let f2 = Fixture::new();
        let old2 = f2.file("old/codex.exe");
        let hung = f2.file("hung/codex.exe");
        let fallback = resolve_candidates(Some(&f2.0), &[], |p, _| {
            if p == hung {
                Err(timeout())
            } else {
                Ok("codex-cli 0.149.0".into())
            }
        })
        .unwrap();
        assert_eq!(fallback.path, old2);
        assert!(fallback.probe_fallback.is_none());
        assert_eq!(fallback.skipped_candidates.len(), 2);
        assert!(fallback.skipped_candidates[1].contains("slow retry"));
    }
    #[test]
    fn duplicate_fallback_paths_are_probed_once() {
        let f = Fixture::new();
        let primary = f.file("primary/codex");
        let secondary = f.file("secondary/codex");
        let mut primary_attempts = 0;
        let selected = resolve_candidates(
            None,
            &[primary.clone(), primary.clone(), secondary.clone()],
            |p, _| {
                if p == primary {
                    primary_attempts += 1;
                    Err(timeout())
                } else {
                    Ok("codex-cli 1.0.0".into())
                }
            },
        )
        .unwrap();
        assert_eq!(selected.path, secondary);
        assert_eq!(primary_attempts, 2);
    }
    #[test]
    fn unusable_candidates_produce_a_discovery_error() {
        let f = Fixture::new();
        let invalid = f.file("hash/codex.exe");
        assert_eq!(
            discover(Some(&f.0), &[invalid], |_| None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
}
