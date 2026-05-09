use super::super::parse;
use super::super::{
    ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit, PresetContext,
    TranscriptFormat, TranscriptSource,
};
use crate::authorship::authorship_log_serialization::generate_session_id;
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::ToolClass;
use crate::error::GitAiError;
use crate::transcripts::model_extraction;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Legacy extension path (before_edit / after_edit)
// ---------------------------------------------------------------------------

pub(super) fn parse_legacy_extension_hooks(
    data: &serde_json::Value,
    hook_event_name: &str,
    trace_id: &str,
) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    let cwd = parse::optional_str_multi(data, &["workspace_folder", "workspaceFolder"])
        .ok_or_else(|| {
            GitAiError::PresetError(
                "workspace_folder or workspaceFolder not found in hook_input for GitHub Copilot preset".to_string(),
            )
        })?;

    let dirty_files = super::dirty_files_from_hook_data(data, cwd);

    let session_id = super::extract_session_id(data);

    if hook_event_name == "before_edit" {
        let will_edit_filepaths = data
            .get("will_edit_filepaths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| parse::resolve_absolute(s, cwd))
                    .collect::<Vec<PathBuf>>()
            })
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "will_edit_filepaths is required for before_edit hook_event_name".to_string(),
                )
            })?;

        if will_edit_filepaths.is_empty() {
            return Err(GitAiError::PresetError(
                "will_edit_filepaths cannot be empty for before_edit hook_event_name".to_string(),
            ));
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "github-copilot".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            external_session_id: session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::new(),
        };

        return Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
            context,
            file_paths: will_edit_filepaths,
            dirty_files,
            tool_use_id: None,
        })]);
    }

    // after_edit path
    let chat_session_path =
        parse::optional_str_multi(data, &["chat_session_path", "chatSessionPath"]).ok_or_else(
            || {
                GitAiError::PresetError(
                    "chat_session_path or chatSessionPath not found in hook_input for after_edit"
                        .to_string(),
                )
            },
        )?;

    let edited_filepaths = data
        .get("edited_filepaths")
        .and_then(|val| val.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| parse::resolve_absolute(s, cwd))
                .collect::<Vec<PathBuf>>()
        })
        .unwrap_or_default();

    let mut metadata = HashMap::new();
    metadata.insert(
        "chat_session_path".to_string(),
        chat_session_path.to_string(),
    );

    let context = PresetContext {
        agent_id: AgentId {
            tool: "github-copilot".to_string(),
            id: session_id.clone(),
            model: model_extraction::extract_model(
                Path::new(chat_session_path),
                crate::transcripts::sweep::TranscriptFormat::CopilotSessionJson,
                None,
            )
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string()),
        },
        external_session_id: session_id,
        trace_id: trace_id.to_string(),
        cwd: PathBuf::from(cwd),
        metadata,
    };

    let transcript_source = Some(TranscriptSource {
        path: PathBuf::from(chat_session_path),
        format: TranscriptFormat::CopilotSessionJson,
        session_id: generate_session_id(&context.external_session_id, "github-copilot"),
        external_session_id: context.external_session_id.clone(),
        external_parent_session_id: None,
    });

    Ok(vec![ParsedHookEvent::PostFileEdit(PostFileEdit {
        context,
        file_paths: edited_filepaths,
        dirty_files,
        transcript_source,
        tool_use_id: None,
    })])
}

// ---------------------------------------------------------------------------
// VS Code native path (PreToolUse / PostToolUse)
// ---------------------------------------------------------------------------

