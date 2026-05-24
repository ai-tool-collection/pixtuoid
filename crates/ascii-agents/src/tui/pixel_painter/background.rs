//! Background pass — depth-independent floor, walls, windows, skyline,
//! clock, corridor runner, entry mat, time-of-day overlays, ceiling
//! light pools, lamp halo, and floor shadows.
//!
//! Everything here paints BEFORE the y-sorted entity pass. Helpers are
//! `pub(super)` so the orchestrator (`pixel_painter/mod.rs`) can call
//! them in the order it wants.

use std::time::SystemTime;

use ascii_agents_core::sprite::{Rgb, RgbBuffer};

use super::palette::{blend, lerp_rgb};

// --- Color constants ------------------------------------------------------
const CARPET_BASE: Rgb = Rgb(150, 110, 72);
const CARPET_LIGHT: Rgb = Rgb(178, 138, 96);
const CARPET_DARK: Rgb = Rgb(118, 82, 50);
const WALL: Rgb = Rgb(56, 56, 70);
const WALL_TRIM: Rgb = Rgb(80, 80, 100);
const BASEBOARD: Rgb = Rgb(40, 40, 52);

// --- Floor / walls / windows ---------------------------------------------

pub(super) fn paint_floor_and_walls(
    buf: &mut RgbBuffer,
    buf_w: u16,
    buf_h: u16,
    now: SystemTime,
    look: &TimeOfDayLook,
    top_wall_h: u16,
    skip_window_x_range: Option<(u16, u16)>,
) {
    const BASEBOARD_H: u16 = 3;
    const WINDOW_FRAME: Rgb = Rgb(24, 24, 32);

    // Carpet: warm tan/grey base with deterministic light/dark flecks.
    // No seams, no grid — softer than the previous plank pattern.
    for y in 0..buf_h {
        for x in 0..buf_w {
            let hash = (x as u32)
                .wrapping_mul(73)
                .wrapping_add((y as u32).wrapping_mul(151))
                ^ ((x as u32).wrapping_mul(11) ^ (y as u32).wrapping_mul(37));
            let color = match hash % 17 {
                0 | 1 => CARPET_LIGHT,
                2 | 3 => CARPET_DARK,
                _ => CARPET_BASE,
            };
            buf.put(x, y, color);
        }
    }
    for y in 0..top_wall_h.min(buf_h) {
        for x in 0..buf_w {
            buf.put(x, y, WALL);
        }
    }

    // Floor-to-ceiling windows: 落地窗 — height grows with the wall band so
    // taller terminals get dramatic floor-to-ceiling glass. Width stays
    // fixed (mullion every 22 px) so the skyline detail reads consistently.
    const WINDOW_W: u16 = 22;
    const WINDOW_GAP: u16 = 3;
    let window_y: u16 = 1;
    let window_h: u16 = top_wall_h.saturating_sub(3).max(8);
    let mut x = 3u16;
    let mut idx: u32 = 0;
    while x + WINDOW_W + 2 <= buf_w {
        // Skip any window whose x-range overlaps the elevator door —
        // the elevator sits in the wall and would otherwise show the
        // window's glass + skyline behind its frame.
        let overlaps_door =
            skip_window_x_range.is_some_and(|(dx0, dx1)| x < dx1 && x + WINDOW_W > dx0);
        if !overlaps_door {
            paint_floor_to_ceiling_window(
                buf,
                x,
                window_y,
                WINDOW_W,
                window_h,
                WINDOW_FRAME,
                look,
                idx as u16,
                now,
            );
            if look.spill_strength > 0.0 {
                paint_window_light_spill(
                    buf,
                    x,
                    WINDOW_W,
                    top_wall_h,
                    look.spill_strength,
                    look.spill_slant,
                );
            }
        }
        x += WINDOW_W + WINDOW_GAP;
        idx += 1;
    }

    // Wall trim line at the bottom of the wall band.
    let trim_y = top_wall_h.saturating_sub(1);
    if trim_y < buf_h {
        for x in 0..buf_w {
            buf.put(x, trim_y, WALL_TRIM);
        }
    }

    let base_y = buf_h.saturating_sub(BASEBOARD_H);
    for y in base_y..buf_h {
        for x in 0..buf_w {
            buf.put(x, y, BASEBOARD);
        }
    }
}

