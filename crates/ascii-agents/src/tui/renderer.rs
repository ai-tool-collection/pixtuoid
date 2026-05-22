//! Terminal-coupled rendering: orchestrator (`draw_scene`), half-block
//! flush, label/tooltip/notice widget overlays, and terminal lifecycle.
//!
//! The pure-pixel pass (floor/walls/decor/characters → `RgbBuffer`) lives
//! in `tui::pixel_painter`. This file is the integrator that calls into
//! that pipeline and then hands the buffer to ratatui.

use std::collections::HashMap;
use std::io::{stdout, Stdout};
use std::time::SystemTime;

use anyhow::Result;
use ascii_agents_core::sprite::format::Pack;
use ascii_agents_core::sprite::{Rgb, RgbBuffer};
use ascii_agents_core::state::ActivityState;
use ascii_agents_core::{AgentId, SceneState};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

use ascii_agents_core::walkable::OccupancyOverlay;

use crate::tui::frame_cache::FrameCache;
use crate::tui::layout::{Layout, DESK_W};
use crate::tui::pathfind::Router;
use crate::tui::pixel_painter::{character_anchor, render_to_rgb_buffer};
use crate::tui::pose;

/// Wall band background — used to fall back to a sensible color when the
/// terminal is too small to render the full scene.
const BG: Rgb = Rgb(28, 32, 40);

/// Clip a widget rect to fit inside `bounds`. Returns `None` if the rect
/// falls fully outside or has zero width/height after clipping — callers
/// use that to skip the render entirely. Prevents ratatui's
/// "index outside of buffer" panic when label/notice widgets land near
/// the right or bottom edge.
fn clip_widget_rect(rect: Rect, bounds: Rect) -> Option<Rect> {
    if rect.x >= bounds.x + bounds.width || rect.y >= bounds.y + bounds.height {
        return None;
    }
    if rect.x + rect.width <= bounds.x || rect.y + rect.height <= bounds.y {
        return None;
    }
    let x = rect.x.max(bounds.x);
    let y = rect.y.max(bounds.y);
    let right = (rect.x + rect.width).min(bounds.x + bounds.width);
    let bot = (rect.y + rect.height).min(bounds.y + bounds.height);
    if right <= x || bot <= y {
        return None;
    }
    Some(Rect {
        x,
        y,
        width: right - x,
        height: bot - y,
    })
}

pub type Term = Terminal<CrosstermBackend<Stdout>>;


// --- Terminal lifecycle ---------------------------------------------------
pub fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = stdout();
    // EnableMouseCapture turns on the terminal's mouse-event reporting.
    // Modern terminals emit MouseEventKind::Moved on cursor motion (no
    // button required), which is how we drive the hover tooltip.
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

pub fn teardown_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    term.show_cursor()?;
    Ok(())
}


// --- draw_scene ----------------------------------------------------------
//
// `draw_scene` is the orchestrator: get terminal geometry, compute the
// layout, run the pure pixel pass, then flush to the terminal. The two
// helpers below are deliberately split:
//
//   * `render_to_rgb_buffer` — pure RGB output. No ratatui types, no
//     terminal I/O. Can be called by any renderer (web canvas, PNG
//     snapshot, GIF capture).
//   * `flush_to_terminal` — ratatui half-block compression + label overlay
//     + bulletin notice + footer. Terminal-specific, runs inside
//     `term.draw`.
#[allow(clippy::too_many_arguments)]
pub fn draw_scene<B: Backend>(
    term: &mut Terminal<B>,
    scene: &SceneState,
    pack: &Pack,
    now: SystemTime,
    buf: &mut RgbBuffer,
    cache: &mut FrameCache,
    router: &mut dyn Router,
    overlay: &mut OccupancyOverlay,
    history: &mut pose::PoseHistory,
    mouse_pos: Option<(u16, u16)>,
) -> Result<()> {
    let term_size = term.size()?;
    let full_rect = Rect {
        x: 0,
        y: 0,
        width: term_size.width,
        height: term_size.height,
    };
    let scene_rect = Rect {
        x: 0,
        y: 0,
        width: full_rect.width,
        height: full_rect.height.saturating_sub(1),
    };
    if scene_rect.width < 20 || scene_rect.height < 12 {
        term.draw(|f| paint_footer(f, full_rect))?;
        return Ok(());
    }

    let buf_w = scene_rect.width;
    let buf_h = scene_rect.height * 2;
    buf.ensure_size(buf_w, buf_h, BG);
    let Some(layout) = Layout::compute(buf_w, buf_h, scene.max_desks) else {
        term.draw(|f| paint_footer(f, full_rect))?;
        return Ok(());
    };

    // Bias the router toward the corridor (the office "main aisle") so
    // walkers naturally use the hallway instead of cutting diagonally
    // across the cubicle floor. Cheap call — invalidates the cache only
    // when the zone actually changes (layout resize).
    router.set_preferred_zone(layout.corridor);

    // Pure pixel pass — no ratatui types touched. Pixel pass writes
    // into PoseHistory for every walking/waypoint agent so the next
    // frame's snap-back lookup is fresh.
    render_to_rgb_buffer(
        scene, &layout, pack, now, buf, cache, router, overlay, history,
    );

    // Hit-test the cursor against each agent's current sprite footprint
    // so the tooltip + focus ring know who's under the pointer. Cell-
    // accurate (one terminal cell = 2 vertical pixels in the half-block
    // buffer).
    let hovered = mouse_pos
        .and_then(|(mx, my)| hit_test_agent(scene, &layout, now, router, overlay, history, mx, my));

    // Terminal-flush pass — half-block + widgets, inside ratatui's draw.
    term.draw(|f| {
        paint_footer(f, full_rect);
        flush_buffer_to_term(f, buf, scene_rect);
        paint_label_widgets(
            f, scene, &layout, now, router, overlay, history, scene_rect, hovered,
        );
        paint_bulletin_notice(f, scene, &layout, scene_rect);
        if let (Some(agent_id), Some((mx, my))) = (hovered, mouse_pos) {
            paint_hover_tooltip(f, scene, agent_id, mx, my, scene_rect);
        }
    })?;
    Ok(())
}

