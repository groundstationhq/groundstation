//! The loopback listener `cloud login` waits on. The browser lands on
//! `http://127.0.0.1:PORT/callback?state=...&code=...` after the person
//! approves. Only a request carrying the `state` this login generated counts,
//! so another page can't hand the CLI a code of its choosing. The browser
//! waits for its answer until the CLI has collected the token with the code,
//! so the page it shows says how it really ended.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// The request line, `GET /callback?state=…&code=… HTTP/1.1`, is short.
const MAX_REQUEST_LINE: usize = 8 * 1024;
/// How much of the rest of a request is read, and dropped: browsers send
/// every cookie set for 127.0.0.1, whatever the port.
const MAX_HEAD: usize = 64 * 1024;
/// How long one connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections handled at once. Past this the oldest is closed, so idle
/// connections (from any local process) can't keep the browser's out.
const MAX_CONNECTIONS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The one-time code that collects the token the person approved.
    Code(String),
    Denied,
}

/// The browser's request that brought this login's `state`, waiting for its
/// answer.
pub struct Callback {
    pub outcome: Outcome,
    stream: TcpStream,
}

impl Callback {
    /// Tells the browser how it ended, in a sentence.
    pub async fn answer(mut self, text: &str) {
        respond(&mut self.stream, "200 OK", text).await;
    }
}

/// Answers requests until one brings this login's `state`, or `timeout` passes.
///
/// Each connection is handled on its own: browsers open connections they
/// never use, and one of those must not hold up the one with the callback.
pub async fn wait(listener: TcpListener, state: &str, timeout: Duration) -> Result<Callback> {
    let accept = async {
        let mut connections = JoinSet::new();
        let mut oldest_first = VecDeque::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        if oldest_first.len() == MAX_CONNECTIONS
                            && let Some(oldest) = oldest_first.pop_front()
                        {
                            tokio::task::AbortHandle::abort(&oldest);
                        }
                        let state = state.to_string();
                        let handling = connections.spawn(async move { handle(stream, &state).await });
                        oldest_first.push_back(handling);
                    }
                    // Out of descriptors, or a connection gone before it was
                    // taken: neither ends the login.
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                },
                Some(answered) = connections.join_next() => {
                    if let Ok(Some(callback)) = answered {
                        return callback;
                    }
                }
            }
        }
    };
    match tokio::time::timeout(timeout, accept).await {
        Ok(callback) => Ok(callback),
        Err(_) => bail!(
            "no answer from the browser within {} minutes. The approval page has to be opened \
             in a browser on this computer; on one you reach over SSH, use --token-stdin",
            timeout.as_secs() / 60
        ),
    }
}

