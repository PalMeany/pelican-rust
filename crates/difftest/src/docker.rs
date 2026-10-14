//! Minimal Docker Engine API client over the unix socket (mounted by
//! `scripts/cargo` when `PELICAN_DOCKER_SOCK=1`): runs artisan commands in the
//! original panel's container for `command` steps.

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const SOCKET: &str = "/var/run/docker.sock";

async fn request(method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<(u16, Vec<u8>)> {
    let mut s = UnixStream::connect(SOCKET).await.with_context(|| format!("connecting to {SOCKET}"))?;
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let head = format!(
        "{method} /v1.41{path} HTTP/1.1\r\nHost: docker\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    s.write_all(head.as_bytes()).await?;
    s.write_all(payload.as_bytes()).await?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").context("malformed docker response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).context("docker status line")?;
    let mut body = raw[split + 4..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    Ok((status, body))
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..pos]).trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = pos + 2;
        let end = (start + size).min(b.len());
        out.extend_from_slice(&b[start..end]);
        b = &b[(end + 2).min(b.len())..];
    }
    out
}

/// `docker exec -u www-data <container> <cmd...>`; returns the exit code.
pub async fn exec(container: &str, cmd: &[&str]) -> Result<i64> {
    let (st, body) = request(
        "POST",
        &format!("/containers/{container}/exec"),
        Some(&serde_json::json!({"AttachStdout": true, "AttachStderr": true, "User": "www-data", "Cmd": cmd})),
    )
    .await?;
    if st != 201 {
        bail!("docker exec create: HTTP {st}: {}", String::from_utf8_lossy(&body));
    }
    let id = serde_json::from_slice::<serde_json::Value>(&body)?["Id"].as_str().context("exec id")?.to_string();
    let (st, out) =
        request("POST", &format!("/exec/{id}/start"), Some(&serde_json::json!({"Detach": false, "Tty": false})))
            .await?;
    if st != 200 {
        bail!("docker exec start: HTTP {st}");
    }
    let (_, inspect) = request("GET", &format!("/exec/{id}/json"), None).await?;
    let code = serde_json::from_slice::<serde_json::Value>(&inspect)?["ExitCode"].as_i64().unwrap_or(-1);
    if code != 0 {
        eprintln!("[orig] {} exited {code}: {}", cmd.join(" "), String::from_utf8_lossy(&out));
    }
    Ok(code)
}
