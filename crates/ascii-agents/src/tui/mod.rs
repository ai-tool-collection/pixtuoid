pub mod embedded_pack;
pub mod renderer;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ascii_agents_core::SceneState;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use tokio::sync::RwLock;

use renderer::{draw_scene, setup_terminal, teardown_terminal};

pub async fn run_tui(scene: Arc<RwLock<SceneState>>) -> Result<()> {
    let pack = embedded_pack::load_default_pack()?;
    let mut term = setup_terminal()?;

    let tick = Duration::from_millis(33); // ~30 fps
    let result: Result<()> = (async {
        loop {
            let now = Instant::now();
            let snapshot = { scene.read().await.clone() };
            draw_scene(&mut term, &snapshot, &pack, now)?;

            let start = Instant::now();
            if event::poll(tick)? {
                if let Event::Key(k) = event::read()? {
                    match (k.code, k.modifiers) {
                        (KeyCode::Char('q'), _)
                        | (KeyCode::Esc, _)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                        _ => {}
                    }
                }
            }
            // Sleep the leftover budget so we cap at ~30 fps.
            let elapsed = start.elapsed();
            if let Some(rem) = tick.checked_sub(elapsed) {
                tokio::time::sleep(rem).await;
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    })
    .await;

    teardown_terminal(&mut term)?;
    result
}
