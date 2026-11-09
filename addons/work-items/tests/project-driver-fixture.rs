//! Offline project-driver fixture; no Azure or SQL server is contacted.
use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    let args: Vec<_> = env::args().collect();
    let root = PathBuf::from(env::var_os("BOXEN_PROJECT_FIXTURE").expect("fixture root"));
    let executable = env::current_exe().unwrap();
    let name = executable.file_stem().unwrap();
    if name == "git" {
        if args[3..] == ["remote", "get-url", "origin"] {
            println!("{}", env::var("BOXEN_PROJECT_REMOTE").unwrap());
            return;
        }
        let status = Command::new(env::var_os("BOXEN_PROJECT_GIT").unwrap())
            .args(&args[1..])
            .status()
            .unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
    if name == "sqlcmd" {
        println!("absent");
        return;
    }
    let contains = |s: &str| args.iter().any(|a| a == s);
    let value = |s: &str| {
        args.iter()
            .position(|a| a == s)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default()
    };
    let quote = |s: &str| format!("{s:?}");
    let repo = "22222222-2222-4222-8222-222222222222";
    if contains("repos") && contains("show") && !contains("pr") {
        println!(
            "{{\"id\":\"{repo}\",\"remoteUrl\":{}}}",
            quote(&env::var("BOXEN_PROJECT_REMOTE").unwrap())
        );
    } else if contains("create") {
        assert_eq!(value("--draft"), "false");
        assert_eq!(value("--auto-complete"), "false");
        assert_eq!(value("--bypass-policy"), "false");
        assert_eq!(
            value("--description"),
            "The `example` form lost the selected tab. I fixed it by restoring the tab from the URL.\n\nThe new URL also preserves the tab when refreshed."
        );
        assert_eq!(value("--work-items"), "1");
        fs::write(root.join("created"), "one").unwrap();
        println!("{{\"pullRequestId\":23}}");
    } else if contains("work-item") {
        println!("[{{\"id\":1}}]");
    } else if contains("show") {
        println!(
            "{{\"repository\":{{\"id\":\"{repo}\"}},\"sourceRefName\":{},\"targetRefName\":\"refs/heads/main\",\"isDraft\":false,\"status\":\"active\",\"lastMergeSourceCommit\":{{\"commitId\":{}}},\"pullRequestId\":23}}",
            quote(&format!(
                "refs/heads/{}",
                env::var("BOXEN_PROJECT_BRANCH").unwrap()
            )),
            quote(&env::var("BOXEN_PROJECT_COMMIT").unwrap())
        );
    } else if contains("pr") && contains("list") {
        if root.join("created").exists() {
            println!("[{{\"pullRequestId\":23,\"status\":\"active\",\"isDraft\":false}}]");
        } else {
            println!("[]");
        }
    } else if contains("comments") {
        if value("--http-method") == "POST" {
            fs::copy(value("--in-file"), root.join("comment.json")).unwrap();
            println!("{{\"id\":8,\"version\":1}}");
        } else if let Ok(text) = fs::read_to_string(root.join("comment.json")) {
            println!(
                "{{\"comments\":[{{\"id\":8,\"version\":1,\"text\":{}}}]}}",
                quote(&text)
            );
        } else {
            println!("{{\"comments\":[]}}");
        }
    } else {
        panic!("Unexpected fixture arguments")
    }
}
