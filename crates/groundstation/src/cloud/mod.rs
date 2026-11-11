//! `groundstation cloud`: connecting this computer's gsd to a Ground Station
//! backend (the hosted service or a self-hosted one) and disconnecting it.

mod callback;
mod config_file;
mod disclosure;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gsd::config::Config;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use callback::Outcome;

use crate::client::{encode, reason};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Where the CLI collects the token the person approved.
const COLLECT_PATH: &str = "/auth/cli/token";

pub struct Login<'a> {
    pub endpoint: &'a str,
    pub label: Option<String>,
    pub yes: bool,
    pub no_open: bool,
    /// Take the token from standard input instead of the browser.
    pub token_stdin: bool,
    /// How many events gsd holds that it hasn't uploaded, if it said.
    pub pending: Option<u64>,
}

/// Says what will be uploaded, gets a token (through the browser, or pasted),
/// writes it to `config.toml`. Returns false if nothing changed.
pub async fn login(config: &Config, config_path: &Path, login: Login<'_>) -> Result<bool> {
    let endpoint = login.endpoint.trim_end_matches('/');
    gsd::config::check_endpoint(endpoint)?;
    let label = login.label.clone().unwrap_or_else(default_label);
    if !groundstation_wire::valid_token_label(&label) {
        bail!(
            "--label must be 1 to {} characters, none of them control or invisible characters",
            groundstation_wire::MAX_TOKEN_LABEL
        );
    }
    // Whatever would keep the token from being saved fails now, before one
    // exists.
    let text = config_file::read(config_path)?;
    config_file::connect(&text, endpoint, "placeholder")?;
    config_file::check_writable(config_path)?;
    let current = config_file::current(&text)?;
    if current.cloud {
        let at = current.endpoint.as_deref().unwrap_or("a backend");
        println!(
            "This computer uploads to {at} now. Connecting again replaces its token, which keeps \
             working until you revoke it on {at}'s Tokens page.\n"
        );
    }
    println!(
        "{}\n",
        disclosure::describe(config, endpoint, login.pending)?
    );
    if !login.yes && !confirm("Upload to this backend?")? {
        println!("Not connected.");
        return Ok(false);
    }

    if login.token_stdin {
        let token = read_token(endpoint)?;
        check_token(endpoint, &token).await?;
        let written = save(config_path, endpoint, &token)?;
        println!(
            "Connected. Token saved to {}.",
            shown(config_path, &written)
        );
        return Ok(true);
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let state = uuid::Uuid::new_v4().simple().to_string();
    // Only this login knows the verifier: the browser carries a code back,
    // and the token comes only for the code and the verifier together.
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let url = format!(
        "{endpoint}/auth/cli?port={port}&state={state}&label={}&challenge={}",
        encode(&label),
        challenge(&verifier)
    );
    println!("Approve this computer in your browser:\n  {url}");
    let opened = !login.no_open && open::that(&url).is_ok();
    if !opened || over_ssh() {
        println!(
            "Open it in a browser on this computer: the approval comes back to its port {port}. \
             On a computer you reach over SSH, create a token at {endpoint}/#/tokens and run \
             this again with --token-stdin."
        );
    }

    let callback = callback::wait(listener, &state, APPROVAL_TIMEOUT).await?;
    let code = match callback.outcome.clone() {
        Outcome::Code(code) => code,
        Outcome::Denied => {
            callback
                .answer("Not connected. You can close this tab.")
                .await;
            println!("Not connected: you cancelled in the browser.");
            return Ok(false);
        }
    };
    let collected = match collect(endpoint, &code, &verifier).await {
        Ok(collected) => collected,
        Err(e) => {
            callback
                .answer("Not connected: the terminal says why.")
                .await;
            return Err(e);
        }
    };
    let workspace = &collected.workspace;
    println!(
        "Approved by {} for the workspace “{workspace}”.",
        collected.approved_by.as_deref().unwrap_or("someone")
    );
    // The api approves at any address it answers on; uploads need ingest's.
    if let Err(e) = check_token(endpoint, &collected.token).await {
        callback
            .answer("Not connected: the terminal says why.")
            .await;
        println!(
            "Revoke the token “{label}” on the Tokens page of “{workspace}”: it wasn't saved."
        );
        return Err(e);
    }
    // Someone else on this computer who saw the approval URL could have
    // approved it for their own workspace: say where, and ask, before saving.
    let mut callback = Some(callback);
    if !login.yes && std::io::stdin().is_terminal() {
        if let Some(callback) = callback.take() {
            callback.answer("Approved. Finish in the terminal.").await;
        }
        if !confirm(&format!("Upload to “{workspace}”?"))? {
            println!(
                "Not connected. The token “{label}” stays in “{workspace}” until someone revokes \
                 it on its Tokens page."
            );
            return Ok(false);
        }
    }
    let written = match save(config_path, endpoint, &collected.token) {
        Ok(written) => written,
        Err(e) => {
            if let Some(callback) = callback {
                callback
                    .answer("Not connected: the terminal says why.")
                    .await;
            }
            println!(
                "Revoke the token “{label}” on the Tokens page of “{workspace}”: it wasn't saved."
            );
            return Err(e);
        }
    };
    if let Some(callback) = callback {
        callback
            .answer("This computer is connected. You can close this tab.")
            .await;
    }
    println!(
        "Connected as “{label}”. Token saved to {}.",
        shown(config_path, &written)
    );
    Ok(true)
}

/// What the Tokens page calls this computer unless `--label` says: the day it
/// connected, here.
fn default_label() -> String {
    format!("gsd, connected {}", chrono::Local::now().format("%Y-%m-%d"))
}

fn over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

/// What the backend gets to check the verifier: SHA-256, in hex.
fn challenge(verifier: &str) -> String {
    Sha256::digest(verifier.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the backend hands over for an approval.
#[derive(Deserialize)]
struct Collected {
    token: String,
    workspace: String,
    approved_by: Option<String>,
}

/// Like gsd's uploader: a redirect would send the code, or the token,
/// somewhere else.
fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// Collects the token the person approved, with the code the browser
/// brought and the verifier only this login knows.
async fn collect(endpoint: &str, code: &str, verifier: &str) -> Result<Collected> {
    let response = http()?
        .post(format!("{endpoint}{COLLECT_PATH}"))
        .form(&[("code", code), ("verifier", verifier)])
        .send()
        .await
        .with_context(|| format!("collecting the token from {endpoint}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "{endpoint} didn't hand over the token: {}",
            reason(response).await
        );
    }
    response
        .json()
        .await
        .with_context(|| format!("{endpoint} answered, but not with a token"))
}

/// Asks the backend whether it takes `token`, with an empty batch, so a
/// mistyped or revoked token is caught here and not in failing uploads.
async fn check_token(endpoint: &str, token: &str) -> Result<()> {
    let empty = serde_json::json!({ "schema": groundstation_schema::SCHEMA, "events": [] });
    let sent = http()?
        .post(format!("{endpoint}/v1/events"))
        .bearer_auth(token)
        .json(&empty)
        .send()
        .await;
    match sent {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) if matches!(r.status().as_u16(), 401 | 403) => {
            let reason = reason(r).await;
            // The api, not ingest, answered: the endpoint is wrong, not the token.
            if reason == groundstation_wire::CROSS_ORIGIN_REFUSED {
                bail!(
                    "{endpoint} is Ground Station's api, which doesn't take uploads: give \
                     --endpoint the address that sends POST /v1/events to ingest (the edge in \
                     front of both)"
                );
            }
            bail!("{endpoint} doesn't take that token: {reason}")
        }
        Ok(r) => {
            println!(
                "Couldn't check the token with {endpoint} (HTTP {}); saving it anyway.",
                r.status().as_u16()
            );
            Ok(())
        }
        Err(e) => {
            println!("Couldn't check the token with {endpoint} ({e}); saving it anyway.");
            Ok(())
        }
    }
}

/// Saves the token, and returns the file written.
fn save(config_path: &Path, endpoint: &str, token: &str) -> Result<PathBuf> {
    let text = config_file::read(config_path)?;
    let written = config_file::write(config_path, &config_file::connect(&text, endpoint, token)?)?;
    if std::env::var_os("GROUNDSTATION_TOKEN").is_some() {
        println!("Note: GROUNDSTATION_TOKEN is set and overrides the token just saved.");
    }
    Ok(written)
}

/// Where the token went, naming the file a symlinked config links to (a
/// dotfiles checkout, say), so nobody commits it unawares.
fn shown(config_path: &Path, written: &Path) -> String {
    let tilde = |p: &Path| crate::render::tilde(&p.to_string_lossy());
    let linked = std::fs::symlink_metadata(config_path).is_ok_and(|m| m.file_type().is_symlink());
    if linked {
        format!(
            "{} (the file {} links to)",
            tilde(written),
            tilde(config_path)
        )
    } else {
        tilde(config_path)
    }
}

/// A token created on the backend's Tokens page, pasted or piped in: for a
/// computer whose browser isn't the one you're signed in with, such as one
/// reached over SSH.
fn read_token(endpoint: &str) -> Result<String> {
    let line = if std::io::stdin().is_terminal() {
        print!("Create a token at {endpoint}/#/tokens and paste it here (it won't show): ");
        std::io::stdout().flush()?;
        let line = read_hidden_line();
        println!();
        line?
    } else {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("reading the token")?;
        line
    };
    token_from(&line)
}

/// A line typed at the terminal, not echoed: a token on screen stays in
/// scrollback and screen shares. Where `stty` isn't there, it echoes.
fn read_hidden_line() -> Result<String> {
    let stty = |arg: &str| {
        std::process::Command::new("stty")
            .arg(arg)
            .stdin(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let hidden = stty("-echo");
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if hidden {
        stty("echo");
    }
    read.context("reading the token")?;
    Ok(line)
}

fn token_from(line: &str) -> Result<String> {
    let token = line.trim().trim_matches(['"', '\'']);
    if !groundstation_wire::is_ingest_token(token) {
        bail!(
            "that isn't an ingest token: they start with {} and are 47 characters long, as the \
             Tokens page shows them",
            groundstation_wire::INGEST_TOKEN_PREFIX
        );
    }
    Ok(token.to_string())
}

/// Back to local-only. The token keeps working until someone revokes it on
/// the backend's Tokens page: an ingest token can't revoke itself.
pub fn logout(config_path: &Path) -> Result<bool> {
    let text = config_file::read(config_path)?;
    let current = config_file::current(&text)?;
    if !current.cloud && !current.token {
        println!("Not connected: gsd keeps what it records on this computer.");
        return Ok(false);
    }
    config_file::write(config_path, &config_file::disconnect(&text)?)?;
    println!("Disconnected. Once gsd restarts, it keeps what it records on this computer.");
    if current.token {
        match current.endpoint {
            Some(at) => println!("The token still works until you revoke it at {at}/#/tokens."),
            None => {
                println!("The token still works until you revoke it on the backend's Tokens page.")
            }
        }
    }
    Ok(true)
}

fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("can't ask for confirmation without a terminal; pass --yes to connect anyway");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("reading the answer")?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_verifiers_sha256_in_hex() {
        // The same vector as the api's test, which checks it.
        assert_eq!(
            challenge("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_pasted_token_has_the_shape_the_backend_issues() {
        let token = format!("gsi_{}", "aB3-_".repeat(9).get(..43).unwrap());
        assert_eq!(token_from(&format!("  {token}\n")).unwrap(), token);
        // Copied with its quotes from config.toml or the Tokens page.
        assert_eq!(token_from(&format!("\"{token}\"")).unwrap(), token);
        for bad in [
            "\n".to_string(),
            format!("token = {token}"),
            token[..20].to_string(),
            "0199a4a2-6c1e-7d3c-9b2a-1f2e3d4c5b6a".into(),
            format!("{token}\u{200B}"),
        ] {
            assert!(token_from(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_default_label_is_one_the_backend_takes() {
        assert!(groundstation_wire::valid_token_label(&default_label()));
    }

    #[test]
    fn logging_out_when_not_connected_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\n[transport]\nmode = \"local-only\"\n").unwrap();
        assert!(!logout(&path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# mine\n[transport]\nmode = \"local-only\"\n"
        );
        assert!(!logout(&dir.path().join("missing.toml")).unwrap());
        std::fs::write(
            &path,
            "[transport]\nmode = \"cloud\"\nendpoint = \"https://x\"\ntoken = \"t\"\n",
        )
        .unwrap();
        assert!(logout(&path).unwrap());
        let config_file::Current { cloud, token, .. } =
            config_file::current(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!cloud && !token);
    }

    /// A backend that answers every request with `status`.
    async fn answering(status: &'static str) -> String {
        answering_with(status, r#"{"error":"unknown token"}"#).await
    }

    /// A backend that answers every request with `status` and `body`.
    async fn answering_with(status: &'static str, body: &str) -> String {
        let body = body.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_token_the_backend_refuses_isnt_saved() {
        let token = format!("gsi_{}", "a".repeat(43));
        let refused = check_token(&answering("401 Unauthorized").await, &token).await;
        assert!(format!("{:#}", refused.unwrap_err()).contains("unknown token"));
        assert!(
            check_token(&answering("200 OK").await, &token)
                .await
                .is_ok()
        );
        // A suspended workspace is about the token too.
        let suspended = answering_with("403 Forbidden", r#"{"error":"tenant suspended"}"#).await;
        let e = format!("{:#}", check_token(&suspended, &token).await.unwrap_err());
        assert!(
            e.contains("doesn't take that token: tenant suspended"),
            "{e}"
        );
        // A backend that can't say isn't a refusal.
        assert!(
            check_token(&answering("503 Service Unavailable").await, &token)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn the_api_answering_for_ingest_isnt_blamed_on_the_token() {
        let token = format!("gsi_{}", "a".repeat(43));
        let body = format!(
            r#"{{"error":"{}"}}"#,
            groundstation_wire::CROSS_ORIGIN_REFUSED
        );
        let api = answering_with("403 Forbidden", &body).await;
        let e = format!("{:#}", check_token(&api, &token).await.unwrap_err());
        assert!(!e.contains("doesn't take that token"), "{e}");
        assert!(e.contains("POST /v1/events"), "{e}");
    }
}
