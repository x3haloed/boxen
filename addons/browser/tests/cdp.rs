use boxen_browser::cdp::Connection;
use serde_json::{Value, json};
use std::{
    net::{Ipv4Addr, TcpListener},
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, accept};

const WAIT: Duration = Duration::from_secs(3);
const PATH: &str = "/devtools/browser/00000000-0000-0000-0000-000000000000";

#[test]
fn returns_the_matching_response_and_retains_events_across_calls() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut socket = accept(stream).unwrap();
        let request: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "Debugger.enable");
        assert_eq!(request["sessionId"], "target-session");
        socket.send(Message::Text(json!({
            "method":"Debugger.scriptParsed","sessionId":"target-session","params":{"scriptId":"9"}
        }).to_string().into())).unwrap();
        socket
            .send(Message::Text(
                json!({
                    "id":request["id"],"result":{"debuggerId":"debugger"}
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        socket.send(Message::Text(json!({
            "method":"Debugger.paused","sessionId":"target-session","params":{"callFrames":[]}
        }).to_string().into())).unwrap();
        let request: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "Debugger.resume");
        socket
            .send(Message::Text(
                json!({
                    "id":request["id"],"error":{"code":-32000,"message":"synthetic error"}
                })
                .to_string()
                .into(),
            ))
            .unwrap();
    });
    let mut connection = Connection::connect(port, PATH).unwrap();
    let reply = connection
        .command("Debugger.enable", json!({}), Some("target-session"), WAIT)
        .unwrap();
    assert_eq!(reply["result"]["debuggerId"], "debugger");
    let parsed = connection.events(0, WAIT).unwrap();
    assert_eq!(
        parsed["events"][0]["event"]["method"],
        "Debugger.scriptParsed"
    );
    let paused = connection.events(1, WAIT).unwrap();
    assert_eq!(paused["cursor"], 2);
    assert_eq!(paused["events"][0]["event"]["method"], "Debugger.paused");
    assert_eq!(
        connection.events(2, Duration::ZERO).unwrap()["events"],
        json!([])
    );
    let reply = connection
        .command("Debugger.resume", json!({}), Some("target-session"), WAIT)
        .unwrap();
    assert_eq!(reply["error"]["code"], -32000);
    server.join().unwrap();
}

#[test]
fn invalid_debugger_path_is_rejected_before_connecting() {
    assert!(Connection::connect(9222, "/devtools/browser/id?other").is_err());
}

#[test]
#[ignore = "requires a dedicated Chrome or Edge process with remote debugging enabled"]
fn real_browser_breakpoint_and_step() {
    let endpoint =
        std::fs::read_to_string(std::env::var_os("BOXEN_BROWSER_ACTIVE_PORT").unwrap()).unwrap();
    let mut lines = endpoint.lines();
    let port = lines.next().unwrap().parse().unwrap();
    let mut client = Connection::connect(port, lines.next().unwrap()).unwrap();
    verify_breakpoint(&mut client);
}

#[cfg(windows)]
#[test]
#[ignore = "requires a configured Chrome or Edge executable and a disposable profile directory"]
fn managed_browser_reserves_reuses_and_releases_profile() {
    use boxen_browser::browser::Browser;
    use std::path::PathBuf;
    let executable = PathBuf::from(std::env::var_os("BOXEN_BROWSER_EXECUTABLE").unwrap());
    let profile = PathBuf::from(std::env::var_os("BOXEN_BROWSER_TEST_PROFILE").unwrap());
    assert!(!profile.exists(), "a fresh disposable profile is required");
    let browser = Browser::launch(&executable, &profile, true).unwrap();
    let conflicting = Browser::launch(&executable, &profile, true);
    assert!(
        conflicting
            .err()
            .unwrap()
            .to_string()
            .contains("reservation")
    );
    let endpoint = browser.endpoint().clone();
    let mut client = endpoint.connect().unwrap();
    let version = call(&mut client, "Browser.getVersion", json!({}), None);
    eprintln!("Browser under test: {}", version["product"]);
    verify_breakpoint(&mut client);
    call(
        &mut client,
        "Storage.setCookies",
        json!({"cookies":[{
            "name":"profile-probe", "value":"retained", "domain":"fixture.invalid", "path":"/", "secure":true,
            "expires": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600
        }]}),
        None,
    );
    assert!(
        has_profile_cookie(&mut client),
        "the first browser did not accept the fixture cookie"
    );
    drop(client);
    browser.close().unwrap();
    assert!(endpoint.connect().is_err());
    let browser = Browser::launch(&executable, &profile, true).unwrap();
    let endpoint = browser.endpoint().clone();
    let mut client = endpoint.connect().unwrap();
    assert!(
        has_profile_cookie(&mut client),
        "the restarted browser did not retain the fixture cookie"
    );
    drop(client);
    drop(browser);
    assert!(endpoint.connect().is_err());
}

