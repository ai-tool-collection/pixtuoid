use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use crate::source::{Activity, AgentEvent, ToolDetail};
use crate::AgentId;

pub const SOURCE_NAME: &str = "claude-code";

pub fn decode_hook_payload(v: Value) -> Result<AgentEvent> {
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow!("hook payload must be an object"))?;
    let event = obj
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("missing hook_event_name"))?;

    let session_id = obj
        .get("session_id")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("missing session_id"))?
        .to_string();
    let transcript_path = obj
        .get("transcript_path")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("missing transcript_path"))?;
    let agent_id = AgentId::from_transcript_path(transcript_path);

    match event {
        "SessionStart" => {
            let cwd = obj.get("cwd").and_then(|s| s.as_str()).unwrap_or("").into();
            let source = obj
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or(SOURCE_NAME)
                .to_string();
            Ok(AgentEvent::SessionStart {
                agent_id,
                source,
                session_id,
                cwd,
            })
        }
        "PreToolUse" => {
            let tool_name = obj.get("tool_name").and_then(|s| s.as_str()).unwrap_or("?");
            let target = describe_tool_target(tool_name, obj.get("tool_input"));
            let tool_use_id = obj
                .get("tool_use_id")
                .and_then(|s| s.as_str())
                .map(String::from);
            Ok(AgentEvent::ActivityStart {
                agent_id,
                activity: Activity::Typing,
                tool_use_id,
                detail: Some(make_tool_detail(tool_name, target)),
            })
        }
        "PostToolUse" => {
            let tool_use_id = obj
                .get("tool_use_id")
                .and_then(|s| s.as_str())
                .map(String::from);
            Ok(AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id,
            })
        }
        "Notification" => {
            let msg = obj
                .get("message")
                .and_then(|s| s.as_str())
                .unwrap_or("waiting");
            Ok(AgentEvent::Waiting {
                agent_id,
                reason: msg.into(),
            })
        }
        "SessionEnd" => Ok(AgentEvent::SessionEnd { agent_id }),
        other => bail!("unsupported hook_event_name: {other}"),
    }
}

/// Map a raw `(tool_name, target)` pair to a structured `ToolDetail`. CC's
/// Task tool gets the `Task` variant (the reducer keys on this for
/// subagent-leak suppression); everything else becomes `Generic` carrying
/// the formatted display string.
fn make_tool_detail(tool_name: &str, target: String) -> ToolDetail {
    if tool_name == "Task" {
        ToolDetail::Task
    } else {
        ToolDetail::Generic {
            display: format!("{tool_name}{target}"),
        }
    }
}

fn describe_tool_target(tool: &str, input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let key = match tool {
        "Write" | "Edit" | "MultiEdit" | "Read" => "file_path",
        "Bash" => "command",
        "Grep" | "Glob" => "pattern",
        _ => "",
    };
    if key.is_empty() {
        return String::new();
    }
    let Some(s) = input.get(key).and_then(|v| v.as_str()) else {
        return String::new();
    };
    // Truncate by CHARS, not bytes — s.truncate(40) panics if byte 40 lands
    // mid-UTF-8 sequence (e.g. a path containing CJK characters or emoji).
    let total_chars = s.chars().count();
    let mut s: String = s.chars().take(40).collect();
    if total_chars > 40 {
        s.push('…');
    }
    format!(": {s}")
}

/// Decode one JSONL transcript line into 0..N AgentEvents. Unknown / unrelated
/// lines return an empty vec rather than an error so a noisy transcript never
/// kills the watcher.
pub fn decode_jsonl_line(transcript_path: &str, v: Value) -> Result<Vec<AgentEvent>> {
    let agent_id = AgentId::from_transcript_path(transcript_path);
    let Some(obj) = v.as_object() else {
        return Ok(vec![]);
    };

    let mut out = Vec::new();

    if obj.contains_key("step_index") {
        let step_index = obj.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0);
        let step_type = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");

        if step_type == "PLANNER_RESPONSE" {
            if let Some(Value::Array(tool_calls)) = obj.get("tool_calls") {
                for tc in tool_calls {
                    if let Some(tc_obj) = tc.as_object() {
                        let name = tc_obj.get("name").and_then(|s| s.as_str()).unwrap_or("?");
                        let args = tc_obj.get("args");
                        let mut normalized_input = serde_json::Map::new();
                        if let Some(args_obj) = args.and_then(|v| v.as_object()) {
                            let raw_val = args_obj.get("DirectoryPath")
                                .or_else(|| args_obj.get("AbsolutePath"))
                                .or_else(|| args_obj.get("TargetFile"))
                                .or_else(|| args_obj.get("CommandLine"))
                                .or_else(|| args_obj.get("SearchPath"))
                                .or_else(|| args_obj.get("query"))
                                .and_then(|v| v.as_str());
                            if let Some(s) = raw_val {
                                let clean = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
                                let key = match name {
                                    "run_command" => "command",
                                    "grep_search" => "pattern",
                                    _ => "file_path",
                                };
                                normalized_input.insert(key.to_string(), Value::String(clean.to_string()));
                            }
                        }
                        let target = describe_tool_target(name, Some(&Value::Object(normalized_input)));
                        out.push(AgentEvent::ActivityStart {
                            agent_id,
                            activity: Activity::Typing,
                            tool_use_id: Some(format!("tuid-{step_index}")),
                            detail: Some(make_tool_detail(name, target)),
                        });
                    }
                }
            }
        } else if step_type != "USER_INPUT" && step_type != "CONVERSATION_HISTORY" && step_index > 0 {
            out.push(AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: Some(format!("tuid-{}", step_index - 1)),
            });
        }
        return Ok(out);
    }

    let ty = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");

    // Subagent identity: CC tags subagent assistant lines with the dispatching
    // agent name (e.g. "feature-dev:code-explorer"). Strip the plugin prefix
    // — slot widths are 18 cells and the prefix is mostly noise.
    if let Some(name) = obj.get("attributionAgent").and_then(|v| v.as_str()) {
        let label = name.rsplit(':').next().unwrap_or(name).to_string();
        out.push(AgentEvent::Rename { agent_id, label });
    }

    let Some(message) = obj.get("message").and_then(|m| m.as_object()) else {
        return Ok(out);
    };
    let content = message.get("content");
    match (ty, content) {
        ("assistant", Some(Value::Array(blocks))) => {
            for block in blocks {
                let Some(bobj) = block.as_object() else {
                    continue;
                };
                let btype = bobj.get("type").and_then(|s| s.as_str()).unwrap_or("");
                if btype != "tool_use" {
                    continue;
                }
                let id = bobj.get("id").and_then(|s| s.as_str()).map(String::from);
                let name = bobj.get("name").and_then(|s| s.as_str()).unwrap_or("?");
                let input = bobj.get("input");
                let target = describe_tool_target(name, input);
                out.push(AgentEvent::ActivityStart {
                    agent_id,
                    activity: Activity::Typing,
                    tool_use_id: id,
                    detail: Some(make_tool_detail(name, target)),
                });
            }
        }
        ("user", Some(Value::Array(blocks))) => {
            for block in blocks {
                let Some(bobj) = block.as_object() else {
                    continue;
                };
                let btype = bobj.get("type").and_then(|s| s.as_str()).unwrap_or("");
                if btype != "tool_result" {
                    continue;
                }
                let id = bobj
                    .get("tool_use_id")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                out.push(AgentEvent::ActivityEnd {
                    agent_id,
                    tool_use_id: id,
                });
            }
        }
        _ => {}
    }
    Ok(out)
}