/// Static "is this building window lit?" decision — independent of time.
/// Deterministic hash of (window_idx, dx, dy) so each building's window
/// pattern is stable across frames; only `city_dot_twinkle` animates
/// on top. ~75% of grid slots are lit so the city reads as "alive at
/// night" without every single window being on.
fn city_dot_lit(window_idx: u16, dx: u16, dy: u16) -> bool {
    let mut h = (window_idx as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    h ^= (dx as u64).wrapping_mul(0xc6a4_a793_5bd1_e995);
    h ^= (dy as u64).wrapping_mul(0x1656_67b1_9e37_79b9);
    h ^= h >> 17;
    (h % 100) < 75
}

/// Per-dot twinkle: each city-window dot has its own ~600-1400ms cycle and
/// each cycle rerolls on/off via a deterministic hash. Bias toward "on" so
/// the skyline is mostly lit with the occasional dot blinking off.
fn city_dot_twinkle(window_idx: u16, dx: u16, dy: u16, now: SystemTime) -> bool {
    let now_ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let dot_seed = (window_idx as u64).wrapping_mul(31)
        ^ (dx as u64).wrapping_mul(131)
        ^ (dy as u64).wrapping_mul(521);
    // Per-dot cycle 3-7 s — slow enough that the eye doesn't perceive
    // constant flickering, fast enough that the skyline still "lives".
    let cycle_ms = 3000 + (dot_seed % 4000);
    let phase = now_ms / cycle_ms;
    let hash = dot_seed
        .wrapping_add(phase)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (hash % 10) < 8
}

/// Warm sunlight tint spilling onto the floor below a window. Trapezoid
/// shape (widens by 1 px every 2 rows) blended with the existing floor so
/// it reads as "light through window" not "yellow rectangle". `intensity`
/// (0..1) scales with daylight — zero at night so no spill paints.
/// `slant_per_row` shifts the spill horizontally per row going down —
/// positive = rightward (morning sun in the east casts light right), negative
/// = leftward (evening sun in the west casts light left).
fn paint_window_light_spill(
    buf: &mut RgbBuffer,
    window_x: u16,
    window_w: u16,
    top_y: u16,
    intensity: f32,
    slant_per_row: f32,
) {
    const WARM: Rgb = Rgb(255, 230, 160);
    const DEPTH: u16 = 12;
    let fade_start = 0.32 * intensity;
    for dy in 0..DEPTH {
        let widen = (dy / 2).min(3);
        let shift = (slant_per_row * dy as f32).round() as i32;
        let base_x = (window_x as i32 + shift).max(0) as u16;
        let start_x = base_x.saturating_sub(widen);
        let end_x = (base_x + window_w + widen).min(buf.width);
        let y = top_y + dy;
        if y >= buf.height {
            break;
        }
        let strength = fade_start * (1.0 - dy as f32 / DEPTH as f32);
        for x in start_x..end_x {
            let cur = buf.get(x, y);
            buf.put(
                x,
                y,
                Rgb(
                    blend(cur.0, WARM.0, strength),
                    blend(cur.1, WARM.1, strength),
                    blend(cur.2, WARM.2, strength),
                ),
            );
        }
    }
}

/// Window glass color + spill intensity + spill slant for the current local
/// hour. `spill_slant` is x-shift per row going down: positive = rightward
/// (morning sun in the east), negative = leftward (evening sun in the west).
/// `darkness` is 1 - daylight, used to drive artificial-light effects.
pub(super) struct TimeOfDayLook {
    glass_a: Rgb,
    glass_b: Rgb,
    pub(super) spill_strength: f32,
    spill_slant: f32,
    pub(super) darkness: f32,
}

pub(super) fn time_of_day_look(now: SystemTime) -> TimeOfDayLook {
    use chrono::Timelike;
    let unix_now = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let local = chrono::DateTime::<chrono::Local>::from(std::time::UNIX_EPOCH + unix_now);
    let h = local.hour() as f32 + local.minute() as f32 / 60.0;

    // Daylight intensity: full from 8 to 17, smooth ramp 5..8 and 17..20.
    let day = if !(5.0..20.0).contains(&h) {
        0.0
    } else if h < 8.0 {
        (h - 5.0) / 3.0
    } else if h < 17.0 {
        1.0
    } else {
        1.0 - (h - 17.0) / 3.0
    };

    // Twilight bell at dawn (~6.5) and dusk (~18.5) — adds orange/pink
    // tint that the cyan↔dark-blue base doesn't capture.
    let twilight = super::palette::bell(h, 6.5, 1.5).max(super::palette::bell(h, 18.5, 1.5));

    const DAY_A: Rgb = Rgb(120, 160, 200);
    const DAY_B: Rgb = Rgb(160, 190, 220);
    const NIGHT_A: Rgb = Rgb(18, 26, 52);
    const NIGHT_B: Rgb = Rgb(28, 36, 70);
    const TWILIGHT_A: Rgb = Rgb(220, 130, 80);
    const TWILIGHT_B: Rgb = Rgb(240, 170, 110);

    let glass_a = lerp_rgb(lerp_rgb(NIGHT_A, DAY_A, day), TWILIGHT_A, twilight * 0.5);
    let glass_b = lerp_rgb(lerp_rgb(NIGHT_B, DAY_B, day), TWILIGHT_B, twilight * 0.5);

    // Spill slant: ±0.7 px per row at peak hours (6am leftmost, 6pm
    // rightmost), zero at noon. Conventional read: morning sun on the east
    // (right of image) casts light westward (leftward shift); evening sun
    // on the west casts eastward (rightward shift).
    let slant = if h < 12.0 {
        -((12.0 - h) / 6.0).clamp(0.0, 1.0) * 0.7
    } else {
        ((h - 12.0) / 6.0).clamp(0.0, 1.0) * 0.7
    };

    TimeOfDayLook {
        glass_a,
        glass_b,
        spill_strength: day,
        spill_slant: slant,
        darkness: 1.0 - day,
    }
}

/// Multiplicative dim applied to floor pixels at night. Pulls everything
/// toward a dark navy so the artificial-light pools have something to
/// stand out against. `strength` is 0..1 (no dim..full dim).
pub(super) fn dim_floor_overlay(buf: &mut RgbBuffer, top_y: u16, bottom_y: u16, strength: f32) {
    const NIGHT_TINT: Rgb = Rgb(18, 22, 38);
    let s = strength.clamp(0.0, 0.55);
    for y in top_y..bottom_y.min(buf.height) {
        for x in 0..buf.width {
            let cur = buf.get(x, y);
            buf.put(
                x,
                y,
                Rgb(
                    blend(cur.0, NIGHT_TINT.0, s),
                    blend(cur.1, NIGHT_TINT.1, s),
                    blend(cur.2, NIGHT_TINT.2, s),
                ),
            );
        }
    }
}

/// Elliptical "ceiling fluorescent" pool of pale warm light on the floor.
/// Blended additively (toward POOL color) with a quadratic falloff from
/// center to edge so it reads as a soft round patch, not a stamped oval.
pub(super) fn paint_ceiling_pool(
    buf: &mut RgbBuffer,
    cx: u16,
    cy: u16,
    half_w: u16,
    half_h: u16,
    strength: f32,
) {
    const POOL: Rgb = Rgb(255, 246, 215);
    if half_w == 0 || half_h == 0 || strength <= 0.0 {
        return;
    }
    let min_x = cx.saturating_sub(half_w);
    let max_x = (cx + half_w).min(buf.width);
    let min_y = cy.saturating_sub(half_h);
    let max_y = (cy + half_h).min(buf.height);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let nx = (x as f32 - cx as f32) / half_w as f32;
            let ny = (y as f32 - cy as f32) / half_h as f32;
            let r2 = nx * nx + ny * ny;
            if r2 > 1.0 {
                continue;
            }
            let t = (1.0 - r2) * strength;
            let cur = buf.get(x, y);
            buf.put(
                x,
                y,
                Rgb(
                    blend(cur.0, POOL.0, t),
                    blend(cur.1, POOL.1, t),
                    blend(cur.2, POOL.2, t),
                ),
            );
        }
    }
}