pub(super) fn parse_vscode_native_hooks(
    data: &serde_json::Value,
    hook_event_name: &str,
    trace_id: &str,
) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    let cwd = parse::optional_str_multi(data, &["cwd", "workspace_folder", "workspaceFolder"])
        .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

    let dirty_files = super::dirty_files_from_hook_data(data, cwd);

    let session_id = super::extract_session_id(data);

    let tool_name =
        parse::optional_str_multi(data, &["tool_name", "toolName"]).unwrap_or("unknown");

    // Enforce tool filtering to avoid creating checkpoints for read/search tools
    if !is_supported_vscode_edit_tool_name(tool_name) {
        return Err(GitAiError::PresetError(format!(
            "Skipping VS Code hook for unsupported tool_name '{}' (non-edit tool).",
            tool_name
        )));
    }

    let tool_input = data.get("tool_input").or_else(|| data.get("toolInput"));
    let tool_response = data
        .get("tool_response")
        .or_else(|| data.get("toolResponse"));

    let tool_use_id = parse::optional_str_multi(data, &["tool_use_id", "toolUseId"])
        .unwrap_or("unknown")
        .to_string();

    // Extract file paths from tool_input and tool_response only (not session-level data)
    let (mut extracted_paths, mut path_extraction_source) =
        extract_filepaths_from_current_copilot_tool_call(
        tool_input,
        tool_response,
        cwd,
        &tool_use_id,
        tool_name,
    );

    let transcript_path = transcript_path_from_hook_data(data)
        .or_else(|| chat_session_path_from_hook_data(data))
        .map(|s| s.to_string());
    let chat_session_path = chat_session_path_from_hook_data(data)
        .map(|s| s.to_string())
        .or_else(|| {
            transcript_path
                .as_deref()
                .and_then(|path| derive_chat_session_path_from_transcript(path, &session_id))
        });

    if let Some(ref path) = transcript_path
        && looks_like_claude_transcript_path(path)
    {
        return Err(GitAiError::PresetError(
            "Skipping VS Code hook because transcript_path looks like a Claude transcript path."
                .to_string(),
        ));
    }

    if !is_likely_copilot_native_hook(transcript_path.as_deref()) {
        return Err(GitAiError::PresetError(format!(
            "Skipping VS Code hook for non-Copilot session (tool_name: {}).",
            tool_name,
        )));
    }

    if extracted_paths.is_empty()
        && let Some(path) = transcript_path.as_deref()
    {
        extracted_paths = extract_filepaths_from_exact_copilot_tool_call(
            path,
            &tool_use_id,
            tool_name,
            cwd,
        );
        if !extracted_paths.is_empty() {
            path_extraction_source = CopilotPathExtractionSource::ExactTranscriptToolCall;
        }
    }

    if hook_event_name == "PostToolUse" {
        append_copilot_native_hook_path_debug_event(
            hook_event_name,
            tool_name,
            &tool_use_id,
            cwd,
            transcript_path.as_deref(),
            chat_session_path.as_deref(),
            path_extraction_source,
            &extracted_paths,
        );
    }

    let tool_class = classify_copilot_tool(tool_name);
    let is_bash = tool_class == ToolClass::Bash;

    let mut metadata = HashMap::new();
    if let Some(ref path) = transcript_path {
        metadata.insert("transcript_path".to_string(), path.clone());
    }
    if let Some(ref path) = chat_session_path {
        metadata.insert("chat_session_path".to_string(), path.clone());
    }

    let transcript_format = transcript_path
        .as_deref()
        .map(infer_copilot_transcript_format)
        .unwrap_or(TranscriptFormat::CopilotSessionJson);

    let context = PresetContext {
        agent_id: AgentId {
            tool: "github-copilot".to_string(),
            id: session_id.clone(),
            model: chat_session_path
                .as_ref()
                .or(transcript_path.as_ref())
                .and_then(|tp| {
                    let sweep_format = match infer_copilot_transcript_format(tp.as_str()) {
                        TranscriptFormat::CopilotEventStreamJsonl => {
                            crate::transcripts::sweep::TranscriptFormat::CopilotEventStreamJsonl
                        }
                        _ => crate::transcripts::sweep::TranscriptFormat::CopilotSessionJson,
                    };
                    model_extraction::extract_model(Path::new(tp.as_str()), sweep_format, None)
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| "unknown".to_string()),
        },
        external_session_id: session_id,
        trace_id: trace_id.to_string(),
        cwd: PathBuf::from(cwd),
        metadata,
    };

    let transcript_source = transcript_path.map(|tp| TranscriptSource {
        path: PathBuf::from(tp),
        format: transcript_format,
        session_id: generate_session_id(&context.external_session_id, "github-copilot"),
        external_session_id: context.external_session_id.clone(),
        external_parent_session_id: None,
    });

    if hook_event_name == "PreToolUse" {
        if is_bash {
            return Ok(vec![ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id,
            })]);
        }

        if tool_name.eq_ignore_ascii_case("create_file") {
            if extracted_paths.is_empty() {
                return Err(GitAiError::PresetError(
                    "No file path found in create_file PreToolUse tool_input".to_string(),
                ));
            }

            let mut empty_dirty_files: HashMap<PathBuf, String> = HashMap::new();
            for path in &extracted_paths {
                empty_dirty_files.insert(path.clone(), String::new());
            }
            return Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: extracted_paths,
                dirty_files: Some(empty_dirty_files),
                tool_use_id: Some(tool_use_id),
            })]);
        }

        if extracted_paths.is_empty() {
            return Err(GitAiError::PresetError(format!(
                "No editable file paths found in VS Code hook input (tool_name: {}). Skipping checkpoint.",
                tool_name
            )));
        }

        return Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
            context,
            file_paths: extracted_paths,
            dirty_files,
            tool_use_id: Some(tool_use_id),
        })]);
    }

    // PostToolUse
    if is_bash {
        return Ok(vec![ParsedHookEvent::PostBashCall(PostBashCall {
            context,
            tool_use_id,
            transcript_source,
        })]);
    }

    if extracted_paths.is_empty() {
        return Err(GitAiError::PresetError(format!(
            "No editable file paths found in VS Code PostToolUse hook input (tool_name: {}). Skipping checkpoint.",
            tool_name
        )));
    }

    Ok(vec![ParsedHookEvent::PostFileEdit(PostFileEdit {
        context,
        file_paths: extracted_paths,
        dirty_files,
        transcript_source,
        tool_use_id: Some(tool_use_id),
    })])
}

