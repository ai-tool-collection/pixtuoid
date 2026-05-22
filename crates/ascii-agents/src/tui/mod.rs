pub mod embedded_pack;
pub mod frame_cache;
pub mod layout;
pub mod pathfind;
pub mod pose;
pub mod renderer;

use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ascii_agents_core::sprite::{Rgb, RgbBuffer};
use ascii_agents_core::walkable::OccupancyOverlay;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};

use pathfind::{AStarRouter, Router};
use renderer::{draw_scene, setup_terminal, teardown_terminal};

use crate::runtime::SceneRx;

pub async fn run_tui(mut scene_rx: SceneRx) -> Result<()> {
    let pack = embedded_pack::load_default_pack()?;
    let mut term = setup_terminal()?;
    let mut rgb_buf = RgbBuffer::filled(0, 0, Rgb(0, 0, 0));
    let mut frame_cache = frame_cache::FrameCache::new();
    let mut router: AStarRouter = AStarRouter::new();
    let mut overlay = OccupancyOverlay::new();
    let mut history = pose::PoseHistory::new();
    // Track the static-mask signature so the router drops its cache when the
    // obstacle set changes (terminal resize, agent count crosses the
    // visible-desk threshold). Dynamic occupancy churn is handled inside
    // the router via overlay signature.
    let mut last_layout_sig: Option<(u16, u16, usize)> = None;
    // Mouse cell coordinates of the most recent move event. `None` when the
    // pointer is outside the terminal or before any mouse event has fired.
    let mut mouse_pos: Option<(u16, u16)> = None;

    let tick = Duration::from_millis(33); // ~30 fps
    let result: Result<()> = (async {
        loop {
            let now = SystemTime::now();
            // O(1) pointer read — no lock, no clone of SceneState's contents.
            let snapshot = scene_rx.borrow_and_update().clone();
            frame_cache.evict_missing(&snapshot);
            let sig = (rgb_buf.width, rgb_buf.height, snapshot.max_desks);
            if last_layout_sig != Some(sig) {
                router.invalidate();
                last_layout_sig = Some(sig);
            }
            // draw_scene rebuilds `overlay` internally from current agent
            // positions before computing routed poses, so the router
            // routes around live agents and characters don't overlap.
            draw_scene(
                &mut term,
                &snapshot,
                &pack,
                now,
                &mut rgb_buf,
                &mut frame_cache,
                &mut router,
                &mut overlay,
                &mut history,
                mouse_pos,
            )?;

            let start = Instant::now();
            // Drain every event that arrived during this tick. Mouse moves
            // can fire 50-200/s on a fast cursor — we want the latest
            // position before the next frame, not just the first one.
            let mut polled = event::poll(tick)?;
            let mut quit = false;
            while polled {
                match event::read()? {
                    Event::Key(k) => match (k.code, k.modifiers) {
                        (KeyCode::Char('q'), _)
                        | (KeyCode::Esc, _)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            quit = true;
                        }
                        _ => {}
                    },
                    Event::Mouse(m) => {
                        // Track move + drag positions; ignore scroll/button
                        // press events for now (no other interaction yet).
                        if matches!(
                            m.kind,
                            MouseEventKind::Moved
                                | MouseEventKind::Drag(_)
                                | MouseEventKind::Down(_)
                        ) {
                            mouse_pos = Some((m.column, m.row));
                        }
                    }
                    _ => {}
                }
                polled = event::poll(Duration::from_millis(0))?;
            }
            if quit {
                break;
            }
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
