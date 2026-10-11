//! Incremental evidence for Codex collaboration calls that bypass native hooks.
use super::native_invocation;
use crate::commands::checkpoint_agent::presets::ParsedHookEvent;
use crate::streams::types::{StreamBatch, StreamError};
use crate::streams::watermark::WatermarkStrategy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;

const MAX_PENDING: usize = 128;
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct InvocationCursor {
    offset: u64,
    cwd: String,
    external_session: String,
    pending: BTreeMap<String, PendingCall>,
    #[serde(default)]
    not_before: Option<i64>,
}

impl InvocationCursor {
    pub(crate) fn starting_at(timestamp: i64) -> Self {
        Self {
            not_before: Some(timestamp),
            ..Default::default()
        }
    }

    pub(crate) fn with_start_time(mut self, timestamp: i64) -> Self {
        self.not_before = Some(timestamp);
        self
    }
    pub(crate) fn at_offset(
        path: &Path,
        offset: u64,
        cwd: String,
        external_session: String,
    ) -> Self {
        let mut cursor = Self {
            offset,
            cwd,
            external_session,
            ..Default::default()
        };
        // Recover in-flight requests before the metrics offset without emitting
        // completed historical calls. Bounded lookbehind avoids rereading huge rollouts.
        if let Ok(file) = std::fs::File::open(path) {
            let start = offset.saturating_sub(512 * 1024);
            let mut reader = BufReader::new(file);
            if reader.seek(SeekFrom::Start(start)).is_ok() {
                let mut line = String::new();
                use std::io::BufRead;
                let mut position = start;
                if start > 0 {
                    position += reader.read_line(&mut line).unwrap_or(0) as u64;
                }
                while position < offset {
                    line.clear();
                    let Ok(bytes) = reader.read_line(&mut line) else {
                        break;
                    };
                    if bytes == 0 || !line.ends_with('\n') {
                        break;
                    }
                    position += bytes as u64;
                    if position > offset {
                        break;
                    }
                    if let Ok(raw) = serde_json::from_str(&line) {
                        cursor.consume_inner(&raw, false);
                    }
                }
            }
        }
        cursor
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingCall {
    tool: String,
    input: Value,
    cwd: String,
    timestamp: Option<String>,
}

impl WatermarkStrategy for InvocationCursor {
    fn serialize(&self) -> String {
        serde_json::to_string(self).expect("invocation cursor is serializable")
    }
    fn advance(&mut self, bytes_read: usize, _records_read: usize) {
        self.offset += bytes_read as u64;
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn collaboration_tool(payload: &Value) -> Option<&str> {
    let name = payload.get("name")?.as_str()?;
    let name = name.strip_prefix("collaboration.").unwrap_or(name);
    matches!(name, "spawn_agent" | "followup_task").then_some(name)
}

impl InvocationCursor {
    fn consume(&mut self, raw: &Value) -> Option<Value> {
        self.consume_inner(raw, true)
    }

    fn consume_inner(&mut self, raw: &Value, emit: bool) -> Option<Value> {
        let payload = raw.get("payload")?;
        if matches!(raw.get("type")?.as_str()?, "session_meta" | "turn_context") {
            if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                self.cwd = cwd.into();
            }
            if raw["type"] == "session_meta"
                && let Some(id) = payload.get("id").and_then(Value::as_str)
            {
                self.external_session = id.into();
            }
            return None;
        }
        if raw["type"] != "response_item" {
            return None;
        }
        let id = payload.get("call_id")?.as_str()?;
        if payload["type"] == "function_call" {
            let tool = collaboration_tool(payload)?;
            if emit && let Some(cutoff) = self.not_before {
                let timestamp = raw
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())?;
                if timestamp.timestamp() < cutoff {
                    return None;
                }
            }
            let arguments = payload.get("arguments")?.as_str()?;
            if arguments.len() > MAX_INPUT_BYTES || self.pending.len() >= MAX_PENDING {
                return None;
            }
            let input = serde_json::from_str(arguments).ok()?;
            self.pending.insert(
                id.into(),
                PendingCall {
                    tool: tool.into(),
                    input,
                    cwd: self.cwd.clone(),
                    timestamp: raw
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            );
            return None;
        }
        if payload["type"] != "function_call_output" {
            return None;
        }
        let call = self.pending.remove(id)?;
        if !emit {
            return None;
        }
        if let Some(cutoff) = self.not_before {
            let timestamp = raw
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())?;
            if timestamp.timestamp() < cutoff {
                return None;
            }
        }
        if call.cwd.is_empty() || self.external_session.is_empty() {
            return None;
        }
        // Inspect only structured success/error evidence; never upload the output.
        let output = payload.get("output")?;
        let dispatch_only = call.tool == "followup_task"
            && output.as_str() == Some("")
            && call
                .input
                .get("target")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
        let response: Value = if let Some(s) = output.as_str() {
            if s.len() > MAX_INPUT_BYTES {
                return None;
            }
            if dispatch_only {
                json!({})
            } else {
                serde_json::from_str(s).ok()?
            }
        } else {
            output.clone()
        };
        let failed = response
            .get("isError")
            .or_else(|| response.get("is_error"))
            .and_then(Value::as_bool)
            == Some(true)
            || response.get("error").is_some_and(|v| !v.is_null())
            || response.get("success").and_then(Value::as_bool) == Some(false);
        // An arbitrary textual response is not proof that an Agent was started.
        if !failed
            && !dispatch_only
            && response
                .get("task_name")
                .or_else(|| response.get("agent_id"))
                .and_then(Value::as_str)
                .is_none()
        {
            return None;
        }
        let mut input = call.input;
        let mut encrypted_input = false;
        for key in ["message", "prompt", "instructions"] {
            if input
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("gAAAAA"))
            {
                input.as_object_mut()?.remove(key);
                encrypted_input = true;
            }
        }
        let data = json!({
            "hook_event_name": if failed { "PostToolUseFailure" } else { "PostToolUse" },
            "tool_name": call.tool, "tool_input": input,
            "tool_use_id": id, "cwd": call.cwd,
        });
        let ParsedHookEvent::ToolInvocation { mut event, cwd } = native_invocation(
            &data,
            "codex",
            &self.external_session,
            &crate::authorship::authorship_log_serialization::generate_trace_id(),
        )?
        else {
            return None;
        };
        if encrypted_input {
            event.content_truncated = Some(true);
        }
        if dispatch_only {
            // followup_task has a void return in the native orchestration bridge.
            // A paired return proves dispatch, not successful task completion.
            event.event_type = "agent.invoked".into();
            event.status = super::ToolUsageStatus::Unknown;
            event.event_id = format!(
                "evt_{}",
                super::digest(
                    &serde_json::to_string(&(
                        "codex",
                        &self.external_session,
                        id,
                        &event.event_type
                    ))
                    .ok()?
                )
                .trim_start_matches("sha256:")
            );
        }
        let config = crate::config::Config::get();
        if config.has_repository_filters() {
            let repo = crate::git::repository::discover_repository_in_path_no_git_exec(&cwd).ok();
            if !config.is_allowed_repository(&repo) {
                return None;
            }
        }
        // Preserve the occurrence time, rather than the later sweep time.
        if let Some(ts) = call
            .timestamp
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        {
            event.occurred_at = ts
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
        }
        if failed {
            event.status = super::ToolUsageStatus::Failed;
            event.event_type = "agent.failed".into();
        }
        serde_json::to_value(event).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_usage::tests::TelemetryFlag;
    use std::io::Write;

    fn write(file: &mut std::fs::File, value: Value) {
        writeln!(file, "{value}").unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn codex_invocation_survives_partial_output_and_cursor_restart() {
        let _flag = TelemetryFlag::set("true");
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write(
            file.as_file_mut(),
            json!({"type":"session_meta","payload":{"id":"real-shape-session","cwd":"C:/project"}}),
        );
        write(
            file.as_file_mut(),
            json!({"type":"response_item","timestamp":"2026-10-11T14:49:50.657Z","payload":{
                "type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"call-agent",
                "arguments":json!({"task_name":"review","message":"review this patch"}).to_string()
            }}),
        );
        let first = read_batch(file.path(), InvocationCursor::default(), "s", 1).unwrap();
        assert!(first.events.is_empty());
        let serialized = first.new_watermark.serialize();
        let restarted: InvocationCursor = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restarted.pending.len(), 1);
        let output = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call-agent","output":"{\"task_name\":\"/root/review\",\"output\":\"private-AI-output\"}"}}).to_string();
        let split = output.len() / 2;
        file.write_all(output[..split].as_bytes()).unwrap();
        let partial = read_batch(file.path(), restarted, "s", 1).unwrap();
        assert!(partial.events.is_empty());
        assert_eq!(partial.new_watermark.serialize(), serialized);
        file.write_all(output[split..].as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        let restored = serde_json::from_str(&partial.new_watermark.serialize()).unwrap();
        let batch = read_batch(file.path(), restored, "s", 1).unwrap();
        assert_eq!(batch.events.len(), 1);
        let event = &batch.events[0];
        assert_eq!(event["event_type"], "agent.started");
        assert_eq!(event["occurred_at"], "2026-10-11 14:49:50");
        assert_eq!(event["agent_name"], "review");
        assert!(
            event["input_content"]
                .as_str()
                .unwrap()
                .contains("review this patch")
        );
        assert!(!event.to_string().contains("private-AI-output"));
        let second = read_batch(
            file.path(),
            serde_json::from_str(&batch.new_watermark.serialize()).unwrap(),
            "s",
            1,
        )
        .unwrap();
        assert!(second.events.is_empty());
        let replay = read_batch(file.path(), InvocationCursor::default(), "s", 1).unwrap();
        assert_eq!(event["event_id"], replay.events[0]["event_id"]);
    }

    #[test]
    #[serial_test::serial]
    fn codex_invocation_failures_disabled_and_unrelated_tools() {
        for enabled in ["true", "false"] {
            let _flag = TelemetryFlag::set(enabled);
            let mut cursor = InvocationCursor {
                cwd: "C:/project".into(),
                external_session: "s".into(),
                ..Default::default()
            };
            for (tool, output, expected) in [
                (
                    "spawn_agent",
                    json!({"error":"private-error-output"}),
                    Some("agent.failed"),
                ),
                (
                    "spawn_agent",
                    json!({"task_name":"/root/review"}),
                    Some("agent.started"),
                ),
                ("exec_command", json!({"task_name":"/root/review"}), None),
                ("spawn_agent", json!("unstructured output"), None),
            ] {
                assert!(cursor.consume(&json!({"type":"response_item","payload":{"type":"function_call","name":tool,"call_id":"call","arguments":"{\"task_name\":\"review\",\"message\":\"gAAAAAencrypted\"}"}})).is_none());
                let result = cursor.consume(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call","output":output.to_string()}}));
                if enabled == "false" || expected.is_none() {
                    assert!(result.is_none());
                } else {
                    let event = result.unwrap();
                    assert_eq!(event["event_type"], expected.unwrap());
                    assert!(!event.to_string().contains("private-error-output"));
                    assert!(!event.to_string().contains("gAAAAAencrypted"));
                    assert_eq!(event["content_truncated"], true);
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn cursor_seed_preserves_pending_without_replaying_history() {
        let _flag = TelemetryFlag::set("true");
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write(
            file.as_file_mut(),
            json!({"type":"session_meta","payload":{"id":"session","cwd":"C:/project"}}),
        );
        for id in ["completed", "pending"] {
            write(
                file.as_file_mut(),
                json!({"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","call_id":id,"arguments":"{\"task_name\":\"review\"}"}}),
            );
            if id == "completed" {
                write(
                    file.as_file_mut(),
                    json!({"type":"response_item","payload":{"type":"function_call_output","call_id":id,"output":"{\"task_name\":\"/root/review\"}"}}),
                );
            }
        }
        let offset = file.as_file().metadata().unwrap().len();
        let cursor = InvocationCursor::at_offset(file.path(), offset, String::new(), String::new());
        assert_eq!(cursor.pending.len(), 1);
        assert!(cursor.pending.contains_key("pending"));
        write(
            file.as_file_mut(),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"pending","output":"{\"task_name\":\"/root/review\"}"}}),
        );
        let batch = read_batch(file.path(), cursor, "s", 1).unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0]["tool_use_id"], "pending");
    }