fn paint_footer(f: &mut ratatui::Frame<'_>, full_rect: Rect) {
    let footer =
        Paragraph::new(Span::raw(" [q] quit ")).style(Style::default().fg(Color::DarkGray));
    f.render_widget(
        footer,
        Rect {
            x: full_rect.x,
            y: full_rect.y + full_rect.height.saturating_sub(1),
            width: full_rect.width,
            height: 1,
        },
    );
}

fn flush_buffer_to_term(f: &mut ratatui::Frame<'_>, buf: &RgbBuffer, scene_rect: Rect) {
    let term_buf = f.buffer_mut();
    let w = buf.width as usize;
    let cell_rows = (buf.height / 2) as usize;
    for cy in 0..cell_rows {
        for cx in 0..(buf.width as usize) {
            let x = scene_rect.x + cx as u16;
            let y = scene_rect.y + cy as u16;
            if x >= scene_rect.x + scene_rect.width || y >= scene_rect.y + scene_rect.height {
                continue;
            }
            let py_top = cy * 2;
            let py_bot = cy * 2 + 1;
            let fg = buf.pixels[py_top * w + cx];
            let bg = buf.pixels[py_bot * w + cx];
            let cell = &mut term_buf[(x, y)];
            cell.set_symbol("▀");
            cell.fg = Color::Rgb(fg.0, fg.1, fg.2);
            cell.bg = Color::Rgb(bg.0, bg.1, bg.2);
        }
    }
}

/// Labels above each character — uses `character_anchor` to follow the
/// agent along its current path, color-codes by activity, falls back to
/// disambiguating session-id suffix only when multiple agents share a label.
///
/// `hovered` highlights one agent's label: bright white + bold + leading
/// ▸ marker so the focused character is easy to pick out of a crowd.
#[allow(clippy::too_many_arguments)]
fn paint_label_widgets(
    f: &mut ratatui::Frame<'_>,
    scene: &SceneState,
    layout: &Layout,
    now: SystemTime,
    router: &mut dyn Router,
    overlay: &OccupancyOverlay,
    history: &mut pose::PoseHistory,
    scene_rect: Rect,
    hovered: Option<AgentId>,
) {
    let agents: Vec<_> = scene.agents.values().cloned().collect();
    let mut label_counts: HashMap<&str, usize> = HashMap::new();
    for agent in &agents {
        *label_counts.entry(&*agent.label).or_insert(0) += 1;
    }
    for agent in &agents {
        let Some(anchor) = character_anchor(agent, layout, now, router, overlay, history) else {
            continue;
        };
        let lx = scene_rect.x + anchor.x.saturating_sub(2);
        let ly = scene_rect.y + (anchor.y / 2).saturating_sub(1);
        let needs_disambig = label_counts.get(&*agent.label).copied().unwrap_or(0) > 1
            && agent.session_id.len() >= 4;
        let raw: std::borrow::Cow<'_, str> = if needs_disambig {
            std::borrow::Cow::Owned(format!("{}·{}", agent.label, &agent.session_id[..4]))
        } else {
            std::borrow::Cow::Borrowed(&*agent.label)
        };
        let display = truncate_label(&raw, (DESK_W + 4) as usize);
        let is_hovered = hovered == Some(agent.agent_id);
        let label_color = if is_hovered {
            Color::Rgb(255, 255, 255)
        } else if agent.exiting_at.is_some() {
            Color::Rgb(100, 110, 130)
        } else {
            match &agent.state {
                ActivityState::Active { .. } => Color::Rgb(140, 240, 170),
                ActivityState::Waiting { .. } => Color::Rgb(240, 200, 80),
                ActivityState::Idle => Color::Rgb(160, 160, 160),
            }
        };
        let text = if is_hovered {
            format!("▸{}", display)
        } else {
            display.into_owned()
        };
        let mut style = Style::default().fg(label_color);
        if is_hovered {
            style = style.add_modifier(ratatui::style::Modifier::BOLD);
        }
        let para = Paragraph::new(Span::styled(text, style));
        if let Some(r) = clip_widget_rect(
            Rect {
                x: lx,
                y: ly,
                width: DESK_W + 4,
                height: 1,
            },
            scene_rect,
        ) {
            f.render_widget(para, r);
        }
    }
}

