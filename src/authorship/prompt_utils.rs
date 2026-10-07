use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::transcript::{AiTranscript, Message};
use crate::error::GitAiError;
use crate::git::notes_api::{read_authorship as get_authorship, search_notes as grep_ai_notes};
use crate::git::repository::Repository;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

const MAX_TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TRANSCRIPT_MESSAGES: usize = 256;
const MAX_PROMPT_TEXT_BYTES: usize = 256 * 1024;

/// Copilot records asynchronous terminal output as `user.message` too. This
/// exact provider envelope is generated context, not a human prompt. Keep this
/// guard at upload time as well, since existing notes predate parser fixes.
pub(crate) fn is_generated_user_text(text: &str) -> bool {
    static TERMINAL_NOTIFICATION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    TERMINAL_NOTIFICATION
        .get_or_init(|| {
            regex::Regex::new(
                r"\A\[Terminal [0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12} notification: command completed(?: with exit code -?\d+)?\. The terminal has been cleaned up\.\]\r?\nTerminal output:\r?\n",
            )
            .expect("valid terminal notification pattern")
        })
        .is_match(text)
}

pub enum PromptUpdateResult {
    Updated(AiTranscript, String),
    Unchanged,
    Failed(GitAiError),
}

pub fn update_prompt_from_tool(
    tool: &str,
    _external_thread_id: &str,
    agent_metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    let Some(metadata) = agent_metadata else {
        return PromptUpdateResult::Unchanged;
    };

    let candidate_paths = candidate_transcript_paths(tool, metadata);

    if candidate_paths.is_empty() {
        return PromptUpdateResult::Unchanged;
    }

    let mut first_error = None;

    for path in candidate_paths {
        let transcript_path = Path::new(path);

        match transcript_from_json_or_jsonl_for_tool(tool, transcript_path) {
            Ok(transcript) if !transcript.messages().is_empty() => {
                let latest_model =
                    latest_model_from_transcript(tool, transcript_path, current_model);
                return PromptUpdateResult::Updated(transcript, latest_model);
            }
            Ok(_) => {}
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    match first_error {
        Some(error) => PromptUpdateResult::Failed(error),
        None => PromptUpdateResult::Unchanged,
    }
}

fn candidate_transcript_paths<'a>(
    tool: &str,
    metadata: &'a HashMap<String, String>,
) -> Vec<&'a str> {
    let mut paths = Vec::new();

    let mut push_path = |path: Option<&'a String>| {
        if let Some(path) = path
            .map(String::as_str)
            .filter(|path| !path.trim().is_empty())
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    };

    match tool {
        "github-copilot" => {
            push_path(metadata.get("transcript_path"));
            push_path(metadata.get("chat_session_path"));
        }
        "pi" => push_path(metadata.get("session_path")),
        _ => push_path(metadata.get("transcript_path")),
    }

    paths
}

fn latest_model_from_transcript(tool: &str, transcript_path: &Path, current_model: &str) -> String {
    let inferred_format = match tool {
        "github-copilot" => {
            if transcript_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            {
                crate::streams::sweep::StreamFormat::CopilotEventStreamJsonl
            } else {
                crate::streams::sweep::StreamFormat::CopilotSessionJson
            }
        }
        _ => return current_model.to_string(),
    };

    crate::streams::model_extraction::extract_model(transcript_path, inferred_format, None)
        .ok()
        .flatten()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| current_model.to_string())
}

fn transcript_from_json_or_jsonl_for_tool(
    tool: &str,
    path: &Path,
) -> Result<AiTranscript, GitAiError> {
    if let Ok(metadata) = std::fs::metadata(path)
        && metadata.len() > MAX_TRANSCRIPT_BYTES
    {
        crate::diagnostics::append_debug_event(
            "prompt_transcript_skipped",
            serde_json::json!({
                "tool": tool,
                "path": path.to_string_lossy().replace('\\', "/"),
                "reason": "transcript_too_large",
                "bytes": metadata.len(),
                "byteLimit": MAX_TRANSCRIPT_BYTES,
            }),
        );
        return Ok(AiTranscript::new());
    }
    let content = std::fs::read_to_string(path)?;
    let mut transcript = AiTranscript::new();

    if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        for line in content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let value: Value = serde_json::from_str(line)?;
            collect_messages_from_value_for_tool(&value, &mut transcript, Some(tool));
        }
    } else {
        let value: Value = serde_json::from_str(&content)?;
        collect_messages_from_value_for_tool(&value, &mut transcript, Some(tool));
    }

    Ok(transcript)
}

