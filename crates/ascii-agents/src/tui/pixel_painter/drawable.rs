//! Y-sorted drawable enum (painter's algorithm).
//!
//! Top-down depth: every mid-ground entity carries an `anchor_y` = the
//! y-pixel row where it touches the floor (front-facing bottom edge for
//! items with thickness). Drawables sort ascending by `anchor_y` and
//! paint in order. Larger `anchor_y` = closer to camera = paints last
//! (on top). Solves the classic "character standing south of a desk
//! should appear in front of it" problem without per-pair special cases.
//!
//! What stays OUTSIDE the sort:
//!   - Background (floor / walls / lighting / corridor / room walls /
//!     entry mat / clock / shadows). All depth-independent.
//!   - Per-character attached effects (chair-behind, sleep_z,
//!     waiting_bubble, walking dust, coffee steam, screen glow)
//!     paint AS PART of their parent `Drawable` — they ride along with
//!     the entity in z-order, not as a global foreground pass.

use std::time::SystemTime;

use ascii_agents_core::sprite::blit::blit_frame;
use ascii_agents_core::sprite::format::Pack;
use ascii_agents_core::sprite::{Rgb, RgbBuffer};
use ascii_agents_core::AgentSlot;

use super::effects::{
    paint_coffee_steam, paint_screen_glow, paint_sleep_z, paint_waiting_bubble, paint_walking_dust,
};
use super::{
    paint_area_rug, paint_character_at, paint_coffee_table, paint_pantry_chair, paint_pantry_table,
    paint_side_table,
};
use crate::tui::frame_cache::FrameCache;
use crate::tui::layout::{Layout, Point, DESK_H, DESK_W};

pub(super) struct Drawable<'a> {
    pub(super) anchor_y: u16,
    pub(super) kind: DrawableKind<'a>,
}

pub(super) enum DrawableKind<'a> {
    /// Whole cubicle as one z-unit: divider + filing cabinet (every
    /// other desk) + desk sprite + trash bin + screen-glow if the
    /// occupant is Active. Bundled so the cubicle paints atomically at
    /// the desk's bottom-edge row.
    DeskCubicle {
        desk: Point,
        is_last_col: bool,
        has_cabinet: bool,
        screen_glow: Option<Rgb>,
        session_age_secs: u64,
    },
    Character {
        agent: &'a AgentSlot,
        anim_name: &'static str,
        frame_idx: usize,
        anchor: Point,
        flip_x: bool,
        /// Tool-derived monitor glow color. `Some(color)` tints the
        /// skin toward that color so scanning a row of typing agents
        /// shows tool type at a glance. `None` for non-desk poses.
        glow_tint: Option<Rgb>,
        sleep_z_seed: Option<u64>,
        waiting_bubble: bool,
        walking_dust_frame: Option<usize>,
    },
    /// Lounge couch (mirror_vertical'd — back at bottom, seat at top).
    WaypointCouch {
        pos: Point,
    },
    /// Pantry counter (with coffee steam attached so steam rides above
    /// the counter in z-order). `use_large` picks the detailed 32×10
    /// kitchen sprite vs. the 20×8 compact fallback — derived from
    /// `layout.pantry_counter_size` at queue time.
    WaypointPantry {
        pos: Point,
        use_large: bool,
    },
    MeetingSofa {
        pos: Point,
        mirrored: bool,
    },
    MeetingTable {
        pos: Point,
    },
    /// Area rug — warm patterned rectangle that anchors a seating
    /// arrangement visually. Used for the meeting room (large) and
    /// the lounge (smaller). Painted BEFORE the furniture in z-order
    /// (anchor_y at top of rug) so chairs / couches sit on top.
    AreaRug {
        pos: Point,
        width: u16,
        height: u16,
    },
    /// Lounge side table (5×3 wood + magazine) next to the viewing
    /// couch. Centred at `pos`.
    LoungeSideTable {
        pos: Point,
    },
    PantryTable {
        pos: Point,
    },
    PantryChair {
        pos: Point,
    },
    Plant {
        kind: crate::tui::layout::PlantKind,
        pos: Point,
    },
    /// Aisle decor item between desk pods (plant / whiteboard / TV /
    /// phone booth / standing desk). All are obstacles in the
    /// walkable mask; phone booth + standing desk additionally exist
    /// as waypoints so agents can wander to them.
    PodDecorItem {
        kind: crate::tui::layout::PodDecor,
        pos: Point,
    },
    FloorLamp {
        pos: Point,
    },
    Door {
        pos: Point,
        /// Frame index into the `door` animation. 0 = closed,
        /// 1 = half-open, 2 = fully open. Computed stateless from
        /// agents' entry/exit windows in the orchestrator so the door
        /// transitions smoothly closed → half → open at the start of a
        /// transit and back open → half → closed at the end.
        frame_idx: usize,
    },
    WallDecor {
        kind: crate::tui::layout::WallDecor,
        pos: Point,
    },
    Cat {
        pos: Point,
        flip: bool,
        anim_name: &'static str,
        frame_idx: usize,
    },
}

