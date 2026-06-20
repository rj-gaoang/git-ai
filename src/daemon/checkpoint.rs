use crate::authorship::attribution_tracker::{
    Attribution, AttributionTracker, INITIAL_ATTRIBUTION_TS, LineAttribution,
};
use crate::authorship::authorship_log_serialization::generate_session_id;
#[cfg(not(any(test, feature = "test-support")))]
use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::imara_diff_utils::{
    DiffOp, LineChangeTag, capture_diff_slices, compute_line_changes, normalize_line_endings,
};
use crate::authorship::working_log::CheckpointKind;
use crate::authorship::working_log::{Checkpoint, WorkingLogEntry};
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::error::GitAiError;
use crate::git::repo_storage::PersistedWorkingLog;
use crate::git::repository::Repository;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
#[cfg(not(any(test, feature = "test-support")))]
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-file line statistics (in-memory only, not persisted)
#[derive(Debug, Clone, Default)]
#[doc(hidden)]
pub struct FileLineStats {
    pub additions: u32,
    pub deletions: u32,
    pub additions_sloc: u32,
    pub deletions_sloc: u32,
}

/// Latest checkpoint state needed to process a file in the next checkpoint.
#[derive(Clone)]
struct PreviousFileState {
    blob_sha: String,
    attributions: Vec<Attribution>,
    kind: CheckpointKind,
    timestamp: u64,
    skip_as_ai_baseline: bool,
    source_working_log: PersistedWorkingLog,
}

use crate::authorship::working_log::AgentId;

#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
const AGENT_USAGE_MIN_INTERVAL_SECS: u64 = 150;

#[cfg(not(any(test, feature = "test-support")))]
const KNOWN_HUMAN_REJECT_SECS_AFTER_AI: u64 = 1;
const KNOWN_HUMAN_RECENT_AI_SAVE_LIMIT_SECS: u64 = 30;
const ARCHIVED_AI_STATE_LOOKBACK_SECS: u64 = 24 * 60 * 60;

#[cfg(not(any(test, feature = "test-support")))]
pub(crate) fn should_emit_agent_usage(agent_id: &AgentId) -> bool {
    let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);
    let now_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let Ok(db) = crate::metrics::db::MetricsDatabase::global() else {
        return true;
    };
    let Ok(mut db_lock) = db.lock() else {
        return true;
    };

    db_lock
        .should_emit_agent_usage(&prompt_id, now_ts, AGENT_USAGE_MIN_INTERVAL_SECS)
        .unwrap_or(true)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn should_emit_agent_usage(_agent_id: &AgentId) -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedPathRole {
    Edited,
    WillEdit,
}

#[derive(Debug, Clone)]
pub struct ResolvedCheckpointExecution {
    pub base_commit: String,
    pub ts: u128,
    pub files: Vec<String>,
    pub dirty_files: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AiPreEditCloseExecution {
    pub tool_use_id: String,
    pub trace_id: String,
    pub reason: String,
    pub ts: u128,
}

/// Build EventAttributes for AgentUsage events.
/// When repo is available, includes repo_url and branch. Always includes tool, model,
/// session_id, and custom attributes.
pub fn build_agent_usage_attrs(
    repo: Option<&Repository>,
    agent_id: &AgentId,
) -> crate::metrics::EventAttributes {
    let session_id = generate_session_id(&agent_id.id, &agent_id.tool);

    let mut attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .session_id(session_id)
        .tool(&agent_id.tool)
        .model(&agent_id.model)
        .external_session_id(&agent_id.id)
        .custom_attributes_map(crate::config::Config::fresh().custom_attributes());

    if let Some(repo) = repo {
        if let Some(url) = crate::repo_url::resolve_repo_url_from_repo(repo) {
            attrs = attrs.repo_url(url);
        }

        if let Ok(head_ref) = repo.head()
            && let Ok(short_branch) = head_ref.shorthand()
        {
            attrs = attrs.branch(short_branch);
        }
    }

    attrs
}

/// Build EventAttributes with repo metadata.
/// Reused for both AgentUsage and Checkpoint events.
fn build_checkpoint_attrs(
    repo: &Repository,
    base_commit: &str,
    agent_id: Option<&AgentId>,
) -> crate::metrics::EventAttributes {
    // Extract session_id from agent_id if available
    let session_id = agent_id
        .as_ref()
        .map(|aid| generate_session_id(&aid.id, &aid.tool))
        .unwrap_or_default();

    let mut attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .session_id(session_id)
        .base_commit_sha(base_commit);

    // Add AI-specific attributes
    if let Some(agent_id) = agent_id {
        attrs = attrs
            .tool(&agent_id.tool)
            .model(&agent_id.model)
            .external_session_id(&agent_id.id);
    }

    // Attach custom attributes using Config::fresh() to support runtime config updates
    attrs = attrs.custom_attributes_map(crate::config::Config::fresh().custom_attributes());

    // Add repo URL
    if let Some(url) = crate::repo_url::resolve_repo_url_from_repo(repo) {
        attrs = attrs.repo_url(url);
    }

    // Add branch
    if let Ok(head_ref) = repo.head()
        && let Ok(short_branch) = head_ref.shorthand()
    {
        attrs = attrs.branch(short_branch);
    }

    attrs
}

pub fn execute_resolved_checkpoint_from_daemon(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    checkpoint_request: CheckpointRequest,
    resolved: ResolvedCheckpointExecution,
) -> Result<(), GitAiError> {
    let checkpoint_start = Instant::now();
    tracing::debug!("[BENCHMARK] Starting daemon replay checkpoint");
    execute_resolved_checkpoint(
        repo,
        author,
        kind,
        true,
        checkpoint_request,
        resolved,
        checkpoint_start,
    )
    .map(|_| ())
}

pub fn close_ai_pre_edit_from_daemon(
    repo: &Repository,
    author: &str,
    close: AiPreEditCloseExecution,
) -> Result<(), GitAiError> {
    let base_commit = match crate::git::repo_state::read_head_state_for_worktree(&repo.workdir()?) {
        Some(state) => match state.head {
            Some(sha) => sha,
            None => "initial".to_string(),
        },
        None => "initial".to_string(),
    };
    let working_log = repo.storage.working_log_for_base_commit(&base_commit)?;
    let mut checkpoints = working_log.read_all_checkpoints()?;
    let entries = entries_for_unclosed_ai_pre_edit_tool_use(&checkpoints, &close.tool_use_id);
    if entries.is_empty() {
        crate::diagnostics::append_debug_event(
            "ai_pre_edit_close_noop",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "baseCommit": base_commit,
                "toolUseId": close.tool_use_id,
                "traceId": close.trace_id,
                "reason": close.reason,
            }),
        );
        return Ok(());
    }

    let files = entries
        .iter()
        .map(|entry| entry.file.clone())
        .collect::<Vec<_>>();
    let mut metadata = HashMap::new();
    metadata.insert("ai_pre_edit_closed".to_string(), "true".to_string());
    metadata.insert("tool_use_id".to_string(), close.tool_use_id.clone());
    metadata.insert("close_reason".to_string(), close.reason.clone());

    let mut checkpoint = Checkpoint::new(
        CheckpointKind::Human,
        String::new(),
        author.to_string(),
        entries,
    );
    checkpoint.timestamp = (close.ts / 1000) as u64;
    checkpoint.trace_id = Some(close.trace_id.clone());
    checkpoint.agent_metadata = Some(metadata);

    working_log.append_checkpoint(&checkpoint)?;
    checkpoints.push(checkpoint);
    crate::diagnostics::append_debug_event(
        "ai_pre_edit_closed",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "baseCommit": base_commit,
            "toolUseId": close.tool_use_id,
            "traceId": close.trace_id,
            "reason": close.reason,
            "fileCount": files.len(),
            "files": files,
        }),
    );
    Ok(())
}