/// Warm radial halo around the floor lamp — only visible at night.
pub(super) fn paint_floor_lamp_halo(buf: &mut RgbBuffer, cx: u16, cy: u16, strength: f32) {
    const WARM: Rgb = Rgb(255, 210, 130);
    const RADIUS: u16 = 11;
    if strength <= 0.0 {
        return;
    }
    let min_x = cx.saturating_sub(RADIUS);
    let max_x = (cx + RADIUS).min(buf.width);
    let min_y = cy.saturating_sub(RADIUS);
    let max_y = (cy + RADIUS).min(buf.height);
    let r2max = (RADIUS as f32) * (RADIUS as f32);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let dx = x as f32 - cx as f32;
            let dy = y as f32 - cy as f32;
            let r2 = dx * dx + dy * dy;
            if r2 > r2max {
                continue;
            }
            let t = (1.0 - (r2 / r2max).sqrt()) * strength;
            let cur = buf.get(x, y);
            buf.put(
                x,
                y,
                Rgb(
                    blend(cur.0, WARM.0, t),
                    blend(cur.1, WARM.1, t),
                    blend(cur.2, WARM.2, t),
                ),
            );
        }
    }
}

/// Floor-to-ceiling window with frame, mullion, and a procedural city view
/// inside the glass. Sky gradient at top blends with time-of-day glass
/// colors; the lower portion shows building silhouettes whose "windows"
/// (1-pixel dots) light up at night and twinkle on a per-dot cycle so the
/// skyline reads as alive instead of stamped.
#[allow(clippy::too_many_arguments)]
fn paint_floor_to_ceiling_window(
    buf: &mut RgbBuffer,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    frame: Rgb,
    look: &TimeOfDayLook,
    window_idx: u16,
    now: SystemTime,
) {
    const BUILDING_DARK: Rgb = Rgb(20, 22, 32);
    const BUILDING_LIGHT: Rgb = Rgb(60, 65, 82);
    const LIT_WINDOW: Rgb = Rgb(252, 215, 110);
    const DARK_WINDOW: Rgb = Rgb(30, 32, 44);

    let lit_strength = look.darkness.clamp(0.0, 1.0);
    let lit_color = lerp_rgb(DARK_WINDOW, LIT_WINDOW, lit_strength);
    let building = lerp_rgb(BUILDING_LIGHT, BUILDING_DARK, look.darkness);

    // Skyline silhouette as a 0..15 PATTERN; the actual pixel height is
    // computed per-window so the skyline auto-scales with the glass
    // height. On a 12-px-tall window the buildings are 3..7 px, on a
    // 50-px-tall window they fill 12..24 px — same visual proportion.
    const SKYLINE_PATTERN: &[u8] = &[8, 14, 11, 15, 6, 13, 9, 12, 7, 15, 10, 13];
    const PATTERN_MAX: u16 = 15;
    let glass_h = h.saturating_sub(2);
    let min_bh = (glass_h / 5).max(3);
    let max_bh = (glass_h * 50 / 100).max(min_bh + 4);
    let bh_range = max_bh.saturating_sub(min_bh);
    let sky_norm = (glass_h as f32) * 0.7;
    let sky_row: Vec<Rgb> = (0..glass_h)
        .map(|gy| {
            let sky_t = (gy as f32 / sky_norm).min(1.0);
            lerp_rgb(look.glass_b, look.glass_a, sky_t)
        })
        .collect();

    for dy in 0..h {
        for dx in 0..w {
            let px = x + dx;
            let py = y + dy;
            if px >= buf.width || py >= buf.height {
                continue;
            }
            let on_edge = dx == 0 || dx == w - 1 || dy == 0 || dy == h - 1;
            let on_mullion = dx == w / 2 || dy == h * 7 / 10;
            if on_edge || on_mullion {
                buf.put(px, py, frame);
                continue;
            }
            let glass_dx = dx - 1;
            let glass_dy = dy - 1;
            let pat_idx = ((glass_dx + window_idx * 3) % SKYLINE_PATTERN.len() as u16) as usize;
            let pat = SKYLINE_PATTERN[pat_idx] as u16;
            let building_h = min_bh + (pat * bh_range) / PATTERN_MAX;
            let in_building = glass_dy >= glass_h.saturating_sub(building_h);

            if in_building {
                let bldg_y = glass_dy - (glass_h - building_h);
                // Lit-window dots arranged on a 2-px grid (every other
                // column + every other row of the building). Per-dot
                // lit/unlit decision is hashed from (col, row, win_idx)
                // so the same building always shows the same pattern;
                // ~70 % of grid slots are lit at night. Twinkle animates
                // the lit ones on independent cycles.
                let on_grid = glass_dx % 2 == 1 && bldg_y % 2 == 1;
                let lit_base = on_grid && city_dot_lit(window_idx, glass_dx, bldg_y);
                if lit_base && city_dot_twinkle(window_idx, glass_dx, bldg_y, now) {
                    buf.put(px, py, lit_color);
                } else {
                    buf.put(px, py, building);
                }
            } else {
                buf.put(px, py, sky_row[glass_dy as usize]);
            }
        }
    }
}

