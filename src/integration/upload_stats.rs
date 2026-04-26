//! Auto-upload of git-ai authorship statistics to a team-managed remote endpoint.
//!
//! This module mirrors the behavior of the `upload-ai-stats.ps1` script that
//! previously lived in spec-kit. It assembles a single-commit batch payload
//! (matching the spec-kit schema exactly) and POSTs it to the configured
//! remote endpoint right after `post_commit` writes the authorship note.
//!
//! Configuration (all via environment variables, no `git-ai config` keys):
//!   * `GIT_AI_REPORT_REMOTE_URL`        - full upload URL (preferred)
//!   * `GIT_AI_REPORT_REMOTE_ENDPOINT`   - base host (combined with PATH)
//!   * `GIT_AI_REPORT_REMOTE_PATH`       - path appended to ENDPOINT
//!   * `GIT_AI_REPORT_REMOTE_API_KEY`    - bearer token (Authorization header)
//!   * `GIT_AI_REPORT_REMOTE_USER_ID`    - X-USER-ID header (falls back to IDE MCP config)
//!
//! Uploads are best-effort: failures are logged and never propagated to the
//! caller, so a network outage cannot break a `git commit`.

use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::authorship::authorship_log_serialization::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};
use crate::authorship::stats::CommitStats;
use crate::authorship::transcript::Message;
use crate::git::repository::Repository;
use crate::http;
use crate::integration::ide_mcp::resolve_x_user_id;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const DEFAULT_UPLOAD_URL: &str =
    "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats";
const UPLOAD_TIMEOUT_SECS: u64 = 10;

/// Resolve the upload endpoint URL from the environment, falling back to the
/// team-managed default when no override is provided.
fn resolve_upload_url() -> String {
    if let Some(url) = env_non_empty("GIT_AI_REPORT_REMOTE_URL") {
        return url;
    }

    let endpoint = env_non_empty("GIT_AI_REPORT_REMOTE_ENDPOINT");
    let path = env_non_empty("GIT_AI_REPORT_REMOTE_PATH");
    if let (Some(endpoint), Some(path)) = (endpoint.as_ref(), path.as_ref()) {
        let endpoint = endpoint.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        return format!("{}/{}", endpoint, path);
    }

    DEFAULT_UPLOAD_URL.to_string()
}

