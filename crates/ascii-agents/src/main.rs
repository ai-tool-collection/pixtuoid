mod cli;
mod install;
mod runtime;
mod tui;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Cmd};

fn main() -> Result<()> {
    let (log_level, cmd) = Cli::parse().cmd_or_default();
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&log_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cmd {
        Cmd::Run {
            socket,
            projects_root,
            max_desks,
            headless,
        } => runtime::run(socket, projects_root, max_desks, headless),
        Cmd::InstallHooks {
            hook_path,
            settings,
        } => install::install(hook_path, settings),
        Cmd::UninstallHooks { settings } => install::uninstall(settings),
    }
}
