//! Shared process-time Codex discovery for the daemon and desktop adapter.
//! Explicit paths are pins; `auto` and `codex` discover on every launch.
use std::{
    cmp::Ordering,
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug)]
pub struct Selection {
    pub path: PathBuf,
    pub version: Option<String>,
    pub automatic: bool,
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
    let deadline = Instant::now() + Duration::from_secs(5);
    discover(
        desktop.as_deref(),
        &fallback_candidates(
            os,
            std::env::consts::ARCH,
            home.as_deref(),
            std::env::var_os("PATH").as_deref(),
        ),
        |p| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            probe(p, remaining.min(Duration::from_secs(1))).ok()
        },
    )
}

fn discover(
    desktop: Option<&Path>,
    fallback: &[PathBuf],
    mut version: impl FnMut(&Path) -> Option<String>,
) -> io::Result<Selection> {
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
    let mut valid = Vec::new();
    for (modified, path) in installations.into_iter().take(16) {
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
        });
    }
    for path in fallback.iter().filter(|p| p.is_file()).take(16) {
        if let Some(text) = version(path)
            && Version::parse(&text).is_some()
        {
            return Ok(Selection {
                path: path.clone(),
                version: Some(text),
                automatic: true,
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
                return Err(io::Error::other("Codex version probe failed"));
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