fn env_non_empty(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Public entry point invoked from `post_commit` once the authorship note and
/// (optional) `CommitStats` are available. Always returns silently so the
/// commit hook is never disrupted.
pub fn maybe_upload_after_commit(
    repo: &Repository,
    commit_sha: &str,
    authorship_log: &AuthorshipLog,
    stats: Option<&CommitStats>,
) {
    let config = crate::config::Config::fresh();
    let feature_flags = config.feature_flags();

    if !feature_flags.auto_upload_ai_stats {
        log_debug(&format!(
            "feature flag auto_upload_ai_stats disabled; skipping upload for {}",
            short_sha(commit_sha)
        ));
        return;
    }

    // Stats are only present when the post-commit fast path computed them.
    // Without stats we have nothing meaningful to upload (the PowerShell
    // script effectively did the same thing by calling `git-ai stats`).
    let Some(stats) = stats else {
        log_debug("stats unavailable for this commit; skipping upload");
        return;
    };

    let payload = match build_payload(repo, commit_sha, authorship_log, stats) {
        Ok(payload) => payload,
        Err(err) => {
            log_debug(&format!(
                "payload build failed for {}: {}",
                short_sha(commit_sha),
                err
            ));
            return;
        }
    };

    let url = resolve_upload_url();
    let api_key = env_non_empty("GIT_AI_REPORT_REMOTE_API_KEY");
    let user_id = env_non_empty("GIT_AI_REPORT_REMOTE_USER_ID")
        .or_else(|| resolve_x_user_id(Some(repo.canonical_workdir())));
    let commit_short = short_sha(commit_sha).to_string();
    dispatch_upload(
        feature_flags.async_mode,
        url,
        payload,
        api_key,
        user_id,
        commit_short,
    );
}

fn dispatch_upload(
    run_in_background: bool,
    url: String,
    payload: Value,
    api_key: Option<String>,
    user_id: Option<String>,
    commit_short: String,
) {
    let upload_mode = if run_in_background {
        "background"
    } else {
        "inline"
    };

    run_upload_task(run_in_background, move || {
        log_info(&format!(
            "starting upload for {} mode={} url={} has_api_key={} has_user_id={}",
            commit_short,
            upload_mode,
            url,
            api_key.is_some(),
            user_id.is_some()
        ));
        if let Err(err) = perform_upload(&url, &payload, api_key.as_deref(), user_id.as_deref()) {
            log_warn(&format!("upload failed for {}: {}", commit_short, err));
        } else {
            log_info(&format!("uploaded stats for {}", commit_short));
        }
    });
}

// Inline execution is required in the sync wrapper path because git-ai exits
// immediately after post-commit hooks finish. The daemon-backed async path can
// safely keep uploads fire-and-forget because the daemon process stays alive.
fn run_upload_task<F>(run_in_background: bool, task: F)
where
    F: FnOnce() + Send + 'static,
{
    if run_in_background {
        let _ = std::thread::spawn(task);
    } else {
        task();
    }
}

fn log_debug(message: &str) {
    tracing::debug!(target: "git_ai::upload_stats", "{}", message);
    emit_debug_stderr(message);
}

fn log_info(message: &str) {
    tracing::info!(target: "git_ai::upload_stats", "{}", message);
    emit_debug_stderr(message);
}

fn log_warn(message: &str) {
    tracing::warn!(target: "git_ai::upload_stats", "{}", message);
    emit_debug_stderr(message);
}

fn emit_debug_stderr(message: &str) {
    if cfg!(debug_assertions) || std::env::var("GIT_AI_DEBUG").is_ok() {
        eprintln!("[git-ai] upload-ai-stats: {}", message);
    }
}

fn perform_upload(
    url: &str,
    payload: &Value,
    api_key: Option<&str>,
    user_id: Option<&str>,
) -> Result<(), String> {
    log_debug(&format!(
        "perform_upload url={} has_api_key={} has_user_id={}",
        url,
        api_key.is_some(),
        user_id.is_some()
    ));

    let agent = http::build_agent(Some(UPLOAD_TIMEOUT_SECS));
    let mut request = agent.post(url).set("Content-Type", "application/json");
    if let Some(key) = api_key {
        request = request.set("Authorization", &format!("Bearer {}", key));
    }
    if let Some(id) = user_id {
        request = request.set("X-USER-ID", id);
    }

    let body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    let response = http::send_with_body(request, &body)?;
    log_debug(&format!(
        "perform_upload response status={} url={}",
        response.status_code, url
    ));
    if (200..300).contains(&response.status_code) {
        Ok(())
    } else {
        let body_excerpt = response
            .as_str()
            .map(|s| s.chars().take(200).collect::<String>())
            .unwrap_or_default();
        Err(format!("HTTP {}: {}", response.status_code, body_excerpt))
    }
}

/// Build the batch JSON payload for a single commit (matches the spec-kit
/// `upload-ai-stats.ps1` schema 1:1).
fn build_payload(
    repo: &Repository,
    commit_sha: &str,
    authorship_log: &AuthorshipLog,
    stats: &CommitStats,
) -> Result<Value, String> {
    let workdir = repo.canonical_workdir().to_path_buf();
    let repo_url = git_repo_url(&workdir);
    let project_name = derive_project_name(repo_url.as_deref(), &workdir);
    let branch = git_current_branch(&workdir);
    let (commit_message, commit_author, commit_timestamp) =
        git_commit_metadata(&workdir, commit_sha)
            .ok_or_else(|| "failed to read commit metadata".to_string())?;

    let file_stats = build_file_stats(&workdir, commit_sha, authorship_log);
    let stats_json = stats_to_camel_case(stats, file_stats);
    let prompt_stats = build_prompt_stats(&authorship_log.metadata.prompts);

    let mut commit_entry = Map::new();
    commit_entry.insert(
        "commitSha".to_string(),
        Value::String(commit_sha.to_string()),
    );
    commit_entry.insert("commitMessage".to_string(), Value::String(commit_message));
    commit_entry.insert("author".to_string(), Value::String(commit_author));
    commit_entry.insert("timestamp".to_string(), Value::String(commit_timestamp));
    commit_entry.insert("hasAuthorshipNote".to_string(), Value::Bool(true));
    commit_entry.insert("stats".to_string(), stats_json);
    commit_entry.insert("prompts".to_string(), Value::Array(prompt_stats));

    let payload = json!({
        "repoUrl": repo_url.unwrap_or_default(),
        "projectName": project_name,
        "branch": branch.unwrap_or_default(),
        "source": "auto",
        "reviewDocumentId": Value::Null,
        "authorshipSchemaVersion": AUTHORSHIP_LOG_VERSION,
        "commits": [Value::Object(commit_entry)],
    });

    Ok(payload)
}

fn stats_to_camel_case(stats: &CommitStats, files: Vec<Value>) -> Value {
    let mut tool_breakdown = Vec::new();
    for (key, value) in &stats.tool_model_breakdown {
        let (split_tool, split_model) = split_tool_model(key);
        let (tool, model) = normalize_tool_model(Some(split_tool.as_str()), split_model.as_deref());
        tool_breakdown.push(json!({
            "tool": tool,
            "model": model,
            "aiAdditions": value.ai_additions,
            "aiAccepted": value.ai_accepted,
            "mixedAdditions": value.mixed_additions,
            "totalAiAdditions": value.total_ai_additions,
            "totalAiDeletions": value.total_ai_deletions,
            "timeWaitingForAi": value.time_waiting_for_ai,
        }));
    }

    json!({
        "humanAdditions": stats.human_additions,
        "unknownAdditions": stats.unknown_additions,
        "mixedAdditions": stats.mixed_additions,
        "aiAdditions": stats.ai_additions,
        "aiAccepted": stats.ai_accepted,
        "totalAiAdditions": stats.total_ai_additions,
        "totalAiDeletions": stats.total_ai_deletions,
        "gitDiffAddedLines": stats.git_diff_added_lines,
        "gitDiffDeletedLines": stats.git_diff_deleted_lines,
        "timeWaitingForAi": stats.time_waiting_for_ai,
        "files": files,
        "toolModelBreakdown": tool_breakdown,
    })
}

fn build_prompt_stats(prompts: &BTreeMap<String, PromptRecord>) -> Vec<Value> {
    prompts
        .iter()
        .map(|(prompt_hash, prompt)| {
            let (tool, model) = normalize_tool_model(
                Some(prompt.agent_id.tool.as_str()),
                Some(prompt.agent_id.model.as_str()),
            );
            json!({
                "promptHash": prompt_hash,
                "tool": tool,
                "model": model,
                "humanAuthor": prompt.human_author.as_ref().and_then(|value| trim_non_empty(value)),
                "promptText": extract_prompt_text(&prompt.messages),
                "messages": serialize_prompt_messages(&prompt.messages),
                "messagesUrl": prompt.messages_url.as_ref().and_then(|value| trim_non_empty(value)),
                "totalAdditions": prompt.total_additions,
                "totalDeletions": prompt.total_deletions,
                "acceptedLines": prompt.accepted_lines,
                "overridenLines": prompt.overriden_lines,
                "customAttributes": prompt.custom_attributes,
            })
        })
        .collect()
}

fn extract_prompt_text(messages: &[Message]) -> Option<String> {
    let prompt_parts = messages
        .iter()
        .filter_map(|message| match message {
            Message::User { text, .. } => trim_non_empty(text),
            _ => None,
        })
        .collect::<Vec<_>>();

    if prompt_parts.is_empty() {
        None
    } else {
        Some(prompt_parts.join("\n\n"))
    }
}

fn serialize_prompt_messages(messages: &[Message]) -> Value {
    serde_json::to_value(messages).unwrap_or_else(|_| Value::Array(Vec::new()))
}

fn trim_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn tool_family_key(value: &str) -> Option<String> {
    trim_non_empty(value).map(|trimmed| {
        let normalized = trimmed
            .chars()
            .map(|ch| match ch {
                ' ' | '_' => '-',
                _ => ch.to_ascii_lowercase(),
            })
            .collect::<String>();

        match normalized.as_str() {
            "copilot" | "github-copilot" => "github-copilot".to_string(),
            _ => normalized,
        }
    })
}

fn same_tool_family(left: &str, right: &str) -> bool {
    matches!((tool_family_key(left), tool_family_key(right)), (Some(l), Some(r)) if l == r)
}

fn normalize_tool_model(tool: Option<&str>, model: Option<&str>) -> (String, Option<String>) {
    let mut normalized_tool = tool.and_then(trim_non_empty);
    let mut normalized_model = model.and_then(trim_non_empty);

    if let Some(tool_text) = normalized_tool.clone() {
        let (split_tool, split_model) = split_tool_model(&tool_text);
        if split_model.is_some() && normalized_model.is_none() {
            normalized_tool = Some(split_tool);
            normalized_model = split_model;
        }
    }

    if let Some(model_text) = normalized_model.clone() {
        let (model_tool, model_tail) = split_tool_model(&model_text);
        if let Some(model_value) = model_tail {
            let should_strip_prefix = normalized_tool
                .as_deref()
                .map(|tool_value| {
                    model_text.eq_ignore_ascii_case(&format!("{}::{}", tool_value, model_value))
                        || model_text.eq_ignore_ascii_case(&format!("{}/{}", tool_value, model_value))
                        || same_tool_family(tool_value, &model_tool)
                })
                .unwrap_or(true);

            if should_strip_prefix {
                if normalized_tool
                    .as_deref()
                    .map(|value| value.eq_ignore_ascii_case("unknown"))
                    .unwrap_or(true)
                {
                    normalized_tool = Some(model_tool);
                }
                normalized_model = Some(model_value);
            }
        }
    }

    (
        normalized_tool.unwrap_or_else(|| "unknown".to_string()),
        normalized_model,
    )
}

fn split_tool_model(key: &str) -> (String, Option<String>) {
    if let Some((tool, model)) = key.split_once("::") {
        (tool.to_string(), Some(model.to_string()))
    } else if let Some((tool, model)) = key.split_once('/') {
        (tool.to_string(), Some(model.to_string()))
    } else {
        (key.to_string(), None)
    }
}

fn build_file_stats(
    workdir: &Path,
    commit_sha: &str,
    authorship_log: &AuthorshipLog,
) -> Vec<Value> {
    let numstat = git_diff_tree_numstat(workdir, commit_sha);
    if numstat.is_empty() {
        return Vec::new();
    }

    // Per-file (ai_lines, human_lines, breakdown)
    let mut ai_per_file: BTreeMap<String, u32> = BTreeMap::new();
    let mut human_per_file: BTreeMap<String, u32> = BTreeMap::new();
    let mut breakdown_per_file: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();

    for attestation in &authorship_log.attestations {
        for entry in &attestation.entries {
            let count: u32 = entry.line_ranges.iter().map(line_range_count).sum();
            if count == 0 {
                continue;
            }

            if entry.hash.starts_with("h_") {
                *human_per_file
                    .entry(attestation.file_path.clone())
                    .or_insert(0) += count;
            } else {
                *ai_per_file
                    .entry(attestation.file_path.clone())
                    .or_insert(0) += count;

                // Resolve tool/model via metadata.prompts entry
                let key = if let Some(prompt) = authorship_log.metadata.prompts.get(&entry.hash) {
                    let (tool, model) = normalize_tool_model(
                        Some(prompt.agent_id.tool.as_str()),
                        Some(prompt.agent_id.model.as_str()),
                    );
                    if model.is_none() {
                        tool
                    } else {
                        format!("{}::{}", tool, model.unwrap())
                    }
                } else {
                    "unknown".to_string()
                };

                let bucket = breakdown_per_file
                    .entry(attestation.file_path.clone())
                    .or_default();
                *bucket.entry(key).or_insert(0) += count;
            }
        }
    }

    let mut files = Vec::with_capacity(numstat.len());
    for (file_path, added, deleted) in numstat {
        let ai_attr = ai_per_file.get(&file_path).copied().unwrap_or(0);
        let human_attr = human_per_file.get(&file_path).copied().unwrap_or(0);

        let ai_add = ai_attr.min(added);
        let human_add = human_attr.min(added.saturating_sub(ai_add));
        let unknown_add = added.saturating_sub(ai_add).saturating_sub(human_add);

        let breakdown = breakdown_per_file
            .get(&file_path)
            .map(|map| {
                map.iter()
                    .map(|(key, count)| {
                        let (tool, model) = split_tool_model(key);
                        json!({
                            "tool": tool,
                            "model": model,
                            "aiAdditions": *count,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        files.push(json!({
            "filePath": file_path,
            "gitDiffAddedLines": added,
            "gitDiffDeletedLines": deleted,
            "aiAdditions": ai_add,
            "humanAdditions": human_add,
            "unknownAdditions": unknown_add,
            "toolModelBreakdown": breakdown,
        }));
    }

    files
}

fn line_range_count(range: &LineRange) -> u32 {
    match range {
        LineRange::Single(_) => 1,
        LineRange::Range(start, end) => {
            if end >= start {
                end - start + 1
            } else {
                0
            }
        }
    }
}

fn git_diff_tree_numstat(workdir: &Path, commit_sha: &str) -> Vec<(String, u32, u32)> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["diff-tree", "--no-commit-id", "--numstat", "-r", commit_sha])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut entries = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let added = parts.next().unwrap_or("0");
        let deleted = parts.next().unwrap_or("0");
        let path = match parts.next() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let added: u32 = if added == "-" {
            0
        } else {
            added.parse().unwrap_or(0)
        };
        let deleted: u32 = if deleted == "-" {
            0
        } else {
            deleted.parse().unwrap_or(0)
        };
        entries.push((path, added, deleted));
    }

    entries
}

fn git_repo_url(workdir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

fn git_current_branch(workdir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn git_commit_metadata(workdir: &Path, commit_sha: &str) -> Option<(String, String, String)> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["log", "-1", "--format=%ae|%s|%aI", commit_sha])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?.trim().to_string();
    let mut parts = line.splitn(3, '|');
    let author = parts.next()?.to_string();
    let message = parts.next().unwrap_or("").to_string();
    let raw_ts = parts.next().unwrap_or("").to_string();
    let timestamp = format_timestamp(&raw_ts);
    Some((message, author, timestamp))
}

fn format_timestamp(raw: &str) -> String {
    // Convert ISO-8601 (e.g. "2026-04-24T16:09:33+08:00") to "yyyy-MM-dd HH:mm:ss"
    // exactly like the PowerShell script. Best-effort: fall back to the input.
    let trimmed = raw.trim();
    if trimmed.len() < 19 {
        return trimmed.to_string();
    }
    // First 10 chars = date, next char = 'T' or ' ', then 8 chars time
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 19 && (bytes[10] == b'T' || bytes[10] == b' ') {
        let mut out = String::with_capacity(19);
        out.push_str(&trimmed[..10]);
        out.push(' ');
        out.push_str(&trimmed[11..19]);
        return out;
    }
    trimmed.to_string()
}

fn derive_project_name(repo_url: Option<&str>, workdir: &Path) -> String {
    if let Some(url) = repo_url {
        let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
        if let Some(idx) = trimmed.rfind(['/', ':']) {
            let candidate = &trimmed[idx + 1..];
            if !candidate.is_empty() {
                return candidate.to_string();
            }
        }
    }
    workdir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

fn short_sha(sha: &str) -> &str {
    if sha.len() > 7 { &sha[..7] } else { sha }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::working_log::AgentId;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn split_tool_model_with_separator() {
        let (tool, model) = split_tool_model("github-copilot::gpt-4");
        assert_eq!(tool, "github-copilot");
        assert_eq!(model.as_deref(), Some("gpt-4"));

        let (tool, model) = split_tool_model("github-copilot/gpt-5.4");
        assert_eq!(tool, "github-copilot");
        assert_eq!(model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn split_tool_model_without_separator() {
        let (tool, model) = split_tool_model("cursor");
        assert_eq!(tool, "cursor");
        assert_eq!(model, None);
    }

    #[test]
    fn normalize_tool_model_strips_alias_prefix_from_model() {
        let (tool, model) = normalize_tool_model(Some("github-copilot"), Some("copilot/gpt-5.4"));
        assert_eq!(tool, "github-copilot");
        assert_eq!(model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn line_range_count_single() {
        assert_eq!(line_range_count(&LineRange::Single(5)), 1);
    }

    #[test]
    fn line_range_count_range() {
        assert_eq!(line_range_count(&LineRange::Range(3, 7)), 5);
    }

    #[test]
    fn line_range_count_inverted() {
        assert_eq!(line_range_count(&LineRange::Range(8, 5)), 0);
    }

    #[test]
    fn format_timestamp_iso() {
        assert_eq!(
            format_timestamp("2026-04-24T16:09:33+08:00"),
            "2026-04-24 16:09:33"
        );
    }

    #[test]
    fn format_timestamp_already_formatted() {
        assert_eq!(
            format_timestamp("2026-04-24 16:09:33"),
            "2026-04-24 16:09:33"
        );
    }

    #[test]
    fn derive_project_name_from_https_url() {
        assert_eq!(
            derive_project_name(Some("https://gitlab.com/team/proj.git"), Path::new("/tmp")),
            "proj"
        );
    }

    #[test]
    fn derive_project_name_from_ssh_url() {
        assert_eq!(
            derive_project_name(Some("git@github.com:team/proj.git"), Path::new("/tmp")),
            "proj"
        );
    }

    #[test]
    fn derive_project_name_falls_back_to_workdir() {
        assert_eq!(
            derive_project_name(None, Path::new("/tmp/my-app")),
            "my-app"
        );
    }

    #[test]
    fn build_prompt_stats_includes_prompt_text_and_messages() {
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "prompt-123".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: " github copilot ".to_string(),
                    id: "session-1".to_string(),
                    model: " gpt-5.4 ".to_string(),
                },
                human_author: Some(" dev@example.com ".to_string()),
                messages: vec![
                    Message::user("  first prompt  ".to_string(), None),
                    Message::assistant("assistant reply".to_string(), None),
                    Message::user("second prompt".to_string(), None),
                ],
                total_additions: 9,
                total_deletions: 2,
                accepted_lines: 7,
                overriden_lines: 1,
                messages_url: Some(" https://cas.example/prompt-123 ".to_string()),
                custom_attributes: Some(HashMap::from([(
                    "language".to_string(),
                    "rust".to_string(),
                )])),
            },
        );

        let payload = build_prompt_stats(&prompts);
        assert_eq!(payload.len(), 1);
        let prompt = &payload[0];

        assert_eq!(prompt["promptHash"], "prompt-123");
        assert_eq!(prompt["tool"], "github copilot");
        assert_eq!(prompt["model"], "gpt-5.4");
        assert_eq!(prompt["humanAuthor"], "dev@example.com");
        assert_eq!(
            prompt["promptText"],
            "first prompt\n\nsecond prompt"
        );
        assert_eq!(prompt["messagesUrl"], "https://cas.example/prompt-123");
        assert_eq!(prompt["acceptedLines"], 7);
        assert_eq!(prompt["customAttributes"]["language"], "rust");
        assert!(prompt["messages"].is_array());
        assert_eq!(prompt["messages"].as_array().map(Vec::len), Some(3));
    }

    #[test]
    fn build_prompt_stats_defaults_unknown_tool_and_null_prompt_text() {
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "prompt-empty".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "   ".to_string(),
                    id: "session-2".to_string(),
                    model: "   ".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: None,
            },
        );

        let payload = build_prompt_stats(&prompts);
        assert_eq!(payload[0]["tool"], "unknown");
        assert!(payload[0]["model"].is_null());
        assert!(payload[0]["promptText"].is_null());
        assert_eq!(payload[0]["messages"], json!([]));
    }

    #[test]
    fn build_prompt_stats_normalizes_model_alias_prefix() {
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "prompt-copilot".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "github-copilot".to_string(),
                    id: "session-3".to_string(),
                    model: "copilot/gpt-5.4".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 4,
                total_deletions: 0,
                accepted_lines: 4,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: None,
            },
        );

        let payload = build_prompt_stats(&prompts);
        assert_eq!(payload[0]["tool"], "github-copilot");
        assert_eq!(payload[0]["model"], "gpt-5.4");
    }

    #[test]
    fn resolve_upload_url_from_full_url() {
        let _g = EnvGuard::new();
        unsafe {
            std::env::set_var("GIT_AI_REPORT_REMOTE_URL", "https://example.com/api");
            std::env::remove_var("GIT_AI_REPORT_REMOTE_ENDPOINT");
            std::env::remove_var("GIT_AI_REPORT_REMOTE_PATH");
        }
        assert_eq!(resolve_upload_url(), "https://example.com/api");
    }

    #[test]
    fn resolve_upload_url_from_endpoint_and_path() {
        let _g = EnvGuard::new();
        unsafe {
            std::env::remove_var("GIT_AI_REPORT_REMOTE_URL");
            std::env::set_var("GIT_AI_REPORT_REMOTE_ENDPOINT", "https://api.example.com/");
            std::env::set_var("GIT_AI_REPORT_REMOTE_PATH", "/v1/upload");
        }
        assert_eq!(resolve_upload_url(), "https://api.example.com/v1/upload");
    }

    #[test]
    fn resolve_upload_url_default() {
        let _g = EnvGuard::new();
        unsafe {
            std::env::remove_var("GIT_AI_REPORT_REMOTE_URL");
            std::env::remove_var("GIT_AI_REPORT_REMOTE_ENDPOINT");
            std::env::remove_var("GIT_AI_REPORT_REMOTE_PATH");
        }
        assert_eq!(resolve_upload_url(), DEFAULT_UPLOAD_URL);
    }

    #[test]
    fn run_upload_task_runs_inline_when_background_disabled() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_for_task = Arc::clone(&ran);

        run_upload_task(false, move || {
            ran_for_task.store(true, Ordering::SeqCst);
        });

        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn run_upload_task_spawns_when_background_enabled() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_for_task = Arc::clone(&ran);
        let (done_tx, done_rx) = mpsc::channel();

        run_upload_task(true, move || {
            ran_for_task.store(true, Ordering::SeqCst);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background upload task should complete");
        assert!(ran.load(Ordering::SeqCst));
    }

    /// Restore the upload-related env vars after each test to prevent
    /// cross-test contamination when running serially.
    struct EnvGuard {
        url: Option<String>,
        endpoint: Option<String>,
        path: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                url: std::env::var("GIT_AI_REPORT_REMOTE_URL").ok(),
                endpoint: std::env::var("GIT_AI_REPORT_REMOTE_ENDPOINT").ok(),
                path: std::env::var("GIT_AI_REPORT_REMOTE_PATH").ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.url {
                    Some(v) => std::env::set_var("GIT_AI_REPORT_REMOTE_URL", v),
                    None => std::env::remove_var("GIT_AI_REPORT_REMOTE_URL"),
                }
                match &self.endpoint {
                    Some(v) => std::env::set_var("GIT_AI_REPORT_REMOTE_ENDPOINT", v),
                    None => std::env::remove_var("GIT_AI_REPORT_REMOTE_ENDPOINT"),
                }
                match &self.path {
                    Some(v) => std::env::set_var("GIT_AI_REPORT_REMOTE_PATH", v),
                    None => std::env::remove_var("GIT_AI_REPORT_REMOTE_PATH"),
                }
            }
        }
    }
}