fn collect_messages_from_value_for_tool(
    value: &Value,
    transcript: &mut AiTranscript,
    tool: Option<&str>,
) {
    if transcript.messages().len() >= MAX_TRANSCRIPT_MESSAGES {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_messages_from_value_for_tool(item, transcript, tool);
            }
        }
        Value::Object(object) => {
            // Message bodies are opaque: never discover a nested user role in
            // assistant output, a tool result, or an unrecognised message.
            if object.get("role").is_some()
                || object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            "tool_result" | "tool_use" | "function_call_output" | "function_call"
                        )
                    })
            {
                if let Some(message) = message_from_object_for_tool(object, tool)
                    && message
                        .text()
                        .is_none_or(|text| text.len() <= MAX_PROMPT_TEXT_BYTES)
                {
                    transcript.add_message(message);
                }
                return;
            }
            if let Some(message) = message_from_object_for_tool(object, tool) {
                if message
                    .text()
                    .is_none_or(|text| text.len() <= MAX_PROMPT_TEXT_BYTES)
                {
                    transcript.add_message(message);
                }
                return;
            }

            if is_claude_tool_result(object)
                || object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind.split('.').next().unwrap_or(kind),
                            "user" | "assistant" | "human" | "ai"
                        )
                    })
            {
                return;
            }

            // Traverse transcript containers, not arbitrary response payloads.
            for key in ["messages", "events", "requests", "payload", "message"] {
                let Some(nested) = object.get(key) else {
                    continue;
                };
                collect_messages_from_value_for_tool(nested, transcript, tool);
            }
        }
        _ => {}
    }
}

fn message_from_object_for_tool(
    object: &serde_json::Map<String, Value>,
    _tool: Option<&str>,
) -> Option<Message> {
    // A user message may contain text and a tool result together. Extract only
    // its text blocks rather than discarding the genuine user text as well.
    message_from_object(object)
}

fn is_claude_tool_result(object: &serde_json::Map<String, Value>) -> bool {
    object.get("type").and_then(Value::as_str) == Some("user")
        && object
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            })
}