// ---------------------------------------------------------------------------
// IDE-specific helpers
// ---------------------------------------------------------------------------

const COPILOT_TOOL_CALL_ID_KEYS: &[&str] = &[
    "toolCallId",
    "tool_call_id",
    "toolUseId",
    "tool_use_id",
    "id",
];

const COPILOT_TOOL_CALL_NAME_KEYS: &[&str] = &["toolName", "tool_name", "name"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopilotPathExtractionSource {
    CurrentToolCall,
    HookPayloadFallback,
    ExactTranscriptToolCall,
    None,
}

impl CopilotPathExtractionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::CurrentToolCall => "current_tool_call",
            Self::HookPayloadFallback => "hook_payload_fallback",
            Self::ExactTranscriptToolCall => "exact_transcript_tool_call",
            Self::None => "none",
        }
    }
}

fn append_copilot_native_hook_path_debug_event(
    hook_event_name: &str,
    tool_name: &str,
    tool_use_id: &str,
    cwd: &str,
    transcript_path: Option<&str>,
    chat_session_path: Option<&str>,
    path_extraction_source: CopilotPathExtractionSource,
    extracted_paths: &[PathBuf],
) {
    crate::diagnostics::append_debug_event(
        "copilot_native_hook_tool_call_paths_parsed",
        serde_json::json!({
            "hookEventName": hook_event_name,
            "toolName": tool_name,
            "toolUseId": tool_use_id,
            "cwd": cwd.replace('\\', "/"),
            "transcriptPath": transcript_path,
            "chatSessionPath": chat_session_path,
            "pathExtractionSource": path_extraction_source.as_str(),
            "parsedFileCount": extracted_paths.len(),
            "parsedFilepaths": extracted_paths
                .iter()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .collect::<Vec<_>>(),
        }),
    );
}

fn extract_filepaths_from_current_copilot_tool_call(
    tool_input: Option<&serde_json::Value>,
    tool_response: Option<&serde_json::Value>,
    cwd: &str,
    tool_use_id: &str,
    tool_name: &str,
) -> (Vec<PathBuf>, CopilotPathExtractionSource) {
    for value in [tool_input, tool_response].into_iter().flatten() {
        if let Some(paths) = extract_filepaths_from_matching_copilot_tool_call(
            value,
            COPILOT_TOOL_CALL_ID_KEYS,
            COPILOT_TOOL_CALL_NAME_KEYS,
            tool_use_id,
            tool_name,
            cwd,
        ) && !paths.is_empty()
        {
            return (paths, CopilotPathExtractionSource::CurrentToolCall);
        }
    }

    let fallback_paths = super::extract_filepaths_from_vscode_hook_payload(tool_input, tool_response, cwd);
    let source = if fallback_paths.is_empty() {
        CopilotPathExtractionSource::None
    } else {
        CopilotPathExtractionSource::HookPayloadFallback
    };
    (fallback_paths, source)
}

fn extract_filepaths_from_exact_copilot_tool_call(
    transcript_path: &str,
    tool_use_id: &str,
    tool_name: &str,
    cwd: &str,
) -> Vec<PathBuf> {
    if is_copilot_event_stream_transcript(transcript_path) {
        return extract_filepaths_from_copilot_event_stream_jsonl(
            transcript_path,
            tool_use_id,
            tool_name,
            cwd,
        );
    }

    extract_filepaths_from_copilot_session_json(transcript_path, tool_use_id, tool_name, cwd)
}

fn is_copilot_event_stream_transcript(transcript_path: &str) -> bool {
    if Path::new(transcript_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
    {
        return true;
    }

    let normalized = transcript_path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/workspacestorage/") || normalized.contains("/transcripts/")
}

fn extract_filepaths_from_copilot_event_stream_jsonl(
    transcript_path: &str,
    tool_use_id: &str,
    tool_name: &str,
    cwd: &str,
) -> Vec<PathBuf> {
    let Ok(jsonl_content) = std::fs::read_to_string(transcript_path) else {
        return Vec::new();
    };

    for line in jsonl_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        if let Some(paths) = find_matching_copilot_tool_call_paths(&entry, tool_use_id, tool_name, cwd)
            && !paths.is_empty()
        {
            return paths;
        }
    }

    Vec::new()
}