/// Cat state machine. 45s cycle with desk visits:
///   0-30%: walk corridor left→right
///  30-40%: walk from corridor to target desk
///  40-70%: sit/sleep at target desk
///  70-80%: walk from desk back to corridor
///  80-100%: walk corridor right→left
///
/// Target desk is deterministic per cycle (hash of cycle number).
/// If no desks exist, falls back to corridor-only walk.
/// Returns (position, flip, animation_name, frame_idx).
pub(super) fn cat_position(
    layout: &Layout,
    pack: &Pack,
    now: SystemTime,
    idle_desk_indices: &[usize],
    all_idle: bool,
) -> Option<(Point, bool, &'static str, usize)> {
    pack.animation("cat_walk")?;
    let corridor = layout.corridor?;

    let elapsed_ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    const CYCLE_MS: u64 = 45_000;
    let cycle_n = elapsed_ms / CYCLE_MS;
    let phase = elapsed_ms % CYCLE_MS;
    let frac = phase as f32 / CYCLE_MS as f32;

    let left_x = corridor.x + corridor.width * 8 / 100;
    let right_x = corridor.x + corridor.width * 92 / 100;
    let corridor_y = corridor.y + corridor.height / 2;

    let has_target = !layout.home_desks.is_empty();
    let target_desk_idx = if has_target {
        let hash = cycle_n.wrapping_mul(0x9e37_79b9) as usize;
        hash % layout.home_desks.len()
    } else {
        0
    };

    let desk_target = if has_target {
        let desk = layout.home_desks[target_desk_idx];
        Point {
            x: desk.x + DESK_W + 1,
            y: desk.y + DESK_H + 2,
        }
    } else {
        Point {
            x: (left_x + right_x) / 2,
            y: corridor_y,
        }
    };

    let corridor_mid = Point {
        x: desk_target.x.clamp(left_x, right_x),
        y: corridor_y,
    };

    let is_at_idle_desk = idle_desk_indices.contains(&target_desk_idx);

    // Phase 1: walk corridor left → corridor_mid (directly below target desk)
    if !has_target || frac < 0.25 {
        let t = if has_target { frac / 0.25 } else { frac / 0.5 };
        let t = t.min(1.0);
        let cx = left_x + ((corridor_mid.x - left_x) as f32 * t) as u16;
        let frame_idx = (elapsed_ms / 220) as usize % 2;
        return Some((
            Point {
                x: cx,
                y: corridor_y,
            },
            false,
            "cat_walk",
            frame_idx,
        ));
    }

    // Phase 2: walk from corridor_mid up to desk
    if frac < 0.35 {
        let t = (frac - 0.25) / 0.10;
        let x = corridor_mid.x as f32 + (desk_target.x as f32 - corridor_mid.x as f32) * t;
        let y = corridor_y as f32 + (desk_target.y as f32 - corridor_y as f32) * t;
        let frame_idx = (elapsed_ms / 220) as usize % 2;
        return Some((
            Point {
                x: x as u16,
                y: y as u16,
            },
            false,
            "cat_walk",
            frame_idx,
        ));
    }

    // Phase 3: sit/sleep at desk
    if frac < 0.65 {
        let anim = if all_idle || is_at_idle_desk {
            "cat_sleep"
        } else {
            "cat_sit"
        };
        return Some((desk_target, false, anim, 0));
    }

    // Phase 4: walk from desk back to corridor_mid
    if frac < 0.75 {
        let t = (frac - 0.65) / 0.10;
        let x = desk_target.x as f32 + (corridor_mid.x as f32 - desk_target.x as f32) * t;
        let y = desk_target.y as f32 + (corridor_y as f32 - desk_target.y as f32) * t;
        let frame_idx = (elapsed_ms / 220) as usize % 2;
        return Some((
            Point {
                x: x as u16,
                y: y as u16,
            },
            true,
            "cat_walk",
            frame_idx,
        ));
    }

    // Phase 5: walk corridor_mid → left (return)
    let t = (frac - 0.75) / 0.25;
    let cx = corridor_mid.x - ((corridor_mid.x - left_x) as f32 * t) as u16;
    let frame_idx = (elapsed_ms / 220) as usize % 2;
    Some((
        Point {
            x: cx,
            y: corridor_y,
        },
        true,
        "cat_walk",
        frame_idx,
    ))
}

