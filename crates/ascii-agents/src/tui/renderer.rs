use std::io::{stdout, Stdout};
use std::time::Instant;

use anyhow::Result;
use ascii_agents_core::sprite::animator::frame_index_at;
use ascii_agents_core::sprite::blit::{blit_frame, half_block_cells, HalfCell};
use ascii_agents_core::sprite::format::Pack;
use ascii_agents_core::sprite::{Frame, Palette, Pixel, Rgb, RgbBuffer};
use ascii_agents_core::state::ActivityState;
use ascii_agents_core::SceneState;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

const SHIRT_PRESETS: &[Rgb] = &[
    Rgb(0x2e, 0x62, 0xcf),
    Rgb(0x16, 0xa0, 0x6e),
    Rgb(0xb0, 0x32, 0xa8),
    Rgb(0xc6, 0x6a, 0x1e),
    Rgb(0x6c, 0x4f, 0x9e),
    Rgb(0x9c, 0x27, 0x27),
    Rgb(0x32, 0x82, 0x9b),
    Rgb(0x80, 0x55, 0x32),
];

const HAIR_PRESETS: &[Rgb] = &[
    Rgb(0x2a, 0x1a, 0x0e),
    Rgb(0x52, 0x32, 0x10),
    Rgb(0xc7, 0xa3, 0x4a),
    Rgb(0x7a, 0x32, 0x10),
    Rgb(0x3a, 0x3a, 0x3a),
];

const BG: Rgb = Rgb(20, 22, 28);
const DESK_TOP: Rgb = Rgb(110, 80, 50);
const DESK_BOT: Rgb = Rgb(70, 50, 30);

pub fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

pub fn teardown_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

fn agent_shirt(seed: u64) -> Rgb {
    SHIRT_PRESETS[(seed as usize) % SHIRT_PRESETS.len()]
}

fn agent_hair(seed: u64) -> Rgb {
    HAIR_PRESETS[((seed >> 8) as usize) % HAIR_PRESETS.len()]
}

/// Look up the base RGB for a palette key. Returns None if the key isn't
/// defined or maps to transparent.
fn base_rgb_for(palette: &Palette, key: char) -> Option<Rgb> {
    palette.get(key).flatten()
}

/// Recolor a frame: substitute any pixel matching base 'B' or 'H' RGB
/// with the per-agent equivalents. v1's "pixel substitution" approach —
/// works because each palette key has a unique RGB.
fn recolor_frame(frame: &Frame, base_palette: &Palette, shirt: Rgb, hair: Rgb) -> Frame {
    let base_b = base_rgb_for(base_palette, 'B');
    let base_h = base_rgb_for(base_palette, 'H');
    let mut out = frame.clone();
    for px in out.pixels.iter_mut() {
        if let Some(rgb) = *px {
            if Some(rgb) == base_b {
                *px = Some(shirt);
            } else if Some(rgb) == base_h {
                *px = Some(hair);
            }
        }
    }
    out
}