#[cfg(windows)]
fn has_profile_cookie(client: &mut Connection) -> bool {
    call(client, "Storage.getCookies", json!({}), None)["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|cookie| cookie["name"] == "profile-probe" && cookie["value"] == "retained")
}

fn verify_breakpoint(client: &mut Connection) {
    let target = call(
        client,
        "Target.createTarget",
        json!({"url":"about:blank"}),
        None,
    );
    let session = call(
        client,
        "Target.attachToTarget",
        json!({"targetId":target["targetId"],"flatten":true}),
        None,
    );
    let session = session["sessionId"].as_str().unwrap();
    call(client, "Debugger.enable", json!({}), Some(session));
    let breakpoint = call(
        client,
        "Debugger.setBreakpointByUrl",
        json!({
            "url":"boxen-browser-fixture.js","lineNumber":2,"condition":"n === 41"
        }),
        Some(session),
    );
    call(
        client,
        "Runtime.evaluate",
        json!({"expression":
            "setTimeout(() => {\nconst n = 41;\nwindow.answer = n + 1;\nwindow.finished = true;\n}, 150);\n//# sourceURL=boxen-browser-fixture.js"
        }),
        Some(session),
    );
    let mut cursor = 0;
    let hit = paused(client, &mut cursor, session);
    assert_eq!(hit["hitBreakpoints"][0], breakpoint["breakpointId"]);
    let local = call(
        client,
        "Debugger.evaluateOnCallFrame",
        json!({
            "callFrameId":hit["callFrames"][0]["callFrameId"],"expression":"n","returnByValue":true
        }),
        Some(session),
    );
    assert_eq!(local["result"]["value"], 41);
    call(client, "Debugger.stepOver", json!({}), Some(session));
    let stepped = paused(client, &mut cursor, session);
    assert_eq!(stepped["callFrames"][0]["location"]["lineNumber"], 3);
    call(client, "Debugger.resume", json!({}), Some(session));
    let result = call(
        client,
        "Runtime.evaluate",
        json!({
            "expression":"[window.answer, window.finished]","returnByValue":true
        }),
        Some(session),
    );
    assert_eq!(result["result"]["value"], json!([42, true]));
    call(
        client,
        "Target.closeTarget",
        json!({"targetId":target["targetId"]}),
        None,
    );
}

fn call(client: &mut Connection, method: &str, params: Value, session: Option<&str>) -> Value {
    let response = client.command(method, params, session, WAIT).unwrap();
    assert!(response.get("error").is_none(), "{method}: {response}");
    response["result"].clone()
}

fn paused(client: &mut Connection, cursor: &mut u64, session: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let batch = client
            .events(*cursor, deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        for entry in batch["events"].as_array().unwrap() {
            *cursor = entry["cursor"].as_u64().unwrap();
            let event = &entry["event"];
            if event["sessionId"] == session && event["method"] == "Debugger.paused" {
                return event["params"].clone();
            }
        }
    }
    panic!("browser did not pause");
}
