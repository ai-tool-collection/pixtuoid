use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::Value;

fn main() -> Result<()> {
    let socket = std::env::var("ASCII_AGENTS_SOCKET")
        .unwrap_or_else(|_| "/tmp/ascii-agents.sock".to_string());

    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read stdin")?;
    let mut payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        // If we can't parse, exit 0 silently so CC isn't blocked.
        Err(_) => return Ok(()),
    };

    if let Value::Object(map) = &mut payload {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        map.insert("_shim_ts_ms".into(), Value::from(ts as u64));
    }

    // Best-effort send. If the daemon isn't running, exit 0 — never block CC.
    if let Ok(mut s) = UnixStream::connect(&socket) {
        let mut line = serde_json::to_vec(&payload).unwrap_or_default();
        line.push(b'\n');
        let _ = s.write_all(&line);
    }
    Ok(())
}