fn extract_filepaths_from_copilot_session_json(
    transcript_path: &str,
    tool_use_id: &str,
    tool_name: &str,
    cwd: &str,
) -> Vec<PathBuf> {
    let Ok(session_json) = std::fs::read_to_string(transcript_path) else {
        return Vec::new();
    };
    let Ok(entry) = serde_json::from_str::<serde_json::Value>(&session_json) else {
        return Vec::new();
    };

    find_matching_copilot_tool_call_paths(&entry, tool_use_id, tool_name, cwd).unwrap_or_default()
}

fn find_matching_copilot_tool_call_paths(
    value: &serde_json::Value,
    tool_use_id: &str,
    tool_name: &str,
    cwd: &str,
) -> Option<Vec<PathBuf>> {
    if let Some(paths) = extract_filepaths_from_matching_copilot_tool_call(
        value,
        COPILOT_TOOL_CALL_ID_KEYS,
        COPILOT_TOOL_CALL_NAME_KEYS,
        tool_use_id,
        tool_name,
        cwd,
    ) && !paths.is_empty()
    {
        return Some(paths);
    }

    match value {
        serde_json::Value::Object(map) => map
            .values()
            .find_map(|child| find_matching_copilot_tool_call_paths(child, tool_use_id, tool_name, cwd)),
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|child| find_matching_copilot_tool_call_paths(child, tool_use_id, tool_name, cwd)),
        _ => None,
    }
}

fn extract_filepaths_from_matching_copilot_tool_call(
    value: &serde_json::Value,
    id_keys: &[&str],
    name_keys: &[&str],
    tool_use_id: &str,
    tool_name: &str,
    cwd: &str,
) -> Option<Vec<PathBuf>> {
    let id = string_field_from_keys(value, id_keys)?;
    if !copilot_tool_call_id_matches(id, tool_use_id) {
        return None;
    }

    if !copilot_tool_name_matches(string_field_from_keys(value, name_keys), tool_name) {
        return None;
    }

    let input = copilot_tool_call_arguments(value);
    Some(super::extract_filepaths_from_vscode_hook_payload(
        Some(&input),
        None,
        cwd,
    ))
}

fn string_field_from_keys<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|field| field.as_str()))
}

fn copilot_tool_call_id_matches(candidate_id: &str, tool_use_id: &str) -> bool {
    if candidate_id == tool_use_id {
        return true;
    }

    let normalized_tool_use_id = strip_vscode_tool_call_suffix(tool_use_id);
    if normalized_tool_use_id != tool_use_id && candidate_id == normalized_tool_use_id {
        return true;
    }

    let normalized_candidate_id = strip_vscode_tool_call_suffix(candidate_id);
    normalized_candidate_id != candidate_id && normalized_candidate_id == tool_use_id
}

fn strip_vscode_tool_call_suffix(id: &str) -> &str {
    match id.rsplit_once("__vscode-") {
        Some((prefix, suffix))
            if !prefix.is_empty()
                && !suffix.is_empty()
                && suffix.chars().all(|ch| ch.is_ascii_digit()) =>
        {
            prefix
        }
        _ => id,
    }
}

fn copilot_tool_name_matches(candidate: Option<&str>, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() || expected.eq_ignore_ascii_case("unknown") {
        return true;
    }
    candidate
        .map(|value| value.eq_ignore_ascii_case(expected))
        .unwrap_or(true)
}

fn copilot_tool_call_arguments(value: &serde_json::Value) -> serde_json::Value {
    ["arguments", "input", "tool_input", "toolInput"]
        .iter()
        .find_map(|key| value.get(*key))
        .map(normalize_copilot_tool_arguments)
        .unwrap_or(serde_json::Value::Null)
}

fn normalize_copilot_tool_arguments(value: &serde_json::Value) -> serde_json::Value {
    if let Some(as_str) = value.as_str() {
        serde_json::from_str::<serde_json::Value>(as_str)
            .unwrap_or_else(|_| serde_json::Value::String(as_str.to_string()))
    } else {
        value.clone()
    }
}

fn transcript_path_from_hook_data(data: &serde_json::Value) -> Option<&str> {
    parse::optional_str_multi(data, &["transcript_path", "transcriptPath"])
}

fn chat_session_path_from_hook_data(data: &serde_json::Value) -> Option<&str> {
    parse::optional_str_multi(data, &["chat_session_path", "chatSessionPath"])
}

fn derive_chat_session_path_from_transcript(
    transcript_path: &str,
    session_id: &str,
) -> Option<String> {
    let transcript_path = Path::new(transcript_path);
    let parent = transcript_path.parent()?;
    let parent_name = parent.file_name()?.to_str()?;

    if parent_name.eq_ignore_ascii_case("chatSessions") {
        return transcript_path
            .is_file()
            .then(|| transcript_path.to_string_lossy().to_string());
    }

    if !parent_name.eq_ignore_ascii_case("transcripts") {
        return None;
    }

    let copilot_dir = parent.parent()?;
    let copilot_dir_name = copilot_dir.file_name()?.to_str()?;
    if !copilot_dir_name.eq_ignore_ascii_case("GitHub.copilot-chat") {
        return None;
    }

    let workspace_storage_dir = copilot_dir.parent()?;
    let chat_sessions_dir = workspace_storage_dir.join("chatSessions");

    ["jsonl", "json"]
        .into_iter()
        .map(|ext| chat_sessions_dir.join(format!("{}.{}", session_id, ext)))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().to_string())
}

