use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ascii-agents",
    version,
    about = "Terminal pixel-art office for AI coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    #[arg(long, global = true, default_value = "info")]
    pub log_level: String,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run the TUI (default if no subcommand given).
    Run {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long)]
        projects_root: Option<PathBuf>,
        #[arg(long, default_value_t = 8)]
        max_desks: usize,
        /// Skip the TUI entirely — useful for CI / scripting.
        /// Prints a JSON snapshot of SceneState every 200ms when it changes.
        #[arg(long, default_value_t = false)]
        headless: bool,
    },
    /// Install Claude Code hooks into ~/.claude/settings.json.
    InstallHooks {
        #[arg(long)]
        hook_path: Option<PathBuf>,
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Remove ascii-agents hook entries from settings.json.
    UninstallHooks {
        #[arg(long)]
        settings: Option<PathBuf>,
    },
}

impl Cli {
    pub fn cmd_or_default(self) -> (String, Cmd) {
        let level = self.log_level;
        let cmd = self.cmd.unwrap_or(Cmd::Run {
            socket: None,
            projects_root: None,
            max_desks: 8,
            headless: false,
        });
        (level, cmd)
    }
}