/// Neon sign panel — dark background with colored glow border, painted in
/// the wall band. The ratatui text widget renders on top with bright colors.
pub(super) fn paint_neon_panel(
    buf: &mut RgbBuffer,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    now: SystemTime,
) {
    let elapsed_ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let pulse = 0.7 + 0.3 * ((elapsed_ms as f32 / 1200.0).sin() * 0.5 + 0.5);

    const PANEL_BG: Rgb = Rgb(12, 14, 22);

    let frame_color = Rgb(
        (20.0 + 25.0 * pulse) as u8,
        (60.0 + 50.0 * pulse) as u8,
        (80.0 + 50.0 * pulse) as u8,
    );

    for dy in 0..h {
        for dx in 0..w {
            let px = x + dx;
            let py = y + dy;
            if px >= buf.width || py >= buf.height {
                continue;
            }
            let on_border = dx == 0 || dx == w - 1 || dy == 0 || dy == h - 1;
            if on_border {
                buf.put(px, py, frame_color);
            } else {
                buf.put(px, py, PANEL_BG);
            }
        }
    }
}

/// Live wall clock — reads system local time and renders hour + minute hands.
/// 5x5 clock face. Hands quantize to 8 cardinal/intercardinal directions
/// (the most a 5x5 sprite can express).
pub(super) fn paint_clock(buf: &mut RgbBuffer, x: u16, y: u16, now: SystemTime) {
    const RIM: Rgb = Rgb(200, 200, 210);
    const FACE: Rgb = Rgb(240, 240, 240);
    const HAND_HOUR: Rgb = Rgb(20, 20, 25);
    const HAND_MIN: Rgb = Rgb(60, 60, 80);

    // Face + rim background.
    let bg: &[(u16, u16, Rgb)] = &[
        (1, 0, RIM),
        (2, 0, RIM),
        (3, 0, RIM),
        (0, 1, RIM),
        (1, 1, FACE),
        (2, 1, FACE),
        (3, 1, FACE),
        (4, 1, RIM),
        (0, 2, RIM),
        (1, 2, FACE),
        (2, 2, FACE),
        (3, 2, FACE),
        (4, 2, RIM),
        (0, 3, RIM),
        (1, 3, FACE),
        (2, 3, FACE),
        (3, 3, FACE),
        (4, 3, RIM),
        (1, 4, RIM),
        (2, 4, RIM),
        (3, 4, RIM),
    ];
    for (dx, dy, c) in bg {
        let px = x + dx;
        let py = y + dy;
        if px < buf.width && py < buf.height {
            buf.put(px, py, *c);
        }
    }

    // Decompose `now` into local hour + minute via chrono.
    let unix_now = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let local = chrono::DateTime::<chrono::Local>::from(std::time::UNIX_EPOCH + unix_now);
    use chrono::Timelike;
    let hour = local.hour() % 12;
    let minute = local.minute();

    // Fractional positions around the clock (0.0 = 12 o'clock, 0.25 = 3 o'clock).
    let hour_turns = (hour as f32 + minute as f32 / 60.0) / 12.0;
    let min_turns = minute as f32 / 60.0;

    let put = |buf: &mut RgbBuffer, ox: i32, oy: i32, color: Rgb| {
        let px = x as i32 + 2 + ox;
        let py = y as i32 + 2 + oy;
        if px >= 0 && py >= 0 && (px as u16) < buf.width && (py as u16) < buf.height {
            buf.put(px as u16, py as u16, color);
        }
    };

    // Center pin (always painted).
    put(buf, 0, 0, HAND_HOUR);

    // Hour hand: 1 px from center along quantized angle.
    let (hdx, hdy) = octant_offset(hour_turns);
    put(buf, hdx, hdy, HAND_HOUR);

    // Minute hand: 2 px from center (longer than hour hand) along its angle.
    let (mdx, mdy) = octant_offset(min_turns);
    put(buf, mdx, mdy, HAND_MIN);
    // (Don't put a 2nd pixel if it falls off the 5x5 — the rim handles it.)
}