async fn handle(mut stream: TcpStream, state: &str) -> Option<Callback> {
    let target = tokio::time::timeout(READ_TIMEOUT, read_target(&mut stream))
        .await
        .ok()?
        .ok()?;
    match parse(&target, state) {
        Some(outcome) => Some(Callback { outcome, stream }),
        None => {
            respond(&mut stream, "404 Not Found", "Nothing here.").await;
            None
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, text: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Ground Station</title>\
         <body style=\"font:15px system-ui;background:#0a0a0b;color:#ededef;display:grid;place-items:center;min-height:90vh\">{text}"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n\
         cache-control: no-store\r\nreferrer-policy: no-referrer\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

/// The request target of `GET <target> HTTP/1.1`. The rest of the request
/// is read and dropped: closing with it unread would reset the connection
/// before the browser reads the answer.
async fn read_target(stream: &mut TcpStream) -> Result<String> {
    let mut head = Vec::new();
    let mut buf = [0; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            bail!("connection closed");
        }
        head.extend_from_slice(&buf[..n]);
    }
    let end = head
        .windows(2)
        .position(|w| w == b"\r\n")
        .filter(|end| *end <= MAX_REQUEST_LINE)
        .context("no request line")?;
    let line = String::from_utf8_lossy(&head[..end]);
    match line.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["GET", target, _] => Ok(target.to_string()),
        _ => bail!("not a GET"),
    }
}

/// `/callback?state=S&code=C` or `/callback?state=S&error=denied`, for this `state`.
fn parse(target: &str, state: &str) -> Option<Outcome> {
    let query = target.strip_prefix("/callback?")?;
    let param = |name: &str| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    if param("state")? != state {
        return None;
    }
    if let Some(code) = param("code").filter(|c| !c.is_empty()) {
        return Some(Outcome::Code(code));
    }
    param("error").map(|_| Outcome::Denied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_logins_state_counts() {
        let s = "abc123";
        assert_eq!(
            parse("/callback?state=abc123&code=gsc_x%2Dy", s),
            Some(Outcome::Code("gsc_x-y".into()))
        );
        // The token itself is never what the browser brings.
        assert_eq!(parse("/callback?state=abc123&token=gsi_x", s), None);
        assert_eq!(
            parse("/callback?state=abc123&error=denied", s),
            Some(Outcome::Denied)
        );
        assert_eq!(parse("/callback?state=other&code=gsc_x", s), None);
        assert_eq!(parse("/favicon.ico", s), None);
        assert_eq!(parse("/callback?code=gsc_x", s), None);
    }

    #[tokio::test]
    async fn takes_a_callback_carrying_many_cookies() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let waiting =
            tokio::spawn(async move { wait(listener, "s3cr3t", Duration::from_secs(5)).await });
        // Every dev server on 127.0.0.1 shares its cookies with this port.
        let cookies = (0..40)
            .map(|i| format!("app{i}_session={}", "x".repeat(500)))
            .collect::<Vec<_>>()
            .join("; ");
        let right = tokio::spawn(
            reqwest::Client::new()
                .get(format!("{base}/callback?state=s3cr3t&code=gsc_good"))
                .header("cookie", cookies)
                .send(),
        );
        let callback = waiting.await.unwrap().unwrap();
        assert_eq!(callback.outcome, Outcome::Code("gsc_good".into()));
        callback.answer("Connected.").await;
        assert_eq!(right.await.unwrap().unwrap().status(), 200);
    }

    #[tokio::test]
    async fn idle_connections_dont_keep_the_browser_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let waiting =
            tokio::spawn(async move { wait(listener, "s3cr3t", Duration::from_secs(5)).await });
        let mut idle = Vec::new();
        for _ in 0..(MAX_CONNECTIONS * 3) {
            idle.push(TcpStream::connect(addr).await.unwrap());
        }
        let right = tokio::spawn(
            reqwest::Client::new()
                .get(format!("http://{addr}/callback?state=s3cr3t&code=gsc_good"))
                .send(),
        );
        let callback = waiting.await.unwrap().unwrap();
        assert_eq!(callback.outcome, Outcome::Code("gsc_good".into()));
        callback.answer("Connected.").await;
        assert_eq!(right.await.unwrap().unwrap().status(), 200);
    }

    #[tokio::test]
    async fn waits_past_strangers_for_the_right_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let waiting =
            tokio::spawn(async move { wait(listener, "s3cr3t", Duration::from_secs(5)).await });
        let http = reqwest::Client::new();
        let wrong = http
            .get(format!("{base}/callback?state=guess&code=evil"))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 404);
        // A connection that never sends anything, as browsers open them.
        let _idle = TcpStream::connect(base.trim_start_matches("http://"))
            .await
            .unwrap();
        let right = tokio::spawn(
            http.get(format!("{base}/callback?state=s3cr3t&code=gsc_good"))
                .send(),
        );
        let callback = waiting.await.unwrap().unwrap();
        assert_eq!(callback.outcome, Outcome::Code("gsc_good".into()));
        // The browser hears back only once the CLI answers.
        assert!(!right.is_finished());
        callback.answer("Connected.").await;
        let right = right.await.unwrap().unwrap();
        assert_eq!(right.status(), 200);
        assert!(right.text().await.unwrap().ends_with("Connected."));
    }
}