fn message_from_object(object: &serde_json::Map<String, Value>) -> Option<Message> {
    let role = object
        .get("role")
        .or_else(|| object.get("type"))
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    let normalized_role = role.split('.').next().unwrap_or(role.as_str());

    let text = object
        .get("text")
        .or_else(|| object.get("content"))
        .or_else(|| object.get("message"))
        .or_else(|| object.get("data"))
        .and_then(|value| {
            text_from_value(
                value,
                matches!(normalized_role, "user" | "human" | "user_message"),
            )
        })?;

    let timestamp = object
        .get("timestamp")
        .or_else(|| object.get("created_at"))
        .or_else(|| object.get("createdAt"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    match normalized_role {
        "user" | "human" | "user_message" if !is_generated_user_text(&text) => {
            Some(Message::user(text, timestamp))
        }
        "assistant" | "ai" | "agent_message" => Some(Message::assistant(text, timestamp)),
        "thinking" => Some(Message::thinking(text, timestamp)),
        "plan" => Some(Message::plan(text, timestamp)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn copilot_terminal_notifications_are_not_human_prompts() {
        let notification = "[Terminal 092bcfda-91c1-49f2-962c-e69922f24cc2 notification: command completed. The terminal has been cleaned up.]\nTerminal output:\nAI generated text";
        let failed =
            notification.replace("command completed.", "command completed with exit code 1.");
        let value = serde_json::json!({"events": [
            {"type": "user.message", "data": {"content": notification}},
            {"type": "user.message", "data": {"content": failed}},
            {"type": "user.message", "data": {"content": "请修复终端错误"}},
            {"type": "user.message", "data": {"content": format!("解释这段日志：\n{notification}")}}
        ]});
        let mut transcript = AiTranscript::new();
        collect_messages_from_value_for_tool(&value, &mut transcript, Some("github-copilot"));
        assert_eq!(transcript.messages().len(), 2);
        assert_eq!(transcript.messages()[0].text().unwrap(), "请修复终端错误");
        assert!(!is_generated_user_text(
            "[Terminal test notification: command completed.]"
        ));
    }

    #[test]
    fn nested_output_roles_and_mixed_tool_blocks_are_not_user_input() {
        let value = serde_json::json!({"messages": [
            {"role": "assistant", "content": {"role": "user", "text": "AI output"}},
            {"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "content": {"role": "user", "text": "tool output"}}
            ]}},
            {"role": "user", "content": [
                {"type": "input_text", "text": "real input"},
                {"role": "assistant", "text": "nested AI reply"},
                {"type": "output_text", "text": "output block"},
                {"type": "tool_result", "content": "tool output"}
            ]},
            {"type": "user", "message": {"role": "user", "content": [
                {"type": "text", "text": "Claude input"},
                {"type": "tool_result", "content": "tool output"}
            ]}},
            {"type": "event_msg", "payload": {"type": "user_message", "message": "Codex input"}}
        ]});
        let mut transcript = AiTranscript::new();
        collect_messages_from_value_for_tool(&value, &mut transcript, Some("claude"));
        let users: Vec<_> = transcript
            .messages()
            .iter()
            .filter_map(|m| match m {
                Message::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users, ["real input", "Claude input", "Codex input"]);
    }

    #[test]
    fn transcript_from_jsonl_parses_copilot_event_stream_messages() {
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            file,
            r#"{{"type":"user.message","data":{{"content":"write tests"}},"timestamp":"2026-05-09T10:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant.message","data":{{"content":"I will update the test file.","modelId":"copilot/gpt-5.4"}},"timestamp":"2026-05-09T10:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let transcript =
            transcript_from_json_or_jsonl_for_tool("github-copilot", file.path()).unwrap();
        let messages = transcript.messages();

        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::User { text, .. } if text == "write tests"));
        assert!(
            matches!(&messages[1], Message::Assistant { text, .. } if text == "I will update the test file.")
        );
    }

    #[test]
    fn update_prompt_from_tool_refreshes_copilot_model_from_event_stream() {
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            file,
            r#"{{"type":"user.message","data":{{"content":"add tests"}},"timestamp":"2026-05-09T10:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant.message","data":{{"content":"Done","modelId":"copilot/gpt-5.4"}},"timestamp":"2026-05-09T10:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let metadata = HashMap::from([(
            "transcript_path".to_string(),
            file.path().to_string_lossy().to_string(),
        )]);

        let result =
            update_prompt_from_tool("github-copilot", "session-1", Some(&metadata), "unknown");

        match result {
            PromptUpdateResult::Updated(transcript, model) => {
                assert_eq!(model, "copilot/gpt-5.4");
                assert_eq!(transcript.messages().len(), 2);
            }
            PromptUpdateResult::Unchanged => panic!("expected Updated result"),
            PromptUpdateResult::Failed(error) => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn update_prompt_from_tool_prefers_copilot_transcript_path_over_chat_session_path() {
        let mut chat_session = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            chat_session,
            r#"{{"type":"assistant.message","data":{{"content":"session-state assistant message","modelId":"copilot/gpt-5-mini"}},"timestamp":"2026-05-09T10:00:00Z"}}"#
        )
        .unwrap();
        chat_session.flush().unwrap();

        let mut transcript = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            transcript,
            r#"{{"type":"user.message","data":{{"content":"capture this prompt"}},"timestamp":"2026-05-09T10:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            transcript,
            r#"{{"type":"assistant.message","data":{{"content":"Captured","modelId":"copilot/gpt-5.4"}},"timestamp":"2026-05-09T10:00:01Z"}}"#
        )
        .unwrap();
        transcript.flush().unwrap();

        let metadata = HashMap::from([
            (
                "chat_session_path".to_string(),
                chat_session.path().to_string_lossy().to_string(),
            ),
            (
                "transcript_path".to_string(),
                transcript.path().to_string_lossy().to_string(),
            ),
        ]);

        let result =
            update_prompt_from_tool("github-copilot", "session-1", Some(&metadata), "unknown");

        match result {
            PromptUpdateResult::Updated(transcript, model) => {
                assert_eq!(model, "copilot/gpt-5.4");
                assert_eq!(transcript.messages().len(), 2);
                assert!(matches!(
                    &transcript.messages()[0],
                    Message::User { text, .. } if text == "capture this prompt"
                ));
            }
            PromptUpdateResult::Unchanged => panic!("expected Updated result"),
            PromptUpdateResult::Failed(error) => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn claude_tool_results_are_not_recorded_as_user_prompts() {
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_1","content":"tool output"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":[{{"type":"text","text":"fix the parser"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let transcript = transcript_from_json_or_jsonl_for_tool("claude", file.path()).unwrap();
        assert_eq!(transcript.messages().len(), 1);
        assert!(matches!(
            &transcript.messages()[0],
            Message::User { text, .. } if text == "fix the parser"
        ));
    }

    #[test]
    fn generic_parser_keeps_user_events_with_nested_content() {
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(
            file,
            r#"{{"type":"user.message","data":{{"content":"write tests"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let transcript =
            transcript_from_json_or_jsonl_for_tool("github-copilot", file.path()).unwrap();
        assert_eq!(transcript.messages().len(), 1);
    }

    #[test]
    fn oversized_transcript_is_skipped_before_reading_contents() {
        let file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        file.as_file().set_len(MAX_TRANSCRIPT_BYTES + 1).unwrap();

        let transcript = transcript_from_json_or_jsonl_for_tool("claude", file.path()).unwrap();
        assert!(transcript.messages().is_empty());
    }

    #[test]
    fn transcript_message_count_is_bounded() {
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        for index in 0..(MAX_TRANSCRIPT_MESSAGES + 20) {
            writeln!(
                file,
                r#"{{"type":"user.message","data":{{"content":"prompt-{index}"}}}}"#
            )
            .unwrap();
        }
        file.flush().unwrap();

        let transcript =
            transcript_from_json_or_jsonl_for_tool("github-copilot", file.path()).unwrap();
        assert_eq!(transcript.messages().len(), MAX_TRANSCRIPT_MESSAGES);
    }

    #[test]
    fn oversized_prompt_message_is_not_recorded() {
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        let oversized = "x".repeat(MAX_PROMPT_TEXT_BYTES + 1);
        writeln!(
            file,
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}",
            oversized
        )
        .unwrap();
        file.flush().unwrap();

        let transcript = transcript_from_json_or_jsonl_for_tool("claude", file.path()).unwrap();
        assert!(transcript.messages().is_empty());
    }
}

fn text_from_value(value: &Value, user_only: bool) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|value| text_from_value(value, user_only))
                .filter(|part| !part.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => {
            if user_only
                && object
                    .get("role")
                    .and_then(Value::as_str)
                    .is_some_and(|role| !matches!(role, "user" | "human"))
            {
                return None;
            }
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    !matches!(kind, "text" | "input_text") && (user_only || kind != "output_text")
                })
            {
                return None;
            }
            object
                .get("text")
                .or_else(|| object.get("content"))
                .and_then(|value| text_from_value(value, user_only))
        }
        _ => None,
    }
}