/// Dispatch one Drawable's paint. Effects attached to characters paint
/// inline so they ride along with the character in z-order.
pub(super) fn paint_drawable(
    d: &Drawable<'_>,
    buf: &mut RgbBuffer,
    pack: &Pack,
    cache: &mut FrameCache,
    now: SystemTime,
) {
    match &d.kind {
        DrawableKind::DeskCubicle {
            desk,
            is_last_col,
            has_cabinet,
            screen_glow,
            session_age_secs,
        } => {
            const DIVIDER: Rgb = Rgb(72, 82, 104);
            if !is_last_col {
                let div_x = desk.x + DESK_W + 3;
                for dy in 0..(DESK_H + 1) {
                    let py = desk.y.saturating_sub(1) + dy;
                    if div_x < buf.width && py < buf.height {
                        buf.put(div_x, py, DIVIDER);
                    }
                }
            }
            if *has_cabinet {
                if let Some(cab) = pack
                    .animation("filing_cabinet")
                    .and_then(|a| a.frames.first())
                {
                    let cab_x = desk.x.saturating_sub(cab.width + 1);
                    let cab_y = desk.y;
                    if cab_y + cab.height <= buf.height {
                        blit_frame(cab, cab_x, cab_y, buf);
                    }
                }
            }
            if let Some(frame) = pack.animation("desk").and_then(|a| a.frames.first()) {
                blit_frame(frame, desk.x, desk.y, buf);
            }
            if let Some(bin) = pack.animation("trash_bin").and_then(|a| a.frames.first()) {
                let bin_x = desk.x + DESK_W;
                let bin_y = desk.y + 4;
                if bin_x + bin.width <= buf.width && bin_y + bin.height <= buf.height {
                    blit_frame(bin, bin_x, bin_y, buf);
                }
            }
            paint_desk_personalization(buf, *desk, *session_age_secs);
            if let Some(tint) = screen_glow {
                paint_screen_glow(buf, desk.x, desk.y, now, *tint);
            }
        }
        DrawableKind::Character {
            agent,
            anim_name,
            frame_idx,
            anchor,
            flip_x,
            glow_tint,
            sleep_z_seed,
            waiting_bubble,
            walking_dust_frame,
        } => {
            if let Some(dust_frame) = walking_dust_frame {
                paint_walking_dust(buf, *anchor, *dust_frame);
            }
            paint_character_at(
                buf, anim_name, *frame_idx, *anchor, agent, pack, *flip_x, *glow_tint, cache,
            );
            if let Some(seed) = sleep_z_seed {
                paint_sleep_z(buf, *anchor, now, *seed);
            }
            if *waiting_bubble {
                paint_waiting_bubble(buf, *anchor);
            }
        }
        DrawableKind::WaypointCouch { pos } => {
            // Lounge couch reuses the meeting_sofa sprite (16×7) so
            // both seating areas have the same readable 3-cushion
            // silhouette. Flipped vertically so the back faces NORTH
            // (toward the windows the viewer is looking at).
            if let Some(f) = pack
                .animation("meeting_sofa")
                .and_then(|a| a.frames.first())
            {
                let cx = pos.x.saturating_sub(f.width / 2);
                let cy = pos.y.saturating_sub(f.height / 2);
                let flipped = f.mirror_vertical();
                blit_frame(&flipped, cx, cy, buf);
            }
        }
        DrawableKind::WaypointPantry { pos, use_large } => {
            // Pick the big detailed kitchen sprite when the pantry is
            // large enough; fall back to the compact 20×8 layout on
            // narrow terminals.
            let anim_name = if *use_large { "pantry" } else { "pantry_small" };
            if let Some(f) = pack.animation(anim_name).and_then(|a| a.frames.first()) {
                let cx = pos.x.saturating_sub(f.width / 2);
                let cy = pos.y.saturating_sub(f.height / 2);
                blit_frame(f, cx, cy, buf);
            }
            // Large sprite: coffee machine at sprite cols 11-18 of
            // a 32-wide sprite → world x ≈ pos.x - 2.
            // Small sprite: coffee at sprite cols 9-11 of a 20-wide
            // sprite → world x = pos.x + 1.
            let steam_dx: i16 = if *use_large { -2 } else { 1 };
            let steam_x = (pos.x as i32 + steam_dx as i32).max(0) as u16;
            paint_coffee_steam(
                buf,
                Point {
                    x: steam_x,
                    y: pos.y.saturating_sub(2),
                },
                now,
            );
        }
        DrawableKind::MeetingSofa { pos, mirrored } => {
            if let Some(f) = pack
                .animation("meeting_sofa")
                .and_then(|a| a.frames.first())
            {
                let sx = pos.x.saturating_sub(f.width / 2);
                let sy = pos.y.saturating_sub(f.height / 2);
                if *mirrored {
                    let flipped = f.mirror_vertical();
                    blit_frame(&flipped, sx, sy, buf);
                } else {
                    blit_frame(f, sx, sy, buf);
                }
            }
        }
        DrawableKind::MeetingTable { pos } => {
            paint_coffee_table(buf, pos.x, pos.y, 11, 5);
        }
        DrawableKind::AreaRug { pos, width, height } => {
            paint_area_rug(buf, pos.x, pos.y, *width, *height);
        }
        DrawableKind::LoungeSideTable { pos } => {
            paint_side_table(buf, pos.x, pos.y);
        }
        DrawableKind::PantryTable { pos } => {
            paint_pantry_table(buf, pos.x, pos.y);
        }
        DrawableKind::PantryChair { pos } => {
            paint_pantry_chair(buf, pos.x, pos.y);
        }
        DrawableKind::Plant { kind, pos } => {
            use crate::tui::layout::PlantKind;
            let anim_name = match kind {
                PlantKind::Ficus => "plant",
                PlantKind::Tall => "plant_tall",
                PlantKind::Flower => "plant_flower",
                PlantKind::Succulent => "plant_succulent",
            };
            if let Some(f) = pack.animation(anim_name).and_then(|a| a.frames.first()) {
                let px = pos.x.saturating_sub(f.width / 2);
                let py = pos.y.saturating_sub(f.height / 2);
                blit_frame(f, px, py, buf);
            }
        }
        DrawableKind::PodDecorItem { kind, pos } => {
            use crate::tui::layout::PodDecor;
            let anim_name = match kind {
                PodDecor::PlantTall => "plant_tall",
                PodDecor::Whiteboard => "whiteboard",
                PodDecor::Tv => "tv_stand",
                PodDecor::PhoneBooth => "phone_booth",
                PodDecor::StandingDesk => "standing_desk",
            };
            if let Some(f) = pack.animation(anim_name).and_then(|a| a.frames.first()) {
                let px = pos.x.saturating_sub(f.width / 2);
                let py = pos.y.saturating_sub(f.height / 2);
                blit_frame(f, px, py, buf);
            }
        }
        DrawableKind::FloorLamp { pos } => {
            if let Some(f) = pack.animation("floor_lamp").and_then(|a| a.frames.first()) {
                let px = pos.x.saturating_sub(f.width / 2);
                let py = pos.y.saturating_sub(f.height / 2);
                blit_frame(f, px, py, buf);
            }
        }
        DrawableKind::Door { pos, frame_idx } => {
            if let Some(f) = pack
                .animation("door")
                .and_then(|a| a.frames.get(*frame_idx).or_else(|| a.frames.first()))
            {
                blit_frame(f, pos.x, pos.y, buf);
            }
        }
        DrawableKind::WallDecor { kind, pos } => {
            use crate::tui::layout::WallDecor;
            let anim_name = match kind {
                WallDecor::Bookshelf => "bookshelf",
                WallDecor::BulletinBoard => "bulletin_board",
                WallDecor::ExitSign => "exit_sign",
                WallDecor::Whiteboard => "whiteboard",
                WallDecor::MeetingScreen => "meeting_screen",
            };
            if let Some(f) = pack.animation(anim_name).and_then(|a| a.frames.first()) {
                blit_frame(f, pos.x, pos.y, buf);
            }
        }
        DrawableKind::Cat {
            pos,
            flip,
            anim_name,
            frame_idx,
        } => {
            let Some(anim) = pack.animation(anim_name) else {
                return;
            };
            let Some(frame) = anim.frames.get(*frame_idx).or(anim.frames.first()) else {
                return;
            };
            let final_frame = if *flip {
                frame.mirror_horizontal()
            } else {
                frame.clone()
            };
            let px = pos.x.saturating_sub(final_frame.width / 2);
            let py = pos.y.saturating_sub(final_frame.height / 2);
            blit_frame(&final_frame, px, py, buf);
            if *anim_name == "cat_sleep" {
                paint_sleep_z(buf, *pos, now, 0xCAFE);
            }
        }
    }
}