/// Hit-test the mouse cursor against each agent's current sprite footprint.
/// Returns the agent under `(mx, my)` (in terminal cell coordinates), or
/// `None` if no agent occupies that cell.
///
/// The character sprite is 8×12 pixels, which in cell space is 8 cells
/// wide × 6 cells tall (one cell = 2 vertical pixels). We test against
/// that exact bounding box anchored on the agent's `character_anchor`.
#[allow(clippy::too_many_arguments)]
fn hit_test_agent(
    scene: &SceneState,
    layout: &Layout,
    now: SystemTime,
    router: &mut dyn Router,
    overlay: &OccupancyOverlay,
    history: &mut pose::PoseHistory,
    mx: u16,
    my: u16,
) -> Option<AgentId> {
    // Width-in-cells (sprite is 8 px wide; we don't divide x by 2 because
    // each pixel column is one cell column in the half-block grid).
    const SPRITE_W_CELLS: u16 = 8;
    // Height-in-cells: sprite is 12 px tall = 6 cells.
    const SPRITE_H_CELLS: u16 = 6;
    for agent in scene.agents.values() {
        let Some(anchor) = character_anchor(agent, layout, now, router, overlay, history) else {
            continue;
        };
        let cell_x = anchor.x;
        let cell_y = anchor.y / 2;
        if mx >= cell_x
            && mx < cell_x.saturating_add(SPRITE_W_CELLS)
            && my >= cell_y
            && my < cell_y.saturating_add(SPRITE_H_CELLS)
        {
            return Some(agent.agent_id);
        }
    }
    None
}