fn infer_copilot_transcript_format(path: &str) -> TranscriptFormat {
    let is_workspace_storage = path.contains("/workspaceStorage/") || path.contains("\\workspaceStorage\\");
    let is_jsonl = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"));

    if is_jsonl || is_workspace_storage {
        TranscriptFormat::CopilotEventStreamJsonl
    } else {
        TranscriptFormat::CopilotSessionJson
    }
}

fn looks_like_claude_transcript_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/.claude/") || normalized.contains("/claude/projects/")
}

fn looks_like_copilot_transcript_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/github.copilot-chat/transcripts/")
        || normalized.contains("vscode-chat-session")
        || normalized.contains("copilot_session")
        || (normalized.contains("/workspacestorage/") && normalized.contains("/chatsessions/"))
}

fn is_likely_copilot_native_hook(transcript_path: Option<&str>) -> bool {
    let Some(path) = transcript_path else {
        return false;
    };
    if looks_like_claude_transcript_path(path) {
        return false;
    }
    looks_like_copilot_transcript_path(path)
}

fn is_supported_vscode_edit_tool_name(tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();

    // Explicit bash/terminal tools
    let bash_tools = ["run_in_terminal"];
    if bash_tools.iter().any(|name| lower == *name) {
        return true;
    }

    let non_edit_keywords = [
        "find", "search", "read", "grep", "glob", "list", "ls", "fetch", "web", "open", "todo",
    ];
    if non_edit_keywords.iter().any(|kw| lower.contains(kw)) {
        return false;
    }

    let exact_edit_tools = [
        "write",
        "edit",
        "multiedit",
        "applypatch",
        "apply_patch",
        "copilot_insertedit",
        "copilot_replacestring",
        "vscode_editfile_internal",
        "create_file",
        "delete_file",
        "rename_file",
        "move_file",
        "replace_string_in_file",
        "insert_edit_into_file",
    ];
    if exact_edit_tools.iter().any(|name| lower == *name) {
        return true;
    }

    lower.contains("edit") || lower.contains("write") || lower.contains("replace")
}

/// Classify GitHub Copilot tool for bash vs file edit handling.
/// GithubCopilot is not in the `Agent` enum, so we implement classification locally.
fn classify_copilot_tool(tool_name: &str) -> ToolClass {
    let lower = tool_name.to_ascii_lowercase();
    match lower.as_str() {
        "run_in_terminal" => ToolClass::Bash,
        "create_file"
        | "replace_string_in_file"
        | "apply_patch"
        | "delete_file"
        | "rename_file"
        | "move_file" => ToolClass::FileEdit,
        _ if lower.contains("edit") || lower.contains("write") || lower.contains("replace") => {
            ToolClass::FileEdit
        }
        _ => ToolClass::Skip,
    }
}

