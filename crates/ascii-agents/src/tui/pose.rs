//! State → pose derivation for the coworking-lounge renderer.
//!
//! Pure function: given an `AgentSlot`, current `SystemTime`, and `Layout`,
//! returns which `Pose` the agent should appear in this frame. Includes the
//! wander state machine for Idle agents (cycles between desk and waypoints).

use std::time::{Duration, SystemTime};

use ascii_agents_core::state::{ActivityState, AgentSlot};

use crate::tui::layout::{Layout, Point};

/// Length of one full wander cycle. After 9 seconds we loop.
pub const WANDER_CYCLE_MS: u64 = 9_000;
/// Per-phase boundaries (cumulative).
const PHASE_SEATED_END: u64 = 3_500;
const PHASE_WALK_OUT_END: u64 = 5_000;
const PHASE_AT_WAYPOINT_END: u64 = 7_500;
/// PHASE_WALK_BACK_END == WANDER_CYCLE_MS.

/// Frame-cycle period for animated poses.
pub const TYPING_FRAME_MS: u64 = 140;
pub const WALKING_FRAME_MS: u64 = 220;
pub const TYPING_FRAMES: usize = 2;
pub const WALKING_FRAMES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pose {
    SeatedIdle,
    SeatedTyping { frame: usize },
    StandingAtDesk,
    StandingAtWaypoint { wp: usize },
    Walking { from: Point, to: Point, t_x1000: u16, frame: usize },
}

/// Returns `None` if the slot's desk_index is out of range for `layout`.
pub fn derive(slot: &AgentSlot, now: SystemTime, layout: &Layout) -> Option<Pose> {
    let desk = *layout.home_desks.get(slot.desk_index)?;

    let elapsed = now
        .duration_since(slot.state_started_at)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;

    match &slot.state {
        ActivityState::Active { .. } => {
            let frame = ((elapsed / TYPING_FRAME_MS) as usize) % TYPING_FRAMES;
            Some(Pose::SeatedTyping { frame })
        }
        ActivityState::Waiting { .. } => Some(Pose::StandingAtDesk),
        ActivityState::Idle => Some(idle_pose(slot, desk, layout, elapsed)),
    }
}

fn idle_pose(slot: &AgentSlot, desk: Point, layout: &Layout, elapsed_ms: u64) -> Pose {
    let phase_t = elapsed_ms % WANDER_CYCLE_MS;
    let wp_idx = (slot.agent_id.raw() as usize) % layout.waypoints.len();
    let wp = layout.waypoints[wp_idx];

    if phase_t < PHASE_SEATED_END {
        Pose::SeatedIdle
    } else if phase_t < PHASE_WALK_OUT_END {
        let span = PHASE_WALK_OUT_END - PHASE_SEATED_END;
        let t = ((phase_t - PHASE_SEATED_END) * 1000 / span) as u16;
        let frame = ((elapsed_ms / WALKING_FRAME_MS) as usize) % WALKING_FRAMES;
        Pose::Walking { from: desk, to: wp, t_x1000: t, frame }
    } else if phase_t < PHASE_AT_WAYPOINT_END {
        Pose::StandingAtWaypoint { wp: wp_idx }
    } else {
        let span = WANDER_CYCLE_MS - PHASE_AT_WAYPOINT_END;
        let t = ((phase_t - PHASE_AT_WAYPOINT_END) * 1000 / span) as u16;
        let frame = ((elapsed_ms / WALKING_FRAME_MS) as usize) % WALKING_FRAMES;
        Pose::Walking { from: wp, to: desk, t_x1000: t, frame }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use ascii_agents_core::source::Activity;
    use ascii_agents_core::AgentId;

    fn slot(state: ActivityState, age_ms: u64) -> (AgentSlot, SystemTime) {
        let id = AgentId::from_transcript_path("/p/a.jsonl");
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let now = started + Duration::from_millis(age_ms);
        let s = AgentSlot {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "abc".into(),
            cwd: PathBuf::from("/repo"),
            label: "cc".into(),
            state,
            state_started_at: started,
            desk_index: 0,
        };
        (s, now)
    }

    fn layout() -> Layout {
        Layout::compute(120, 80, 4).expect("fits")
    }

    fn typing() -> ActivityState {
        ActivityState::Active {
            activity: Activity::Typing,
            tool_use_id: Some("t".into()),
            detail: Some("Edit".into()),
        }
    }

    #[test]
    fn active_state_is_seated_typing_with_cycling_frame() {
        let (s, now) = slot(typing(), 0);
        let l = layout();
        assert_eq!(derive(&s, now, &l), Some(Pose::SeatedTyping { frame: 0 }));
        let (s, now) = slot(typing(), TYPING_FRAME_MS);
        assert_eq!(derive(&s, now, &l), Some(Pose::SeatedTyping { frame: 1 }));
        let (s, now) = slot(typing(), TYPING_FRAME_MS * 2);
        assert_eq!(derive(&s, now, &l), Some(Pose::SeatedTyping { frame: 0 }));
    }

    #[test]
    fn waiting_state_is_standing_at_desk() {
        let (s, now) = slot(
            ActivityState::Waiting { reason: "perm".into() },
            5_000,
        );
        let l = layout();
        assert_eq!(derive(&s, now, &l), Some(Pose::StandingAtDesk));
    }

    #[test]
    fn idle_phase_0_is_seated_idle() {
        let (s, now) = slot(ActivityState::Idle, PHASE_SEATED_END - 1);
        let l = layout();
        assert_eq!(derive(&s, now, &l), Some(Pose::SeatedIdle));
    }

    #[test]
    fn idle_phase_1_is_walking_out() {
        // Halfway through walk-out (3500..5000), t=0.5 → t_x1000 ≈ 500.
        let (s, now) = slot(ActivityState::Idle, 4_250);
        let l = layout();
        match derive(&s, now, &l).expect("pose") {
            Pose::Walking { t_x1000, frame, .. } => {
                assert!((400..=600).contains(&t_x1000), "t_x1000={t_x1000}");
                assert!(frame < WALKING_FRAMES);
            }
            other => panic!("expected Walking, got {other:?}"),
        }
    }

    #[test]
    fn idle_phase_2_is_standing_at_waypoint() {
        let (s, now) = slot(ActivityState::Idle, 6_000);
        let l = layout();
        match derive(&s, now, &l).expect("pose") {
            Pose::StandingAtWaypoint { wp } => assert!(wp < l.waypoints.len()),
            other => panic!("expected StandingAtWaypoint, got {other:?}"),
        }
    }

    #[test]
    fn idle_phase_3_is_walking_back() {
        let (s, now) = slot(ActivityState::Idle, 8_250);
        let l = layout();
        match derive(&s, now, &l).expect("pose") {
            Pose::Walking { t_x1000, .. } => {
                assert!((400..=600).contains(&t_x1000));
            }
            other => panic!("expected Walking, got {other:?}"),
        }
    }

    #[test]
    fn idle_cycle_loops_after_wander_cycle_ms() {
        let (s_early, now_early) = slot(ActivityState::Idle, 1_000);
        let (s_loop, now_loop) = slot(ActivityState::Idle, 1_000 + WANDER_CYCLE_MS);
        let l = layout();
        assert_eq!(derive(&s_early, now_early, &l), derive(&s_loop, now_loop, &l));
    }

    #[test]
    fn derive_returns_none_when_desk_index_out_of_range() {
        let (mut s, now) = slot(ActivityState::Idle, 0);
        s.desk_index = 999;
        assert!(derive(&s, now, &layout()).is_none());
    }
}
