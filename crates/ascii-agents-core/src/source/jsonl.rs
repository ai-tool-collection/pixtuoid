use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::source::decoder::{decode_jsonl_line, SOURCE_NAME};
use crate::source::{AgentEvent, TaggedSender, Transport};
use crate::AgentId;

pub trait NamingStrategy: Send + Sync + 'static {
    fn derive_label(&self, path: &Path, source: &str, cwd: &Path) -> String;
}

pub struct DefaultNamingStrategy;

impl NamingStrategy for DefaultNamingStrategy {
    fn derive_label(&self, path: &Path, source: &str, cwd: &Path) -> String {
        let is_subagent = path.to_string_lossy().contains("subagents");
        if is_subagent {
            "subagent".to_string()
        } else if source == "claude-code" && cwd != Path::new("") && cwd != Path::new("/") {
            cwd.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(source)
                .to_string()
        } else {
            source.to_string()
        }
    }
}

pub struct JsonlWatcher {
    root: PathBuf,
    /// On startup, only emit SessionStart for transcripts whose mtime is
    /// within this window. Older files have their cursor seeded at end-of-file
    /// so any future writes still bring them live (next SessionStart fires
    /// then). Without this, every historical .jsonl floods the desk allocator.
    initial_window: Duration,
    source_name: String,
    naming_strategy: Arc<dyn NamingStrategy>,
}

/// On startup, transcripts modified within this window are treated as
/// "live" — `SessionStart` fires for them so their sprites appear without
/// the user needing to fire a fresh tool call. Older files have cursors
/// seeded at EOF (no flood). Bumped from 10 min to 1 hour after users hit
/// the case "I had a CC session open but it had been idle a while; when I
/// started ascii-agents nothing showed up until I made a new tool call."
const DEFAULT_INITIAL_WINDOW: Duration = Duration::from_secs(3600);

impl JsonlWatcher {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            initial_window: DEFAULT_INITIAL_WINDOW,
            source_name: SOURCE_NAME.to_string(),
            naming_strategy: Arc::new(DefaultNamingStrategy),
        }
    }

    pub fn with_initial_window(root: PathBuf, window: Duration) -> Self {
        Self {
            root,
            initial_window: window,
            source_name: SOURCE_NAME.to_string(),
            naming_strategy: Arc::new(DefaultNamingStrategy),
        }
    }

    pub fn with_source(mut self, source: String) -> Self {
        self.source_name = source;
        self
    }

    pub fn with_naming_strategy(mut self, strategy: Arc<dyn NamingStrategy>) -> Self {
        self.naming_strategy = strategy;
        self
    }

    pub async fn run(self, tx: TaggedSender) -> Result<()> {
        let cursors: Arc<Mutex<HashMap<PathBuf, u64>>> = Arc::new(Mutex::new(HashMap::new()));
        let seen_sessions: Arc<Mutex<HashMap<PathBuf, bool>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                            let _ = notify_tx.send(path);
                        }
                    }
                }
            })?;
        let _ = tokio::fs::create_dir_all(&self.root).await;
        watcher.watch(&self.root, RecursiveMode::Recursive)?;

        let source_arc: Arc<str> = Arc::from(self.source_name.as_str());
        let strategy = self.naming_strategy.clone();

        initial_seed_root(
            &self.root,
            self.initial_window,
            &source_arc,
            &strategy,
            &cursors,
            &seen_sessions,
            &tx,
        )
        .await;

        loop {
            let source_arc = source_arc.clone();
            let strategy = strategy.clone();
            tokio::select! {
                Some(path) = notify_rx.recv() => {
                    walk_jsonl(&path, &source_arc, &strategy, &cursors, &seen_sessions, &tx).await;
                }
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    scan_root(&self.root, &source_arc, &strategy, &cursors, &seen_sessions, &tx).await;
                }
            }
        }
    }
}

async fn initial_seed_root(
    root: &Path,
    window: Duration,
    source: &Arc<str>,
    naming_strategy: &Arc<dyn NamingStrategy>,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, bool>>>,
    tx: &TaggedSender,
) {
    if let Ok(mut read) = tokio::fs::read_dir(root).await {
        while let Ok(Some(entry)) = read.next_entry().await {
            initial_seed_walk(&entry.path(), window, source, naming_strategy, cursors, seen, tx).await;
        }
    }
}

async fn initial_seed_walk(
    path: &Path,
    window: Duration,
    source: &Arc<str>,
    naming_strategy: &Arc<dyn NamingStrategy>,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, bool>>>,
    tx: &TaggedSender,
) {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.is_dir() {
        if let Ok(mut read) = tokio::fs::read_dir(path).await {
            while let Ok(Some(entry)) = read.next_entry().await {
                Box::pin(initial_seed_walk(&entry.path(), window, source, naming_strategy, cursors, seen, tx)).await;
            }
        }
        return;
    }
    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return;
    }

    let recent = meta
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|elapsed| elapsed <= window)
        .unwrap_or(false);

    if recent {
        // Live session: let the normal walk_jsonl flow read from offset 0,
        // emit SessionStart, and replay content so in-flight Task / tool
        // state survives an ascii-agents restart.
        walk_jsonl(path, source, naming_strategy, cursors, seen, tx).await;
    } else {
        // Stale: seed cursor at end so historical events don't replay, and
        // leave `seen` untouched so the first future write triggers a fresh
        // SessionStart-on-first-sight via walk_jsonl.
        cursors.lock().await.insert(path.to_path_buf(), meta.len());
    }
}

