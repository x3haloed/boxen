//! Assignment-scoped proxy for the native Safari MCP server.

use crate::{Result, browser_assignment::Settings, mcp::MAX_REQUEST};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

pub fn serve(settings: Settings, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    let mut driver = Command::new(&settings.executable)
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = driver.stdin.take().unwrap();
    let mut reply = BufReader::new(driver.stdout.take().unwrap());
    let mut reservation = None;
    let result = relay(
        &settings,
        &mut reservation,
        &mut input,
        &mut output,
        &mut stdin,
        &mut reply,
    );
    drop(stdin);
    drop(reply);
    let cleanup = close(&mut driver);
    if cleanup.is_err() {
        let _ = driver.kill();
        let _ = driver.wait();
    }
    // Keep the reservation until the native driver has exited, including on errors.
    drop(reservation);
    result.and(cleanup)
}

fn relay(
    settings: &Settings,
    reservation: &mut Option<File>,
    input: &mut impl BufRead,
    output: &mut impl Write,
    driver: &mut impl Write,
    reply: &mut impl BufRead,
) -> Result<()> {
    while let Some(mut request) = read(input)? {
        let Some(id) = request.get("id").cloned() else {
            // A tool call can have effects and requires a response ID.
            if request["method"] != "tools/call" {
                write(driver, &request)?;
            }
            continue;
        };
        let artifact = match prepare(settings, reservation, &mut request) {
            Ok(file) => file,
            Err(error) => {
                write(output, &tool_error(&id, error))?;
                continue;
            }
        };
        write(driver, &request)?;
        let mut response = read(reply)?.ok_or("Safari MCP closed without a reply")?;
        if response["id"] != id {
            return Err("Safari MCP returned an unexpected response ID".into());
        }
        if let Some(file) = artifact
            && response.get("result").is_some()
            && response["result"]["isError"] != true
            && let Err(error) = retain_png(file)
        {
            response = tool_error(&id, error);
        }
        write(output, &response)?;
    }
    Ok(())
}

fn prepare(
    settings: &Settings,
    reservation: &mut Option<File>,
    request: &mut Value,
) -> Result<Option<NamedTempFile>> {
    if request["method"] != "tools/call" {
        return Ok(None);
    }
    if reservation.is_none() {
        fs::create_dir_all(&settings.profile)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(settings.profile.join(".boxen-browser.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|error| format!("Safari reservation could not be acquired: {error}"))?;
        *reservation = Some(lock);
    }
    if request["params"]["name"] != "screenshot" {
        return Ok(None);
    }
    fs::create_dir_all(&settings.evidence)?;
    let file = tempfile::Builder::new()
        .prefix("browser-")
        .suffix(".png")
        .tempfile_in(&settings.evidence)?;
    if request["params"]["arguments"].is_null() {
        request["params"]["arguments"] = json!({});
    }
    let arguments = request["params"]["arguments"]
        .as_object_mut()
        .ok_or("Safari screenshot arguments must be an object")?;
    arguments.insert("savePath".into(), json!(file.path()));
    Ok(Some(file))
}

fn retain_png(file: NamedTempFile) -> Result<()> {
    let mut signature = [0; 8];
    File::open(file.path())?.read_exact(&mut signature)?;
    if &signature != b"\x89PNG\r\n\x1a\n" {
        return Err("Safari did not return a PNG screenshot".into());
    }
    file.keep()?;
    Ok(())
}

fn tool_error(id: &Value, error: impl ToString) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"isError":true,
        "content":[{"type":"text","text":error.to_string()}]}})
}

fn read(input: &mut impl BufRead) -> Result<Option<Value>> {
    let mut line = Vec::new();
    let size = input.take(MAX_REQUEST + 1).read_until(b'\n', &mut line)?;
    if size == 0 {
        return Ok(None);
    }
    if size as u64 > MAX_REQUEST {
        return Err("Safari MCP message exceeded its frame bound".into());
    }
    Ok(Some(serde_json::from_slice(&line)?))
}

fn write(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn close(driver: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = driver.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("Safari MCP exited with {status}").into())
            };
        }
        if Instant::now() >= deadline {
            return Err("Safari MCP did not stop after its input closed".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}
