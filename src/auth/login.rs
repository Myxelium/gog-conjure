use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config;

use super::extract_code;

#[derive(Debug)]
pub enum LoginOutcome {
    Code(String),
    Error(String),
}

/// Spawn `gog-conjure --gog-login <file>` and wait for an OAuth code.
pub fn begin_login(tx: mpsc::Sender<LoginOutcome>) {
    thread::spawn(move || match run_login_subprocess() {
        Ok(code) => {
            let _ = tx.send(LoginOutcome::Code(code));
        }
        Err(err) => {
            let _ = tx.send(LoginOutcome::Error(err.to_string()));
        }
    });
}

fn run_login_subprocess() -> Result<String> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    let out_path = login_code_path()?;
    let _ = std::fs::remove_file(&out_path);

    let mut child = Command::new(&exe)
        .arg("--gog-login")
        .arg(&out_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to start login helper ({}). On Linux install: libwebkit2gtk-4.1-0",
                exe.display()
            )
        })?;

    let timeout = Duration::from_secs(5 * 60);
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(anyhow!("login timed out"));
                }
                thread::sleep(Duration::from_millis(150));
            }
            Err(err) => return Err(anyhow!("wait for login helper: {err}")),
        }
    }

    if !out_path.is_file() {
        return Err(anyhow!("login cancelled"));
    }

    let raw = std::fs::read_to_string(&out_path).context("read login code")?;
    let _ = std::fs::remove_file(&out_path);
    let raw = raw.trim();
    extract_code(raw).with_context(|| {
        format!(
            "invalid code from login helper (got {} bytes: {:?})",
            raw.len(),
            raw.chars().take(48).collect::<String>()
        )
    })
}

fn login_code_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("login_code.txt"))
}
