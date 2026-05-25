use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use crate::source::jsonl::JsonlWatcher;
use crate::source::{Source, TaggedSender};

/// Source that watches Antigravity conversation log directories.
/// The hook events are multiplexed over the same UNIX socket as Claude Code hooks
/// since we generalized decoder.rs to dynamically support multiple sources.
pub struct AntigravitySource {
    pub brain_root: PathBuf,
}

impl AntigravitySource {
    pub fn default_paths() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        Self {
            brain_root: PathBuf::from(format!("{home}/.gemini/antigravity-cli/brain")),
        }
    }
}

#[async_trait]
impl Source for AntigravitySource {
    fn name(&self) -> &str {
        "antigravity"
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let watcher = JsonlWatcher::new(self.brain_root.clone())
            .with_source("antigravity".to_string());
        watcher.run(tx).await
    }
}
