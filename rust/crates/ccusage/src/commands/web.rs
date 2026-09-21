//! Local web UI server for the `csusage web` command.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command as ProcessCommand,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use csusage_cli::WebArgs;

use crate::Result;

const INDEX_HTML: &str = include_str!("../../assets/web/index.html");
const REMOTE_CACHE_SECONDS: u64 = 60;

/// Runs the web UI server until interrupted.
pub(crate) fn run_web(args: WebArgs) -> Result<()> {
    let host = args.host.clone();
    let port = args.port;
    let cache = RemoteCache::default();
    let listener = TcpListener::bind((host.as_str(), port)).map_err(|error| {
        csusage_core::cli_error(format!("failed to bind {host}:{port}: {error}"))
    })?;
    println!("csusage web UI listening on http://{host}:{port}");
    println!("Press Ctrl+C to stop.");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                handle_connection(stream, &args, &cache);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, args: &WebArgs, cache: &RemoteCache) {
    let mut buffer = [0u8; 8192];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let (status, content_type, body) = match path.as_str() {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", INDEX_HTML.to_string()),
        "/api/usage" => match build_usage_payload(args, cache) {
            Ok(payload) => (
                "200 OK",
                "application/json",
                serde_json::to_string(&payload).unwrap_or_default(),
            ),
            Err(error) => (
                "500 Internal Server Error",
                "application/json",
                serde_json::json!({ "error": error.to_string() }).to_string(),
            ),
        },
        _ => ("404 Not Found", "text/plain; charset=utf-8", String::new()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.read(&mut [0u8; 0]);
}

/// The full dashboard payload: local usage plus one dashboard per remote.
fn build_usage_payload(args: &WebArgs, cache: &RemoteCache) -> Result<serde_json::Value> {
    let local = csusage_adapter_all::load_dashboard(&args.shared)?;
    let mut remotes = serde_json::Map::new();
    for remote in &args.remote {
        let dashboard = cache.fetch(remote);
        remotes.insert(remote.clone(), dashboard);
    }
    Ok(serde_json::json!({
        "local": local,
        "remotes": remotes,
    }))
}

/// Caches the SSH-fetched remote dashboards so page refreshes do not open a
/// new SSH connection each time.
#[derive(Default)]
struct RemoteCache {
    entries: Mutex<std::collections::HashMap<String, (SystemTime, serde_json::Value)>>,
}

impl RemoteCache {
    fn fetch(&self, remote: &str) -> serde_json::Value {
        let mut entries = self.entries.lock().unwrap();
        if let Some((fetched_at, dashboard)) = entries.get(remote) {
            if fetched_at.elapsed().unwrap_or_default().as_secs() < REMOTE_CACHE_SECONDS {
                return dashboard.clone();
            }
        }
        let dashboard = fetch_remote_dashboard(remote);
        entries.insert(remote.to_string(), (SystemTime::now(), dashboard.clone()));
        dashboard
    }
}

/// Pulls one remote host's dashboard over SSH.
///
/// The remote machine must have `csusage` installed; its `dashboard` command
/// emits the same JSON the local web UI serves.
fn fetch_remote_dashboard(remote: &str) -> serde_json::Value {
    let output = ProcessCommand::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("--")
        .arg(remote)
        .arg("csusage")
        .arg("dashboard")
        .arg("--json")
        .output();
    let Ok(output) = output else {
        return remote_error("failed to spawn ssh".to_string());
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return remote_error(format!(
            "ssh {remote}: {}",
            stderr.chars().take(300).collect::<String>()
        ));
    }
    match serde_json::from_slice(&output.stdout) {
        Ok(dashboard) => dashboard,
        Err(error) => remote_error(format!("invalid csusage dashboard output: {error}")),
    }
}

fn remote_error(message: String) -> serde_json::Value {
    serde_json::json!({ "error": message })
}