fn execute_resolved_checkpoint(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: CheckpointRequest,
    resolved: ResolvedCheckpointExecution,
    checkpoint_start: Instant,
) -> Result<(usize, usize, usize), GitAiError> {
    if kind.is_ai() && checkpoint_request.agent_id.is_none() {
        return Err(GitAiError::Generic(
            "AI checkpoint is missing agent_id".to_string(),
        ));
    }

    let mut working_log = repo
        .storage
        .working_log_for_base_commit(&resolved.base_commit)?;
    if !resolved.dirty_files.is_empty() {
        working_log.set_dirty_files(Some(resolved.dirty_files.clone()));
    }

    let read_checkpoints_start = Instant::now();
    let mut checkpoints = working_log.read_all_checkpoints()?;
    tracing::debug!(
        "[BENCHMARK] Reading {} checkpoints took {:?}",
        checkpoints.len(),
        read_checkpoints_start.elapsed()
    );

    // Reject KnownHuman checkpoints that arrive immediately after an AI checkpoint
    // seconds of an AI checkpoint on any of the same files. These are likely spurious
    // IDE save events triggered by the AI completing its edit, not genuine human keystrokes.
    #[cfg(not(any(test, feature = "test-support")))]
    if kind == CheckpointKind::KnownHuman {
        let now_secs = (resolved.ts / 1000) as u64;
        let too_soon = checkpoints.iter().rev().any(|cp| {
            cp.kind.is_ai()
                && now_secs.saturating_sub(cp.timestamp) < KNOWN_HUMAN_REJECT_SECS_AFTER_AI
                && cp.entries.iter().any(|e| resolved.files.contains(&e.file))
        });
        if too_soon {
            tracing::debug!(
                "[KnownHuman] Rejected: fired within {}s of an AI checkpoint on the same file",
                KNOWN_HUMAN_REJECT_SECS_AFTER_AI
            );
            return Ok((0, 0, 0));
        }
    }

    let save_states_start = Instant::now();
    let file_content_hashes = save_current_file_states(&working_log, &resolved.files)?;
    tracing::debug!(
        "[BENCHMARK] save_current_file_states for {} files took {:?}",
        resolved.files.len(),
        save_states_start.elapsed()
    );

    let hash_compute_start = Instant::now();
    let mut ordered_hashes: Vec<_> = file_content_hashes.iter().collect();
    ordered_hashes.sort_by_key(|(file_path, _)| *file_path);

    let mut combined_hasher = Sha256::new();
    for (file_path, hash) in ordered_hashes {
        combined_hasher.update(file_path.as_bytes());
        combined_hasher.update(hash.as_bytes());
    }
    let combined_hash = format!("{:x}", combined_hasher.finalize());
    tracing::debug!(
        "[BENCHMARK] Hash computation took {:?}",
        hash_compute_start.elapsed()
    );

    let unclosed_ai_pre_edit_files = if kind == CheckpointKind::KnownHuman {
        unclosed_ai_pre_edit_files(&checkpoints, &resolved.files)
    } else {
        HashSet::new()
    };
    let has_unclosed_ai_pre_edit = !unclosed_ai_pre_edit_files.is_empty();
    let trace_id = checkpoint_request.trace_id.clone();
    let effective_kind = effective_checkpoint_kind(kind, &checkpoint_request);
    let downgraded_known_human =
        kind == CheckpointKind::KnownHuman && effective_kind == CheckpointKind::Human;
    let is_ai_pre_edit = is_ai_pre_edit_request(effective_kind, &checkpoint_request);
    let attest_human_lines = kind == CheckpointKind::Human
        && effective_kind == CheckpointKind::Human
        && checkpoint_request.agent_id.is_none()
        && checkpoint_request.metadata.is_empty();
    let limit_current_author_to_changed_lines = effective_kind.is_ai()
        || attest_human_lines
        || has_unclosed_ai_pre_edit
        || checkpoint_request
            .metadata
            .get("git_ai_replay_checkpoint")
            .is_some_and(|value| value == "true");

    let entries_start = Instant::now();
    let (entries, file_stats) = smol::block_on(get_checkpoint_entries(
        effective_kind,
        author,
        repo,
        &working_log,
        &resolved.files,
        &file_content_hashes,
        &checkpoints,
        &checkpoint_request,
        resolved.ts,
        Some(resolved.base_commit.as_str()),
        trace_id.clone(),
        attest_human_lines,
        limit_current_author_to_changed_lines,
        is_ai_pre_edit,
        unclosed_ai_pre_edit_files,
    ))?;
    tracing::debug!(
        "[BENCHMARK] get_checkpoint_entries generated {} entries, took {:?}",
        entries.len(),
        entries_start.elapsed()
    );

    if !entries.is_empty() {
        let checkpoint_create_start = Instant::now();
        let mut checkpoint = Checkpoint::new(
            effective_kind,
            combined_hash.clone(),
            author.to_string(),
            entries.clone(),
        );
        checkpoint.timestamp = (resolved.ts / 1000) as u64;
        checkpoint.line_stats = compute_line_stats(&file_stats)?;
        checkpoint.trace_id = Some(trace_id.clone());

        if effective_kind.is_ai() {
            checkpoint.agent_id = checkpoint_request.agent_id.clone();
            checkpoint.agent_metadata = if checkpoint_request.metadata.is_empty() {
                None
            } else {
                Some(checkpoint_request.metadata.clone())
            };
        } else if is_ai_pre_edit {
            checkpoint.agent_metadata = Some(checkpoint_request.metadata.clone());
        } else if downgraded_known_human {
            let mut metadata = checkpoint_request.metadata.clone();
            metadata.insert("known_human_downgraded".to_string(), "true".to_string());
            if has_unclosed_ai_pre_edit {
                metadata.insert(
                    "known_human_after_unclosed_ai_pre_edit".to_string(),
                    "true".to_string(),
                );
            }
            checkpoint.agent_metadata = Some(metadata);
        } else if effective_kind == CheckpointKind::KnownHuman
            && !checkpoint_request.metadata.is_empty()
        {
            let editor = checkpoint_request
                .metadata
                .get("kh_editor")
                .cloned()
                .unwrap_or_default();
            let editor_version = checkpoint_request
                .metadata
                .get("kh_editor_version")
                .cloned()
                .unwrap_or_default();
            let extension_version = checkpoint_request
                .metadata
                .get("kh_extension_version")
                .cloned()
                .unwrap_or_default();
            if !editor.is_empty() {
                use crate::authorship::working_log::KnownHumanMetadata;
                checkpoint.known_human_metadata = Some(KnownHumanMetadata {
                    editor,
                    editor_version,
                    extension_version,
                });
            }
        }
        tracing::debug!(
            "[BENCHMARK] Checkpoint creation took {:?}",
            checkpoint_create_start.elapsed()
        );

        let append_start = Instant::now();
        working_log.append_checkpoint(&checkpoint)?;
        tracing::debug!(
            "[BENCHMARK] Appending checkpoint to working log took {:?}",
            append_start.elapsed()
        );
        checkpoints.push(checkpoint.clone());

        let mut attrs =
            build_checkpoint_attrs(repo, &resolved.base_commit, checkpoint.agent_id.as_ref());

        // Add trace_id to attributes - links all checkpoint events together
        if let Some(ref tid) = checkpoint.trace_id {
            attrs = attrs.trace_id(tid);
        }

        // Extract tool_use_id from metadata if available
        // tool_use_id tracks specific tool invocations (e.g., bash tool calls from AI agents)
        // Allows linking checkpoint events to the exact tool use that triggered them
        let tool_use_id = checkpoint_request
            .metadata
            .get("tool_use_id")
            .map(|s| s.as_str());

        let edit_kind = checkpoint_request
            .metadata
            .get("edit_kind")
            .map(|s| s.as_str());

        for (entry, file_stat) in entries.iter().zip(file_stats.iter()) {
            let mut values = crate::metrics::CheckpointValues::new()
                .checkpoint_ts(checkpoint.timestamp)
                .kind(checkpoint.kind.to_str().to_string())
                .file_path(entry.file.clone())
                .lines_added(file_stat.additions)
                .lines_deleted(file_stat.deletions)
                .lines_added_sloc(file_stat.additions_sloc)
                .lines_deleted_sloc(file_stat.deletions_sloc);

            if let Some(tuid) = tool_use_id {
                values = values.external_tool_use_id(tuid);
            }
            if let Some(ek) = edit_kind {
                values = values.edit_kind(ek);
            }

            let file_attrs = attrs.clone().author(&checkpoint.author);
            crate::metrics::record(values, file_attrs);
        }
    }

    let agent_tool = if effective_kind.is_ai() {
        checkpoint_request
            .agent_id
            .as_ref()
            .map(|aid| aid.tool.as_str())
    } else {
        None
    };

    let label = if entries.len() > 1 {
        "checkpoint"
    } else {
        "commit"
    };

    if !quiet {
        let log_author = agent_tool.unwrap_or(author);
        let files_with_entries = entries.len();
        let total_uncommitted_files = resolved.files.len();

        if files_with_entries == total_uncommitted_files {
            eprintln!(
                "{} {} changed {} file(s) that have changed since the last {}",
                effective_kind.to_str(),
                log_author,
                files_with_entries,
                label
            );
        } else {
            eprintln!(
                "{} {} changed {} of the {} file(s) that have changed since the last {} ({} already checkpointed)",
                effective_kind.to_str(),
                log_author,
                files_with_entries,
                total_uncommitted_files,
                label,
                total_uncommitted_files - files_with_entries
            );
        }
    }

    tracing::debug!(
        "[BENCHMARK] Total checkpoint run took {:?}",
        checkpoint_start.elapsed()
    );
    Ok((entries.len(), resolved.files.len(), checkpoints.len()))
}