async fn scan_root(
    root: &Path,
    source: &Arc<str>,
    naming_strategy: &Arc<dyn NamingStrategy>,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, bool>>>,
    tx: &TaggedSender,
) {
    if let Ok(mut read) = tokio::fs::read_dir(root).await {
        while let Ok(Some(entry)) = read.next_entry().await {
            walk_jsonl(&entry.path(), source, naming_strategy, cursors, seen, tx).await;
        }
    }
}

async fn walk_jsonl(
    path: &Path,
    source: &Arc<str>,
    naming_strategy: &Arc<dyn NamingStrategy>,
    cursors: &Arc<Mutex<HashMap<PathBuf, u64>>>,
    seen: &Arc<Mutex<HashMap<PathBuf, bool>>>,
    tx: &TaggedSender,
) {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.is_dir() {
        if let Ok(mut read) = tokio::fs::read_dir(path).await {
            while let Ok(Some(entry)) = read.next_entry().await {
                Box::pin(walk_jsonl(&entry.path(), source, naming_strategy, cursors, seen, tx)).await;
            }
        }
        return;
    }
    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return;
    }

    let file_len = meta.len();

    // Cap on how many unprocessed bytes we'll tolerate without a newline.
    // Protects against an attacker (or buggy writer) emitting a giant single
    // line — without this cap, every notify event re-reads the entire pending
    // tail, growing without bound.
    const MAX_PENDING_BYTES: u64 = 1 << 20; // 1 MiB

    let cursor_now: u64 = {
        let cursors_g = cursors.lock().await;
        *cursors_g.get(path).unwrap_or(&0)
    };
    if cursor_now > file_len {
        // File shrank (truncation / rotation). Reset cursor; treat as fresh.
        warn!(
            "{} truncated below cursor ({} < {}), resetting cursor",
            path.display(),
            file_len,
            cursor_now
        );
        cursors.lock().await.insert(path.to_path_buf(), 0);
        return;
    }
    if cursor_now == file_len {
        return; // nothing new
    }
    if file_len - cursor_now > MAX_PENDING_BYTES {
        warn!(
            "{} has > {} pending bytes with no newline; skipping to end",
            path.display(),
            MAX_PENDING_BYTES
        );
        cursors.lock().await.insert(path.to_path_buf(), file_len);
        return;
    }

    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            warn!("open {} failed: {e}", path.display());
            return;
        }
    };
    if let Err(e) = file.seek(SeekFrom::Start(cursor_now)).await {
        warn!("seek {} failed: {e}", path.display());
        return;
    }
    let mut new_chunk = Vec::with_capacity((file_len - cursor_now) as usize);
    if let Err(e) = file.read_to_end(&mut new_chunk).await {
        warn!("read tail of {} failed: {e}", path.display());
        return;
    }

    // Consume only up to the last complete (newline-terminated) line; any
    // partial trailing line stays buffered until the next notify event
    // completes it. `safe_end_relative` is within `new_chunk`.
    let safe_end_relative = match new_chunk.iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => 0,
    };
    if safe_end_relative == 0 {
        return; // only a partial line — wait for more
    }
    let new_cursor = cursor_now + safe_end_relative as u64;
    {
        let mut cursors_g = cursors.lock().await;
        cursors_g.insert(path.to_path_buf(), new_cursor);
    }

    let new_bytes = &new_chunk[..safe_end_relative];
    let transcript_path_str = path.to_string_lossy().into_owned();

    // Emit SessionStart on first sight of this transcript.
    {
        let mut seen = seen.lock().await;
        if seen.insert(path.to_path_buf(), true).is_none() {
            let id = AgentId::from_transcript_path(&transcript_path_str);
            let session_id = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Try to extract cwd from the first parseable line; harmless if it fails.
            // First-sight only: cursor_now==0 here, so new_bytes is the
            // entire transcript prefix — fine to scan for cwd.
            let cwd = extract_cwd(new_bytes).unwrap_or_default();
            let _ = tx
                .send((
                    Transport::Jsonl,
                    AgentEvent::SessionStart {
                        agent_id: id,
                        source: source.to_string(),
                        session_id: session_id.clone(),
                        cwd: cwd.clone(),
                    },
                ))
                .await;

            let label = naming_strategy.derive_label(path, source, &cwd);
            let _ = tx
                .send((
                    Transport::Jsonl,
                    AgentEvent::Rename {
                        agent_id: id,
                        label,
                    },
                ))
                .await;
        }
    }

    for line in new_bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let s = match std::str::from_utf8(line) {
            Ok(s) => s,
            Err(_) => {
                warn!("non-utf8 line in {}", path.display());
                continue;
            }
        };
        let v: serde_json::Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(e) => {
                debug!("skip non-json line in {}: {e}", path.display());
                continue;
            }
        };
        match decode_jsonl_line(&transcript_path_str, v) {
            Ok(events) => {
                for ev in events {
                    if tx.send((Transport::Jsonl, ev)).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => warn!("decode error in {}: {e}", path.display()),
        }
    }
}

fn extract_cwd(bytes: &[u8]) -> Option<PathBuf> {
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
            continue;
        };
        if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}