/// Find a prompt in the repository history
///
/// If `commit` is provided, look only in that specific commit.
/// Otherwise, search through history and skip `offset` occurrences (0 = most recent).
pub fn find_prompt(
    repo: &Repository,
    prompt_id: &str,
    commit: Option<&str>,
    offset: usize,
) -> Result<(String, PromptRecord), GitAiError> {
    if let Some(commit_rev) = commit {
        // Look in specific commit
        find_prompt_in_commit(repo, prompt_id, commit_rev)
    } else {
        // Search through history with offset
        find_prompt_in_history(repo, prompt_id, offset)
    }
}

/// Find a prompt in a specific commit (searches both prompts and sessions)
pub fn find_prompt_in_commit(
    repo: &Repository,
    prompt_id: &str,
    commit_rev: &str,
) -> Result<(String, PromptRecord), GitAiError> {
    // Resolve the revision to a commit SHA
    let commit = repo.revparse_single(commit_rev)?;
    let commit_sha = commit.id();

    // Get the authorship log for this commit
    let authorship_log = get_authorship(repo, &commit_sha).ok_or_else(|| {
        GitAiError::Generic(format!(
            "No authorship data found for commit: {}",
            commit_rev
        ))
    })?;

    // Look for the prompt in the prompts map first
    if let Some(prompt) = authorship_log.metadata.prompts.get(prompt_id) {
        return Ok((commit_sha, prompt.clone()));
    }

    // Fall back to sessions map (session IDs start with "s_")
    // Strip ::t_ trace suffix if present — attestation hashes use s_xxx::t_yyy but session keys are just s_xxx
    let session_key = if prompt_id.starts_with("s_") {
        prompt_id.split("::").next().unwrap_or(prompt_id)
    } else {
        prompt_id
    };
    if let Some(session) = authorship_log.metadata.sessions.get(session_key) {
        return Ok((commit_sha, session.to_prompt_record()));
    }

    Err(GitAiError::Generic(format!(
        "Prompt '{}' not found in commit {}",
        prompt_id, commit_rev
    )))
}