fn save_current_file_states(
    working_log: &PersistedWorkingLog,
    files: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let _read_start = Instant::now();

    let blobs_dir = working_log.dir.join("blobs");
    let dirty_files = working_log.dirty_files.clone();

    let file_content_hashes = smol::block_on(async {
        let semaphore = Arc::new(smol::lock::Semaphore::new(8));
        let blobs_dir = Arc::new(blobs_dir);
        let dirty_files = Arc::new(dirty_files);

        let futures = files.iter().map(|file_path| {
            let file_path = file_path.clone();
            let blobs_dir = Arc::clone(&blobs_dir);
            let dirty_files = Arc::clone(&dirty_files);
            let semaphore = Arc::clone(&semaphore);

            async move {
                // Acquire semaphore permit
                let _permit = semaphore.acquire().await;

                // Read file content - check dirty_files first, then filesystem
                let content = if let Some(ref dirty_map) = *dirty_files {
                    dirty_map.get(&file_path).cloned()
                } else {
                    None
                }
                .ok_or_else(|| {
                    GitAiError::Generic(format!(
                        "save_current_file_states: file '{}' not found in dirty_files snapshot (filesystem fallback is not allowed in checkpoint flow)",
                        file_path
                    ))
                })?;

                // Create SHA256 hash of the content
                let mut hasher = Sha256::new();
                hasher.update(content.as_bytes());
                let sha = format!("{:x}", hasher.finalize());

                // Ensure blobs directory exists
                std::fs::create_dir_all(&*blobs_dir)?;

                // Write content to blob file
                let blob_path = blobs_dir.join(&sha);
                std::fs::write(blob_path, content)?;

                Ok::<(String, String), GitAiError>((file_path, sha))
            }
        });

        // Collect results from all concurrent operations
        let results: Vec<Result<(String, String), GitAiError>> =
            stream::iter(futures).buffer_unordered(8).collect().await;

        // Convert results into HashMap
        let mut file_content_hashes = HashMap::new();
        for result in results {
            let (file_path, content_hash) = result?;
            file_content_hashes.insert(file_path, content_hash);
        }

        Ok::<HashMap<String, String>, GitAiError>(file_content_hashes)
    })?;

    Ok(file_content_hashes)
}

fn get_previous_content_from_head_opt(
    repo: &Repository,
    file_path: &str,
    head_tree_id: &Option<String>,
) -> Option<String> {
    let Some(tree_id) = head_tree_id.as_ref() else {
        return None;
    };
    match repo.read_file_blob_at_tree(tree_id, std::path::Path::new(file_path)) {
        Ok(content) => Some(String::from_utf8_lossy(&content).to_string()),
        Err(_) => None,
    }
}

fn get_previous_content_from_head(
    repo: &Repository,
    file_path: &str,
    head_tree_id: &Option<String>,
) -> String {
    get_previous_content_from_head_opt(repo, file_path, head_tree_id).unwrap_or_default()
}

/// Compare file contents ignoring CRLF/LF differences.
fn content_eq_normalized(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    normalize_line_endings(a) == normalize_line_endings(b)
}

#[doc(hidden)]
pub fn is_ai_author_id(author_id: &str) -> bool {
    author_id != "human" && !author_id.starts_with("h_")
}

fn working_log_entry_has_non_human_attribution(entry: &WorkingLogEntry) -> bool {
    entry
        .line_attributions
        .iter()
        .any(|attr| is_ai_author_id(&attr.author_id))
        || entry
            .attributions
            .iter()
            .any(|attr| is_ai_author_id(&attr.author_id))
}

fn has_known_human_editor_metadata(request: &CheckpointRequest) -> bool {
    request
        .metadata
        .get("kh_editor")
        .is_some_and(|value| !value.trim().is_empty() && !value.eq_ignore_ascii_case("unknown"))
}

fn is_human_tool_name(tool: &str) -> bool {
    let tool = tool.trim();
    tool.eq_ignore_ascii_case("human") || tool.eq_ignore_ascii_case("known_human")
}

fn request_has_ai_pre_edit_agent(checkpoint_request: &CheckpointRequest) -> bool {
    checkpoint_request
        .agent_id
        .as_ref()
        .is_some_and(|agent_id| !is_human_tool_name(&agent_id.tool))
}

fn metadata_allows_ai_pre_edit(metadata: &HashMap<String, String>) -> bool {
    !metadata
        .get("agent_tool")
        .is_some_and(|tool| is_human_tool_name(tool))
}

fn is_ai_pre_edit_request(
    effective_kind: CheckpointKind,
    checkpoint_request: &CheckpointRequest,
) -> bool {
    effective_kind == CheckpointKind::Human
        && checkpoint_request.path_role == PreparedPathRole::WillEdit
        && request_has_ai_pre_edit_agent(checkpoint_request)
        && checkpoint_request
            .metadata
            .get("ai_pre_edit")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        && metadata_allows_ai_pre_edit(&checkpoint_request.metadata)
}

fn checkpoint_tool_use_id(checkpoint: &Checkpoint) -> Option<&str> {
    checkpoint
        .agent_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("tool_use_id"))
        .map(String::as_str)
        .filter(|tool_use_id| !tool_use_id.trim().is_empty())
}

fn checkpoint_is_ai_pre_edit(checkpoint: &Checkpoint) -> bool {
    checkpoint.kind == CheckpointKind::Human
        && checkpoint.agent_metadata.as_ref().is_some_and(|metadata| {
            metadata
                .get("ai_pre_edit")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                && metadata_allows_ai_pre_edit(metadata)
                && !metadata
                    .get("edit_kind")
                    .is_some_and(|edit_kind| edit_kind.eq_ignore_ascii_case("bash"))
        })
}

fn checkpoint_is_ai_pre_edit_close(checkpoint: &Checkpoint) -> bool {
    checkpoint.kind == CheckpointKind::Human
        && checkpoint.agent_metadata.as_ref().is_some_and(|metadata| {
            metadata
                .get("ai_pre_edit_closed")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        })
}

fn checkpoint_should_be_skipped_as_ai_baseline(checkpoint: &Checkpoint) -> bool {
    checkpoint_is_ai_pre_edit(checkpoint)
        || checkpoint_is_ai_pre_edit_close(checkpoint)
        || checkpoint.agent_metadata.as_ref().is_some_and(|metadata| {
            metadata
                .get("known_human_downgraded")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                || metadata
                    .get("known_human_after_unclosed_ai_pre_edit")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        })
}

fn checkpoint_is_non_bash_ai_edit(checkpoint: &Checkpoint) -> bool {
    checkpoint.kind.is_ai()
        && !checkpoint
            .agent_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("edit_kind"))
            .is_some_and(|edit_kind| edit_kind.eq_ignore_ascii_case("bash"))
}

fn close_pending_ai_pre_edit(
    checkpoint: &Checkpoint,
    target_files: &HashSet<&str>,
    pending_file_tool_uses: &mut HashSet<(String, String)>,
    pending_files_without_tool_use: &mut HashSet<String>,
) {
    let Some(tool_use_id) = checkpoint_tool_use_id(checkpoint) else {
        return;
    };
    let mut files_for_tool = pending_file_tool_uses
        .iter()
        .filter_map(|(pending_tool_use_id, file)| {
            (pending_tool_use_id == tool_use_id && target_files.contains(file.as_str()))
                .then(|| file.clone())
        })
        .collect::<Vec<_>>();

    if files_for_tool.is_empty() {
        files_for_tool = checkpoint
            .entries
            .iter()
            .map(|entry| entry.file.clone())
            .filter(|file| target_files.contains(file.as_str()))
            .collect();
    }

    for file in files_for_tool {
        pending_file_tool_uses.remove(&(tool_use_id.to_string(), file.clone()));
        pending_files_without_tool_use.remove(&file);
    }
}

#[cfg(test)]
fn has_unclosed_ai_pre_edit_for_files(
    previous_checkpoints: &[Checkpoint],
    files: &[String],
) -> bool {
    !unclosed_ai_pre_edit_files(previous_checkpoints, files).is_empty()
}

fn unclosed_ai_pre_edit_files(
    previous_checkpoints: &[Checkpoint],
    files: &[String],
) -> HashSet<String> {
    let target_files: HashSet<&str> = files.iter().map(String::as_str).collect();
    let mut pending_file_tool_uses: HashSet<(String, String)> = HashSet::new();
    let mut pending_files_without_tool_use: HashSet<String> = HashSet::new();

    for checkpoint in previous_checkpoints {
        let touched_target_files: Vec<&str> = checkpoint
            .entries
            .iter()
            .map(|entry| entry.file.as_str())
            .filter(|file| target_files.contains(file))
            .collect();
        if touched_target_files.is_empty() {
            continue;
        }

        if checkpoint_is_ai_pre_edit_close(checkpoint) {
            close_pending_ai_pre_edit(
                checkpoint,
                &target_files,
                &mut pending_file_tool_uses,
                &mut pending_files_without_tool_use,
            );
            continue;
        }

        if checkpoint_is_non_bash_ai_edit(checkpoint) {
            if let Some(tool_use_id) = checkpoint_tool_use_id(checkpoint) {
                for file in &touched_target_files {
                    pending_file_tool_uses.remove(&(tool_use_id.to_string(), (*file).to_string()));
                }
            } else {
                for file in &touched_target_files {
                    pending_files_without_tool_use.remove(*file);
                }
            }
            continue;
        }

        if checkpoint_is_ai_pre_edit(checkpoint) {
            if let Some(tool_use_id) = checkpoint_tool_use_id(checkpoint) {
                for file in &touched_target_files {
                    pending_file_tool_uses.insert((tool_use_id.to_string(), (*file).to_string()));
                }
            } else {
                for file in &touched_target_files {
                    pending_files_without_tool_use.insert((*file).to_string());
                }
            }
        }
    }

    pending_files_without_tool_use
        .into_iter()
        .chain(pending_file_tool_uses.into_iter().map(|(_, file)| file))
        .collect()
}

