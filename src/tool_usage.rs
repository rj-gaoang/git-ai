//! Feature-gated Skill/Agent/MCP usage telemetry.
//!
//! The event is deliberately independent from the legacy metrics schema.  It
//! contains invocation metadata and aggregate artifact information, never the
//! prompt, transcript, MCP arguments, or tool output.

use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::config::Config;
use crate::daemon::control_api::TelemetryEnvelope;
use crate::uuid::generate_v4;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

mod native_hook;
pub(crate) use native_hook::{hook_metadata, native_invocation};

pub const TOOL_USAGE_SCHEMA_VERSION: &str = "tool_usage/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolUsageKind {
    Agent,
    Skill,
    Mcp,
    HumanOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolUsageStatus {
    Success,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsageEvent {
    pub event_id: String,
    pub schema_version: String,
    pub occurred_at: String,
    pub kind: ToolUsageKind,
    pub event_type: String,
    pub status: ToolUsageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_deleted: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsageBatch {
    pub schema_version: String,
    pub events: Vec<ToolUsageEvent>,
}

fn metadata_value(metadata: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| metadata.get(*key))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Return a non-reversible digest suitable for correlating content without
/// sending the content itself.
pub fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// Format timestamps for the ai-cr upload contract. The service maps this
/// value directly to a Java `Date` using its global `yyyy-MM-dd HH:mm:ss`
/// Jackson format, so RFC3339 offsets and fractional seconds are not valid.
fn occurred_at_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn enabled() -> bool {
    Config::fresh().get_feature_flags().tool_usage_telemetry
}

/// Build an event from the metadata already attached to a checkpoint.  This
/// works for Claude, Copilot, Codex, and future adapters because they all
/// provide the same CheckpointRequest contract.
pub fn event_from_checkpoint(
    request: &CheckpointRequest,
    agent_id: &AgentId,
    lines_added: u32,
    lines_deleted: u32,
) -> ToolUsageEvent {
    let metadata = &request.metadata;
    let kind = if metadata_value(
        metadata,
        &["mcp_server", "mcpServer", "mcp_tool", "mcpTool", "mcp_name"],
    )
    .is_some()
    {
        ToolUsageKind::Mcp
    } else if metadata_value(metadata, &["skill_name", "skill"]).is_some() {
        ToolUsageKind::Skill
    } else {
        ToolUsageKind::Agent
    };
    let event_type = match kind {
        ToolUsageKind::Agent => "agent.completed",
        ToolUsageKind::Skill => "skill.completed",
        ToolUsageKind::Mcp => "mcp.call.completed",
        ToolUsageKind::HumanOutcome => "human.modified",
    };
    let task_id = metadata_value(metadata, &["task_id", "taskId"]).or_else(|| {
        std::env::var("GIT_AI_TASK_ID")
            .ok()
            .filter(|v| !v.trim().is_empty())
    });
    let session_id = request
        .stream_source
        .as_ref()
        .map(|source| source.session_id.clone())
        .or_else(|| metadata_value(metadata, &["session_id", "sessionId"]));
    ToolUsageEvent {
        event_id: format!("evt_{}", generate_v4()),
        schema_version: TOOL_USAGE_SCHEMA_VERSION.to_string(),
        occurred_at: occurred_at_now(),
        kind,
        event_type: event_type.to_string(),
        status: match metadata_value(metadata, &["status"]).as_deref() {
            Some("failed") => ToolUsageStatus::Failed,
            Some("unknown") => ToolUsageStatus::Unknown,
            _ => ToolUsageStatus::Success,
        },
        agent_type: Some(agent_id.tool.clone()),
        agent_id: Some(agent_id.id.clone()),
        model: (!agent_id.model.is_empty()).then(|| agent_id.model.clone()),
        session_id,
        trace_id: Some(request.trace_id.clone()),
        task_id,
        tool_use_id: metadata_value(metadata, &["tool_use_id", "toolUseId"]),
        skill_name: metadata_value(metadata, &["skill_name", "skill"]),
        skill_version: metadata_value(metadata, &["skill_version", "skillVersion"]),
        mcp_server: metadata_value(metadata, &["mcp_server", "mcpServer"]),
        mcp_tool: metadata_value(metadata, &["mcp_tool", "mcpTool", "mcp_name"]),
        duration_ms: metadata_value(metadata, &["duration_ms", "durationMs"])
            .and_then(|value| value.parse().ok()),
        artifact_count: Some(request.files.len() as u32),
        artifact_type: metadata_value(metadata, &["artifact_type", "artifactType"])
            .or_else(|| Some("unknown".to_string())),
        lines_added: Some(lines_added),
        lines_deleted: Some(lines_deleted),
        input_hash: metadata_value(metadata, &["input_hash", "inputHash"]),
        output_hash: metadata_value(metadata, &["output_hash", "outputHash"]),
        error_code: metadata_value(metadata, &["error_code", "errorCode"]),
    }
}

/// Submit one event through the existing daemon telemetry path.  The feature
/// check is repeated here so callers cannot accidentally collect when disabled.
pub fn record(event: ToolUsageEvent) {
    if !enabled() {
        return;
    }
    crate::observability::submit_tool_usage(vec![event]);
}

pub fn record_checkpoint(
    request: &CheckpointRequest,
    agent_id: Option<&AgentId>,
    lines_added: u32,
    lines_deleted: u32,
) {
    let Some(agent_id) = agent_id else { return };
    if !enabled() || !is_completed_checkpoint(request) {
        return;
    }
    record(event_from_checkpoint(
        request,
        agent_id,
        lines_added,
        lines_deleted,
    ));
}

fn is_completed_checkpoint(request: &CheckpointRequest) -> bool {
    request.checkpoint_kind.is_ai()
        && request.path_role != crate::daemon::checkpoint::PreparedPathRole::WillEdit
        && request
            .metadata
            .get("tool_usage_already_recorded")
            .map(String::as_str)
            != Some("true")
}

fn is_human_modification(request: &CheckpointRequest, added: u32, deleted: u32) -> bool {
    request.checkpoint_kind == crate::authorship::working_log::CheckpointKind::KnownHuman
        && request.path_role != crate::daemon::checkpoint::PreparedPathRole::WillEdit
        && (added > 0 || deleted > 0)
}

/// Record a conservative human follow-up signal. Detailed accepted/rejected
/// classification requires later Git history and is left to server aggregation.
pub fn record_human_outcome(
    request: &CheckpointRequest,
    event_type: &str,
    lines_added: u32,
    lines_deleted: u32,
) {
    if !enabled() || !is_human_modification(request, lines_added, lines_deleted) {
        return;
    }
    let metadata = &request.metadata;
    record(ToolUsageEvent {
        event_id: format!("evt_{}", generate_v4()),
        schema_version: TOOL_USAGE_SCHEMA_VERSION.to_string(),
        occurred_at: occurred_at_now(),
        kind: ToolUsageKind::HumanOutcome,
        event_type: event_type.to_string(),
        status: ToolUsageStatus::Success,
        agent_type: Some("human".to_string()),
        agent_id: None,
        model: None,
        session_id: request
            .stream_source
            .as_ref()
            .map(|source| source.session_id.clone())
            .or_else(|| metadata_value(metadata, &["session_id", "sessionId"])),
        trace_id: Some(request.trace_id.clone()),
        task_id: metadata_value(metadata, &["task_id", "taskId"]).or_else(|| {
            std::env::var("GIT_AI_TASK_ID")
                .ok()
                .filter(|v| !v.trim().is_empty())
        }),
        tool_use_id: metadata_value(metadata, &["tool_use_id", "toolUseId"]),
        skill_name: None,
        skill_version: None,
        mcp_server: None,
        mcp_tool: None,
        duration_ms: None,
        artifact_count: Some(request.files.len() as u32),
        artifact_type: Some("unknown".to_string()),
        lines_added: Some(lines_added),
        lines_deleted: Some(lines_deleted),
        input_hash: None,
        output_hash: None,
        error_code: None,
    });
}

pub fn envelope(events: Vec<ToolUsageEvent>) -> TelemetryEnvelope {
    TelemetryEnvelope::ToolUsage { events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::working_log::CheckpointKind;
    use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
    use crate::daemon::checkpoint::PreparedPathRole;

    pub(super) struct TelemetryFlag(Option<String>);

    impl TelemetryFlag {
        pub(super) fn set(value: &str) -> Self {
            let old = std::env::var("GIT_AI_TOOL_USAGE_TELEMETRY").ok();
            unsafe {
                std::env::set_var("GIT_AI_TOOL_USAGE_TELEMETRY", value);
            }
            Self(old)
        }
    }

    impl Drop for TelemetryFlag {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(value) => std::env::set_var("GIT_AI_TOOL_USAGE_TELEMETRY", value),
                    None => std::env::remove_var("GIT_AI_TOOL_USAGE_TELEMETRY"),
                }
            }
        }
    }

    #[test]
    fn excludes_pre_edit_baselines_and_untracked_changes() {
        let mut request = CheckpointRequest {
            trace_id: "trace".into(),
            checkpoint_kind: CheckpointKind::Human,
            agent_id: None,
            files: vec![],
            path_role: PreparedPathRole::WillEdit,
            stream_source: None,
            metadata: HashMap::new(),
        };
        assert!(!is_completed_checkpoint(&request));
        assert!(!is_human_modification(&request, 5, 1));
        request.path_role = PreparedPathRole::Edited;
        assert!(!is_human_modification(&request, 5, 1));
        request.checkpoint_kind = CheckpointKind::AiAgent;
        assert!(is_completed_checkpoint(&request));
        request
            .metadata
            .insert("tool_usage_already_recorded".into(), "true".into());
        assert!(!is_completed_checkpoint(&request));
        request.metadata.clear();
        assert!(!is_human_modification(&request, 5, 1));
        request.checkpoint_kind = CheckpointKind::KnownHuman;
        assert!(!is_completed_checkpoint(&request));
        assert!(!is_human_modification(&request, 0, 0));
        assert!(is_human_modification(&request, 0, 1));
    }

    #[test]
    fn camel_case_mcp_metadata_is_classified_consistently() {
        let request = CheckpointRequest {
            trace_id: "trace".into(),
            checkpoint_kind: CheckpointKind::AiAgent,
            agent_id: None,
            files: vec![],
            path_role: PreparedPathRole::Edited,
            stream_source: None,
            metadata: HashMap::from([
                ("mcpServer".into(), "review".into()),
                ("mcpTool".into(), "search".into()),
            ]),
        };
        let event = event_from_checkpoint(
            &request,
            &AgentId {
                tool: "codex".into(),
                id: "a".into(),
                model: "".into(),
            },
            0,
            0,
        );
        assert_eq!(event.kind, ToolUsageKind::Mcp);
        assert_eq!(event.mcp_server.as_deref(), Some("review"));
    }

    #[test]
    fn digest_does_not_return_plaintext() {
        assert!(!digest("secret").contains("secret"));
    }

    #[test]
    fn occurred_at_uses_ai_cr_date_format() {
        let value = occurred_at_now();
        assert_eq!(value.len(), 19);
        assert!(chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M:%S").is_ok());
        assert!(!value.contains('T'));
        assert!(!value.ends_with('Z'));
    }

    #[test]
    fn maps_mcp_metadata_without_payload() {
        let request = CheckpointRequest {
            trace_id: "trace".into(),
            checkpoint_kind: CheckpointKind::AiAgent,
            agent_id: None,
            files: vec![],
            path_role: PreparedPathRole::Edited,
            stream_source: None,
            metadata: HashMap::from([
                ("mcp_server".into(), "review".into()),
                ("mcp_tool".into(), "search".into()),
            ]),
        };
        let event = event_from_checkpoint(
            &request,
            &AgentId {
                tool: "claude".into(),
                id: "a".into(),
                model: "m".into(),
            },
            1,
            0,
        );
        assert_eq!(event.kind, ToolUsageKind::Mcp);
        assert_eq!(event.mcp_tool.as_deref(), Some("search"));
        assert!(
            serde_json::to_string(&event)
                .unwrap()
                .contains("mcp_server")
        );
    }

    #[test]
    #[serial_test::serial]
    fn remote_url_ignores_empty_values() {
        let previous = std::env::var(crate::api::tool_usage::TOOL_USAGE_REMOTE_URL_ENV).ok();
        unsafe {
            std::env::set_var(crate::api::tool_usage::TOOL_USAGE_REMOTE_URL_ENV, "  ");
        }
        assert_eq!(
            crate::api::tool_usage::remote_url(),
            crate::api::tool_usage::DEFAULT_TOOL_USAGE_REMOTE_URL
        );
        match previous {
            Some(value) => unsafe {
                std::env::set_var(crate::api::tool_usage::TOOL_USAGE_REMOTE_URL_ENV, value)
            },
            None => unsafe {
                std::env::remove_var(crate::api::tool_usage::TOOL_USAGE_REMOTE_URL_ENV)
            },
        }
    }
}
