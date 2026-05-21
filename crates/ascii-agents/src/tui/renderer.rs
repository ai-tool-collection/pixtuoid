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
use ratatui::backend::{Backend, CrosstermBackend};
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
const WALL: Rgb = Rgb(40, 44, 60);
const WALL_TRIM: Rgb = Rgb(64, 60, 50);
const WINDOW_FRAME: Rgb = Rgb(24, 24, 32);
const WINDOW_LIGHT: Rgb = Rgb(120, 160, 200);
const WINDOW_LIGHT_2: Rgb = Rgb(160, 190, 220);
const FLOOR_A: Rgb = Rgb(96, 70, 44);
const FLOOR_B: Rgb = Rgb(78, 56, 34);

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

const SCREEN_IDLE: Rgb = Rgb(70, 110, 140);
const SCREEN_TYPING: Rgb = Rgb(80, 220, 110);
const SCREEN_WAITING: Rgb = Rgb(240, 200, 60);

/// Blit the monitor sprite with its screen recolored to reflect agent state.
fn blit_monitor_state(
    pack: &Pack,
    state: &ActivityState,
    dx: u16,
    dy: u16,
    buf: &mut RgbBuffer,
) {
    let Some(anim) = pack.animation("monitor") else { return; };
    let Some(frame) = anim.frames.first() else { return; };
    let base_c = base_rgb_for(&pack.palette, 'c');
    let target = match state {
        ActivityState::Idle => SCREEN_IDLE,
        ActivityState::Active { .. } => SCREEN_TYPING,
        ActivityState::Waiting { .. } => SCREEN_WAITING,
    };
    let mut out = frame.clone();
    for px in out.pixels.iter_mut() {
        if let Some(rgb) = *px {
            if Some(rgb) == base_c {
                *px = Some(target);
            }
        }
    }
    blit_frame(&out, dx, dy, buf);
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

pub fn draw_scene<B: Backend>(
    term: &mut Terminal<B>,
    scene: &SceneState,
    pack: &Pack,
    now: Instant,
) -> Result<()> {
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

        // --- Background: top wall band + checkered floor below ---
        let wall_h: u16 = 4; // top wall band
        for y in 0..wall_h.min(buf_h) {
            for x in 0..buf_w {
                buf.put(x, y, WALL);
            }
        }
        // Wall/floor trim line.
        if buf_h > wall_h {
            for x in 0..buf_w {
                buf.put(x, wall_h, WALL_TRIM);
            }
        }
        // Floor tiles (checkered) below the trim.
        let floor_start = wall_h + 1;
        for y in floor_start..buf_h {
            for x in 0..buf_w {
                // 4x2 px checker — readable as floor tiles from a distance.
                let cell = ((x / 4) + ((y - floor_start) / 2)) % 2;
                let c = if cell == 0 { FLOOR_A } else { FLOOR_B };
                buf.put(x, y, c);
            }
        }

        // --- Windows in the top wall band ---
        let window_w: u16 = 6;
        let window_h: u16 = 2;
        let window_y: u16 = 1;
        let stride: u16 = 16;
        let mut wx: u16 = 4;
        while wx + window_w < buf_w {
            for y in window_y..window_y + window_h {
                for x in wx..wx + window_w {
                    if y < buf_h && x < buf_w {
                        let inner = x > wx && x < wx + window_w - 1;
                        buf.put(x, y, if inner { WINDOW_LIGHT } else { WINDOW_FRAME });
                    }
                }
            }
            // Small horizontal mullion in the middle row.
            let mid = window_y + window_h / 2;
            for x in wx..wx + window_w {
                if x < buf_w && mid < buf_h {
                    buf.put(x, mid, WINDOW_LIGHT_2);
                }
            }
            wx += stride;
        }

        // --- Furniture + characters per desk slot ---
        // Top-down layout: chair at top, character below it, desk in front of
        // character (occluding lower body), monitor on desk between character
        // and viewer. Slots arranged horizontally with comfortable padding.
        let slot_w: u16 = 18;
        let slot_left_padding: u16 = 4;
        // Center the character vertically in the floor area.
        let floor_h = buf_h.saturating_sub(floor_start);
        let stack_h: u16 = 4 /*chair*/ + 12 /*character*/ + 6 /*desk*/;
        let stack_top = floor_start + (floor_h.saturating_sub(stack_h)) / 2;

        // Helper to safely blit a pack animation's first frame.
        let blit_static = |buf: &mut RgbBuffer, name: &str, dx: u16, dy: u16| {
            if let Some(anim) = pack.animation(name) {
                if let Some(frame) = anim.frames.first() {
                    blit_frame(frame, dx, dy, buf);
                }
            }
        };

        let max_slots = (buf_w.saturating_sub(slot_left_padding)) / slot_w;
        for slot in &agents {
            let i = slot.desk_index as u16;
            if i >= max_slots {
                continue;
            }
            let slot_x = slot_left_padding + i * slot_w;
            let shirt = agent_shirt(slot.agent_id.raw());
            let hair = agent_hair(slot.agent_id.raw());

            // 1. Chair (8 wide), centered behind character.
            blit_static(&mut buf, "chair", slot_x + 4, stack_top);

            // 2. Character animation (10 wide, 12 tall).
            let anim_name = match &slot.state {
                ActivityState::Idle => "idle",
                ActivityState::Active { .. } => "typing",
                ActivityState::Waiting { .. } => "waiting",
            };
            if let Some(anim) = pack.animation(anim_name).or_else(|| pack.animation("idle")) {
                let idx = frame_index_at(
                    slot.state_started_at,
                    now,
                    anim.frame_ms,
                    anim.frames.len(),
                );
                let frame = &anim.frames[idx];
                let frame_rc = recolor_frame(frame, &pack.palette, shirt, hair);
                let char_y = if matches!(slot.state, ActivityState::Waiting { .. }) {
                    // Waiting sprite is 14 tall (raised arm above head) — shift up.
                    stack_top.saturating_add(1)
                } else {
                    stack_top + 3
                };
                blit_frame(&frame_rc, slot_x + 3, char_y, &mut buf);
            }

            // 3. Desk in front of character (16 wide, 6 tall, slightly oversized
            //    so it occludes the character's lower body / hands).
            let desk_y = stack_top + 4 + 12;
            blit_static(&mut buf, "desk", slot_x, desk_y);

            // 4. Monitor sitting on desk — color reflects current activity state.
            let monitor_y = desk_y + 1;
            let monitor_x = slot_x + 5;
            blit_monitor_state(&pack, &slot.state, monitor_x, monitor_y, &mut buf);
        }

        // --- Decorative plant in each empty slot ---
        for i in 0..max_slots {
            let occupied = agents.iter().any(|a| a.desk_index as u16 == i);
            if occupied {
                continue;
            }
            let slot_x = slot_left_padding + i * slot_w;
            // Plant sits on a desk surface too — at the same desk row.
            blit_static(&mut buf, "desk", slot_x, stack_top + 4 + 12);
            blit_static(&mut buf, "plant", slot_x + 5, stack_top + 4 + 8);
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

        // Labels under each desk + speech bubble overlay for waiting state.
        for slot in &agents {
            let i = slot.desk_index as u16;
            if i >= max_slots {
                continue;
            }
            let slot_x = scene_rect.x + slot_left_padding + i * slot_w;
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
                    width: slot_w,
                    height: 1,
                },
            );

            if let ActivityState::Waiting { .. } = slot.state {
                // Bubble above character's head — top of stack region.
                let bubble_y = scene_rect
                    .y
                    .saturating_add((stack_top / 2).saturating_sub(2));
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