fn entries_for_unclosed_ai_pre_edit_tool_use(
    previous_checkpoints: &[Checkpoint],
    tool_use_id: &str,
) -> Vec<WorkingLogEntry> {
    if tool_use_id.trim().is_empty() {
        return Vec::new();
    }

    let mut pending: BTreeMap<String, WorkingLogEntry> = BTreeMap::new();
    let mut latest_entry_by_file: BTreeMap<String, WorkingLogEntry> = BTreeMap::new();
    for checkpoint in previous_checkpoints {
        let Some(checkpoint_tool_use_id) = checkpoint_tool_use_id(checkpoint) else {
            for entry in &checkpoint.entries {
                latest_entry_by_file.insert(entry.file.clone(), entry.clone());
            }
            continue;
        };
        if checkpoint_tool_use_id != tool_use_id {
            for entry in &checkpoint.entries {
                latest_entry_by_file.insert(entry.file.clone(), entry.clone());
            }
            continue;
        }

        if checkpoint_is_ai_pre_edit(checkpoint) {
            for entry in &checkpoint.entries {
                let prior = latest_entry_by_file.get(&entry.file);
                let inherited = prior.filter(|prior| {
                    prior.blob_sha == entry.blob_sha
                        && (!prior.attributions.is_empty() || !prior.line_attributions.is_empty())
                });
                let source = inherited.unwrap_or(entry);
                pending.insert(
                    entry.file.clone(),
                    WorkingLogEntry::new(
                        entry.file.clone(),
                        entry.blob_sha.clone(),
                        source.attributions.clone(),
                        source.line_attributions.clone(),
                    ),
                );
            }
        } else if checkpoint_is_non_bash_ai_edit(checkpoint)
            || checkpoint_is_ai_pre_edit_close(checkpoint)
        {
            for entry in &checkpoint.entries {
                pending.remove(&entry.file);
            }
        }

        for entry in &checkpoint.entries {
            latest_entry_by_file.insert(entry.file.clone(), entry.clone());
        }
    }

    pending.into_values().collect()
}

fn effective_checkpoint_kind(
    kind: CheckpointKind,
    checkpoint_request: &CheckpointRequest,
) -> CheckpointKind {
    if kind == CheckpointKind::KnownHuman && !has_known_human_editor_metadata(checkpoint_request) {
        CheckpointKind::Human
    } else {
        kind
    }
}

fn build_previous_file_state_maps(
    working_log: &PersistedWorkingLog,
    previous_checkpoints: &[Checkpoint],
    initial_attributions: &HashMap<String, Vec<LineAttribution>>,
) -> (HashMap<String, Vec<PreviousFileState>>, HashSet<String>) {
    let mut previous_file_state_by_file: HashMap<String, Vec<PreviousFileState>> = HashMap::new();
    let mut ai_touched_files: HashSet<String> = initial_attributions.keys().cloned().collect();

    // Keep per-file checkpoint history so AI checkpoints can look past a
    // same-content KnownHuman save that landed just before the AI checkpoint.
    for checkpoint in previous_checkpoints {
        for entry in &checkpoint.entries {
            previous_file_state_by_file
                .entry(entry.file.clone())
                .or_default()
                .push(PreviousFileState {
                    blob_sha: entry.blob_sha.clone(),
                    attributions: previous_file_state_attributions(
                        entry,
                        working_log,
                        checkpoint.timestamp as u128,
                    ),
                    kind: checkpoint.kind,
                    timestamp: checkpoint.timestamp,
                    skip_as_ai_baseline: checkpoint_should_be_skipped_as_ai_baseline(checkpoint),
                    source_working_log: working_log.clone(),
                });

            if checkpoint.kind.is_ai() || working_log_entry_has_non_human_attribution(entry) {
                ai_touched_files.insert(entry.file.clone());
            }
        }
    }

    (previous_file_state_by_file, ai_touched_files)
}

fn previous_file_state_content(state: &PreviousFileState) -> String {
    state
        .source_working_log
        .get_file_version(&state.blob_sha)
        .unwrap_or_default()
}

fn collect_recent_archived_ai_states(
    repo: &Repository,
    files: &[String],
    ts: u128,
) -> HashMap<String, PreviousFileState> {
    let target_files: HashSet<&str> = files.iter().map(String::as_str).collect();
    let now_secs = (ts / 1000) as u64;
    let mut latest_by_file: HashMap<String, PreviousFileState> = HashMap::new();

    for archived_log in repo.storage.archived_working_logs() {
        let Ok(checkpoints) = archived_log.read_all_checkpoints() else {
            continue;
        };

        for checkpoint in checkpoints {
            if !checkpoint_is_non_bash_ai_edit(&checkpoint)
                || now_secs.saturating_sub(checkpoint.timestamp) > ARCHIVED_AI_STATE_LOOKBACK_SECS
            {
                continue;
            }

            for entry in &checkpoint.entries {
                if !target_files.contains(entry.file.as_str()) {
                    continue;
                }

                let state = PreviousFileState {
                    blob_sha: entry.blob_sha.clone(),
                    attributions: previous_file_state_attributions(
                        entry,
                        &archived_log,
                        checkpoint.timestamp as u128,
                    ),
                    kind: checkpoint.kind,
                    timestamp: checkpoint.timestamp,
                    skip_as_ai_baseline: checkpoint_should_be_skipped_as_ai_baseline(&checkpoint),
                    source_working_log: archived_log.clone(),
                };

                let should_replace = latest_by_file
                    .get(&entry.file)
                    .map(|existing| existing.timestamp < state.timestamp)
                    .unwrap_or(true);
                if should_replace {
                    latest_by_file.insert(entry.file.clone(), state);
                }
            }
        }
    }

    latest_by_file
}

fn merge_recent_archived_ai_states(
    previous_file_state_by_file: &mut HashMap<String, Vec<PreviousFileState>>,
    ai_touched_files: &mut HashSet<String>,
    archived_ai_states: HashMap<String, PreviousFileState>,
) -> usize {
    let mut merged = 0usize;

    for (file, state) in archived_ai_states {
        let already_has_ai = previous_file_state_by_file
            .get(&file)
            .is_some_and(|states| {
                states.iter().any(|state| {
                    state.kind.is_ai()
                        || state
                            .attributions
                            .iter()
                            .any(|attr| is_ai_author_id(&attr.author_id))
                })
            });
        if already_has_ai {
            continue;
        }

        previous_file_state_by_file
            .entry(file.clone())
            .or_default()
            .push(state);
        ai_touched_files.insert(file);
        merged += 1;
    }

    merged
}

fn select_previous_state_for_ai_checkpoint(
    current_content: &str,
    file_history: &[PreviousFileState],
) -> Option<(String, Vec<Attribution>)> {
    let latest = file_history.last()?;
    let latest_content = previous_file_state_content(latest);

    if latest.skip_as_ai_baseline && content_eq_normalized(current_content, &latest_content) {
        for state in file_history.iter().rev().skip(1) {
            let state_content = previous_file_state_content(state);
            if !content_eq_normalized(current_content, &state_content) {
                return Some((state_content, state.attributions.clone()));
            }
        }
        return None;
    }

    if latest.kind != CheckpointKind::KnownHuman
        || !content_eq_normalized(current_content, &latest_content)
    {
        return Some((latest_content, latest.attributions.clone()));
    }

    for state in file_history.iter().rev().skip(1) {
        let state_content = previous_file_state_content(state);
        if !content_eq_normalized(current_content, &state_content) {
            return Some((state_content, state.attributions.clone()));
        }
    }

    None
}

fn previous_file_state_attributions(
    entry: &WorkingLogEntry,
    working_log: &PersistedWorkingLog,
    ts: u128,
) -> Vec<Attribution> {
    if !entry.attributions.is_empty() {
        return entry.attributions.clone();
    }

    if entry.line_attributions.is_empty() {
        return Vec::new();
    }

    let content = working_log
        .get_file_version(&entry.blob_sha)
        .unwrap_or_default();
    crate::authorship::attribution_tracker::line_attributions_to_attributions(
        &entry.line_attributions,
        &content,
        ts,
    )
}