/// Quantize a fractional turn (0.0..1.0, 0.0 = north) to one of 8 octant
/// (dx, dy) unit offsets.
fn octant_offset(turn: f32) -> (i32, i32) {
    let oct = ((turn * 8.0).round() as i32).rem_euclid(8);
    match oct {
        0 => (0, -1),
        1 => (1, -1),
        2 => (1, 0),
        3 => (1, 1),
        4 => (0, 1),
        5 => (-1, 1),
        6 => (-1, 0),
        7 => (-1, -1),
        _ => (0, 0),
    }
}

/// Office corridor runner — a darker wood strip with subtle lighter stripes,
/// painted along the walkway band so the eye traces a path connecting the
/// door, meeting room, pantry, cubicles, and lounge. Just texture over the
/// floor — walls and decor paint on top.
pub(super) fn paint_corridor_runner(buf: &mut RgbBuffer, rect: crate::tui::layout::Bounds) {
    const RUNNER_BASE: Rgb = Rgb(94, 62, 36);
    const RUNNER_STRIPE: Rgb = Rgb(118, 82, 50);
    const RUNNER_EDGE: Rgb = Rgb(60, 40, 24);
    let max_x = (rect.x + rect.width).min(buf.width);
    let max_y = (rect.y + rect.height).min(buf.height);
    for y in rect.y..max_y {
        for x in rect.x..max_x {
            let is_edge = y == rect.y || y + 1 == max_y;
            let dy = y - rect.y;
            let stripe = (x.wrapping_add(dy * 3)) % 9 == 0;
            let color = if is_edge {
                RUNNER_EDGE
            } else if stripe {
                RUNNER_STRIPE
            } else {
                RUNNER_BASE
            };
            buf.put(x, y, color);
        }
    }
}

