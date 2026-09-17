//! Local web UI server for the `csusage web` command.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

use csusage_cli::WebArgs;

use crate::Result;

const INDEX_HTML: &str = include_str!("../../assets/web/index.html");

/// Runs the web UI server until interrupted.
pub(crate) fn run_web(args: WebArgs) -> Result<()> {
    let host = args.host.clone();
    let port = args.port;
    let listener = TcpListener::bind((host.as_str(), port)).map_err(|error| {
        csusage_core::cli_error(format!("failed to bind {host}:{port}: {error}"))
    })?;
    println!("csusage web UI listening on http://{host}:{port}");
    println!("Press Ctrl+C to stop.");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                handle_connection(stream, &args);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, args: &WebArgs) {
    let mut buffer = [0u8; 4096];
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
        "/api/usage" => match csusage_adapter_all::load_dashboard(&args.shared) {
            Ok(dashboard) => (
                "200 OK",
                "application/json",
                serde_json::to_string(&dashboard).unwrap_or_default(),
            ),
            Err(error) => (
                "500 Internal Server Error",
                "application/json",
                format!("{{\"error\":{}}}", serde_json::json!(error.to_string())),
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
