use ascii_agents_core::source::decoder::{decode_hook_payload, decode_jsonl_line};
use ascii_agents_core::source::{Activity, AgentEvent};
use ascii_agents_core::AgentId;

fn load(name: &str) -> serde_json::Value {
    let s = std::fs::read_to_string(format!("tests/fixtures/hooks/{name}.json")).unwrap();
    serde_json::from_str(&s).unwrap()
}

fn load_jsonl(name: &str) -> serde_json::Value {
    let s = std::fs::read_to_string(format!("tests/fixtures/jsonl/{name}.json")).unwrap();
    serde_json::from_str(&s).unwrap()
}

#[test]
fn decode_session_start() {
    let ev = decode_hook_payload(load("session_start")).unwrap();
    let expected_id = AgentId::from_transcript_path("/Users/me/.claude/projects/x/ses-abc.jsonl");
    match ev {
        AgentEvent::SessionStart {
            agent_id,
            session_id,
            source,
            ..
        } => {
            assert_eq!(agent_id, expected_id);
            assert_eq!(session_id, "ses-abc");
            assert_eq!(source, "claude-code");
        }
        other => panic!("expected SessionStart, got {other:?}"),
    }
}

#[test]
fn decode_session_start_with_custom_source() {
    let mut payload = load("session_start");
    payload["source"] = serde_json::Value::String("antigravity".into());
    let ev = decode_hook_payload(payload).unwrap();
    let expected_id = AgentId::from_transcript_path("/Users/me/.claude/projects/x/ses-abc.jsonl");
    match ev {
        AgentEvent::SessionStart {
            agent_id,
            session_id,
            source,
            ..
        } => {
            assert_eq!(agent_id, expected_id);
            assert_eq!(session_id, "ses-abc");
            assert_eq!(source, "antigravity");
        }
        other => panic!("expected SessionStart, got {other:?}"),
    }
}