fn has_recent_ai_file_state(file_history: &[PreviousFileState], ts: u128) -> bool {
    let now_secs = (ts / 1000) as u64;
    file_history.iter().rev().any(|state| {
        state.kind.is_ai()
            && now_secs.saturating_sub(state.timestamp) < KNOWN_HUMAN_RECENT_AI_SAVE_LIMIT_SECS
    })
}

#[allow(clippy::too_many_arguments)]
fn get_checkpoint_entry_for_file(
    file_path: String,
    kind: CheckpointKind,
    repo: Repository,
    working_log: PersistedWorkingLog,
    previous_file_state_by_file: Arc<HashMap<String, Vec<PreviousFileState>>>,
    ai_touched_files: Arc<HashSet<String>>,
    file_content_hash: String,
    author_id: Arc<String>,
    weak_known_human_files: Arc<HashSet<String>>,
    head_tree_id: Arc<Option<String>>,
    initial_attributions: Arc<HashMap<String, Vec<LineAttribution>>>,
    initial_snapshot_contents: Arc<HashMap<String, String>>,
    attest_human_lines: bool,
    limit_current_author_to_changed_lines: bool,
    preserve_same_content_entry: bool,
    ts: u128,
) -> Result<Option<(WorkingLogEntry, FileLineStats)>, GitAiError> {
    let file_start = Instant::now();
    let initial_attrs_for_file = initial_attributions
        .get(&file_path)
        .cloned()
        .unwrap_or_default();
    let initial_snapshot_content = initial_snapshot_contents.get(&file_path).cloned();

    let previous_file_history = previous_file_state_by_file
        .get(&file_path)
        .cloned()
        .unwrap_or_default();
    let previous_state = previous_file_history.last().cloned();
    let weak_known_human_for_file =
        kind == CheckpointKind::KnownHuman && weak_known_human_files.contains(&file_path);
    let file_author_id = if weak_known_human_for_file {
        CheckpointKind::Human.to_str()
    } else {
        author_id.as_ref().clone()
    };
    let has_prior_ai_edits = ai_touched_files.contains(&file_path);
    let limit_to_changed_lines_for_known_human_on_ai_content =
        kind == CheckpointKind::KnownHuman && has_prior_ai_edits;
    let limit_to_changed_lines_for_recent_known_human_save =
        kind == CheckpointKind::KnownHuman && has_recent_ai_file_state(&previous_file_history, ts);

    let current_content = working_log
        .read_current_file_content(&file_path)
        .unwrap_or_default();

    // Baseline-only human fast path:
    // AI pre-edit snapshots and downgraded weak KnownHuman checkpoints use the plain
    // "human" sentinel and must not create h_* attestations. Explicit legacy human
    // checkpoints bypass this path so committed human lines can be counted.
    if kind == CheckpointKind::Human
        && !attest_human_lines
        && !has_prior_ai_edits
        && initial_attrs_for_file.is_empty()
    {
        let previous_content = if let Some(state) = previous_state.as_ref() {
            previous_file_state_content(state)
        } else {
            get_previous_content_from_head(&repo, &file_path, head_tree_id.as_ref())
        };

        if content_eq_normalized(&current_content, &previous_content) {
            if preserve_same_content_entry {
                let prev_attributions = previous_state
                    .as_ref()
                    .map(|state| state.attributions.clone())
                    .unwrap_or_default();
                let line_attributions =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
                        &prev_attributions,
                        &previous_content,
                        false,
                    );
                let remapped_attributions =
                    crate::authorship::attribution_tracker::line_attributions_to_attributions(
                        &line_attributions,
                        &current_content,
                        ts,
                    );
                let entry = WorkingLogEntry::new(
                    file_path,
                    file_content_hash,
                    remapped_attributions,
                    line_attributions,
                );
                return Ok(Some((entry, FileLineStats::default())));
            }
            return Ok(None);
        }

        let stats = compute_file_line_stats(&previous_content, &current_content);
        let entry = WorkingLogEntry::new(file_path, file_content_hash, Vec::new(), Vec::new());
        return Ok(Some((entry, stats)));
    }

    let from_checkpoint = if kind.is_ai() {
        select_previous_state_for_ai_checkpoint(&current_content, &previous_file_history)
    } else {
        previous_state.as_ref().map(|state| {
            (
                previous_file_state_content(state),
                state.attributions.clone(),
            )
        })
    };

    let is_from_checkpoint = from_checkpoint.is_some();
    let (previous_content, prev_attributions) = if let Some((content, attrs)) = from_checkpoint {
        // File exists in a previous checkpoint - use that
        (content, attrs)
    } else {
        // File doesn't exist in any previous checkpoint - need to initialize from git + INITIAL
        let previous_content_from_head =
            get_previous_content_from_head_opt(&repo, &file_path, head_tree_id.as_ref());

        if kind.is_ai()
            && previous_content_from_head.is_none()
            && let Some(state) = previous_state.as_ref()
            && state.kind == CheckpointKind::KnownHuman
        {
            let latest_content = previous_file_state_content(state);
            if content_eq_normalized(&current_content, &latest_content) {
                return Ok(None);
            }
        }

        let previous_content = previous_content_from_head.unwrap_or_default();

        // Skip if no changes, UNLESS we have INITIAL attributions for this file
        // (in which case we need to create an entry to record those attributions)
        if content_eq_normalized(&current_content, &previous_content)
            && initial_attrs_for_file.is_empty()
        {
            return Ok(None);
        }

        // Build a set of lines covered by INITIAL attributions
        let mut initial_covered_lines: HashSet<u32> = HashSet::new();
        for attr in &initial_attrs_for_file {
            for line in attr.start_line..=attr.end_line {
                initial_covered_lines.insert(line);
            }
        }

        // Start with INITIAL attributions (they win)
        let mut prev_line_attributions = initial_attrs_for_file.clone();
        let mut blamed_lines: HashSet<u32> = HashSet::new();

        // Default all previous-content lines to "human" (no cross-commit blame)
        let prev_total_lines = previous_content.lines().count() as u32;
        for line_num in 1..=prev_total_lines {
            blamed_lines.insert(line_num);
        }

        // For AI checkpoints, attribute any lines NOT in INITIAL and NOT returned by ai_blame
        if kind.is_ai() {
            let total_lines = current_content.lines().count() as u32;
            for line_num in 1..=total_lines {
                if !initial_covered_lines.contains(&line_num) && !blamed_lines.contains(&line_num) {
                    prev_line_attributions.push(LineAttribution {
                        start_line: line_num,
                        end_line: line_num,
                        author_id: file_author_id.clone(),
                        overrode: None,
                    });
                }
            }
        }

        // INITIAL line numbers refer to the file state at the moment INITIAL was written.
        // Snapshot-aware INITIAL storage preserves that exact content; older INITIAL files
        // fall back to the legacy "current content" behavior.
        let content_for_line_conversion = if !initial_attrs_for_file.is_empty() {
            initial_snapshot_content
                .as_deref()
                .unwrap_or(&current_content)
        } else {
            &previous_content
        };

        // Convert any line attributions to character attributions
        let prev_attributions =
            crate::authorship::attribution_tracker::line_attributions_to_attributions(
                &prev_line_attributions,
                content_for_line_conversion,
                INITIAL_ATTRIBUTION_TS,
            );

        // When INITIAL has a persisted snapshot, use that as the previous content so later
        // edits after a restore/squash are tracked correctly. Older INITIAL files fall back
        // to the legacy current-content behavior.
        let adjusted_previous = if !initial_attrs_for_file.is_empty() {
            initial_snapshot_content.unwrap_or_else(|| current_content.clone())
        } else {
            previous_content
        };

        (adjusted_previous, prev_attributions)
    };

    // Skip if no changes (but we already checked this earlier, accounting for INITIAL attributions)
    // For files from previous checkpoints, check if content has changed
    if is_from_checkpoint && content_eq_normalized(&current_content, &previous_content) {
        if current_content == previous_content {
            if preserve_same_content_entry {
                let line_attributions =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
                        &prev_attributions,
                        &current_content,
                        kind.is_ai(),
                    );
                let entry = WorkingLogEntry::new(
                    file_path,
                    file_content_hash,
                    prev_attributions,
                    line_attributions,
                );
                return Ok(Some((entry, FileLineStats::default())));
            }
            // Byte-identical — truly no change.
            return Ok(None);
        }
        // Content differs only in line endings (CRLF ↔ LF). Update the stored blob
        // to the current content so future diffs compare LF-vs-LF. Without this,
        // the stale CRLF blob causes capture_diff_slices to see every line as changed,
        // and AI checkpoints (force_split=true) would re-attribute all lines to AI.
        // Remap attributions through line-number space to adjust byte offsets.
        let line_attributions =
            crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
                &prev_attributions,
                &previous_content,
                kind.is_ai(),
            );
        let remapped_attributions =
            crate::authorship::attribution_tracker::line_attributions_to_attributions(
                &line_attributions,
                &current_content,
                ts,
            );
        let entry = WorkingLogEntry::new(
            file_path,
            file_content_hash,
            remapped_attributions,
            line_attributions,
        );
        return Ok(Some((entry, FileLineStats::default())));
    }

    let (entry, stats) = make_entry_for_file(FileEntryInput {
        file_path: &file_path,
        blob_sha: &file_content_hash,
        author_id: &file_author_id,
        is_ai_checkpoint: kind.is_ai(),
        limit_current_author_to_changed_lines: limit_current_author_to_changed_lines
            || weak_known_human_for_file
            || limit_to_changed_lines_for_known_human_on_ai_content
            || limit_to_changed_lines_for_recent_known_human_save,
        previous_content: &previous_content,
        previous_attributions: &prev_attributions,
        content: &current_content,
        ts,
    })?;
    tracing::debug!(
        "[BENCHMARK] Processing file {} took {:?}",
        file_path,
        file_start.elapsed()
    );
    Ok(Some((entry, stats)))
}