/// Find a prompt in history, skipping `offset` occurrences
/// Returns the (N+1)th occurrence where N = offset (0 = most recent)
pub fn find_prompt_in_history(
    repo: &Repository,
    prompt_id: &str,
    offset: usize,
) -> Result<(String, PromptRecord), GitAiError> {
    // Strip ::t_ trace suffix for session lookups — attestation hashes use s_xxx::t_yyy
    // but session keys in metadata are just s_xxx
    let session_key = if prompt_id.starts_with("s_") {
        prompt_id.split("::").next().unwrap_or(prompt_id)
    } else {
        prompt_id
    };

    // Use git grep to search for the prompt ID in authorship notes
    // grep_ai_notes returns commits sorted by date (newest first)
    let shas = grep_ai_notes(repo, &format!("\"{}\"", session_key)).unwrap_or_default();

    if shas.is_empty() {
        return Err(GitAiError::Generic(format!(
            "Prompt not found in history: {}",
            prompt_id
        )));
    }

    // Iterate through commits, looking for the prompt and counting occurrences
    let mut found_count = 0;
    for sha in &shas {
        if let Some(authorship_log) = get_authorship(repo, sha) {
            // Check prompts map first
            if let Some(prompt) = authorship_log.metadata.prompts.get(prompt_id) {
                if found_count == offset {
                    return Ok((sha.clone(), prompt.clone()));
                }
                found_count += 1;
            // Then check sessions map
            } else if let Some(session) = authorship_log.metadata.sessions.get(session_key) {
                if found_count == offset {
                    return Ok((sha.clone(), session.to_prompt_record()));
                }
                found_count += 1;
            }
        }
    }

    // If we get here, we didn't find enough occurrences
    if found_count == 0 {
        Err(GitAiError::Generic(format!(
            "Prompt not found in history: {}",
            prompt_id
        )))
    } else {
        Err(GitAiError::Generic(format!(
            "Prompt '{}' found {} time(s), but offset {} requested (max offset: {})",
            prompt_id,
            found_count,
            offset,
            found_count - 1
        )))
    }
}