fn paint_desk_personalization(buf: &mut RgbBuffer, desk: Point, age_secs: u64) {
    if age_secs == 0 {
        return;
    }
    let put = |buf: &mut RgbBuffer, x: u16, y: u16, c: Rgb| {
        if x < buf.width && y < buf.height {
            buf.put(x, y, c);
        }
    };
    // Coffee cup after 10 minutes
    if age_secs >= 600 {
        let cx = desk.x + 2;
        let cy = desk.y + 2;
        put(buf, cx, cy, Rgb(200, 190, 170));
        put(buf, cx + 1, cy, Rgb(200, 190, 170));
        put(buf, cx, cy + 1, Rgb(180, 160, 130));
        put(buf, cx + 1, cy + 1, Rgb(180, 160, 130));
    }
    // Small desk plant after 30 minutes
    if age_secs >= 1800 {
        let px = desk.x + DESK_W - 2;
        let py = desk.y + 1;
        put(buf, px, py, Rgb(80, 160, 80));
        put(buf, px + 1, py, Rgb(60, 140, 60));
        put(buf, px, py + 1, Rgb(100, 180, 100));
        put(buf, px + 1, py + 1, Rgb(80, 160, 80));
        put(buf, px, py + 2, Rgb(140, 100, 70));
        put(buf, px + 1, py + 2, Rgb(140, 100, 70));
    }
    // Photo frame after 1 hour
    if age_secs >= 3600 {
        let fx = desk.x + 1;
        let fy = desk.y;
        put(buf, fx, fy, Rgb(120, 100, 80));
        put(buf, fx + 1, fy, Rgb(120, 100, 80));
        put(buf, fx, fy + 1, Rgb(160, 200, 230));
        put(buf, fx + 1, fy + 1, Rgb(160, 200, 230));
    }
}