#[allow(clippy::too_many_arguments)]
async fn get_checkpoint_entries(
    kind: CheckpointKind,
    author: &str,
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    files: &[String],
    file_content_hashes: &HashMap<String, String>,
    previous_checkpoints: &[Checkpoint],
    checkpoint_request: &CheckpointRequest,
    ts: u128,
    head_commit_override: Option<&str>,
    trace_id: String,
    attest_human_lines: bool,
    limit_current_author_to_changed_lines: bool,
    preserve_empty_entries: bool,
    weak_known_human_files: HashSet<String>,
) -> Result<(Vec<WorkingLogEntry>, Vec<FileLineStats>), GitAiError> {
    let entries_fn_start = Instant::now();

    let effective_kind = if kind == CheckpointKind::KnownHuman
        && !has_known_human_editor_metadata(checkpoint_request)
        && previous_checkpoints.iter().any(|checkpoint| {
            checkpoint.kind.is_ai()
                || checkpoint
                    .entries
                    .iter()
                    .any(working_log_entry_has_non_human_attribution)
        }) {
        CheckpointKind::Human
    } else {
        kind
    };

    // Read INITIAL attributions from working log (empty if file doesn't exist)
    let initial_read_start = Instant::now();
    let initial_data = working_log.read_initial_attributions();
    let initial_snapshot_contents: HashMap<String, String> = {
        let mut map = HashMap::new();
        for file_path in initial_data.files.keys() {
            if let Some(content) =
                working_log.initial_file_content_from(&initial_data, file_path)?
            {
                map.insert(file_path.clone(), content);
            }
        }
        map
    };
    let initial_attributions = initial_data.files;
    tracing::debug!(
        "[BENCHMARK] Reading initial attributions took {:?}",
        initial_read_start.elapsed()
    );

    let precompute_start = Instant::now();
    let (mut previous_file_state_by_file, mut ai_touched_files) =
        build_previous_file_state_maps(working_log, previous_checkpoints, &initial_attributions);
    let archived_ai_state_file_count = if effective_kind == CheckpointKind::KnownHuman {
        let archived_ai_states = collect_recent_archived_ai_states(repo, files, ts);
        merge_recent_archived_ai_states(
            &mut previous_file_state_by_file,
            &mut ai_touched_files,
            archived_ai_states,
        )
    } else {
        0
    };
    if archived_ai_state_file_count > 0 {
        crate::diagnostics::append_debug_event(
            "checkpoint_archived_ai_state_merged",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "checkpointKind": effective_kind.to_str(),
                "traceId": trace_id.clone(),
                "fileCount": archived_ai_state_file_count,
            }),
        );
    }
    tracing::debug!(
        "[BENCHMARK] Precomputing previous state maps took {:?}",
        precompute_start.elapsed()
    );

    // Determine author_id based on checkpoint kind and agent_id
    let author_id = match effective_kind {
        CheckpointKind::Human if attest_human_lines => {
            crate::authorship::authorship_log_serialization::generate_human_short_hash(author)
        }
        CheckpointKind::Human => effective_kind.to_str(),
        CheckpointKind::KnownHuman => {
            crate::authorship::authorship_log_serialization::generate_human_short_hash(author)
        }
        _ => {
            // AI kinds: compose session_id::trace_id
            checkpoint_request
                .agent_id
                .as_ref()
                .map(|aid| {
                    let session_id = generate_session_id(&aid.id, &aid.tool);
                    format!("{}::{}", session_id, trace_id)
                })
                .unwrap_or_else(|| kind.to_str())
        }
    };

    // Get HEAD commit info for git operations
    let head_commit = head_commit_override
        .map(str::trim)
        .filter(|sha| !sha.is_empty() && *sha != "initial")
        .and_then(|sha| repo.find_commit(sha.to_string()).ok())
        .or_else(|| {
            repo.head()
                .ok()
                .and_then(|h| h.target().ok())
                .and_then(|oid| repo.find_commit(oid).ok())
        });
    let head_tree_id = head_commit
        .as_ref()
        .and_then(|c| c.tree().ok())
        .map(|t| t.id().to_string());

    const MAX_CONCURRENT: usize = 30;

    // Create a semaphore to limit concurrent tasks
    let semaphore = Arc::new(smol::lock::Semaphore::new(MAX_CONCURRENT));

    // Move other repeated allocations outside the loop
    let previous_file_state_by_file = Arc::new(previous_file_state_by_file);
    let ai_touched_files = Arc::new(ai_touched_files);
    let author_id = Arc::new(author_id);
    let weak_known_human_files = Arc::new(weak_known_human_files);
    let head_tree_id = Arc::new(head_tree_id);
    let initial_attributions = Arc::new(initial_attributions);
    let initial_snapshot_contents = Arc::new(initial_snapshot_contents);

    // Spawn tasks for each file
    let spawn_start = Instant::now();
    let mut tasks = Vec::new();

    for file_path in files {
        let file_path = file_path.clone();
        let repo = repo.clone();
        let working_log = working_log.clone();
        let previous_file_state_by_file = Arc::clone(&previous_file_state_by_file);
        let ai_touched_files = Arc::clone(&ai_touched_files);
        let author_id = Arc::clone(&author_id);
        let weak_known_human_files = Arc::clone(&weak_known_human_files);
        let head_tree_id = Arc::clone(&head_tree_id);
        let blob_sha = file_content_hashes
            .get(&file_path)
            .cloned()
            .unwrap_or_default();
        let initial_attributions = Arc::clone(&initial_attributions);
        let initial_snapshot_contents = Arc::clone(&initial_snapshot_contents);
        let semaphore = Arc::clone(&semaphore);

        let task = smol::spawn(async move {
            // Acquire semaphore permit to limit concurrency
            let _permit = semaphore.acquire().await;

            // Wrap all the blocking git operations in smol::unblock
            smol::unblock(move || {
                get_checkpoint_entry_for_file(
                    file_path,
                    effective_kind,
                    repo,
                    working_log,
                    previous_file_state_by_file,
                    ai_touched_files,
                    blob_sha,
                    author_id.clone(),
                    weak_known_human_files.clone(),
                    head_tree_id.clone(),
                    initial_attributions.clone(),
                    initial_snapshot_contents.clone(),
                    attest_human_lines,
                    limit_current_author_to_changed_lines,
                    preserve_empty_entries,
                    ts,
                )
            })
            .await
        });

        tasks.push(task);
    }
    tracing::debug!(
        "[BENCHMARK] Spawning {} tasks took {:?}",
        tasks.len(),
        spawn_start.elapsed()
    );

    // Await all tasks concurrently
    let await_start = Instant::now();
    let results = futures::future::join_all(tasks).await;
    tracing::debug!(
        "[BENCHMARK] Awaiting {} tasks took {:?}",
        results.len(),
        await_start.elapsed()
    );

    // Process results
    let process_start = Instant::now();
    let results_count = results.len();
    let mut entries = Vec::new();
    let mut file_stats = Vec::new();
    for result in results {
        match result {
            Ok(Some((entry, stats))) => {
                entries.push(entry);
                file_stats.push(stats);
            }
            Ok(None) => {} // File had no changes
            Err(e) => return Err(e),
        }
    }
    if preserve_empty_entries {
        let mut existing_files: HashSet<String> =
            entries.iter().map(|entry| entry.file.clone()).collect();
        for file_path in files {
            if !existing_files.insert(file_path.clone()) {
                continue;
            }
            let blob_sha = file_content_hashes
                .get(file_path)
                .cloned()
                .unwrap_or_default();
            entries.push(WorkingLogEntry::new(
                file_path.clone(),
                blob_sha,
                Vec::new(),
                Vec::new(),
            ));
        }
    }
    tracing::debug!(
        "[BENCHMARK] Processing {} results took {:?}",
        results_count,
        process_start.elapsed()
    );
    tracing::debug!(
        "[BENCHMARK] get_checkpoint_entries function total took {:?}",
        entries_fn_start.elapsed()
    );

    Ok((entries, file_stats))
}