/// Extract file paths from apply_patch text format. Called from the shared
/// `collect_tool_paths` because apply_patch payloads embed paths in the patch
/// text rather than in JSON keys.
pub(super) fn collect_apply_patch_paths_from_text(raw: &str, out: &mut Vec<String>) {
    for line in raw.lines() {
        let trimmed = line.trim();
        let maybe_path = trimmed
            .strip_prefix("*** Update File: ")
            .or_else(|| trimmed.strip_prefix("*** Add File: "))
            .or_else(|| trimmed.strip_prefix("*** Delete File: "))
            .or_else(|| trimmed.strip_prefix("*** Move to: "));

        if let Some(path) = maybe_path {
            let path = path.trim();
            if !path.is_empty() && !out.iter().any(|existing| existing == path) {
                out.push(path.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::AgentPreset;
    use super::super::GithubCopilotPreset;
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // Legacy extension path tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_copilot_legacy_before_edit() {
        let input = json!({
            "hook_event_name": "before_edit",
            "workspace_folder": "/home/user/project",
            "will_edit_filepaths": ["/home/user/project/src/main.rs"],
            "chat_session_id": "sess-123",
            "dirty_files": {"/home/user/project/src/main.rs": "old content"}
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(e.context.external_session_id, "sess-123");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_some());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_copilot_dirty_files_camel_case() {
        let input = json!({
            "hook_event_name": "before_edit",
            "workspace_folder": "/home/user/project",
            "will_edit_filepaths": ["/home/user/project/src/main.rs"],
            "chat_session_id": "sess-123",
            "dirtyFiles": {"/home/user/project/src/main.rs": "content"}
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert!(e.dirty_files.is_some());
                let df = e.dirty_files.as_ref().unwrap();
                assert!(df.contains_key(&PathBuf::from("/home/user/project/src/main.rs")));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_copilot_legacy_after_edit() {
        let input = json!({
            "hook_event_name": "after_edit",
            "workspace_folder": "/home/user/project",
            "chat_session_path": "/home/user/.vscode/sessions/sess-123.json",
            "session_id": "sess-123",
            "edited_filepaths": ["src/main.rs"]
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(e.context.external_session_id, "sess-123");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::CopilotSessionJson,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_copilot_legacy_before_edit_empty_filepaths() {
        let input = json!({
            "hook_event_name": "before_edit",
            "workspace_folder": "/home/user/project",
            "will_edit_filepaths": [],
            "chat_session_id": "sess-123"
        })
        .to_string();
        let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // VS Code native path tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_copilot_native_pre_file_edit() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/home/user/project",
            "tool_name": "replace_string_in_file",
            "session_id": "sess-456",
            "tool_use_id": "tu-1",
            "tool_input": {"file_path": "/home/user/project/src/main.rs"},
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(e.context.external_session_id, "sess-456");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_copilot_native_post_file_edit() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": "/home/user/project",
            "tool_name": "create_file",
            "session_id": "sess-456",
            "tool_use_id": "tu-2",
            "tool_input": {"file_path": "/home/user/project/src/new.rs"},
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/new.rs")]
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::CopilotSessionJson,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_copilot_native_pre_bash_call() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/home/user/project",
            "tool_name": "run_in_terminal",
            "session_id": "sess-456",
            "tool_use_id": "tu-3",
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(e.tool_use_id, "tu-3");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_copilot_native_post_bash_call() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": "/home/user/project",
            "tool_name": "run_in_terminal",
            "session_id": "sess-456",
            "tool_use_id": "tu-3",
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "github-copilot");
                assert_eq!(e.tool_use_id, "tu-3");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_copilot_native_create_file_pre_empty_dirty() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/home/user/project",
            "tool_name": "create_file",
            "session_id": "sess-456",
            "tool_input": {"file_path": "/home/user/project/src/new_file.rs"},
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/new_file.rs")]
                );
                assert_eq!(
                    e.dirty_files
                        .as_ref()
                        .unwrap()
                        .get(&PathBuf::from("/home/user/project/src/new_file.rs")),
                    Some(&String::new())
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_copilot_skips_non_edit_tools() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/home/user/project",
            "tool_name": "search_files",
            "session_id": "sess-456",
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();
        let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn test_copilot_skips_claude_transcript() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/home/user/project",
            "tool_name": "create_file",
            "session_id": "sess-456",
            "tool_input": {"file_path": "src/main.rs"},
            "transcript_path": "/home/user/.claude/projects/test.json"
        })
        .to_string();
        let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());
    }

    #[test]
    fn test_copilot_session_id_fallback() {
        let input = json!({
            "hook_event_name": "before_edit",
            "workspace_folder": "/home/user/project",
            "will_edit_filepaths": ["/home/user/project/src/main.rs"],
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.external_session_id, "unknown");
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    // -----------------------------------------------------------------------
    // Helper function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_classify_copilot_tool_bash() {
        assert_eq!(classify_copilot_tool("run_in_terminal"), ToolClass::Bash);
    }

    #[test]
    fn test_classify_copilot_tool_file_edit() {
        assert_eq!(classify_copilot_tool("create_file"), ToolClass::FileEdit);
        assert_eq!(
            classify_copilot_tool("replace_string_in_file"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_copilot_tool("apply_patch"), ToolClass::FileEdit);
        assert_eq!(classify_copilot_tool("delete_file"), ToolClass::FileEdit);
    }

    #[test]
    fn test_classify_copilot_tool_heuristic() {
        assert_eq!(
            classify_copilot_tool("custom_edit_tool"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_copilot_tool("write_changes"), ToolClass::FileEdit);
    }

    #[test]
    fn test_classify_copilot_tool_skip() {
        assert_eq!(classify_copilot_tool("search_files"), ToolClass::Skip);
        assert_eq!(classify_copilot_tool("unknown_tool"), ToolClass::Skip);
    }

    #[test]
    fn test_collect_apply_patch_paths() {
        let text = "*** Update File: /home/user/src/main.rs\n--- some diff ---\n*** Add File: /home/user/src/new.rs\n";
        let mut paths = Vec::new();
        collect_apply_patch_paths_from_text(text, &mut paths);
        assert_eq!(
            paths,
            vec!["/home/user/src/main.rs", "/home/user/src/new.rs"]
        );
    }

    #[test]
    fn test_looks_like_copilot_transcript_path() {
        assert!(looks_like_copilot_transcript_path(
            "/home/user/.vscode/data/github.copilot-chat/transcripts/test.json"
        ));
        assert!(looks_like_copilot_transcript_path(
            "/path/to/vscode-chat-session-123.json"
        ));
        assert!(!looks_like_copilot_transcript_path(
            "/home/user/.claude/projects/test.json"
        ));
    }

    #[test]
    fn test_is_supported_vscode_edit_tool_name() {
        assert!(is_supported_vscode_edit_tool_name("create_file"));
        assert!(is_supported_vscode_edit_tool_name("run_in_terminal"));
        assert!(is_supported_vscode_edit_tool_name("replace_string_in_file"));
        assert!(is_supported_vscode_edit_tool_name("custom_edit_tool"));
        assert!(!is_supported_vscode_edit_tool_name("search_files"));
        assert!(!is_supported_vscode_edit_tool_name("read_file"));
    }

    #[test]
    fn test_copilot_tool_call_id_matches_vscode_suffix_pair() {
        assert!(copilot_tool_call_id_matches("tu-1__vscode-123", "tu-1"));
        assert!(copilot_tool_call_id_matches("tu-1", "tu-1__vscode-123"));
    }

    #[test]
    fn test_copilot_tool_call_id_matches_keeps_distinct_vscode_suffix_ids_distinct() {
        assert!(!copilot_tool_call_id_matches(
            "tu-1__vscode-123",
            "tu-1__vscode-456"
        ));
    }

    #[test]
    fn test_copilot_native_post_file_edit_uses_exact_transcript_tool_call_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let transcript_dir = temp
            .path()
            .join("workspaceStorage")
            .join("abc")
            .join("GitHub.copilot-chat")
            .join("transcripts");
        std::fs::create_dir_all(&transcript_dir).unwrap();

        let transcript_path = transcript_dir.join("session.jsonl");
        let transcript = r#"{"type":"session.start","data":{"sessionId":"session-1","producer":"copilot-agent"}}
{"type":"assistant.message","data":{"toolRequests":[{"toolCallId":"call_exact","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n+fn main() {}\\n*** End Patch\"}"}]}}
{"type":"tool.execution_start","data":{"toolCallId":"call_exact","toolName":"apply_patch","arguments":{"input":"..."}}}
"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "apply_patch",
            "session_id": "session-1",
            "tool_use_id": "call_exact",
            "tool_input": "...",
            "tool_response": "",
            "transcript_path": transcript_path.to_string_lossy()
        })
        .to_string();

        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();

        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![cwd.join("src/main.rs")]);
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::CopilotEventStreamJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_copilot_native_prefers_chat_session_path_for_model() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let transcript_dir = temp
            .path()
            .join("workspaceStorage")
            .join("abc")
            .join("GitHub.copilot-chat")
            .join("transcripts");
        std::fs::create_dir_all(&transcript_dir).unwrap();

        let transcript_path = transcript_dir.join("session.jsonl");
        let transcript = r#"{"type":"assistant.message","data":{"toolRequests":[{"toolCallId":"call_exact","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n+fn main() {}\\n*** End Patch\"}"}]}}
{"type":"tool.execution_start","data":{"toolCallId":"call_exact","toolName":"apply_patch","arguments":{"input":"..."}}}
"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let chat_session_path = temp.path().join("session.json");
        std::fs::write(
            &chat_session_path,
            r#"{"inputState":{"selectedModel":{"identifier":"copilot/gpt-5.4"}},"requests":[]}"#,
        )
        .unwrap();

        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "apply_patch",
            "session_id": "session-1",
            "tool_use_id": "call_exact",
            "tool_input": "...",
            "tool_response": "",
            "transcript_path": transcript_path.to_string_lossy(),
            "chat_session_path": chat_session_path.to_string_lossy()
        })
        .to_string();

        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();

        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "copilot/gpt-5.4");
                assert_eq!(e.file_paths, vec![cwd.join("src/main.rs")]);
                assert_eq!(
                    e.context.metadata.get("chat_session_path").map(String::as_str),
                    Some(chat_session_path.to_string_lossy().as_ref())
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::CopilotEventStreamJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_copilot_native_transcript_fallback_accepts_vscode_suffix_tool_use_id() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let transcript_dir = temp
            .path()
            .join("workspaceStorage")
            .join("abc")
            .join("GitHub.copilot-chat")
            .join("transcripts");
        std::fs::create_dir_all(&transcript_dir).unwrap();

        let transcript_path = transcript_dir.join("session.jsonl");
        let transcript = r#"{"type":"assistant.message","data":{"toolRequests":[{"toolCallId":"call_exact","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n+fn main() {}\\n*** End Patch\"}"}]}}
{"type":"tool.execution_start","data":{"toolCallId":"call_exact","toolName":"apply_patch","arguments":{"input":"..."}}}
"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "apply_patch",
            "session_id": "session-1",
            "tool_use_id": "call_exact__vscode-1777821655374",
            "tool_input": "...",
            "tool_response": "",
            "transcript_path": transcript_path.to_string_lossy()
        })
        .to_string();

        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();

        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![cwd.join("src/main.rs")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_copilot_native_transcript_fallback_ignores_other_tool_calls() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let transcript_dir = temp
            .path()
            .join("workspaceStorage")
            .join("abc")
            .join("GitHub.copilot-chat")
            .join("transcripts");
        std::fs::create_dir_all(&transcript_dir).unwrap();

        let transcript_path = transcript_dir.join("session.jsonl");
        let transcript = r#"{"type":"assistant.message","data":{"toolRequests":[{"toolCallId":"call_other","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/other.rs\\n+fn other() {}\\n*** End Patch\"}"}]}}
"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "apply_patch",
            "session_id": "session-1",
            "tool_use_id": "call_exact",
            "tool_input": "...",
            "tool_response": "",
            "transcript_path": transcript_path.to_string_lossy()
        })
        .to_string();

        let result = GithubCopilotPreset.parse(&input, "t_test123456789a");
        assert!(result.is_err());

        let Err(GitAiError::PresetError(message)) = result else {
            panic!("Expected PresetError when no exact tool-call paths are found");
        };
        assert!(message.contains("No editable file paths found"));
    }

    #[test]
    fn test_copilot_native_filters_tool_payload_with_vscode_suffix() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/home/user/project",
            "tool_name": "replace_string_in_file",
            "session_id": "sess-456",
            "tool_use_id": "tu-1",
            "tool_input": {
                "toolCallId": "tu-1__vscode-123",
                "name": "replace_string_in_file",
                "arguments": {"file_path": "src/main.rs"}
            },
            "transcript_path": "/home/user/.vscode/data/github.copilot-chat/transcripts/sess-456.json"
        })
        .to_string();

        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_copilot_native_derives_chat_session_path_for_model() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();

        let workspace_storage = temp.path().join("workspaceStorage").join("abc");
        let transcript_dir = workspace_storage.join("GitHub.copilot-chat").join("transcripts");
        std::fs::create_dir_all(&transcript_dir).unwrap();

        let transcript_path = transcript_dir.join("session.jsonl");
        let transcript = r#"{"type":"assistant.message","data":{"toolRequests":[{"toolCallId":"call_exact","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n+fn main() {}\\n*** End Patch\"}"}]}}
    {"type":"tool.execution_start","data":{"toolCallId":"call_exact","toolName":"apply_patch","arguments":{"input":"..."}}}
    "#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let chat_session_dir = workspace_storage.join("chatSessions");
        std::fs::create_dir_all(&chat_session_dir).unwrap();
        let chat_session_path = chat_session_dir.join("session-1.jsonl");
        std::fs::write(
            &chat_session_path,
            r#"{"kind":0,"v":{"inputState":{"selectedModel":{"identifier":"copilot/gpt-5.4-mini"}},"requests":[{"modelId":"copilot/gpt-5.4-mini"}]}}"#,
        )
        .unwrap();

        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "apply_patch",
            "session_id": "session-1",
            "tool_use_id": "call_exact",
            "tool_input": "...",
            "tool_response": "",
            "transcript_path": transcript_path.to_string_lossy()
        })
        .to_string();

        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();

        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "copilot/gpt-5.4-mini");
                assert_eq!(
                    e.context.metadata.get("chat_session_path").map(String::as_str),
                    Some(chat_session_path.to_string_lossy().as_ref())
                );
                assert_eq!(e.file_paths, vec![cwd.join("src/main.rs")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_copilot_camel_case_keys() {
        let input = json!({
            "hookEventName": "before_edit",
            "workspaceFolder": "/home/user/project",
            "will_edit_filepaths": ["/home/user/project/src/main.rs"],
            "chatSessionId": "sess-789"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.external_session_id, "sess-789");
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_copilot_default_after_edit_when_no_hook_event_name() {
        // When hook_event_name is missing, defaults to "after_edit"
        let input = json!({
            "workspace_folder": "/home/user/project",
            "chat_session_path": "/home/user/.vscode/sessions/sess-123.json",
            "session_id": "sess-123",
            "edited_filepaths": ["src/main.rs"]
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ParsedHookEvent::PostFileEdit(_)));
    }

    #[test]
    fn test_copilot_native_workspace_storage_format() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "cwd": "/home/user/project",
            "tool_name": "create_file",
            "session_id": "sess-456",
            "tool_input": {"file_path": "/home/user/project/src/new.rs"},
            "transcript_path": "/home/user/.vscode/data/workspaceStorage/abc/chatSessions/sess-456.json"
        })
        .to_string();
        let events = GithubCopilotPreset
            .parse(&input, "t_test123456789a")
            .unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::CopilotEventStreamJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
