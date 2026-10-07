//! Native invocation capture that does not require edited files or transcripts.
use super::*;
use crate::authorship::authorship_log_serialization::generate_session_id;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint_agent::presets::ParsedHookEvent;
use crate::daemon::checkpoint::PreparedPathRole;
use serde_json::Value;
use std::path::PathBuf;

fn string(data: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| data.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Copy only contract fields; never copy the payload wholesale.
pub(crate) fn hook_metadata(data: &Value) -> HashMap<String, String> {
    if !enabled() {
        return HashMap::new();
    }
    let mut metadata = HashMap::new();
    for (key, aliases) in [
        ("task_id", &["task_id", "taskId"][..]),
        ("skill_name", &["skill_name", "skillName"][..]),
        ("skill_version", &["skill_version", "skillVersion"][..]),
        ("mcp_server", &["mcp_server", "mcpServer"][..]),
        ("mcp_tool", &["mcp_tool", "mcpTool"][..]),
        ("artifact_type", &["artifact_type", "artifactType"][..]),
    ] {
        if let Some(value) =
            string(data, aliases).or_else(|| data.get("metadata").and_then(|v| string(v, aliases)))
        {
            metadata.insert(key.to_owned(), value);
        }
    }
    if let Some(duration) = data
        .get("duration_ms")
        .or_else(|| data.get("durationMs"))
        .and_then(Value::as_u64)
    {
        metadata.insert("duration_ms".into(), duration.to_string());
    }
    metadata
}

/// Called by presets only after their existing source/session validation.
/// Utility invocations produce telemetry, never a synthetic file checkpoint.
pub(crate) fn native_invocation(
    data: &Value,
    agent_type: &str,
    session: &str,
    trace_id: &str,
) -> Option<ParsedHookEvent> {
    if !enabled() {
        return None;
    }
    let phase = string(data, &["hook_event_name", "hookEventName"])?;
    if !matches!(phase.as_str(), "PostToolUse" | "PostToolUseFailure") {
        return None;
    }
    let tool = string(data, &["tool_name", "toolName"])?;
    let input = data.get("tool_input").or_else(|| data.get("toolInput"));
    // Only named invocations are intercepted. Ordinary edit/bash hooks retain
    // their original checkpoint and attribution behavior.
    let mut metadata = hook_metadata(data);
    let kind = if let Some(name) = tool.strip_prefix("mcp__") {
        let (server, method) = name.split_once("__")?;
        if server.is_empty() || method.is_empty() {
            return None;
        }
        metadata.insert("mcp_server".into(), server.into());
        metadata.insert("mcp_tool".into(), method.into());
        ToolUsageKind::Mcp
    } else if matches!(
        tool.as_str(),
        "Skill" | "skill" | "read_skill" | "use_skill"
    ) {
        let name = metadata
            .get("skill_name")
            .cloned()
            .or_else(|| input.and_then(|v| string(v, &["skill", "name", "skill_name"])))?;
        metadata.insert("skill_name".into(), name);
        ToolUsageKind::Skill
    } else if matches!(
        tool.as_str(),
        "Agent" | "Task" | "agent" | "task" | "spawn_agent"
    ) {
        ToolUsageKind::Agent
    } else {
        return None;
    };
    let cwd = string(data, &["cwd", "workspace_folder", "workspaceFolder"])?;
    if let Some(id) = string(
        data,
        &["tool_use_id", "toolUseId", "tool_call_id", "toolCallId"],
    ) {
        metadata.insert("tool_use_id".into(), id);
    }
    metadata.insert(
        "session_id".into(),
        generate_session_id(session, agent_type),
    );
    let response = data
        .get("tool_response")
        .or_else(|| data.get("toolResponse"))
        .or_else(|| data.get("tool_result"))
        .or_else(|| data.get("toolResult"));
    let failed = phase == "PostToolUseFailure"
        || data.get("success").and_then(Value::as_bool) == Some(false)
        || response
            .and_then(|v| v.get("is_error").or_else(|| v.get("isError")))
            .and_then(Value::as_bool)
            == Some(true);
    if failed {
        metadata.insert("status".into(), "failed".into());
    }
    let request = CheckpointRequest {
        trace_id: trace_id.into(),
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: None,
        files: vec![],
        path_role: PreparedPathRole::Edited,
        stream_source: None,
        metadata,
    };
    let agent = AgentId {
        tool: agent_type.into(),
        id: session.into(),
        model: string(data, &["model"]).unwrap_or_default(),
    };
    let mut event = event_from_checkpoint(&request, &agent, 0, 0);
    event.kind = kind;
    // Absence of a file checkpoint is not evidence of zero artifacts.
    event.artifact_count = None;
    event.lines_added = None;
    event.lines_deleted = None;
    if failed {
        event.event_type = match kind {
            ToolUsageKind::Agent => "agent.failed",
            ToolUsageKind::Skill => "skill.failed",
            _ => "mcp.call.failed",
        }
        .into();
    }
    if let Some(id) = &event.tool_use_id {
        event.event_id = format!(
            "evt_{}",
            digest(&serde_json::to_string(&(agent_type, session, id, &event.event_type)).ok()?)
                .trim_start_matches("sha256:")
        );
    }
    Some(ParsedHookEvent::ToolInvocation {
        event: Box::new(event),
        cwd: PathBuf::from(cwd),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::resolve_preset;
    use crate::tool_usage::tests::TelemetryFlag;
    use serde_json::json;

    fn hook(provider: &str, tool: &str, phase: &str) -> Value {
        let mut data = json!({
            "cwd": "C:/telemetry-test", "hookEventName": phase,
            "toolName": tool, "sessionId": "smoke-session", "session_id": "smoke-session",
            "toolUseId": format!("call-{tool}"), "taskId": "smoke-task",
            "artifactType": "test", "durationMs": 12,
            "toolInput": {"skill": "code-review", "prompt": "private-input", "arguments": "private-args"},
            "toolResponse": {"output": "private-output"}
        });
        if provider == "claude" {
            data["transcript_path"] = json!("C:/user/.claude/projects/session.jsonl");
        } else if provider == "github-copilot" {
            data["transcript_path"] =
                json!("C:/user/github.copilot-chat/transcripts/session.jsonl");
        }
        data
    }

    fn parse(provider: &str, data: Value) -> ToolUsageEvent {
        let preset = if provider == "github-copilot-cli" {
            "github-copilot"
        } else {
            provider
        };
        let events = resolve_preset(preset)
            .unwrap()
            .parse(&data.to_string(), "trace")
            .unwrap();
        assert_eq!(events.len(), 1);
        match events.into_iter().next().unwrap() {
            ParsedHookEvent::ToolInvocation { event, .. } => *event,
            _ => panic!("native utility invocation must not create a file checkpoint"),
        }
    }

    fn matrix() -> Vec<ToolUsageEvent> {
        let mut events = vec![];
        for provider in ["claude", "codex", "github-copilot", "github-copilot-cli"] {
            for (tool, kind) in [
                ("Agent", ToolUsageKind::Agent),
                ("Skill", ToolUsageKind::Skill),
                ("mcp__review__search", ToolUsageKind::Mcp),
            ] {
                let event = parse(provider, hook(provider, tool, "PostToolUse"));
                assert_eq!(event.kind, kind);
                assert_eq!(event.agent_type.as_deref(), Some(provider));
                assert_eq!(event.task_id.as_deref(), Some("smoke-task"));
                assert_eq!(event.status, ToolUsageStatus::Success);
                assert_eq!(event.duration_ms, Some(12));
                assert_eq!(event.artifact_type.as_deref(), Some("test"));
                assert_eq!(event.artifact_count, None);
                assert_eq!(event.lines_added, None);
                let serialized = serde_json::to_string(&event).unwrap();
                for secret in [
                    "private-input",
                    "private-args",
                    "private-output",
                    "transcript_path",
                ] {
                    assert!(!serialized.contains(secret));
                }
                if kind == ToolUsageKind::Skill {
                    assert_eq!(event.skill_name.as_deref(), Some("code-review"));
                }
                if kind == ToolUsageKind::Mcp {
                    assert_eq!(event.mcp_server.as_deref(), Some("review"));
                    assert_eq!(event.mcp_tool.as_deref(), Some("search"));
                }
                events.push(event);
            }
        }
        events
    }

    #[test]
    #[serial_test::serial]
    fn native_hooks_capture_all_providers_without_files_or_payloads() {
        let _flag = TelemetryFlag::set("true");
        assert_eq!(matrix().len(), 12);
    }

    #[test]
    #[serial_test::serial]
    fn feature_disabled_does_not_intercept_utility_hooks() {
        let _flag = TelemetryFlag::set("false");
        assert!(hook_metadata(&hook("codex", "Skill", "PostToolUse")).is_empty());
        assert!(
            native_invocation(&hook("codex", "Skill", "PostToolUse"), "codex", "s", "t").is_none()
        );
        assert!(
            resolve_preset("codex")
                .unwrap()
                .parse(&hook("codex", "Skill", "PostToolUse").to_string(), "t")
                .is_err()
        );
    }

    #[test]
    #[serial_test::serial]
    fn pre_hooks_are_not_counted_and_failed_hooks_are_not_successful() {
        let _flag = TelemetryFlag::set("true");
        for provider in ["claude", "codex", "github-copilot", "github-copilot-cli"] {
            assert!(
                native_invocation(&hook(provider, "Skill", "PreToolUse"), provider, "s", "t")
                    .is_none()
            );
            let failed = parse(
                provider,
                hook(provider, "mcp__review__search", "PostToolUseFailure"),
            );
            assert_eq!(failed.status, ToolUsageStatus::Failed);
            assert_eq!(failed.event_type, "mcp.call.failed");
            let mut data = hook(provider, "Skill", "PostToolUse");
            data["toolResponse"]["isError"] = json!(true);
            assert_eq!(parse(provider, data).status, ToolUsageStatus::Failed);
        }
    }

    #[test]
    #[serial_test::serial]
    fn native_hook_replay_has_stable_id_and_preserves_source_guards() {
        let _flag = TelemetryFlag::set("true");
        let data = hook("claude", "Skill", "PostToolUse");
        assert_eq!(
            parse("claude", data.clone()).event_id,
            parse("claude", data).event_id
        );
        let wrong_source = hook("github-copilot", "Skill", "PostToolUse").to_string();
        assert!(
            resolve_preset("claude")
                .unwrap()
                .parse(&wrong_source, "t")
                .is_err()
        );
        let wrong_source = hook("claude", "Skill", "PostToolUse").to_string();
        assert!(
            resolve_preset("github-copilot")
                .unwrap()
                .parse(&wrong_source, "t")
                .is_err()
        );
    }

    #[test]
    #[serial_test::serial]
    fn native_tool_usage_preserves_claude_file_attribution() {
        let mut data = hook("claude", "mcp__review__edit", "PostToolUse");
        data["toolInput"]["file_path"] = json!("src/main.rs");
        for enabled in ["false", "true"] {
            let _flag = TelemetryFlag::set(enabled);
            let events = resolve_preset("claude")
                .unwrap()
                .parse(&data.to_string(), "t")
                .unwrap();
            assert_eq!(events.len(), if enabled == "true" { 2 } else { 1 });
            if enabled == "true" {
                assert!(matches!(events[0], ParsedHookEvent::ToolInvocation { .. }));
            }
            let ParsedHookEvent::PostFileEdit(edit) = events.last().unwrap() else {
                panic!("explicit file edits must retain their attribution checkpoint");
            };
            assert_eq!(
                edit.file_paths,
                vec![PathBuf::from("C:/telemetry-test/src/main.rs")]
            );
            assert_eq!(edit.tool_use_id.as_deref(), Some("call-mcp__review__edit"));
            assert_eq!(
                edit.context
                    .metadata
                    .contains_key("tool_usage_already_recorded"),
                enabled == "true"
            );
        }
    }

    #[test]
    #[ignore = "Writes a labeled smoke batch to the configured ai-cr remote endpoint"]
    #[serial_test::serial]
    fn tool_usage_live_upload_and_duplicate_acknowledgement() {
        use crate::api::client::{ApiClient, ApiContext};
        let _flag = TelemetryFlag::set("true");
        let task = format!("git-ai-smoke-{}", generate_v4());
        let mut events = matrix();
        for event in &mut events {
            event.event_id = format!("evt_smoke_{}", generate_v4());
            event.task_id = Some(task.clone());
        }
        let batch = ToolUsageBatch {
            schema_version: TOOL_USAGE_SCHEMA_VERSION.into(),
            events,
        };
        // Deliberately avoid loading or forwarding local Git AI credentials.
        let client = ApiClient::new(ApiContext {
            base_url: "http://unused.invalid".into(),
            auth_token: None,
            api_key: None,
            author_identity: None,
            timeout_secs: Some(30),
        });
        let url = crate::api::tool_usage::remote_url();
        client
            .upload_tool_usage_at(&url, &batch)
            .expect("remote must acknowledge the complete native-hook batch");
        let response = client.context().post_json_url(&url, &batch).unwrap();
        assert_eq!(response.status_code, 200);
        let ack: Value = serde_json::from_slice(response.as_bytes()).unwrap();
        assert_eq!(ack["code"], 200);
        assert_eq!(ack["data"]["accepted"], 0);
        assert_eq!(ack["data"]["duplicate"], batch.events.len());
        assert_eq!(ack["data"]["rejected"], 0);
        println!(
            "task_id={task}, events={}, repeated_upload={}",
            batch.events.len(),
            ack["data"]
        );
    }
}