    #[test]
    #[serial_test::serial]
    fn followup_void_return_is_invocation_evidence_not_completion() {
        let _flag = TelemetryFlag::set("true");
        let mut cursor = InvocationCursor {
            cwd: "C:/project".into(),
            external_session: "s".into(),
            ..Default::default()
        };
        cursor.consume(&json!({"type":"response_item","payload":{"type":"function_call","name":"followup_task","call_id":"followup","arguments":"{\"target\":\"/root/review\",\"message\":\"gAAAAAencrypted\"}"}}));
        let event = cursor.consume(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"followup","output":""}})).unwrap();
        assert_eq!(event["event_type"], "agent.invoked");
        assert_eq!(event["status"], "unknown");
        assert_eq!(event["agent_name"], "/root/review");
        assert!(!event.to_string().contains("encrypted"));
    }

    #[test]
    #[serial_test::serial]
    fn first_discovery_ignores_calls_before_collection_started() {
        let _flag = TelemetryFlag::set("true");
        let mut cursor = InvocationCursor::starting_at(
            chrono::DateTime::parse_from_rfc3339("2026-10-11T15:00:00Z")
                .unwrap()
                .timestamp(),
        );
        cursor.cwd = "C:/project".into();
        cursor.external_session = "s".into();
        for (id, timestamp, expected) in [
            ("old", "2026-10-11T14:00:00Z", false),
            ("new", "2026-10-11T16:00:00Z", true),
        ] {
            cursor.consume(&json!({"type":"response_item","timestamp":timestamp,"payload":{"type":"function_call","name":"spawn_agent","call_id":id,"arguments":"{\"task_name\":\"review\"}"}}));
            let event=cursor.consume(&json!({"type":"response_item","timestamp":timestamp,"payload":{"type":"function_call_output","call_id":id,"output":"{\"task_name\":\"/root/review\"}"}}));
            assert_eq!(event.is_some(), expected);
        }
    }

    #[test]
    #[serial_test::serial]
    fn lagging_metrics_seed_does_not_emit_already_completed_history() {
        let _flag = TelemetryFlag::set("true");
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write(
            file.as_file_mut(),
            json!({"type":"session_meta","payload":{"id":"session","cwd":"C:/project"}}),
        );
        write(
            file.as_file_mut(),
            json!({"type":"response_item","timestamp":"2026-10-11T14:00:00Z","payload":{"type":"function_call","name":"spawn_agent","call_id":"old","arguments":"{\"task_name\":\"review\"}"}}),
        );
        let offset = file.as_file().metadata().unwrap().len();
        write(
            file.as_file_mut(),
            json!({"type":"response_item","timestamp":"2026-10-11T14:00:01Z","payload":{"type":"function_call_output","call_id":"old","output":"{\"task_name\":\"/root/review\"}"}}),
        );
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-10-11T15:00:00Z")
            .unwrap()
            .timestamp();
        let cursor = InvocationCursor::at_offset(file.path(), offset, String::new(), String::new())
            .with_start_time(cutoff);
        let batch = read_batch(file.path(), cursor, "s", 1).unwrap();
        assert!(batch.events.is_empty());
        let cursor = InvocationCursor::at_offset(file.path(), 0, String::new(), String::new())
            .with_start_time(cutoff);
        let batch = read_batch(file.path(), cursor, "s", 1).unwrap();
        assert!(batch.events.is_empty());
    }
}

pub(crate) fn read_batch(
    path: &Path,
    mut cursor: InvocationCursor,
    _session: &str,
    limit: usize,
) -> Result<StreamBatch, StreamError> {
    let file = std::fs::File::open(path).map_err(|e| StreamError::Fatal {
        message: format!("Failed to open invocation stream: {e}"),
    })?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(cursor.offset))
        .map_err(|e| StreamError::Fatal {
            message: format!("Failed to seek invocation stream: {e}"),
        })?;
    let mut events = Vec::new();
    let mut line = String::new();
    while let crate::streams::types::JsonlLineState::Complete(bytes) =
        crate::streams::types::read_jsonl_line(&mut reader, &mut line).map_err(|e| {
            StreamError::Fatal {
                message: format!("Failed to read invocation stream: {e}"),
            }
        })?
    {
        cursor.offset += bytes as u64;
        if let Ok(raw) = serde_json::from_str(&line)
            && let Some(event) = cursor.consume(&raw)
        {
            events.push(event);
            if events.len() >= limit {
                break;
            }
        }
    }
    Ok(StreamBatch {
        events,
        new_watermark: Box::new(cursor),
    })
}
