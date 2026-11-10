//! A separate owner keeps the Chrome group and profile lock alive through MCP exit.

use crate::Result;
use boxen_browser::browser::{Browser as Chromium, Endpoint};
use std::{
    io::{self, BufReader, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
};

pub struct Browser {
    owner: Owner,
    endpoint: Endpoint,
}

impl Browser {
    pub fn launch(executable: &Path, profile: &Path, headless: bool) -> Result<Self> {
        let child = Command::new(std::env::current_exe()?)
            .arg("chrome-owner")
            .arg(executable)
            .arg(profile)
            .arg(if headless { "headless" } else { "headed" })
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut owner = Owner(child);
        let reply = owner.0.stdout.take().unwrap();
        let endpoint: std::result::Result<Endpoint, String> =
            serde_json::Deserializer::from_reader(BufReader::new(reply))
                .into_iter()
                .next()
                .ok_or("Chrome owner exited without a startup result")??;
        Ok(Self {
            owner,
            endpoint: endpoint?,
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn close(mut self) -> Result<()> {
        self.owner.stop()
    }
}

struct Owner(Child);

impl Owner {
    fn stop(&mut self) -> Result<()> {
        drop(self.0.stdin.take());
        if !self.0.wait()?.success() {
            return Err("Chrome owner could not complete graceful profile shutdown".into());
        }
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn run(executable: &Path, profile: &Path, headless: bool) -> Result<()> {
    let browser = Chromium::launch(executable, profile, headless);
    let result: std::result::Result<&Endpoint, String> = browser
        .as_ref()
        .map(Chromium::endpoint)
        .map_err(ToString::to_string);
    let mut output = io::stdout().lock();
    let published = (|| -> Result<()> {
        serde_json::to_writer(&mut output, &result)?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    })();
    let browser = browser?;
    // EOF also arrives if the MCP process is killed. No other process owns
    // the write end, and Chrome never inherits the read end.
    let owner = published.and_then(|()| {
        io::copy(&mut io::stdin().lock(), &mut io::sink())?;
        Ok(())
    });
    let cleanup = browser.close();
    owner.and(cleanup)
}