/// Floating detail panel painted near the cursor when an agent is hovered.
/// Shows the label, source, state, current tool detail, cwd, and session
/// id. Positioned to avoid the cursor itself and the screen edges.
fn paint_hover_tooltip(
    f: &mut ratatui::Frame<'_>,
    scene: &SceneState,
    agent_id: AgentId,
    mx: u16,
    my: u16,
    scene_rect: Rect,
) {
    let Some(agent) = scene.agents.get(&agent_id) else {
        return;
    };

    // Build the tooltip lines.
    let (state_label, state_detail, state_color) = match &agent.state {
        ActivityState::Idle => ("Idle", String::new(), Color::Rgb(160, 160, 160)),
        ActivityState::Active { detail, .. } => (
            "Active",
            detail.as_deref().unwrap_or("").to_string(),
            Color::Rgb(140, 240, 170),
        ),
        ActivityState::Waiting { reason } => {
            ("Waiting", reason.to_string(), Color::Rgb(240, 200, 80))
        }
    };
    let cwd_short = agent
        .cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)");
    let session_short = if agent.session_id.len() >= 8 {
        &agent.session_id[..8]
    } else {
        &agent.session_id
    };

    let mut lines: Vec<ratatui::text::Line> = Vec::new();
    lines.push(ratatui::text::Line::from(Span::styled(
        format!(" {} ", agent.label),
        Style::default()
            .fg(Color::White)
            .add_modifier(ratatui::style::Modifier::BOLD),
    )));
    lines.push(ratatui::text::Line::from(vec![
        Span::raw(" ●  "),
        Span::styled(state_label, Style::default().fg(state_color)),
    ]));
    if !state_detail.is_empty() {
        // Truncate long tool detail (e.g. full file paths) to keep tooltip narrow.
        let trimmed: String = state_detail.chars().take(34).collect();
        lines.push(ratatui::text::Line::from(Span::styled(
            format!("    {}", trimmed),
            Style::default().fg(Color::Rgb(200, 200, 210)),
        )));
    }
    lines.push(ratatui::text::Line::from(Span::styled(
        format!(" 📁 {}", cwd_short),
        Style::default().fg(Color::Rgb(180, 180, 180)),
    )));
    lines.push(ratatui::text::Line::from(Span::styled(
        format!(" ⌗ {} · {}", session_short, agent.source),
        Style::default().fg(Color::Rgb(140, 140, 150)),
    )));

    let lines_h = lines.len() as u16;
    let max_w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(20) + 2;
    let tip_w = max_w.min(scene_rect.width).max(18);
    let tip_h = lines_h;

    // Place the tooltip to the RIGHT and BELOW the cursor when there's
    // room; otherwise flip to the other side so it stays on-screen.
    let mut tx = mx.saturating_add(2);
    if tx.saturating_add(tip_w) > scene_rect.x + scene_rect.width {
        tx = mx.saturating_sub(tip_w + 1);
    }
    let mut ty = my.saturating_add(1);
    if ty.saturating_add(tip_h) > scene_rect.y + scene_rect.height {
        ty = my.saturating_sub(tip_h).max(scene_rect.y);
    }
    let rect = Rect {
        x: tx,
        y: ty,
        width: tip_w,
        height: tip_h,
    };
    let Some(clipped) = clip_widget_rect(rect, scene_rect) else {
        return;
    };

    let para =
        Paragraph::new(lines).style(Style::default().bg(Color::Rgb(20, 22, 30)).fg(Color::White));
    f.render_widget(ratatui::widgets::Clear, clipped);
    f.render_widget(para, clipped);
}

/// Live agent count painted as a sticky on the bulletin board sprite.
fn paint_bulletin_notice(
    f: &mut ratatui::Frame<'_>,
    scene: &SceneState,
    layout: &Layout,
    scene_rect: Rect,
) {
    use crate::tui::layout::WallDecor;
    let Some((_, bb_pos)) = layout
        .wall_decor
        .iter()
        .find(|(k, _)| *k == WallDecor::BulletinBoard)
    else {
        return;
    };
    let cell_x = scene_rect.x + bb_pos.x;
    let cell_y = scene_rect.y + (bb_pos.y / 2).saturating_sub(1);
    let n = scene
        .agents
        .values()
        .filter(|a| a.exiting_at.is_none())
        .count();
    let label = format!("{} live", n);
    let notice = Paragraph::new(Span::styled(label, Style::default().fg(Color::Yellow)));
    if let Some(r) = clip_widget_rect(
        Rect {
            x: cell_x,
            y: cell_y,
            width: 8,
            height: 1,
        },
        scene_rect,
    ) {
        f.render_widget(notice, r);
    }
}

/// Fit a label into `budget` chars without losing the `·xxxx` session-id
/// disambiguation suffix that the reducer appends to colliding cwds.
/// Truncates from the base (left side of the `·`), not from the suffix —
/// otherwise the disambig becomes useless ("TikTok-Android·a" tells us
/// nothing the base alone wouldn't).
fn truncate_label(label: &str, budget: usize) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if label.chars().count() <= budget {
        return Cow::Borrowed(label);
    }
    if let Some(sep_byte) = label.rfind('·') {
        let suffix = &label[sep_byte..];
        let suffix_len = suffix.chars().count();
        if suffix_len < budget {
            let base = &label[..sep_byte];
            let base_take = budget - suffix_len;
            let truncated: String = base.chars().take(base_take).collect();
            return Cow::Owned(format!("{truncated}{suffix}"));
        }
    }
    Cow::Owned(label.chars().take(budget).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_label_passes_short_labels_through() {
        assert_eq!(truncate_label("hello", 16), "hello");
    }

    #[test]
    fn truncate_label_preserves_disambig_suffix() {
        // 19 chars > 16 budget → must drop chars from the base, NOT the suffix.
        let out = truncate_label("TikTok-Android·a09a", 16);
        assert_eq!(out.chars().count(), 16);
        assert!(out.ends_with("·a09a"), "suffix lost: {out}");
        assert!(out.starts_with("TikTok"), "base over-truncated: {out}");
    }

    #[test]
    fn truncate_label_falls_back_to_plain_truncate_when_no_separator() {
        let out = truncate_label("a-very-long-project-name", 8);
        assert_eq!(out, "a-very-l");
    }
}