pub fn draw_scene(term: &mut Term, scene: &SceneState, pack: &Pack, now: Instant) -> Result<()> {
    let agents: Vec<_> = scene.agents.values().cloned().collect();
    term.draw(|f| {
        let size = f.area();

        // Top status bar.
        let title = Paragraph::new(Line::from(vec![
            Span::raw(" ascii-agents — "),
            Span::raw(format!(
                "{} session{} ",
                agents.len(),
                if agents.len() == 1 { "" } else { "s" }
            )),
        ]))
        .block(Block::default().borders(Borders::BOTTOM));
        f.render_widget(
            title,
            Rect {
                x: size.x,
                y: size.y,
                width: size.width,
                height: 2,
            },
        );

        // Footer.
        let footer = Paragraph::new(Span::raw(" [q] quit "))
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP));
        let footer_rect = Rect {
            x: size.x,
            y: size.y + size.height - 2,
            width: size.width,
            height: 2,
        };
        f.render_widget(footer, footer_rect);

        // Scene area between title (2 rows) and footer (2 rows).
        let scene_rect = Rect {
            x: size.x,
            y: size.y + 2,
            width: size.width,
            height: size.height.saturating_sub(4),
        };

        if scene_rect.width < 16 || scene_rect.height < 10 {
            let warn = Paragraph::new("terminal too small — resize to at least 24x14");
            f.render_widget(warn, scene_rect);
            return;
        }

        // Composite the scene into a pixel buffer at 2x vertical resolution.
        let cell_w = scene_rect.width;
        let cell_h = scene_rect.height;
        let buf_w = cell_w;
        let buf_h = cell_h * 2;
        let mut buf = RgbBuffer::filled(buf_w, buf_h, BG);

        // Desk row near the bottom (3 pixels tall = top + body + body).
        let desk_y = buf_h.saturating_sub(6);
        for x in 0..buf_w {
            buf.put(x, desk_y, DESK_TOP);
            buf.put(x, desk_y + 1, DESK_BOT);
            buf.put(x, desk_y + 2, DESK_BOT);
        }

        // Each desk slot is 14 pixels wide.
        let slot_w: u16 = 14;
        for slot in &agents {
            let slot_x = (slot.desk_index as u16) * slot_w + 2;
            if slot_x + 12 > buf_w {
                continue;
            }
            let shirt = agent_shirt(slot.agent_id.raw());
            let hair = agent_hair(slot.agent_id.raw());

            let anim_name = match &slot.state {
                ActivityState::Idle => "idle",
                ActivityState::Active { .. } => "typing",
                ActivityState::Waiting { .. } => "waiting",
            };
            let anim = match pack
                .animation(anim_name)
                .or_else(|| pack.animation("idle"))
            {
                Some(a) => a,
                None => continue,
            };
            let idx = frame_index_at(
                slot.state_started_at,
                now,
                anim.frame_ms,
                anim.frames.len(),
            );
            let frame = &anim.frames[idx];
            let frame_rc = recolor_frame(frame, &pack.palette, shirt, hair);
            // Sprite is 16 tall; sit on the desk by aligning bottom of sprite to desk_y.
            let dst_y = desk_y.saturating_sub(16);
            blit_frame(&frame_rc, slot_x, dst_y, &mut buf);
        }

        // Convert buf → half-block cells → ratatui spans.
        let cells = half_block_cells(&buf);
        let mut lines: Vec<Line> = Vec::with_capacity(cells.len());
        for row in cells {
            let mut spans: Vec<Span> = Vec::with_capacity(row.len());
            for HalfCell { fg, bg } in row {
                spans.push(Span::styled(
                    "▀",
                    Style::default()
                        .fg(Color::Rgb(fg.0, fg.1, fg.2))
                        .bg(Color::Rgb(bg.0, bg.1, bg.2)),
                ));
            }
            lines.push(Line::from(spans));
        }
        let scene_para = Paragraph::new(lines);
        f.render_widget(scene_para, scene_rect);

        // Labels + speech bubbles overlaid above scene.
        for slot in &agents {
            let slot_x = scene_rect.x + (slot.desk_index as u16) * slot_w + 2;
            if slot_x + 12 > scene_rect.x + scene_rect.width {
                continue;
            }
            let label_y = scene_rect.y + scene_rect.height.saturating_sub(1);
            let style = Style::default().fg(Color::White);
            let label = Paragraph::new(Line::from(vec![Span::styled(
                format!("{} {}", slot.label, summarize_state(&slot.state)),
                style,
            )]));
            f.render_widget(
                label,
                Rect {
                    x: slot_x,
                    y: label_y,
                    width: 14,
                    height: 1,
                },
            );

            if let ActivityState::Waiting { .. } = slot.state {
                let bubble_y = scene_rect
                    .y
                    .saturating_add(scene_rect.height.saturating_sub(12));
                let bubble = Paragraph::new(vec![
                    Line::from(Span::styled(
                        "┌─?─┐",
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(Span::styled(
                        "└─v─┘",
                        Style::default().fg(Color::Yellow),
                    )),
                ]);
                f.render_widget(
                    bubble,
                    Rect {
                        x: slot_x + 6,
                        y: bubble_y,
                        width: 6,
                        height: 2,
                    },
                );
            }
        }

        let _ = Pixel::None; // silence unused-import warning on some builds
    })?;
    Ok(())
}

fn summarize_state(s: &ActivityState) -> &'static str {
    match s {
        ActivityState::Idle => "idle",
        ActivityState::Active { .. } => "typing",
        ActivityState::Waiting { .. } => "wait?",
    }
}