/// Elliptical drop-shadow blended toward black at the floor level.
/// Grounds floating sprites so they look like they're standing/sitting
/// on the floor instead of hovering in mid-air. `strength` 0..1 controls
/// the darken amount at the center; falls off quadratically to 0 at edge.
pub(super) fn paint_shadow(
    buf: &mut RgbBuffer,
    cx: u16,
    cy: u16,
    half_w: u16,
    half_h: u16,
    strength: f32,
) {
    const SHADOW: Rgb = Rgb(8, 8, 14);
    if half_w == 0 || half_h == 0 || strength <= 0.0 {
        return;
    }
    let min_x = cx.saturating_sub(half_w);
    let max_x = (cx + half_w).min(buf.width);
    let min_y = cy.saturating_sub(half_h);
    let max_y = (cy + half_h).min(buf.height);
    for y in min_y..max_y {
        for x in min_x..max_x {
            let nx = (x as f32 - cx as f32) / half_w as f32;
            let ny = (y as f32 - cy as f32) / half_h as f32;
            let r2 = nx * nx + ny * ny;
            if r2 > 1.0 {
                continue;
            }
            let t = (1.0 - r2) * strength;
            let cur = buf.get(x, y);
            buf.put(
                x,
                y,
                Rgb(
                    blend(cur.0, SHADOW.0, t),
                    blend(cur.1, SHADOW.1, t),
                    blend(cur.2, SHADOW.2, t),
                ),
            );
        }
    }
}