struct FileEntryInput<'a> {
    file_path: &'a str,
    blob_sha: &'a str,
    author_id: &'a str,
    is_ai_checkpoint: bool,
    limit_current_author_to_changed_lines: bool,
    previous_content: &'a str,
    previous_attributions: &'a [Attribution],
    content: &'a str,
    ts: u128,
}

fn make_entry_for_file(
    input: FileEntryInput<'_>,
) -> Result<(WorkingLogEntry, FileLineStats), GitAiError> {
    let FileEntryInput {
        file_path,
        blob_sha,
        author_id,
        is_ai_checkpoint,
        limit_current_author_to_changed_lines,
        previous_content,
        previous_attributions,
        content,
        ts,
    } = input;

    let tracker = AttributionTracker::new();

    let fill_start = Instant::now();
    let filled_in_prev_attributions = tracker.attribute_unattributed_ranges(
        previous_content,
        previous_attributions,
        &CheckpointKind::Human.to_str(),
        ts - 1,
    );
    tracing::debug!(
        "[BENCHMARK]   attribute_unattributed_ranges for {} took {:?}",
        file_path,
        fill_start.elapsed()
    );

    let update_start = Instant::now();
    let new_attributions = tracker.update_attributions_for_checkpoint(
        previous_content,
        content,
        &filled_in_prev_attributions,
        author_id,
        ts,
        is_ai_checkpoint,
    )?;
    tracing::debug!(
        "[BENCHMARK]   update_attributions for {} took {:?}",
        file_path,
        update_start.elapsed()
    );

    // TODO Consider discarding any "uncontentious" attributions for the human author. Any human attributions that do not share a line with any other author's attributions can be discarded.
    // let filtered_attributions = crate::authorship::attribution_tracker::discard_uncontentious_attributions_for_author(&new_attributions, &CheckpointKind::Human.to_str());

    let line_attr_start = Instant::now();
    let mut line_attributions =
        crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
            &new_attributions,
            content,
            is_ai_checkpoint,
        );
    let new_attributions = if limit_current_author_to_changed_lines {
        line_attributions = restrict_current_author_attributions_to_changed_lines(
            line_attributions,
            previous_content,
            content,
            author_id,
        );
        crate::authorship::attribution_tracker::line_attributions_to_attributions(
            &line_attributions,
            content,
            ts,
        )
    } else {
        new_attributions
    };
    tracing::debug!(
        "[BENCHMARK]   attributions_to_line_attributions for {} took {:?}",
        file_path,
        line_attr_start.elapsed()
    );

    // Compute line stats while we already have both contents in memory
    let stats_start = Instant::now();
    let line_stats = compute_file_line_stats(previous_content, content);
    tracing::debug!(
        "[BENCHMARK]   compute_file_line_stats for {} took {:?}",
        file_path,
        stats_start.elapsed()
    );

    let entry = WorkingLogEntry::new(
        file_path.to_string(),
        blob_sha.to_string(),
        new_attributions,
        line_attributions,
    );

    Ok((entry, line_stats))
}

fn split_lines_preserving_terminators(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;

    for (idx, ch) in s.char_indices() {
        if ch == '\n' {
            lines.push(&s[start..idx + 1]);
            start = idx + 1;
        }
    }

    if start < s.len() {
        lines.push(&s[start..]);
    }

    lines
}

fn changed_new_line_numbers(previous_content: &str, current_content: &str) -> HashSet<u32> {
    let previous_lines = split_lines_preserving_terminators(previous_content);
    let current_lines = split_lines_preserving_terminators(current_content);
    let mut changed = HashSet::new();

    for op in capture_diff_slices(&previous_lines, &current_lines) {
        match op {
            DiffOp::Insert {
                new_index, new_len, ..
            }
            | DiffOp::Replace {
                new_index, new_len, ..
            } => {
                let start = new_index as u32 + 1;
                let end = start + new_len as u32;
                for line in start..end {
                    changed.insert(line);
                }
            }
            DiffOp::Equal { .. } | DiffOp::Delete { .. } => {}
        }
    }

    changed
}

fn restrict_current_author_attributions_to_changed_lines(
    line_attributions: Vec<LineAttribution>,
    previous_content: &str,
    current_content: &str,
    current_author_id: &str,
) -> Vec<LineAttribution> {
    let changed_lines = changed_new_line_numbers(previous_content, current_content);
    if changed_lines.is_empty() {
        return line_attributions
            .into_iter()
            .filter(|attr| attr.author_id != current_author_id)
            .collect();
    }

    let mut restricted = Vec::new();
    for attr in line_attributions {
        if attr.author_id != current_author_id {
            restricted.push(attr);
            continue;
        }

        let mut lines: Vec<u32> = (attr.start_line..=attr.end_line)
            .filter(|line| changed_lines.contains(line))
            .collect();
        lines.sort_unstable();
        lines.dedup();

        if lines.is_empty() {
            continue;
        }

        let mut range_start = lines[0];
        let mut range_end = lines[0];
        for line in lines.into_iter().skip(1) {
            if line == range_end + 1 {
                range_end = line;
            } else {
                restricted.push(LineAttribution {
                    start_line: range_start,
                    end_line: range_end,
                    author_id: attr.author_id.clone(),
                    overrode: attr.overrode.clone(),
                });
                range_start = line;
                range_end = line;
            }
        }

        restricted.push(LineAttribution {
            start_line: range_start,
            end_line: range_end,
            author_id: attr.author_id,
            overrode: attr.overrode,
        });
    }

    restricted
}

/// Compute line statistics for a single file by diffing previous and current content
#[doc(hidden)]
pub fn compute_file_line_stats(previous_content: &str, current_content: &str) -> FileLineStats {
    let mut stats = FileLineStats::default();

    // Use imara_diff to count line changes (matches git's diff algorithm)
    let changes = compute_line_changes(previous_content, current_content);
    for change in changes {
        match change.tag() {
            LineChangeTag::Insert => {
                let non_whitespace_lines = change
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count() as u32;
                stats.additions += change.value().lines().count() as u32;
                stats.additions_sloc += non_whitespace_lines;
            }
            LineChangeTag::Delete => {
                let non_whitespace_lines = change
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count() as u32;
                stats.deletions += change.value().lines().count() as u32;
                stats.deletions_sloc += non_whitespace_lines;
            }
            LineChangeTag::Equal => {}
        }
    }

    stats
}