#[test]
fn decode_pre_tool_use_write_maps_to_typing() {
    let ev = decode_hook_payload(load("pre_tool_use_write")).unwrap();
    match ev {
        AgentEvent::ActivityStart {
            activity, detail, ..
        } => {
            assert_eq!(activity, Activity::Typing);
            assert!(detail.unwrap().display().contains("Write"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_post_tool_use_is_activity_end() {
    let ev = decode_hook_payload(load("post_tool_use_write")).unwrap();
    assert!(matches!(ev, AgentEvent::ActivityEnd { .. }));
}

#[test]
fn decode_notification_is_waiting() {
    let ev = decode_hook_payload(load("notification")).unwrap();
    match ev {
        AgentEvent::Waiting { reason, .. } => assert!(reason.contains("permission")),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_session_end() {
    let ev = decode_hook_payload(load("session_end")).unwrap();
    assert!(matches!(ev, AgentEvent::SessionEnd { .. }));
}

#[test]
fn decode_unknown_event_returns_err() {
    let mut bad = load("session_start");
    bad["hook_event_name"] = serde_json::Value::String("UnknownThing".into());
    assert!(decode_hook_payload(bad).is_err());
}

#[test]
fn jsonl_assistant_tool_use_is_activity_start_with_tool_use_id() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let events = decode_jsonl_line(transcript, load_jsonl("assistant_tool_use")).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::ActivityStart {
            activity,
            tool_use_id,
            detail,
            ..
        } => {
            assert_eq!(*activity, Activity::Typing);
            assert_eq!(tool_use_id.as_deref(), Some("tu_123"));
            assert!(detail.as_ref().unwrap().display().contains("Write"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn jsonl_tool_result_is_activity_end() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let events = decode_jsonl_line(transcript, load_jsonl("tool_result")).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("tu_123"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_hook_payload_with_multibyte_tool_input_does_not_panic() {
    // Regression: describe_tool_target used to call String::truncate(40),
    // which panics if byte 40 lands mid-UTF-8-character (e.g. Chinese path).
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-zh",
        "transcript_path": "/tmp/zh.jsonl",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": {
            "command": "echo 这是一个非常长的中文命令需要被截断这是一个非常长的中文命令需要被截断"
        }
    });
    let ev = decode_hook_payload(payload).unwrap();
    match ev {
        AgentEvent::ActivityStart { detail, .. } => {
            let d = detail.expect("detail set");
            assert!(d.display().contains("Bash"), "got: {}", d.display());
            // Should end with ellipsis if truncated; either way no panic.
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }
}

#[test]
fn decode_pre_tool_use_carries_tool_use_id_from_payload() {
    // Current CC PreToolUse payloads include a `tool_use_id` field. The
    // decoder must surface it so the reducer can pair hook events with the
    // matching JSONL line and track in-flight Task tool_use_ids.
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/Users/me/.claude/projects/x/ses-abc.jsonl",
        "cwd": "/repo",
        "tool_name": "Task",
        "tool_use_id": "toolu_01ABC",
        "tool_input": { "description": "go" }
    });
    let ev = decode_hook_payload(payload).unwrap();
    match ev {
        AgentEvent::ActivityStart {
            tool_use_id,
            detail,
            ..
        } => {
            assert_eq!(tool_use_id.as_deref(), Some("toolu_01ABC"));
            assert!(
                detail.expect("detail set").is_task(),
                "Task tool should produce Task variant"
            );
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn decode_post_tool_use_carries_tool_use_id_from_payload() {
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "ses-abc",
        "transcript_path": "/Users/me/.claude/projects/x/ses-abc.jsonl",
        "cwd": "/repo",
        "tool_name": "Task",
        "tool_use_id": "toolu_01ABC"
    });
    let ev = decode_hook_payload(payload).unwrap();
    match ev {
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("toolu_01ABC"));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn jsonl_subagent_line_with_attribution_emits_rename() {
    // CC tags subagent assistant lines with `attributionAgent` like
    // "feature-dev:code-explorer". The decoder must surface this as a
    // Rename event so the sprite label reads as the real subagent name
    // instead of "cc#N".
    let transcript = "/Users/me/.claude/projects/x/sess/subagents/agent-abc.jsonl";
    let v = serde_json::json!({
        "type": "assistant",
        "sessionId": "sess",
        "cwd": "/repo",
        "attributionAgent": "feature-dev:code-explorer",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "Read",
                  "input": { "file_path": "/repo/src/a.rs" } }
            ]
        }
    });
    let events = decode_jsonl_line(transcript, v).unwrap();
    // Plugin prefix is stripped so the 18-cell slot can fit the name.
    let has_rename = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Rename { label, .. } if label == "code-explorer"
        )
    });
    assert!(has_rename, "expected Rename event, got {events:?}");
}

#[test]
fn jsonl_plain_user_message_yields_no_events() {
    let transcript = "/Users/me/.claude/projects/x/ses-abc.jsonl";
    let events = decode_jsonl_line(transcript, load_jsonl("user_message")).unwrap();
    assert!(events.is_empty());
}

#[test]
fn jsonl_antigravity_decoding() {
    let transcript = "/Users/me/.gemini/antigravity-cli/brain/sess/transcript.jsonl";
    
    // Test ActivityStart from PLANNER_RESPONSE
    let v_start = serde_json::json!({
        "step_index": 2,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "DONE",
        "created_at": "2026-05-25T00:16:13Z",
        "tool_calls": [
            {
                "name": "list_dir",
                "args": {
                    "DirectoryPath": "\"/repo/src\""
                }
            }
        ]
    });
    let events_start = decode_jsonl_line(transcript, v_start).unwrap();
    assert_eq!(events_start.len(), 1);
    match &events_start[0] {
        AgentEvent::ActivityStart { activity, tool_use_id, detail, .. } => {
            assert_eq!(*activity, Activity::Typing);
            assert_eq!(tool_use_id.as_deref(), Some("tuid-2"));
            assert!(detail.as_ref().unwrap().display().contains("list_dir"));
        }
        other => panic!("expected ActivityStart, got {other:?}"),
    }

    // Test ActivityEnd from subsequent LIST_DIRECTORY tool output step
    let v_end = serde_json::json!({
        "step_index": 3,
        "source": "MODEL",
        "type": "LIST_DIRECTORY",
        "status": "DONE",
        "created_at": "2026-05-25T00:16:14Z",
        "content": "some output"
    });
    let events_end = decode_jsonl_line(transcript, v_end).unwrap();
    assert_eq!(events_end.len(), 1);
    match &events_end[0] {
        AgentEvent::ActivityEnd { tool_use_id, .. } => {
            assert_eq!(tool_use_id.as_deref(), Some("tuid-2"));
        }
        other => panic!("expected ActivityEnd, got {other:?}"),
    }
}
