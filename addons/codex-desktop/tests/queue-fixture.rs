//! Native executable fixture: no shell expansion of a notice's contents.
use std::{
    env, fs, thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
fn main() {
    let root =
        std::path::PathBuf::from(env::var_os("BOXEN_SUPPORT_QUEUE_LOG").expect("fixture log root"));
    let args = env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|s| s == "descendant") {
        fs::write(root.join("descendant-pid"), std::process::id().to_string()).unwrap();
        thread::sleep(Duration::from_secs(120));
        return;
    }
    if args.get(1).is_some_and(|s| s == "--version") {
        let folder = env::current_exe().unwrap().parent().unwrap().to_owned();
        match fs::read_to_string(folder.join("probe-mode"))
            .unwrap_or_default()
            .trim()
        {
            "sleep" => thread::sleep(Duration::from_secs(120)),
            "flood" => {
                println!("{}", "x".repeat(8192));
                thread::sleep(Duration::from_secs(120));
            }
            "fail" => std::process::exit(1),
            _ => println!(
                "codex-cli {}",
                fs::read_to_string(folder.join("version"))
                    .unwrap_or_else(|_| "1.0.0".into())
                    .trim()
            ),
        }
        return;
    }
    assert_eq!(args.get(1).map(String::as_str), Some("queue"));
    let i = args.iter().position(|s| s == "--message").unwrap();
    fs::write(
        root.join(format!(
            "notice-{}.txt",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )),
        &args[i + 1],
    )
    .unwrap();
    match fs::read_to_string(root.join("mode"))
        .unwrap_or_default()
        .trim()
    {
        "fail" => std::process::exit(1),
        "sleep" => {
            let mut child = std::process::Command::new(env::current_exe().unwrap())
                .arg("descendant")
                .spawn()
                .unwrap();
            thread::sleep(Duration::from_secs(120));
            let _ = child.wait();
        }
        _ => {}
    }
}