/// Aggregate line statistics from individual file stats
/// This avoids redundant diff computation since stats are already computed during entry creation
fn compute_line_stats(
    file_stats: &[FileLineStats],
) -> Result<crate::authorship::working_log::CheckpointLineStats, GitAiError> {
    let mut stats = crate::authorship::working_log::CheckpointLineStats::default();

    // Aggregate line stats from all files
    for file_stat in file_stats {
        stats.additions += file_stat.additions;
        stats.deletions += file_stat.deletions;
        stats.additions_sloc += file_stat.additions_sloc;
        stats.deletions_sloc += file_stat.deletions_sloc;
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::orchestrator::BaseCommit;
    use crate::commands::checkpoint_agent::orchestrator::CheckpointFile;
    use std::path::PathBuf;

    fn checkpoint_request_with_metadata(
        agent_id: Option<AgentId>,
        path_role: PreparedPathRole,
        metadata: HashMap<String, String>,
    ) -> CheckpointRequest {
        CheckpointRequest {
            trace_id: "t_test".to_string(),
            checkpoint_kind: CheckpointKind::Human,
            agent_id,
            files: vec![CheckpointFile {
                path: PathBuf::from("/repo/src/main.rs"),
                content: Some(String::new()),
                repo_work_dir: PathBuf::from("/repo"),
                base_commit: BaseCommit::Sha("head".to_string()),
            }],
            path_role,
            stream_source: None,
            metadata,
        }
    }

    fn checkpoint_with_metadata(
        kind: CheckpointKind,
        file: &str,
        metadata: &[(&str, &str)],
    ) -> Checkpoint {
        let mut checkpoint = Checkpoint::new(
            kind,
            String::new(),
            "tester".to_string(),
            vec![WorkingLogEntry::new(
                file.to_string(),
                "sha".to_string(),
                Vec::new(),
                Vec::new(),
            )],
        );
        checkpoint.agent_metadata = Some(
            metadata
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        );
        checkpoint
    }

    #[test]
    fn ai_pre_edit_request_requires_agent_marker_and_will_edit_role() {
        let agent = AgentId {
            tool: "github-copilot".to_string(),
            id: "session".to_string(),
            model: "unknown".to_string(),
        };
        let metadata = HashMap::from([
            ("ai_pre_edit".to_string(), "true".to_string()),
            ("tool_use_id".to_string(), "call_1".to_string()),
        ]);

        let request = checkpoint_request_with_metadata(
            Some(agent.clone()),
            PreparedPathRole::WillEdit,
            metadata.clone(),
        );
        assert!(is_ai_pre_edit_request(CheckpointKind::Human, &request));

        let no_agent =
            checkpoint_request_with_metadata(None, PreparedPathRole::WillEdit, metadata.clone());
        assert!(!is_ai_pre_edit_request(CheckpointKind::Human, &no_agent));

        let edited_role =
            checkpoint_request_with_metadata(Some(agent), PreparedPathRole::Edited, metadata);
        assert!(!is_ai_pre_edit_request(CheckpointKind::Human, &edited_role));
    }

    #[test]
    fn ai_pre_edit_request_rejects_human_agent_tool() {
        let human_agent = AgentId {
            tool: "human".to_string(),
            id: "human".to_string(),
            model: "human".to_string(),
        };
        let request = checkpoint_request_with_metadata(
            Some(human_agent),
            PreparedPathRole::WillEdit,
            HashMap::from([
                ("ai_pre_edit".to_string(), "true".to_string()),
                ("agent_tool".to_string(), "human".to_string()),
                ("tool_use_id".to_string(), "call_human".to_string()),
            ]),
        );

        assert!(!is_ai_pre_edit_request(CheckpointKind::Human, &request));
    }

    #[test]
    fn unclosed_ai_pre_edit_blocks_known_human_attestation() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        assert!(has_unclosed_ai_pre_edit_for_files(
            &[pre_edit],
            &["src/main.rs".to_string()]
        ));

        let ai_edit = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/main.rs",
            &[("edit_kind", "file_edit"), ("tool_use_id", "call_1")],
        );
        assert!(!has_unclosed_ai_pre_edit_for_files(
            &[
                checkpoint_with_metadata(
                    CheckpointKind::Human,
                    "src/main.rs",
                    &[
                        ("ai_pre_edit", "true"),
                        ("edit_kind", "file_edit"),
                        ("tool_use_id", "call_1"),
                    ],
                ),
                ai_edit,
            ],
            &["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn human_agent_tool_metadata_is_not_unclosed_ai_pre_edit() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_human"),
                ("agent_tool", "human"),
            ],
        );

        assert!(!has_unclosed_ai_pre_edit_for_files(
            &[pre_edit],
            &["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn unclosed_ai_pre_edit_tracks_tool_use_by_file() {
        let pre_edit_a = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/a.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        let pre_edit_b = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/b.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        let ai_edit_a = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/a.rs",
            &[("edit_kind", "file_edit"), ("tool_use_id", "call_1")],
        );

        assert!(!has_unclosed_ai_pre_edit_for_files(
            &[pre_edit_a.clone(), pre_edit_b.clone(), ai_edit_a.clone()],
            &["src/a.rs".to_string()]
        ));
        assert!(has_unclosed_ai_pre_edit_for_files(
            &[pre_edit_a, pre_edit_b, ai_edit_a],
            &["src/b.rs".to_string()]
        ));
    }

    #[test]
    fn ai_pre_edit_close_clears_matching_tool_use() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        let closed = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit_closed", "true"),
                ("tool_use_id", "call_1"),
                ("close_reason", "bash_no_changes"),
            ],
        );

        assert!(!has_unclosed_ai_pre_edit_for_files(
            &[pre_edit, closed],
            &["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn ai_pre_edit_close_does_not_clear_other_tool_use() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        let closed = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit_closed", "true"),
                ("tool_use_id", "call_2"),
                ("close_reason", "bash_no_changes"),
            ],
        );

        assert!(has_unclosed_ai_pre_edit_for_files(
            &[pre_edit, closed],
            &["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn entries_for_unclosed_ai_pre_edit_close_inherits_prior_attribution() {
        let prior_ai = {
            let mut checkpoint = checkpoint_with_metadata(
                CheckpointKind::AiAgent,
                "src/main.rs",
                &[("edit_kind", "file_edit"), ("tool_use_id", "prior")],
            );
            checkpoint.entries[0].blob_sha = "same-sha".to_string();
            checkpoint.entries[0].attributions = vec![Attribution {
                start: 0,
                end: 7,
                author_id: "s_ai::t_ai".to_string(),
                ts: 1,
            }];
            checkpoint
        };
        let mut pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/main.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        pre_edit.entries[0].blob_sha = "same-sha".to_string();

        let entries = entries_for_unclosed_ai_pre_edit_tool_use(&[prior_ai, pre_edit], "call_1");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].blob_sha, "same-sha");
        assert_eq!(entries[0].attributions.len(), 1);
        assert_eq!(entries[0].attributions[0].author_id, "s_ai::t_ai");
    }

    #[test]
    fn unclosed_ai_pre_edit_ignores_other_files() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/ai.rs",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        assert!(!has_unclosed_ai_pre_edit_for_files(
            &[pre_edit],
            &["src/human.rs".to_string()]
        ));
    }

    #[test]
    fn archived_ai_state_merge_adds_ai_history_for_missing_base() {
        let working_log = PersistedWorkingLog::new(
            PathBuf::from("/repo/.git/ai/working_logs/current"),
            "current",
            PathBuf::from("/repo"),
            PathBuf::from("/repo"),
            None,
        );
        let archived_log = PersistedWorkingLog::new(
            PathBuf::from("/repo/.git/ai/working_logs/old-base"),
            "old-base",
            PathBuf::from("/repo"),
            PathBuf::from("/repo"),
            None,
        );
        let ai_state = PreviousFileState {
            blob_sha: "sha".to_string(),
            attributions: vec![Attribution {
                start: 0,
                end: 7,
                author_id: "s_session::t_trace".to_string(),
                ts: 1,
            }],
            kind: CheckpointKind::AiAgent,
            timestamp: 10,
            skip_as_ai_baseline: false,
            source_working_log: archived_log,
        };

        let (mut states, mut ai_files) =
            build_previous_file_state_maps(&working_log, &[], &HashMap::new());
        let merged = merge_recent_archived_ai_states(
            &mut states,
            &mut ai_files,
            HashMap::from([("src/main.rs".to_string(), ai_state)]),
        );

        assert_eq!(merged, 1);
        assert!(ai_files.contains("src/main.rs"));
        assert!(
            states
                .get("src/main.rs")
                .is_some_and(|states| states.iter().any(|state| state.kind.is_ai()))
        );
    }

    #[test]
    fn archived_ai_state_merge_does_not_replace_current_base_ai_history() {
        let working_log = PersistedWorkingLog::new(
            PathBuf::from("/repo/.git/ai/working_logs/current"),
            "current",
            PathBuf::from("/repo"),
            PathBuf::from("/repo"),
            None,
        );
        let archived_log = PersistedWorkingLog::new(
            PathBuf::from("/repo/.git/ai/working_logs/old-base"),
            "old-base",
            PathBuf::from("/repo"),
            PathBuf::from("/repo"),
            None,
        );
        let current_ai = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/main.rs",
            &[("edit_kind", "file_edit"), ("tool_use_id", "call_current")],
        );
        let archived_ai_state = PreviousFileState {
            blob_sha: "sha".to_string(),
            attributions: vec![Attribution {
                start: 0,
                end: 7,
                author_id: "s_old::t_old".to_string(),
                ts: 1,
            }],
            kind: CheckpointKind::AiAgent,
            timestamp: 10,
            skip_as_ai_baseline: false,
            source_working_log: archived_log,
        };

        let (mut states, mut ai_files) =
            build_previous_file_state_maps(&working_log, &[current_ai], &HashMap::new());
        let merged = merge_recent_archived_ai_states(
            &mut states,
            &mut ai_files,
            HashMap::from([("src/main.rs".to_string(), archived_ai_state)]),
        );

        assert_eq!(merged, 0);
        assert_eq!(states.get("src/main.rs").map(Vec::len), Some(1));
    }

    #[test]
    fn archived_ai_state_reconstructs_from_line_attributions_and_source_blob() {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("repo dir");

        let archived_log = PersistedWorkingLog::new(
            temp.path().join("working_logs").join("old-base"),
            "base",
            repo_root.clone(),
            repo_root,
            None,
        );
        let archived_content = "ai line 1\nai line 2\n";
        let blob_sha = archived_log
            .persist_file_version(archived_content)
            .expect("persist archived blob");
        let entry = WorkingLogEntry::new(
            "src/main.rs".to_string(),
            blob_sha,
            Vec::new(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 2,
                author_id: "s_archived::t_ai".to_string(),
                overrode: None,
            }],
        );

        let attrs = previous_file_state_attributions(&entry, &archived_log, 1234);

        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].start, 0);
        assert_eq!(attrs[0].end, archived_content.len());
        assert_eq!(attrs[0].author_id, "s_archived::t_ai");
        assert_eq!(attrs[0].ts, 1234);
    }
}
