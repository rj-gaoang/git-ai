use crate::api::{ApiClient, ApiContext};
use crate::authorship::archived_ai_state::{
    ArchivedAiFileState, collect_recent_archived_ai_states,
    is_ai_author_id as archived_is_ai_author_id,
};
use crate::authorship::authorship_log::{HumanRecord, LineRange, SessionRecord};
use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
use crate::authorship::ignore::{
    build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
};
use crate::authorship::prompt_utils::{PromptUpdateResult, update_prompt_from_tool};
use crate::authorship::secrets::{
    redact_secrets_from_prompts, retain_user_prompt_messages, strip_prompt_messages,
};
use crate::authorship::stats::{stats_for_commit_stats_from_hunks, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::{AgentId, Checkpoint, CheckpointKind, WorkingLogEntry};
use crate::config::{Config, PromptStorageMode};
use crate::error::GitAiError;
use crate::git::notes_api::write_note as notes_add;
use crate::git::repo_storage::PersistedWorkingLog;
use crate::git::repository::Repository;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::IsTerminal;

/// Skip expensive post-commit stats when this threshold is exceeded.
/// High hunk density is the strongest predictor of slow diff_ai_accepted_stats.
#[doc(hidden)]
pub const STATS_SKIP_MAX_HUNKS: usize = 1000;
/// Skip expensive stats for very large net additions even if hunks are moderate.
#[doc(hidden)]
pub const STATS_SKIP_MAX_ADDED_LINES: usize = 6000;
/// Skip expensive stats for extremely wide commits touching many added-line files.
#[doc(hidden)]
pub const STATS_SKIP_MAX_FILES_WITH_ADDITIONS: usize = 200;
/// Skip expensive stats for commits that delete a large number of lines.
/// Deletion-heavy commits (e.g. removing many files) trigger the same expensive
/// diff-parsing path as large addition commits, but the added-lines estimate is
/// near zero, so the cost was previously invisible to the estimator.
#[doc(hidden)]
pub const STATS_SKIP_MAX_DELETED_LINES: usize = 6000;

pub struct RepairAuthorshipNoteResult {
    pub commit_sha: String,
    pub parent_sha: String,
    pub wrote_note: bool,
    pub stats: crate::authorship::stats::CommitStats,
    pub authorship_log: AuthorshipLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairWorkingLogSource {
    Active,
    Archived,
}

impl RepairWorkingLogSource {
    fn as_str(self) -> &'static str {
        match self {
            RepairWorkingLogSource::Active => "active",
            RepairWorkingLogSource::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct StatsCostEstimate {
    pub files_with_additions: usize,
    pub added_lines: usize,
    pub hunk_ranges: usize,
    pub deleted_lines: usize,
}

fn checkpoint_entry_requires_post_processing(
    checkpoint: &Checkpoint,
    entry: &WorkingLogEntry,
) -> bool {
    if checkpoint.kind != CheckpointKind::Human {
        return true;
    }

    entry
        .line_attributions
        .iter()
        .any(|attr| attr.author_id != CheckpointKind::Human.to_str() || attr.overrode.is_some())
        || entry
            .attributions
            .iter()
            .any(|attr| attr.author_id != CheckpointKind::Human.to_str())
}

fn is_ai_author_id(author_id: &str) -> bool {
    archived_is_ai_author_id(author_id)
}

fn checkpoint_entry_has_ai_path_evidence(checkpoint: &Checkpoint, entry: &WorkingLogEntry) -> bool {
    checkpoint.kind.is_ai()
        || entry
            .line_attributions
            .iter()
            .any(|attr| is_ai_author_id(&attr.author_id))
        || entry
            .attributions
            .iter()
            .any(|attr| is_ai_author_id(&attr.author_id))
}

fn checkpoint_tool_use_id(checkpoint: &Checkpoint) -> Option<&str> {
    checkpoint
        .agent_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("tool_use_id"))
        .map(String::as_str)
        .filter(|tool_use_id| !tool_use_id.trim().is_empty())
}

fn collect_ai_edited_tool_use_files(checkpoints: &[Checkpoint]) -> HashSet<(String, String)> {
    let mut tool_use_files = HashSet::new();
    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.kind.is_ai())
        .filter(|checkpoint| {
            !checkpoint
                .agent_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("edit_kind"))
                .is_some_and(|edit_kind| edit_kind.eq_ignore_ascii_case("bash"))
        })
    {
        let Some(tool_use_id) = checkpoint_tool_use_id(checkpoint) else {
            continue;
        };
        for entry in &checkpoint.entries {
            tool_use_files.insert((tool_use_id.to_string(), entry.file.clone()));
        }
    }
    tool_use_files
}

fn is_human_tool_name(tool: &str) -> bool {
    let tool = tool.trim();
    tool.eq_ignore_ascii_case("human") || tool.eq_ignore_ascii_case("known_human")
}

fn ai_pre_edit_metadata_allowed(metadata: &HashMap<String, String>) -> bool {
    !metadata
        .get("agent_tool")
        .is_some_and(|tool| is_human_tool_name(tool))
}

fn checkpoint_has_ai_pre_edit_path_evidence(
    checkpoint: &Checkpoint,
    entry: &WorkingLogEntry,
    ai_edited_tool_use_files: &HashSet<(String, String)>,
) -> bool {
    if checkpoint.kind != CheckpointKind::Human {
        return false;
    }

    let Some(metadata) = checkpoint.agent_metadata.as_ref() else {
        return false;
    };

    if !metadata
        .get("ai_pre_edit")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return false;
    }

    if !ai_pre_edit_metadata_allowed(metadata) {
        return false;
    }

    if metadata
        .get("edit_kind")
        .is_some_and(|edit_kind| edit_kind.eq_ignore_ascii_case("bash"))
    {
        return false;
    }

    checkpoint_tool_use_id(checkpoint).is_some_and(|tool_use_id| {
        ai_edited_tool_use_files.contains(&(tool_use_id.to_string(), entry.file.clone()))
    })
}

fn is_plain_legacy_human_checkpoint(checkpoint: &Checkpoint) -> bool {
    if checkpoint.kind != CheckpointKind::Human || checkpoint.agent_id.is_some() {
        return false;
    }

    let Some(metadata) = checkpoint.agent_metadata.as_ref() else {
        return true;
    };

    metadata.is_empty()
}

pub fn post_commit(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
) -> Result<(String, AuthorshipLog), GitAiError> {
    post_commit_with_final_state(
        repo,
        base_commit,
        commit_sha,
        human_author,
        supress_output,
        None,
    )
}

pub fn post_commit_with_final_state(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<(String, AuthorshipLog), GitAiError> {
    // Use base_commit parameter if provided, otherwise use "initial" for empty repos
    // This matches the convention in checkpoint.rs
    let parent_sha = base_commit.unwrap_or_else(|| "initial".to_string());
    crate::diagnostics::append_debug_event(
        "post_commit_started",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "humanAuthor": human_author,
            "suppressOutput": supress_output,
            "hasFinalStateOverride": final_state_override.is_some(),
        }),
    );

    // Initialize the new storage system
    let repo_storage = &repo.storage;
    let working_log = repo_storage.working_log_for_base_commit(&parent_sha)?;

    // Refresh prompts/transcripts under the same checkpoints lock used by append_checkpoint so
    // concurrent checkpoint appends cannot be lost between a read and rewrite of the JSONL file.
    let parent_working_log = working_log.mutate_all_checkpoints(|checkpoints| {
        update_prompts_to_latest(checkpoints)?;
        Ok(())
    })?;
    let checkpoint_summary = checkpoint_input_debug_summary(&parent_working_log);
    crate::diagnostics::append_debug_event(
        "post_commit_working_log_loaded",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "checkpointCount": parent_working_log.len(),
            "checkpointEntryCount": parent_working_log.iter().map(|checkpoint| checkpoint.entries.len()).sum::<usize>(),
            "checkpointSummary": checkpoint_summary.clone(),
        }),
    );

    // Batch upsert all prompts to database after refreshing (non-fatal if it fails)
    if let Err(e) = batch_upsert_prompts_to_db(&parent_working_log, &working_log, &commit_sha) {
        tracing::debug!(
            "[Warning] Failed to batch upsert prompts to database: {}",
            e
        );
        crate::observability::log_error(
            &e,
            Some(serde_json::json!({
                "operation": "post_commit_batch_upsert",
                "commit_sha": commit_sha
            })),
        );
    }

    // Create VirtualAttributions from working log (fast path - no blame)
    // We don't need to run blame because we only care about the working log data
    // that was accumulated since the parent commit
    let working_va = if let Some(snapshot) = final_state_override {
        VirtualAttributions::from_working_log_snapshot(
            repo.clone(),
            parent_sha.clone(),
            Some(human_author.clone()),
            snapshot,
        )?
    } else {
        VirtualAttributions::from_just_working_log(
            repo.clone(),
            parent_sha.clone(),
            Some(human_author.clone()),
        )?
    };

    // Build pathspecs from AI-relevant checkpoint entries only.
    // Human-only entries with no AI attribution do not affect authorship output and should not
    // trigger expensive post-commit diff work across large commits.
    let mut pathspecs: HashSet<String> = HashSet::new();
    let mut ai_gap_fill_pathspecs: HashSet<String> = HashSet::new();
    let ai_edited_tool_use_files = collect_ai_edited_tool_use_files(&parent_working_log);
    for checkpoint in &parent_working_log {
        for entry in &checkpoint.entries {
            if checkpoint_entry_requires_post_processing(checkpoint, entry) {
                pathspecs.insert(entry.file.clone());
            }
            if checkpoint_entry_has_ai_path_evidence(checkpoint, entry) {
                ai_gap_fill_pathspecs.insert(entry.file.clone());
            } else if checkpoint_has_ai_pre_edit_path_evidence(
                checkpoint,
                entry,
                &ai_edited_tool_use_files,
            ) {
                ai_gap_fill_pathspecs.insert(entry.file.clone());
            }
        }
    }

    // Also include files from INITIAL attributions (uncommitted files from previous commits)
    // These files may not have checkpoints but still need their attribution preserved
    // when they are finally committed. See issue #356.
    let initial_attributions_for_pathspecs = working_log.read_initial_attributions();
    for file_path in initial_attributions_for_pathspecs.files.keys() {
        pathspecs.insert(file_path.clone());
    }
    crate::diagnostics::append_debug_event(
        "post_commit_pathspecs_prepared",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "pathspecCount": pathspecs.len(),
            "aiGapFillPathspecCount": ai_gap_fill_pathspecs.len(),
            "initialAttributionFileCount": initial_attributions_for_pathspecs.files.len(),
        }),
    );

    let (mut authorship_log, initial_attributions) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            &commit_sha,
            Some(&pathspecs),
            final_state_override,
        )?;

    fill_ai_attribution_gaps_for_commit(
        repo,
        &parent_sha,
        &commit_sha,
        &mut authorship_log,
        &ai_gap_fill_pathspecs,
        &parent_working_log,
        &human_author,
    );
    fill_observed_human_gaps_for_commit(
        repo,
        &parent_sha,
        &commit_sha,
        &mut authorship_log,
        &parent_working_log,
        &human_author,
    );
    fill_legacy_human_manual_gaps_for_commit(
        repo,
        &parent_sha,
        &commit_sha,
        &mut authorship_log,
        &parent_working_log,
        &human_author,
    );
    fill_legacy_human_ai_gaps_for_commit(
        repo,
        &parent_sha,
        &commit_sha,
        &mut authorship_log,
        &parent_working_log,
        &human_author,
    );
    restore_archived_ai_attributions_for_commit(
        repo,
        &parent_sha,
        &commit_sha,
        &mut authorship_log,
        &parent_working_log,
        &human_author,
    );

    crate::diagnostics::append_debug_event(
        "post_commit_authorship_log_built",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "attestationFileCount": authorship_log.attestations.len(),
            "promptSummary": prompt_debug_summary(&authorship_log),
            "initialCarryOverFileCount": initial_attributions.files.len(),
            "initialCarryOverPromptCount": initial_attributions.prompts.len(),
            "initialCarryOverHumanCount": initial_attributions.humans.len(),
        }),
    );

    authorship_log.metadata.base_commit_sha = commit_sha.clone();
    authorship_log.ensure_x_user_id_from_repo(repo);

    // No-hooks background agents (Devin, Codex Cloud, etc.) may not fire checkpoints
    // for all edits. Attribute any committed lines that have no existing attestation
    // ("holes") to the detected agent, preserving explicit attributions.
    if !matches!(
        crate::authorship::background_agent::detect(),
        crate::authorship::background_agent::BackgroundAgent::None
            | crate::authorship::background_agent::BackgroundAgent::WithHooks { .. }
    ) {
        let diff_base = if parent_sha == "initial" {
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        } else {
            &parent_sha
        };
        if let Ok(added_lines) = repo.diff_added_lines(diff_base, &commit_sha, None) {
            let committed_hunks: HashMap<
                String,
                Vec<crate::authorship::authorship_log::LineRange>,
            > = added_lines
                .into_iter()
                .filter(|(_, lines)| !lines.is_empty())
                .map(|(path, lines)| {
                    (
                        path,
                        crate::authorship::authorship_log::LineRange::compress_lines(&lines),
                    )
                })
                .collect();
            crate::authorship::background_agent::fill_unattributed_lines(
                &mut authorship_log,
                &committed_hunks,
                &human_author,
            );
        }
    }

    // Long-lived daemon processes should read a fresh config snapshot.
    // Always use Config::fresh() to support runtime config updates
    // (especially important for daemon mode, but also good for consistency)
    let config = Config::fresh();
    let effective_storage = config.effective_prompt_storage(&Some(repo.clone()));
    let using_custom_api = config.api_base_url() != crate::config::DEFAULT_API_BASE_URL;
    let custom_attrs = config.custom_attributes().clone();

    // Inject custom attributes into all PromptRecords and SessionRecords.
    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
        for sr in authorship_log.metadata.sessions.values_mut() {
            sr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    // Persist and upload only the user's prompt inputs, not assistant/tool outputs.
    retain_user_prompt_messages(&mut authorship_log.metadata.prompts);

    match effective_storage {
        PromptStorageMode::Local => {
            // Local only: strip all messages from notes (they stay in sqlite only)
            strip_prompt_messages(&mut authorship_log.metadata.prompts);
        }
        PromptStorageMode::Notes => {
            // Store in notes: redact secrets but keep messages in notes
            let count = redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
            if count > 0 {
                tracing::debug!("Redacted {} secrets from prompts", count);
            }
        }
        PromptStorageMode::Default => {
            // "default" - attempt CAS upload, NEVER keep messages in notes
            // Check conditions for CAS upload:
            // - user is logged in OR has API key OR using custom API URL
            let context = ApiContext::new(None);
            let client = ApiClient::new(context);
            let should_enqueue_cas =
                client.is_logged_in() || client.has_api_key() || using_custom_api;

            if should_enqueue_cas {
                // Redact secrets before uploading to CAS
                let redaction_count =
                    redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
                if redaction_count > 0 {
                    tracing::debug!(
                        "Redacted {} secrets from prompts before CAS upload",
                        redaction_count
                    );
                }

                if let Err(e) =
                    enqueue_prompt_messages_to_cas(repo, &mut authorship_log.metadata.prompts)
                {
                    tracing::debug!("[Warning] Failed to enqueue prompt messages to CAS: {}", e);
                    // Enqueue failed - still strip messages (never keep in notes for "default")
                    strip_prompt_messages(&mut authorship_log.metadata.prompts);
                }
                // Success: enqueue function already cleared messages
            } else {
                // Not enqueueing - strip messages (never keep in notes for "default")
                strip_prompt_messages(&mut authorship_log.metadata.prompts);
            }
        }
    }

    let authorship_note_str = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    crate::diagnostics::append_debug_event(
        "post_commit_authorship_note_write_started",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "authorshipJsonBytes": authorship_note_str.len(),
        }),
    );
    notes_add(repo, &commit_sha, &authorship_note_str)?;
    crate::diagnostics::append_debug_event(
        "post_commit_authorship_note_written",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "humanAuthor": human_author,
            "effectivePromptStorage": effective_storage.as_str(),
            "authorshipJsonBytes": authorship_note_str.len(),
            "promptSummary": prompt_debug_summary(&authorship_log),
            "attestationFileCount": authorship_log.attestations.len(),
        }),
    );

    // Compute stats once (needed for both metrics and terminal output), unless preflight
    // estimate predicts this would be too expensive for the commit hook path.
    let mut stats: Option<crate::authorship::stats::CommitStats> = None;
    let is_merge_commit = repo
        .find_commit(commit_sha.clone())
        .map(|commit| commit.parent_count().unwrap_or(0) > 1)
        .unwrap_or(false);
    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let skip_reason = if is_merge_commit {
        Some(StatsSkipReason::MergeCommit)
    } else {
        estimate_stats_cost(repo, &parent_sha, &commit_sha, &ignore_patterns)
            .ok()
            .and_then(|estimate| {
                if should_skip_expensive_post_commit_stats(&estimate) {
                    Some(StatsSkipReason::Expensive(estimate))
                } else {
                    None
                }
            })
    };

    if skip_reason.is_none() {
        crate::diagnostics::append_debug_event(
            "post_commit_stats_compute_started",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "commitSha": commit_sha,
                "parentSha": parent_sha,
                "ignorePatternCount": ignore_patterns.len(),
            }),
        );
        let diff_base = if parent_sha == "initial" {
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        } else {
            &parent_sha
        };

        let diff_hunks =
            crate::commands::diff::get_diff_with_line_numbers(repo, diff_base, &commit_sha)?;

        let computed = stats_for_commit_stats_from_hunks(
            repo,
            &commit_sha,
            &ignore_patterns,
            &diff_hunks,
            Some(&authorship_log),
        )?;

        let hunks_json = crate::commands::diff::build_diff_artifacts_from_hunks(
            repo,
            diff_hunks,
            &commit_sha,
            Some(&authorship_log),
        )
        .ok()
        .and_then(|artifacts| serde_json::to_string(&artifacts.json_hunks).ok());

        crate::diagnostics::append_debug_event(
            "post_commit_stats_computed",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "commitSha": commit_sha,
                "parentSha": parent_sha,
                "statsSummary": commit_stats_debug_summary(&computed),
            }),
        );
        if let Some(gap_reason) = attribution_gap_reason(&computed) {
            crate::diagnostics::append_debug_event(
                "post_commit_attribution_gap_detected",
                serde_json::json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "parentSha": parent_sha,
                    "reason": gap_reason,
                    "statsSummary": commit_stats_debug_summary(&computed),
                    "promptSummary": prompt_debug_summary(&authorship_log),
                    "sessionCount": authorship_log.metadata.sessions.len(),
                    "attestationFileCount": authorship_log.attestations.len(),
                    "pathspecCount": pathspecs.len(),
                    "initialCarryOverFileCount": initial_attributions.files.len(),
                    "initialCarryOverPromptCount": initial_attributions.prompts.len(),
                    "initialCarryOverHumanCount": initial_attributions.humans.len(),
                    "checkpointSummary": checkpoint_summary.clone(),
                }),
            );
        }
        // Record metrics only when we have full stats.
        record_commit_metrics(
            repo,
            &commit_sha,
            &parent_sha,
            &human_author,
            &authorship_note_str,
            &computed,
            &parent_working_log,
            hunks_json.as_deref(),
        );
        stats = Some(computed);
    } else {
        match skip_reason.as_ref() {
            Some(StatsSkipReason::MergeCommit) => {
                tracing::debug!("Skipping post-commit stats for merge commit {}", commit_sha);
                log_post_commit_stats_skipped(repo, &commit_sha, &parent_sha, "merge_commit", None);
            }
            Some(StatsSkipReason::Expensive(estimate)) => {
                tracing::debug!(
                    "Skipping expensive post-commit stats for {} (files_with_additions={}, added_lines={}, deleted_lines={}, hunks={})",
                    commit_sha,
                    estimate.files_with_additions,
                    estimate.added_lines,
                    estimate.deleted_lines,
                    estimate.hunk_ranges
                );
                log_post_commit_stats_skipped(
                    repo,
                    &commit_sha,
                    &parent_sha,
                    "expensive_commit",
                    Some(estimate),
                );
            }
            None => {}
        }
    }

    // Write INITIAL file for uncommitted AI attributions (if any)
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo_storage.working_log_for_base_commit(&commit_sha)?;
        let initial_file_contents =
            working_va.snapshot_contents_for_files(initial_attributions.files.keys());
        let initial_file_count = initial_attributions.files.len();
        let initial_prompt_count = initial_attributions.prompts.len();
        let initial_human_count = initial_attributions.humans.len();
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
        )?;
        crate::diagnostics::append_debug_event(
            "post_commit_initial_attributions_written",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "commitSha": commit_sha,
                "parentSha": parent_sha,
                "fileCount": initial_file_count,
                "promptCount": initial_prompt_count,
                "humanCount": initial_human_count,
            }),
        );
    }

    // // Clean up old working log
    repo_storage.delete_working_log_for_base_commit(&parent_sha)?;
    crate::diagnostics::append_debug_event(
        "post_commit_working_log_deleted",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "archiveBase": format!("old-{}", parent_sha),
            "archiveRetentionSecs": crate::git::repo_storage::RepoStorage::OLD_WORKING_LOG_RETENTION_SECS,
        }),
    );

    // Use Config::fresh() to support runtime config updates
    if !supress_output && !Config::fresh().is_quiet() {
        // Only print stats if we're in an interactive terminal and quiet mode is disabled
        let is_interactive = std::io::stdout().is_terminal();
        if let Some(stats) = stats.as_ref() {
            write_stats_to_terminal(stats, is_interactive);
        } else {
            match skip_reason.as_ref() {
                Some(StatsSkipReason::MergeCommit) => {
                    eprintln!(
                        "[git-ai] Skipped git-ai stats for merge commit {}.",
                        commit_sha
                    );
                }
                Some(StatsSkipReason::Expensive(estimate)) => {
                    eprintln!(
                        "[git-ai] Skipped git-ai stats for large commit (files_with_additions={}, added_lines={}, deleted_lines={}, hunks={}). Run `git-ai stats {}` to compute stats on demand.",
                        estimate.files_with_additions,
                        estimate.added_lines,
                        estimate.deleted_lines,
                        estimate.hunk_ranges,
                        commit_sha
                    );
                }
                None => {}
            }
        }
    }

    let will_recompute_missing_stats_for_upload =
        matches!(skip_reason.as_ref(), Some(StatsSkipReason::Expensive(_)));

    // Best-effort upload of authorship stats to the team-managed remote.
    // Always non-blocking and silent on failure so it cannot disrupt commits.
    crate::diagnostics::append_debug_event(
        "post_commit_upload_dispatch_requested",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "hasStats": stats.is_some(),
            "statsSkipReason": stats_skip_reason_debug(skip_reason.as_ref()),
            "willRecomputeMissingStatsForUpload": will_recompute_missing_stats_for_upload,
        }),
    );
    crate::integration::upload_stats::maybe_upload_after_commit(
        repo,
        &commit_sha,
        &authorship_log,
        stats.as_ref(),
        will_recompute_missing_stats_for_upload,
        &ignore_patterns,
    );

    Ok((commit_sha.to_string(), authorship_log))
}

pub fn repair_authorship_note_from_archived_working_log(
    repo: &Repository,
    commit_sha: &str,
    human_author: String,
    write_note: bool,
) -> Result<RepairAuthorshipNoteResult, GitAiError> {
    let commit = repo.revparse_single(commit_sha)?.peel_to_commit()?;
    if commit.parent_count()? > 1 {
        return Err(GitAiError::Generic(
            "repair-authorship-note does not support merge commits".to_string(),
        ));
    }

    let resolved_commit = commit.id();
    let parent_sha = if commit.parent_count()? == 0 {
        "initial".to_string()
    } else {
        commit.parent(0)?.id()
    };
    let active_available = repo.storage.has_working_log(&parent_sha);
    let (working_log, working_log_source) = if active_available {
        (
            repo.storage.working_log_for_base_commit(&parent_sha)?,
            RepairWorkingLogSource::Active,
        )
    } else {
        (
            repo.storage
                .archived_working_log_for_base_commit(&parent_sha)?,
            RepairWorkingLogSource::Archived,
        )
    };
    crate::diagnostics::append_debug_event(
        "repair_authorship_note_working_log_loaded",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": resolved_commit,
            "parentSha": parent_sha,
            "workingLogSource": working_log_source.as_str(),
        }),
    );
    let final_state = final_state_snapshot_for_working_log(repo, &resolved_commit, &working_log)?;
    let (mut authorship_log, initial_attributions, initial_file_contents) =
        build_authorship_log_from_working_log(
            repo,
            &parent_sha,
            &resolved_commit,
            &human_author,
            &working_log,
            Some(&final_state),
        )?;

    apply_note_storage_policy(repo, &mut authorship_log)?;

    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        &parent_sha
    };
    let diff_hunks =
        crate::commands::diff::get_diff_with_line_numbers(repo, diff_base, &resolved_commit)?;
    let stats = stats_for_commit_stats_from_hunks(
        repo,
        &resolved_commit,
        &ignore_patterns,
        &diff_hunks,
        Some(&authorship_log),
    )?;

    if write_note {
        let authorship_note_str = authorship_log
            .serialize_to_string()
            .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
        notes_add(repo, &resolved_commit, &authorship_note_str)?;
        crate::diagnostics::append_debug_event(
            "repair_authorship_note_written",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "commitSha": resolved_commit,
                "parentSha": parent_sha,
                "workingLogSource": working_log_source.as_str(),
                "authorshipJsonBytes": authorship_note_str.len(),
                "statsSummary": commit_stats_debug_summary(&stats),
                "promptSummary": prompt_debug_summary(&authorship_log),
                "attestationFileCount": authorship_log.attestations.len(),
            }),
        );

        if working_log_source == RepairWorkingLogSource::Active {
            if !initial_attributions.files.is_empty() {
                let new_working_log = repo.storage.working_log_for_base_commit(&resolved_commit)?;
                new_working_log.write_initial_attributions_with_contents(
                    initial_attributions.files,
                    initial_attributions.prompts,
                    initial_attributions.humans,
                    initial_file_contents,
                    initial_attributions.sessions,
                )?;
                crate::diagnostics::append_debug_event(
                    "repair_authorship_note_initial_attributions_written",
                    serde_json::json!({
                        "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                        "commitSha": resolved_commit,
                        "parentSha": parent_sha,
                    }),
                );
            }
            repo.storage
                .delete_working_log_for_base_commit(&parent_sha)?;
            crate::diagnostics::append_debug_event(
                "repair_authorship_note_working_log_archived",
                serde_json::json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "commitSha": resolved_commit,
                    "parentSha": parent_sha,
                    "archiveBase": format!("old-{}", parent_sha),
                }),
            );
        }
    }

    Ok(RepairAuthorshipNoteResult {
        commit_sha: resolved_commit,
        parent_sha,
        wrote_note: write_note,
        stats,
        authorship_log,
    })
}

fn build_authorship_log_from_working_log(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    human_author: &str,
    working_log: &PersistedWorkingLog,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<
    (
        AuthorshipLog,
        crate::git::repo_storage::InitialAttributions,
        HashMap<String, String>,
    ),
    GitAiError,
> {
    let parent_working_log = working_log.read_all_checkpoints().unwrap_or_default();
    let mut pathspecs: HashSet<String> = HashSet::new();
    let mut ai_gap_fill_pathspecs: HashSet<String> = HashSet::new();
    let ai_edited_tool_use_files = collect_ai_edited_tool_use_files(&parent_working_log);
    for checkpoint in &parent_working_log {
        for entry in &checkpoint.entries {
            if checkpoint_entry_requires_post_processing(checkpoint, entry) {
                pathspecs.insert(entry.file.clone());
            }
            if checkpoint_entry_has_ai_path_evidence(checkpoint, entry) {
                ai_gap_fill_pathspecs.insert(entry.file.clone());
            } else if checkpoint_has_ai_pre_edit_path_evidence(
                checkpoint,
                entry,
                &ai_edited_tool_use_files,
            ) {
                ai_gap_fill_pathspecs.insert(entry.file.clone());
            }
        }
    }

    let initial_attributions_for_pathspecs = working_log.read_initial_attributions();
    for file_path in initial_attributions_for_pathspecs.files.keys() {
        pathspecs.insert(file_path.clone());
    }

    let working_va = if let Some(snapshot) = final_state_override {
        VirtualAttributions::from_working_log_snapshot_with_log(
            repo.clone(),
            parent_sha.to_string(),
            Some(human_author.to_string()),
            snapshot,
            working_log,
        )?
    } else {
        VirtualAttributions::from_just_working_log(
            repo.clone(),
            parent_sha.to_string(),
            Some(human_author.to_string()),
        )?
    };

    let (mut authorship_log, initial_attributions) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            parent_sha,
            commit_sha,
            Some(&pathspecs),
            final_state_override,
        )?;

    fill_ai_attribution_gaps_for_commit(
        repo,
        parent_sha,
        commit_sha,
        &mut authorship_log,
        &ai_gap_fill_pathspecs,
        &parent_working_log,
        human_author,
    );
    fill_observed_human_gaps_for_commit(
        repo,
        parent_sha,
        commit_sha,
        &mut authorship_log,
        &parent_working_log,
        human_author,
    );
    fill_legacy_human_manual_gaps_for_commit(
        repo,
        parent_sha,
        commit_sha,
        &mut authorship_log,
        &parent_working_log,
        human_author,
    );
    fill_legacy_human_ai_gaps_for_commit(
        repo,
        parent_sha,
        commit_sha,
        &mut authorship_log,
        &parent_working_log,
        human_author,
    );

    authorship_log.metadata.base_commit_sha = commit_sha.to_string();
    authorship_log.ensure_x_user_id_from_repo(repo);

    let initial_file_contents =
        working_va.snapshot_contents_for_files(initial_attributions.files.keys());

    Ok((authorship_log, initial_attributions, initial_file_contents))
}

fn final_state_snapshot_for_working_log(
    repo: &Repository,
    commit_sha: &str,
    working_log: &PersistedWorkingLog,
) -> Result<HashMap<String, String>, GitAiError> {
    let checkpoints = working_log.read_all_checkpoints().unwrap_or_default();
    let initial_attributions = working_log.read_initial_attributions();
    let mut paths: HashSet<String> = initial_attributions.files.keys().cloned().collect();
    for checkpoint in checkpoints {
        for entry in checkpoint.entries {
            paths.insert(entry.file);
        }
    }

    let mut snapshot = HashMap::new();
    for path in paths {
        let content = match repo.get_file_content(&path, commit_sha) {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
            Err(_) => String::new(),
        };
        snapshot.insert(path, content);
    }
    Ok(snapshot)
}

fn apply_note_storage_policy(
    repo: &Repository,
    authorship_log: &mut AuthorshipLog,
) -> Result<(), GitAiError> {
    let config = Config::fresh();
    let effective_storage = config.effective_prompt_storage(&Some(repo.clone()));
    let custom_attrs = config.custom_attributes().clone();

    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
        for sr in authorship_log.metadata.sessions.values_mut() {
            sr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    retain_user_prompt_messages(&mut authorship_log.metadata.prompts);

    match effective_storage {
        PromptStorageMode::Local => {
            strip_prompt_messages(&mut authorship_log.metadata.prompts);
        }
        PromptStorageMode::Notes => {
            redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
        }
        PromptStorageMode::Default => {
            // Repair/rebuild is intentionally side-effect-free: do not enqueue
            // prompt CAS uploads from a dry run or note rewrite.
            strip_prompt_messages(&mut authorship_log.metadata.prompts);
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
enum StatsSkipReason {
    MergeCommit,
    Expensive(StatsCostEstimate),
}

fn log_post_commit_stats_skipped(
    repo: &Repository,
    commit_sha: &str,
    parent_sha: &str,
    reason: &str,
    estimate: Option<&StatsCostEstimate>,
) {
    crate::diagnostics::append_debug_event(
        "post_commit_stats_skipped",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "reason": reason,
            "effect": if reason == "expensive_commit" {
                "auto_upload_will_recompute_stats_before_upload"
            } else {
                "auto_upload_will_skip_because_stats_are_unavailable"
            },
            "estimate": estimate.map(|estimate| serde_json::json!({
                "filesWithAdditions": estimate.files_with_additions,
                "addedLines": estimate.added_lines,
                "deletedLines": estimate.deleted_lines,
                "hunkRanges": estimate.hunk_ranges,
                "thresholds": {
                    "maxFilesWithAdditions": STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
                    "maxAddedLines": STATS_SKIP_MAX_ADDED_LINES,
                    "maxDeletedLines": STATS_SKIP_MAX_DELETED_LINES,
                    "maxHunkRanges": STATS_SKIP_MAX_HUNKS,
                }
            })),
        }),
    );
}

fn prompt_debug_summary(authorship_log: &AuthorshipLog) -> serde_json::Value {
    let mut total_messages = 0usize;
    let mut prompts_with_messages = 0usize;
    let mut prompts_with_messages_url = 0usize;
    let mut tools: HashMap<String, usize> = HashMap::new();

    for prompt in authorship_log.metadata.prompts.values() {
        total_messages += prompt.messages.len();
        if !prompt.messages.is_empty() {
            prompts_with_messages += 1;
        }
        if prompt
            .messages_url
            .as_ref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            prompts_with_messages_url += 1;
        }
        *tools.entry(prompt.agent_id.tool.clone()).or_insert(0) += 1;
    }

    serde_json::json!({
        "promptCount": authorship_log.metadata.prompts.len(),
        "totalMessages": total_messages,
        "promptsWithMessages": prompts_with_messages,
        "promptsWithMessagesUrl": prompts_with_messages_url,
        "tools": tools,
    })
}

fn commit_stats_debug_summary(stats: &crate::authorship::stats::CommitStats) -> serde_json::Value {
    serde_json::json!({
        "humanAdditions": stats.human_additions,
        "unknownAdditions": stats.unknown_additions,
        "aiAdditions": stats.ai_additions,
        "aiAccepted": stats.ai_accepted,
        "mixedAdditions": stats.mixed_additions,
        "gitDiffAddedLines": stats.git_diff_added_lines,
        "gitDiffDeletedLines": stats.git_diff_deleted_lines,
        "toolModelBreakdownCount": stats.tool_model_breakdown.len(),
    })
}

fn sorted_path_sample(paths: impl IntoIterator<Item = String>, limit: usize) -> Vec<String> {
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort();
    paths.truncate(limit);
    paths
}

fn fill_ai_attribution_gaps_for_commit(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    authorship_log: &mut AuthorshipLog,
    pathspecs: &HashSet<String>,
    checkpoints: &[Checkpoint],
    human_author: &str,
) {
    let ai_attestation_hashes = collect_ai_gap_fill_attestation_hashes(authorship_log);
    let Some(attestation_hash) = ai_attestation_hashes.last().cloned() else {
        return;
    };

    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        parent_sha
    };

    let Ok(all_added_lines) = repo.diff_added_lines(diff_base, commit_sha, None) else {
        return;
    };
    let all_added_count = all_added_lines.values().map(Vec::len).sum::<usize>();
    if all_added_count == 0 {
        return;
    }

    let pathspec_added_count = if pathspecs.is_empty() {
        0
    } else {
        repo.diff_added_lines(diff_base, commit_sha, Some(pathspecs))
            .map(|lines| lines.values().map(Vec::len).sum::<usize>())
            .unwrap_or(0)
    };
    let missing_added_count = all_added_count.saturating_sub(pathspec_added_count);

    let attested_ai_added_count =
        attested_ai_added_count(authorship_log, &all_added_lines, &ai_attestation_hashes);
    let landed_ai_files =
        collect_current_ai_attested_files(authorship_log, &all_added_lines, &ai_attestation_hashes);
    let total_ai_additions = authorship_log
        .metadata
        .prompts
        .values()
        .map(|prompt| prompt.total_additions as usize)
        .sum::<usize>();
    let included_candidate_files = all_added_lines
        .iter()
        .filter(|(path, lines)| {
            !lines.is_empty() && (pathspecs.contains(*path) || landed_ai_files.contains(*path))
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let excluded_candidate_files = all_added_lines
        .iter()
        .filter(|(path, lines)| {
            !lines.is_empty() && !pathspecs.contains(*path) && !landed_ai_files.contains(*path)
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let candidate_diagnostics = ai_gap_fill_candidate_diagnostics(
        &included_candidate_files,
        &excluded_candidate_files,
        pathspecs,
        &landed_ai_files,
        checkpoints,
        &collect_ai_edited_tool_use_files(checkpoints),
    );

    let committed_hunks: HashMap<String, Vec<LineRange>> = all_added_lines
        .into_iter()
        .filter(|(path, lines)| {
            if lines.is_empty() {
                return false;
            }

            pathspecs.contains(path) || landed_ai_files.contains(path)
        })
        .map(|(path, lines)| (path, LineRange::compress_lines(&lines)))
        .collect();
    if committed_hunks.is_empty() {
        if !excluded_candidate_files.is_empty() {
            crate::diagnostics::append_debug_event(
                "post_commit_ai_attribution_gap_fill_skipped",
                serde_json::json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "parentSha": parent_sha,
                    "reason": "no_ai_path_evidence",
                    "allAddedLineCount": all_added_count,
                    "pathspecAddedLineCount": pathspec_added_count,
                    "missingAddedLineCount": missing_added_count,
                    "pathspecCount": pathspecs.len(),
                    "pathspecSample": sorted_path_sample(pathspecs.iter().cloned(), 20),
                    "landedAiFileCount": landed_ai_files.len(),
                    "landedAiFileSample": sorted_path_sample(landed_ai_files.iter().cloned(), 20),
                    "includedCandidateFileCount": included_candidate_files.len(),
                    "includedCandidateFileSample": sorted_path_sample(included_candidate_files.clone(), 20),
                    "excludedCandidateFileCount": excluded_candidate_files.len(),
                    "excludedCandidateFileSample": sorted_path_sample(excluded_candidate_files.clone(), 20),
                    "candidateDiagnostics": candidate_diagnostics,
                    "attestationHash": attestation_hash,
                    "humanAuthor": human_author,
                }),
            );
        }
        return;
    }

    let unattributed_added_count = crate::authorship::attribution_gap::count_unattributed_hunks(
        authorship_log,
        &committed_hunks,
    );
    if unattributed_added_count == 0 {
        return;
    }
    if total_ai_additions < attested_ai_added_count.saturating_add(unattributed_added_count) {
        crate::diagnostics::append_debug_event(
            "post_commit_ai_attribution_gap_fill_skipped",
            serde_json::json!({
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "commitSha": commit_sha,
                "parentSha": parent_sha,
                "reason": "not_enough_ai_generated_lines",
                "allAddedLineCount": all_added_count,
                "pathspecAddedLineCount": pathspec_added_count,
                "missingAddedLineCount": missing_added_count,
                "unattributedAddedLineCount": unattributed_added_count,
                "pathspecCount": pathspecs.len(),
                "pathspecSample": sorted_path_sample(pathspecs.iter().cloned(), 20),
                "totalAiAdditions": total_ai_additions,
                "attestedAiAddedLineCount": attested_ai_added_count,
                "landedAiFileCount": landed_ai_files.len(),
                "landedAiFileSample": sorted_path_sample(landed_ai_files.iter().cloned(), 20),
                "includedCandidateFileCount": included_candidate_files.len(),
                "includedCandidateFileSample": sorted_path_sample(included_candidate_files.clone(), 20),
                "excludedCandidateFileCount": excluded_candidate_files.len(),
                "excludedCandidateFileSample": sorted_path_sample(excluded_candidate_files.clone(), 20),
                "candidateDiagnostics": candidate_diagnostics,
                "attestationHash": attestation_hash,
                "humanAuthor": human_author,
            }),
        );
        return;
    }

    let filled_line_count = crate::authorship::attribution_gap::fill_unattributed_hunks(
        authorship_log,
        &committed_hunks,
        &attestation_hash,
    );

    if filled_line_count == 0 {
        return;
    }

    crate::diagnostics::append_debug_event(
        "post_commit_ai_attribution_gaps_filled",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "filledLineCount": filled_line_count,
            "allAddedLineCount": all_added_count,
            "pathspecAddedLineCount": pathspec_added_count,
            "missingAddedLineCount": missing_added_count,
            "unattributedAddedLineCount": unattributed_added_count,
            "pathspecCount": pathspecs.len(),
            "pathspecSample": sorted_path_sample(pathspecs.iter().cloned(), 20),
            "totalAiAdditions": total_ai_additions,
            "attestedAiAddedLineCount": attested_ai_added_count,
            "landedAiFileCount": landed_ai_files.len(),
            "landedAiFileSample": sorted_path_sample(landed_ai_files.iter().cloned(), 20),
            "includedCandidateFileCount": included_candidate_files.len(),
            "includedCandidateFileSample": sorted_path_sample(included_candidate_files, 20),
            "excludedCandidateFileCount": excluded_candidate_files.len(),
            "excludedCandidateFileSample": sorted_path_sample(excluded_candidate_files, 20),
            "candidateDiagnostics": candidate_diagnostics,
            "attestationHash": attestation_hash,
            "humanAuthor": human_author,
        }),
    );
}

fn fill_legacy_human_manual_gaps_for_commit(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    authorship_log: &mut AuthorshipLog,
    checkpoints: &[Checkpoint],
    human_author: &str,
) {
    if !authorship_log.metadata.prompts.is_empty() || !authorship_log.metadata.sessions.is_empty() {
        return;
    }
    if checkpoints
        .iter()
        .any(|checkpoint| checkpoint.kind.is_ai() || checkpoint.kind == CheckpointKind::KnownHuman)
    {
        return;
    }

    let mut file_authors: HashMap<String, String> = HashMap::new();
    for checkpoint in checkpoints.iter().filter(|checkpoint| {
        is_plain_legacy_human_checkpoint(checkpoint)
            && checkpoint
                .entries
                .iter()
                .any(|entry| entry.attributions.is_empty() && entry.line_attributions.is_empty())
    }) {
        let author = if checkpoint.author.trim().is_empty() {
            human_author.to_string()
        } else {
            checkpoint.author.clone()
        };

        for entry in checkpoint
            .entries
            .iter()
            .filter(|entry| entry.attributions.is_empty() && entry.line_attributions.is_empty())
        {
            file_authors.insert(entry.file.clone(), author.clone());
        }
    }
    if file_authors.is_empty() {
        return;
    }

    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        parent_sha
    };
    let legacy_human_files: HashSet<String> = file_authors.keys().cloned().collect();
    let Ok(legacy_added_lines) =
        repo.diff_added_lines(diff_base, commit_sha, Some(&legacy_human_files))
    else {
        return;
    };
    if legacy_added_lines.is_empty() {
        return;
    }

    let mut hunks_by_author: HashMap<String, HashMap<String, Vec<LineRange>>> = HashMap::new();
    for (path, lines) in legacy_added_lines {
        if lines.is_empty() {
            continue;
        }
        let Some(author) = file_authors.get(&path) else {
            continue;
        };
        hunks_by_author
            .entry(author.clone())
            .or_default()
            .insert(path, LineRange::compress_lines(&lines));
    }
    if hunks_by_author.is_empty() {
        return;
    }

    let mut filled_line_count = 0usize;
    let mut author_count = 0usize;
    for (author, committed_hunks) in hunks_by_author {
        if committed_hunks.is_empty() {
            continue;
        }
        let human_hash =
            crate::authorship::authorship_log_serialization::generate_human_short_hash(&author);
        let filled = crate::authorship::attribution_gap::fill_unattributed_hunks(
            authorship_log,
            &committed_hunks,
            &human_hash,
        );
        if filled == 0 {
            continue;
        }

        filled_line_count += filled;
        author_count += 1;
        authorship_log
            .metadata
            .humans
            .entry(human_hash)
            .or_insert_with(|| HumanRecord { author });
    }

    if filled_line_count == 0 {
        return;
    }

    crate::diagnostics::append_debug_event(
        "post_commit_legacy_human_manual_gaps_filled",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "filledLineCount": filled_line_count,
            "legacyHumanFileCount": legacy_human_files.len(),
            "authorCount": author_count,
            "humanAuthor": human_author,
        }),
    );
}

fn fill_observed_human_gaps_for_commit(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    authorship_log: &mut AuthorshipLog,
    checkpoints: &[Checkpoint],
    human_author: &str,
) {
    let human_file_authors = observed_human_gap_fill_file_authors(checkpoints, human_author);
    if human_file_authors.is_empty() {
        return;
    }

    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        parent_sha
    };
    let human_files: HashSet<String> = human_file_authors.keys().cloned().collect();
    let Ok(added_lines) = repo.diff_added_lines(diff_base, commit_sha, Some(&human_files)) else {
        return;
    };
    if added_lines.is_empty() {
        return;
    }

    let mut hunks_by_author: HashMap<String, HashMap<String, Vec<LineRange>>> = HashMap::new();
    for (path, lines) in added_lines {
        if lines.is_empty() {
            continue;
        }
        let Some(author) = human_file_authors.get(&path) else {
            continue;
        };
        hunks_by_author
            .entry(author.clone())
            .or_default()
            .insert(path, LineRange::compress_lines(&lines));
    }
    if hunks_by_author.is_empty() {
        return;
    }

    let mut filled_line_count = 0usize;
    let mut author_count = 0usize;
    for (author, committed_hunks) in hunks_by_author {
        if committed_hunks.is_empty() {
            continue;
        }
        let human_hash =
            crate::authorship::authorship_log_serialization::generate_human_short_hash(&author);
        let filled = crate::authorship::attribution_gap::fill_unattributed_hunks(
            authorship_log,
            &committed_hunks,
            &human_hash,
        );
        if filled == 0 {
            continue;
        }

        filled_line_count += filled;
        author_count += 1;
        authorship_log
            .metadata
            .humans
            .entry(human_hash)
            .or_insert_with(|| HumanRecord { author });
    }

    if filled_line_count == 0 {
        return;
    }

    crate::diagnostics::append_debug_event(
        "post_commit_observed_human_gaps_filled",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "filledLineCount": filled_line_count,
            "observedHumanFileCount": human_files.len(),
            "authorCount": author_count,
            "humanAuthor": human_author,
        }),
    );
}

fn observed_human_gap_fill_file_authors(
    checkpoints: &[Checkpoint],
    human_author: &str,
) -> HashMap<String, String> {
    let ai_edited_tool_use_files = collect_ai_edited_tool_use_files(checkpoints);
    let mut ai_evidence_files = HashSet::new();
    for checkpoint in checkpoints {
        for entry in &checkpoint.entries {
            if checkpoint_entry_has_ai_path_evidence(checkpoint, entry)
                || checkpoint_has_ai_pre_edit_path_evidence(
                    checkpoint,
                    entry,
                    &ai_edited_tool_use_files,
                )
            {
                ai_evidence_files.insert(entry.file.clone());
            }
        }
    }

    let mut file_authors = HashMap::new();
    for checkpoint in checkpoints.iter().filter(|checkpoint| {
        checkpoint.kind == CheckpointKind::KnownHuman
            || is_plain_legacy_human_checkpoint(checkpoint)
    }) {
        let author = if checkpoint.author.trim().is_empty() {
            human_author.to_string()
        } else {
            checkpoint.author.clone()
        };

        for entry in &checkpoint.entries {
            if !ai_evidence_files.contains(&entry.file) {
                file_authors.insert(entry.file.clone(), author.clone());
            }
        }
    }

    file_authors
}

fn fill_legacy_human_ai_gaps_for_commit(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    authorship_log: &mut AuthorshipLog,
    checkpoints: &[Checkpoint],
    human_author: &str,
) {
    if !authorship_log.metadata.prompts.is_empty() || !authorship_log.metadata.sessions.is_empty() {
        return;
    }
    if checkpoints.iter().any(|checkpoint| checkpoint.kind.is_ai()) {
        return;
    }
    if !checkpoints
        .iter()
        .any(|checkpoint| checkpoint.kind == CheckpointKind::KnownHuman)
    {
        return;
    }

    let legacy_human_files: HashSet<String> = checkpoints
        .iter()
        .filter(|checkpoint| is_plain_legacy_human_checkpoint(checkpoint))
        .flat_map(|checkpoint| {
            checkpoint.entries.iter().filter_map(|entry| {
                (entry.attributions.is_empty() && entry.line_attributions.is_empty())
                    .then(|| entry.file.clone())
            })
        })
        .collect();
    if legacy_human_files.len() < 3 {
        return;
    }

    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        parent_sha
    };

    let Ok(legacy_added_lines) =
        repo.diff_added_lines(diff_base, commit_sha, Some(&legacy_human_files))
    else {
        return;
    };
    if legacy_added_lines.is_empty() {
        return;
    }

    let committed_hunks: HashMap<String, Vec<LineRange>> = legacy_added_lines
        .into_iter()
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(path, lines)| (path, LineRange::compress_lines(&lines)))
        .collect();
    if committed_hunks.is_empty() {
        return;
    }

    let agent_id = AgentId {
        tool: "github-copilot".to_string(),
        id: format!("legacy-human-checkpoint:{}", commit_sha),
        model: "legacy-human-checkpoint".to_string(),
    };
    let session_id = crate::authorship::authorship_log_serialization::generate_session_id(
        &agent_id.id,
        &agent_id.tool,
    );
    let attestation_hash = format!(
        "{}::{}",
        session_id,
        crate::authorship::authorship_log_serialization::generate_trace_id()
    );
    let filled_line_count = crate::authorship::attribution_gap::fill_unattributed_hunks(
        authorship_log,
        &committed_hunks,
        &attestation_hash,
    );

    if filled_line_count == 0 {
        return;
    }

    authorship_log
        .metadata
        .sessions
        .entry(session_id.clone())
        .or_insert_with(|| {
            let mut custom_attributes = HashMap::new();
            custom_attributes.insert(
                "legacy_human_checkpoint_gap_fill".to_string(),
                "true".to_string(),
            );
            custom_attributes.insert("commit_sha".to_string(), commit_sha.to_string());

            SessionRecord {
                agent_id: agent_id.clone(),
                human_author: Some(human_author.to_string()),
                custom_attributes: Some(custom_attributes),
            }
        });

    crate::diagnostics::append_debug_event(
        "post_commit_legacy_human_ai_gaps_filled",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "filledLineCount": filled_line_count,
            "legacyHumanFileCount": legacy_human_files.len(),
            "sessionId": session_id,
            "attestationHash": attestation_hash,
            "humanAuthor": human_author,
        }),
    );
}

fn restore_archived_ai_attributions_for_commit(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    authorship_log: &mut AuthorshipLog,
    checkpoints: &[Checkpoint],
    human_author: &str,
) {
    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        parent_sha
    };

    let Ok(mut added_lines_by_file) = repo.diff_added_lines(diff_base, commit_sha, None) else {
        return;
    };
    added_lines_by_file.retain(|_, lines| !lines.is_empty());
    if added_lines_by_file.is_empty() {
        return;
    }

    let current_ai_files = collect_current_working_log_ai_files(checkpoints);
    let candidate_files = added_lines_by_file
        .iter()
        .filter(|(file, added_lines)| {
            !current_ai_files.contains(*file)
                && !authorship_log_has_ai_added_lines(authorship_log, file, added_lines)
        })
        .map(|(file, _)| file.clone())
        .collect::<Vec<_>>();
    if candidate_files.is_empty() {
        return;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let archived_states = collect_recent_archived_ai_states(repo, &candidate_files, now_ms);
    if archived_states.is_empty() {
        return;
    }

    let mut restored_line_count = 0usize;
    let mut restored_file_count = 0usize;
    let mut restored_author_count = 0usize;
    let mut restored_file_sample = Vec::new();

    for (file, archived_state) in archived_states {
        let Some(added_lines) = added_lines_by_file.get(&file) else {
            continue;
        };
        let restored_by_author = archived_ai_lines_matching_commit(
            repo,
            commit_sha,
            &file,
            added_lines,
            &archived_state,
        );
        if restored_by_author.is_empty() {
            continue;
        }

        let restored_ranges = restored_by_author
            .values()
            .flat_map(|lines| LineRange::compress_lines(lines))
            .collect::<Vec<_>>();
        remove_ranges_from_file_attestations(authorship_log, &file, &restored_ranges);

        let mut file_restored_lines = 0usize;
        for (author_id, mut lines) in restored_by_author {
            if lines.is_empty() || !is_ai_author_id(&author_id) {
                continue;
            }

            lines.sort_unstable();
            lines.dedup();
            let line_count = lines.len();
            let line_ranges = LineRange::compress_lines(&lines);
            if line_ranges.is_empty() {
                continue;
            }

            ensure_archived_ai_metadata(
                authorship_log,
                &archived_state,
                &author_id,
                human_author,
                line_count as u32,
            );

            authorship_log
                .get_or_create_file(&file)
                .add_entry(AttestationEntry::new(author_id, line_ranges));

            file_restored_lines += line_count;
            restored_author_count += 1;
        }

        if file_restored_lines == 0 {
            continue;
        }

        restored_line_count += file_restored_lines;
        restored_file_count += 1;
        if restored_file_sample.len() < 20 {
            restored_file_sample.push(file);
        }
    }

    if restored_line_count == 0 {
        return;
    }

    crate::diagnostics::append_debug_event(
        "post_commit_archived_ai_attributions_restored",
        serde_json::json!({
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "commitSha": commit_sha,
            "parentSha": parent_sha,
            "restoredLineCount": restored_line_count,
            "restoredFileCount": restored_file_count,
            "restoredAuthorCount": restored_author_count,
            "candidateFileCount": candidate_files.len(),
            "currentAiFileCount": current_ai_files.len(),
            "restoredFileSample": restored_file_sample,
            "humanAuthor": human_author,
        }),
    );
}

fn collect_current_working_log_ai_files(checkpoints: &[Checkpoint]) -> HashSet<String> {
    checkpoints
        .iter()
        .flat_map(|checkpoint| {
            checkpoint.entries.iter().filter_map(|entry| {
                checkpoint_entry_has_ai_path_evidence(checkpoint, entry).then(|| entry.file.clone())
            })
        })
        .collect()
}

fn authorship_log_has_ai_added_lines(
    authorship_log: &AuthorshipLog,
    file: &str,
    added_lines: &[u32],
) -> bool {
    let Some(file_attestation) = authorship_log
        .attestations
        .iter()
        .find(|attestation| attestation.file_path == file)
    else {
        return false;
    };

    file_attestation.entries.iter().any(|entry| {
        is_current_ai_attestation_hash(authorship_log, &entry.hash)
            && entry.line_ranges.iter().any(|range| {
                range
                    .expand()
                    .into_iter()
                    .any(|line| added_lines.binary_search(&line).is_ok())
            })
    })
}

fn archived_ai_lines_matching_commit(
    repo: &Repository,
    commit_sha: &str,
    file: &str,
    added_lines: &[u32],
    archived_state: &ArchivedAiFileState,
) -> HashMap<String, Vec<u32>> {
    let Ok(final_content_bytes) = repo.get_file_content(file, commit_sha) else {
        return HashMap::new();
    };
    let final_content = String::from_utf8_lossy(&final_content_bytes).to_string();
    let final_lines = final_content.lines().collect::<Vec<_>>();

    let source_content = archived_state
        .source_working_log
        .get_file_version(&archived_state.blob_sha)
        .unwrap_or_default();
    let source_lines = source_content.lines().collect::<Vec<_>>();
    if source_lines.is_empty() || final_lines.is_empty() {
        return HashMap::new();
    }

    let mut source_candidates =
        archived_ai_line_candidates(archived_state, &source_content, &source_lines);
    if source_candidates.is_empty() {
        return HashMap::new();
    }
    let final_added_text_counts = added_line_text_counts(&final_lines, added_lines);
    let source_text_counts = source_candidate_text_counts(&source_candidates);

    let mut restored_by_author: HashMap<String, Vec<u32>> = HashMap::new();
    for line in added_lines {
        let Some(final_text) = line_text(&final_lines, *line) else {
            continue;
        };

        let candidate_index = source_candidates
            .iter()
            .position(|candidate| {
                !candidate.used
                    && candidate.line == *line
                    && candidate.text.as_str() == final_text
                    && is_ai_author_id(&candidate.author_id)
            })
            .or_else(|| {
                if final_text.trim().is_empty() {
                    return None;
                }
                if final_added_text_counts
                    .get(final_text)
                    .copied()
                    .unwrap_or(0)
                    != 1
                    || source_text_counts.get(final_text).copied().unwrap_or(0) != 1
                {
                    return None;
                }
                source_candidates.iter().position(|candidate| {
                    !candidate.used
                        && candidate.text.as_str() == final_text
                        && is_ai_author_id(&candidate.author_id)
                })
            });

        let Some(candidate_index) = candidate_index else {
            continue;
        };
        let candidate = &mut source_candidates[candidate_index];
        candidate.used = true;
        restored_by_author
            .entry(candidate.author_id.clone())
            .or_default()
            .push(*line);
    }

    restored_by_author
}

fn added_line_text_counts(final_lines: &[&str], added_lines: &[u32]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for line in added_lines {
        if let Some(text) = line_text(final_lines, *line) {
            *counts.entry(text.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

fn source_candidate_text_counts(candidates: &[ArchivedAiLineCandidate]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for candidate in candidates {
        *counts.entry(candidate.text.clone()).or_insert(0) += 1;
    }
    counts
}

#[derive(Debug)]
struct ArchivedAiLineCandidate {
    line: u32,
    text: String,
    author_id: String,
    used: bool,
}

fn archived_ai_line_candidates(
    archived_state: &ArchivedAiFileState,
    source_content: &str,
    source_lines: &[&str],
) -> Vec<ArchivedAiLineCandidate> {
    let mut candidates = Vec::new();

    for attr in &archived_state.line_attributions {
        if !is_ai_author_id(&attr.author_id) {
            continue;
        }
        for line in attr.start_line..=attr.end_line {
            let Some(text) = line_text(source_lines, line) else {
                continue;
            };
            candidates.push(ArchivedAiLineCandidate {
                line,
                text: text.to_string(),
                author_id: attr.author_id.clone(),
                used: false,
            });
        }
    }

    if candidates.is_empty() {
        let line_attrs = crate::authorship::attribution_tracker::attributions_to_line_attributions(
            &archived_state.attributions,
            source_content,
        );
        for attr in line_attrs {
            if !is_ai_author_id(&attr.author_id) {
                continue;
            }
            for line in attr.start_line..=attr.end_line {
                let Some(text) = line_text(source_lines, line) else {
                    continue;
                };
                candidates.push(ArchivedAiLineCandidate {
                    line,
                    text: text.to_string(),
                    author_id: attr.author_id.clone(),
                    used: false,
                });
            }
        }
    }

    candidates
}

fn line_text<'a>(lines: &'a [&str], line: u32) -> Option<&'a str> {
    lines.get(line.checked_sub(1)? as usize).copied()
}

fn remove_ranges_from_file_attestations(
    authorship_log: &mut AuthorshipLog,
    file: &str,
    ranges: &[LineRange],
) {
    if ranges.is_empty() {
        return;
    }

    let Some(file_attestation) = authorship_log
        .attestations
        .iter_mut()
        .find(|attestation| attestation.file_path == file)
    else {
        return;
    };

    for entry in &mut file_attestation.entries {
        entry.remove_line_ranges(ranges);
    }
    file_attestation
        .entries
        .retain(|entry| !entry.line_ranges.is_empty());
}

fn ensure_archived_ai_metadata(
    authorship_log: &mut AuthorshipLog,
    archived_state: &ArchivedAiFileState,
    author_id: &str,
    human_author: &str,
    recovered_line_count: u32,
) {
    let agent_id = archived_state
        .checkpoint
        .agent_id
        .clone()
        .unwrap_or_else(|| AgentId {
            tool: if archived_state.checkpoint.author.trim().is_empty() {
                "archived_ai".to_string()
            } else {
                archived_state.checkpoint.author.clone()
            },
            id: author_id.to_string(),
            model: "unknown".to_string(),
        });

    let mut custom_attributes = HashMap::new();
    custom_attributes.insert("archived_ai_state_restored".to_string(), "true".to_string());
    custom_attributes.insert("archive_file".to_string(), archived_state.file.clone());

    if author_id.starts_with("s_") {
        let session_id = author_id
            .split("::")
            .next()
            .unwrap_or(author_id)
            .to_string();
        authorship_log
            .metadata
            .sessions
            .entry(session_id)
            .or_insert_with(|| SessionRecord {
                agent_id: agent_id.clone(),
                human_author: Some(human_author.to_string()),
                custom_attributes: Some(custom_attributes.clone()),
            });
    }

    authorship_log
        .metadata
        .prompts
        .entry(author_id.to_string())
        .or_insert_with(|| crate::authorship::authorship_log::PromptRecord {
            agent_id,
            human_author: Some(human_author.to_string()),
            messages: archived_state
                .checkpoint
                .transcript
                .as_ref()
                .map(|transcript| transcript.messages().to_vec())
                .unwrap_or_default(),
            messages_url: None,
            total_additions: archived_state
                .checkpoint
                .line_stats
                .additions
                .max(recovered_line_count),
            total_deletions: archived_state.checkpoint.line_stats.deletions,
            accepted_lines: recovered_line_count,
            overriden_lines: 0,
            custom_attributes: Some(custom_attributes),
        });
}

fn collect_ai_gap_fill_attestation_hashes(authorship_log: &AuthorshipLog) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut seen = HashSet::new();

    for file_attestation in &authorship_log.attestations {
        for entry in &file_attestation.entries {
            if !is_current_ai_attestation_hash(authorship_log, &entry.hash) {
                continue;
            }
            if seen.insert(entry.hash.clone()) {
                hashes.push(entry.hash.clone());
            }
        }
    }

    hashes
}

fn is_current_ai_attestation_hash(authorship_log: &AuthorshipLog, hash: &str) -> bool {
    if hash.starts_with("h_") {
        return false;
    }

    if hash.starts_with("s_") {
        let session_key = hash.split("::").next().unwrap_or(hash);
        return authorship_log.metadata.sessions.contains_key(session_key);
    }

    authorship_log.metadata.prompts.contains_key(hash)
}

fn attested_ai_added_count(
    authorship_log: &AuthorshipLog,
    added_lines_by_file: &HashMap<String, Vec<u32>>,
    ai_attestation_hashes: &[String],
) -> usize {
    let ai_hashes: HashSet<&str> = ai_attestation_hashes.iter().map(String::as_str).collect();
    let mut counted_by_file: HashMap<&str, HashSet<u32>> = HashMap::new();

    for file_attestation in &authorship_log.attestations {
        let Some(added_lines) = added_lines_by_file.get(&file_attestation.file_path) else {
            continue;
        };
        let counted = counted_by_file
            .entry(file_attestation.file_path.as_str())
            .or_default();

        for entry in &file_attestation.entries {
            if !ai_hashes.contains(entry.hash.as_str()) {
                continue;
            }
            for range in &entry.line_ranges {
                for line in range.expand() {
                    if added_lines.binary_search(&line).is_ok() {
                        counted.insert(line);
                    }
                }
            }
        }
    }

    counted_by_file.values().map(HashSet::len).sum()
}

fn collect_current_ai_attested_files(
    authorship_log: &AuthorshipLog,
    added_lines_by_file: &HashMap<String, Vec<u32>>,
    ai_attestation_hashes: &[String],
) -> HashSet<String> {
    let ai_hashes: HashSet<&str> = ai_attestation_hashes.iter().map(String::as_str).collect();
    let mut files = HashSet::new();

    for file_attestation in &authorship_log.attestations {
        let Some(added_lines) = added_lines_by_file.get(&file_attestation.file_path) else {
            continue;
        };

        let has_current_ai_added_line = file_attestation.entries.iter().any(|entry| {
            ai_hashes.contains(entry.hash.as_str())
                && entry.line_ranges.iter().any(|range| {
                    range
                        .expand()
                        .into_iter()
                        .any(|line| added_lines.binary_search(&line).is_ok())
                })
        });

        if has_current_ai_added_line {
            files.insert(file_attestation.file_path.clone());
        }
    }

    files
}

fn ai_gap_fill_candidate_diagnostics(
    included_candidate_files: &[String],
    excluded_candidate_files: &[String],
    pathspecs: &HashSet<String>,
    landed_ai_files: &HashSet<String>,
    checkpoints: &[Checkpoint],
    ai_edited_tool_use_files: &HashSet<(String, String)>,
) -> Vec<serde_json::Value> {
    let mut paths = included_candidate_files
        .iter()
        .chain(excluded_candidate_files.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    while paths.len() > 20 {
        let Some(last) = paths.iter().next_back().cloned() else {
            break;
        };
        paths.remove(&last);
    }

    paths
        .into_iter()
        .map(|path| {
            let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut ai_pre_edit_count = 0usize;
            let mut tool_use_ids = BTreeSet::new();
            let mut agent_tools = BTreeSet::new();

            for checkpoint in checkpoints {
                if !checkpoint.entries.iter().any(|entry| entry.file == path) {
                    continue;
                }

                *kind_counts.entry(checkpoint.kind.to_str()).or_insert(0) += 1;
                for entry in &checkpoint.entries {
                    if entry.file == path
                        && checkpoint_has_ai_pre_edit_path_evidence(
                            checkpoint,
                            entry,
                            ai_edited_tool_use_files,
                        )
                    {
                        ai_pre_edit_count += 1;
                    }
                }
                if let Some(agent_id) = &checkpoint.agent_id {
                    agent_tools.insert(agent_id.tool.clone());
                }
                if let Some(agent_tool) = checkpoint
                    .agent_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("agent_tool"))
                    .filter(|agent_tool| !agent_tool.trim().is_empty())
                {
                    agent_tools.insert(agent_tool.clone());
                }
                if let Some(tool_use_id) = checkpoint_tool_use_id(checkpoint) {
                    tool_use_ids.insert(tool_use_id.to_string());
                }
            }

            let has_ai_path_evidence = pathspecs.contains(&path);
            let has_landed_ai_lines = landed_ai_files.contains(&path);
            let status = if has_ai_path_evidence || has_landed_ai_lines {
                "included"
            } else if kind_counts.is_empty() {
                "excluded_no_checkpoint_for_added_file"
            } else {
                "excluded_no_ai_path_evidence"
            };

            serde_json::json!({
                "filePath": path,
                "status": status,
                "hasAiPathEvidence": has_ai_path_evidence,
                "hasLandedAiLines": has_landed_ai_lines,
                "checkpointKindCounts": kind_counts,
                "aiPreEditPathEvidenceCount": ai_pre_edit_count,
                "agentToolSample": sorted_path_sample(agent_tools.into_iter().collect::<Vec<_>>(), 5),
                "toolUseIdSample": sorted_path_sample(tool_use_ids.into_iter().collect::<Vec<_>>(), 5),
            })
        })
        .collect()
}

fn checkpoint_input_debug_summary(checkpoints: &[Checkpoint]) -> serde_json::Value {
    let mut checkpoint_kind_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut checkpoint_kind_files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut all_files: BTreeSet<String> = BTreeSet::new();
    let mut ai_checkpoint_count = 0usize;
    let mut ai_entry_count = 0usize;

    for checkpoint in checkpoints {
        let kind = checkpoint.kind.to_str();
        *checkpoint_kind_counts.entry(kind.clone()).or_insert(0) += 1;
        if checkpoint.kind.is_ai() {
            ai_checkpoint_count += 1;
        }

        let files_for_kind = checkpoint_kind_files.entry(kind).or_default();
        for entry in &checkpoint.entries {
            all_files.insert(entry.file.clone());
            files_for_kind.insert(entry.file.clone());
            if checkpoint.kind.is_ai() {
                ai_entry_count += 1;
            }
        }
    }

    let kind_summary: BTreeMap<String, serde_json::Value> = checkpoint_kind_counts
        .iter()
        .map(|(kind, count)| {
            let files = checkpoint_kind_files.get(kind).cloned().unwrap_or_default();
            (
                kind.clone(),
                serde_json::json!({
                    "checkpointCount": count,
                    "uniqueFileCount": files.len(),
                    "uniqueFileSample": sample_checkpoint_files(&files, 5),
                }),
            )
        })
        .collect();

    serde_json::json!({
        "checkpointCount": checkpoints.len(),
        "checkpointEntryCount": checkpoints.iter().map(|checkpoint| checkpoint.entries.len()).sum::<usize>(),
        "checkpointKindsPresent": checkpoint_kind_counts.keys().cloned().collect::<Vec<_>>(),
        "checkpointKindCounts": checkpoint_kind_counts,
        "uniqueFileCount": all_files.len(),
        "uniqueFileSample": sample_checkpoint_files(&all_files, 10),
        "kindSummary": kind_summary,
        "aiCheckpointCount": ai_checkpoint_count,
        "aiEntryCount": ai_entry_count,
        "onlyKnownHuman": checkpoint_kind_counts.len() == 1 && checkpoint_kind_counts.contains_key("known_human"),
        "onlySingleFile": all_files.len() == 1,
    })
}

fn sample_checkpoint_files(files: &BTreeSet<String>, limit: usize) -> Vec<String> {
    files.iter().take(limit).cloned().collect()
}

fn attribution_gap_reason(stats: &crate::authorship::stats::CommitStats) -> Option<&'static str> {
    if stats.git_diff_added_lines == 0 || stats.unknown_additions == 0 {
        return None;
    }

    if stats.ai_additions == 0
        && stats.human_additions == 0
        && stats.mixed_additions == 0
        && stats.unknown_additions == stats.git_diff_added_lines
    {
        Some("all_added_lines_unknown")
    } else {
        Some("partial_added_lines_unknown")
    }
}

fn stats_skip_reason_debug(reason: Option<&StatsSkipReason>) -> serde_json::Value {
    match reason {
        Some(StatsSkipReason::MergeCommit) => serde_json::json!("merge_commit"),
        Some(StatsSkipReason::Expensive(estimate)) => serde_json::json!({
            "reason": "expensive_commit",
            "filesWithAdditions": estimate.files_with_additions,
            "addedLines": estimate.added_lines,
            "deletedLines": estimate.deleted_lines,
            "hunkRanges": estimate.hunk_ranges,
        }),
        None => serde_json::Value::Null,
    }
}

#[doc(hidden)]
pub fn should_skip_expensive_post_commit_stats(estimate: &StatsCostEstimate) -> bool {
    estimate.hunk_ranges >= STATS_SKIP_MAX_HUNKS
        || estimate.added_lines >= STATS_SKIP_MAX_ADDED_LINES
        || estimate.files_with_additions >= STATS_SKIP_MAX_FILES_WITH_ADDITIONS
        || estimate.deleted_lines >= STATS_SKIP_MAX_DELETED_LINES
}

/// Public result of the stats cost estimate for a commit, used by the async
/// wrapper path to decide whether to skip expensive stats computation.
pub struct StatsSkipEstimate {
    should_skip: bool,
}

impl StatsSkipEstimate {
    pub fn should_skip(&self) -> bool {
        self.should_skip
    }
}

/// Estimate whether stats computation for `commit_sha` would be too expensive.
/// Resolves the parent commit automatically. Intended for callers outside the
/// normal post-commit flow (e.g. the async wrapper path).
pub fn estimate_stats_cost_for_head(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<StatsSkipEstimate, GitAiError> {
    let commit = repo.find_commit(commit_sha.to_string())?;
    let parent_sha = if commit.parent_count().unwrap_or(0) > 0 {
        commit
            .parent(0)
            .map(|p| p.id())
            .unwrap_or_else(|_| "initial".to_string())
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
    };
    let estimate = estimate_stats_cost(repo, &parent_sha, commit_sha, ignore_patterns)?;
    Ok(StatsSkipEstimate {
        should_skip: should_skip_expensive_post_commit_stats(&estimate),
    })
}

fn estimate_stats_cost(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<StatsCostEstimate, GitAiError> {
    let (mut added_lines_by_file, total_deleted_lines) =
        repo.diff_added_lines_with_deleted_count(parent_sha, commit_sha)?;
    let ignore_matcher = build_ignore_matcher(ignore_patterns);
    added_lines_by_file
        .retain(|file_path, _| !should_ignore_file_with_matcher(file_path, &ignore_matcher));

    let files_with_additions = added_lines_by_file
        .values()
        .filter(|lines| !lines.is_empty())
        .count();

    let mut added_lines = 0usize;
    let mut hunk_ranges = 0usize;

    for (_file, lines) in added_lines_by_file {
        if lines.is_empty() {
            continue;
        }
        added_lines += lines.len();
        hunk_ranges += count_line_ranges(&lines);
    }

    Ok(StatsCostEstimate {
        files_with_additions,
        added_lines,
        hunk_ranges,
        deleted_lines: total_deleted_lines,
    })
}

#[doc(hidden)]
pub fn count_line_ranges(lines: &[u32]) -> usize {
    if lines.is_empty() {
        return 0;
    }

    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut ranges = 1usize;
    let mut prev = sorted[0];
    for &line in &sorted[1..] {
        if line != prev + 1 {
            ranges += 1;
        }
        prev = line;
    }
    ranges
}

/// Update prompts/transcripts in working log checkpoints to their latest versions.
///
/// For each unique prompt/conversation (identified by agent_id), only the last
/// checkpoint with that agent_id is updated. This prevents duplicating the same
/// full transcript across multiple checkpoints when only the final version matters.
fn update_prompts_to_latest(checkpoints: &mut [Checkpoint]) -> Result<(), GitAiError> {
    let mut agent_checkpoint_indices: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, checkpoint) in checkpoints.iter().enumerate() {
        if let Some(agent_id) = &checkpoint.agent_id {
            let key = format!("{}:{}", agent_id.tool, agent_id.id);
            agent_checkpoint_indices.entry(key).or_default().push(idx);
        }
    }

    for (_agent_key, indices) in agent_checkpoint_indices {
        if indices.is_empty() {
            continue;
        }

        let last_idx = *indices.last().unwrap();
        let checkpoint = &checkpoints[last_idx];

        if let Some(agent_id) = &checkpoint.agent_id {
            let result = update_prompt_from_tool(
                &agent_id.tool,
                &agent_id.id,
                checkpoint.agent_metadata.as_ref(),
                &agent_id.model,
            );

            match result {
                PromptUpdateResult::Updated(latest_transcript, latest_model) => {
                    let checkpoint = &mut checkpoints[last_idx];
                    checkpoint.transcript = Some(latest_transcript);
                    if let Some(agent_id) = &mut checkpoint.agent_id {
                        agent_id.model = latest_model;
                    }
                }
                PromptUpdateResult::Unchanged => {}
                PromptUpdateResult::Failed(_e) => {}
            }
        }
    }

    Ok(())
}

/// Batch upsert the latest prompt checkpoint for each agent into the internal database.
fn batch_upsert_prompts_to_db(
    checkpoints: &[Checkpoint],
    working_log: &crate::git::repo_storage::PersistedWorkingLog,
    commit_sha: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::{InternalDatabase, PromptDbRecord};

    let workdir = working_log.repo_workdir.to_string_lossy().to_string();
    let mut last_checkpoint_by_agent: HashMap<String, usize> = HashMap::new();

    for (idx, checkpoint) in checkpoints.iter().enumerate() {
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }
        if let Some(agent_id) = &checkpoint.agent_id {
            let key = format!("{}:{}", agent_id.tool, agent_id.id);
            last_checkpoint_by_agent.insert(key, idx);
        }
    }

    let mut records = Vec::new();
    for (_agent_key, idx) in last_checkpoint_by_agent {
        let checkpoint = &checkpoints[idx];
        if let Some(record) = PromptDbRecord::from_checkpoint(
            checkpoint,
            Some(workdir.clone()),
            Some(commit_sha.to_string()),
        ) {
            records.push(record);
        }
    }

    if records.is_empty() {
        return Ok(());
    }

    let db = InternalDatabase::global()?;
    let mut db_guard = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    db_guard.batch_upsert_prompts(&records)?;

    Ok(())
}

/// Enqueue prompt messages to CAS for external storage and replace messages with a messages_url.
fn enqueue_prompt_messages_to_cas(
    repo: &Repository,
    prompts: &mut std::collections::BTreeMap<
        String,
        crate::authorship::authorship_log::PromptRecord,
    >,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::InternalDatabase;

    let db = InternalDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    let mut metadata = HashMap::new();
    metadata.insert("api_version".to_string(), "v1".to_string());
    metadata.insert("kind".to_string(), "prompt".to_string());

    let repo_url = repo
        .get_default_remote()
        .ok()
        .flatten()
        .and_then(|remote_name| {
            repo.remotes_with_urls().ok().and_then(|remotes| {
                remotes
                    .into_iter()
                    .find(|(name, _)| name == &remote_name)
                    .map(|(_, url)| url)
            })
        });

    if let Some(url) = repo_url
        && let Ok(normalized) = crate::repo_url::normalize_repo_url(&url)
    {
        metadata.insert("repo_url".to_string(), normalized);
    }

    let api_base_url = Config::fresh().api_base_url().to_string();

    for prompt in prompts.values_mut() {
        if prompt.messages.is_empty() {
            continue;
        }

        let messages_obj = crate::api::types::CasMessagesObject {
            messages: prompt.messages.clone(),
        };
        let messages_json = serde_json::to_value(&messages_obj)
            .map_err(|e| GitAiError::Generic(format!("Failed to serialize messages: {}", e)))?;

        let hash = db_lock.enqueue_cas_object(&messages_json, Some(&metadata))?;

        let metadata_json = serde_json::to_string(&metadata).ok();
        let canonical = serde_json_canonicalizer::to_string(&messages_json)
            .unwrap_or_else(|_| messages_json.to_string());
        let cas_payload = crate::daemon::control_api::CasSyncPayload {
            hash: hash.clone(),
            data: canonical,
            metadata: metadata_json,
        };

        if crate::daemon::daemon_process_active() {
            let _ = crate::daemon::telemetry_worker::submit_daemon_internal_cas(vec![cas_payload]);
        } else if crate::daemon::telemetry_handle::daemon_telemetry_available() {
            crate::daemon::telemetry_handle::submit_cas(vec![cas_payload]);
        }

        prompt.messages_url = Some(format!("{}/cas/{}", api_base_url, hash));
        prompt.messages.clear();
    }

    Ok(())
}

/// Record metrics for a committed change.
/// This is a best-effort operation - failures are silently ignored.
#[allow(clippy::too_many_arguments)]
fn record_commit_metrics(
    repo: &Repository,
    commit_sha: &str,
    parent_sha: &str,
    human_author: &str,
    authorship_note: &str,
    stats: &crate::authorship::stats::CommitStats,
    checkpoints: &[Checkpoint],
    hunks_json: Option<&str>,
) {
    use crate::metrics::{CommittedValues, EventAttributes, record};

    // Never emit telemetry for mock_ai (test preset).  If every tool in the
    // breakdown is mock_ai the entire committed event is test data.
    let only_mock_ai = !stats.tool_model_breakdown.is_empty()
        && stats
            .tool_model_breakdown
            .keys()
            .all(|k| k.starts_with("mock_ai::"));
    if only_mock_ai {
        return;
    }

    // Subtract mock_ai contributions from the aggregates so the "all" entry
    // only reflects real tools.
    let mut agg_ai = stats.ai_additions;
    let mut agg_accepted = stats.ai_accepted;
    for (key, ts) in &stats.tool_model_breakdown {
        if key.starts_with("mock_ai::") {
            agg_ai = agg_ai.saturating_sub(ts.ai_additions);
            agg_accepted = agg_accepted.saturating_sub(ts.ai_accepted);
        }
    }

    // Build parallel arrays: index 0 = "all" (aggregate), index 1+ = per tool/model
    let mut tool_model_pairs: Vec<String> = vec!["all".to_string()];
    let mut ai_additions: Vec<u32> = vec![agg_ai];
    let mut ai_accepted: Vec<u32> = vec![agg_accepted];

    // Add per-tool/model breakdown, skipping mock_ai (test preset)
    for (tool_model, tool_stats) in &stats.tool_model_breakdown {
        if tool_model.starts_with("mock_ai::") {
            continue;
        }
        tool_model_pairs.push(tool_model.clone());
        ai_additions.push(tool_stats.ai_additions);
        ai_accepted.push(tool_stats.ai_accepted);
    }

    // Build values with all stats
    let values = CommittedValues::new()
        .human_additions(stats.human_additions)
        .git_diff_deleted_lines(stats.git_diff_deleted_lines)
        .git_diff_added_lines(stats.git_diff_added_lines)
        .tool_model_pairs(tool_model_pairs)
        .ai_additions(ai_additions)
        .ai_accepted(ai_accepted);

    // Add first checkpoint timestamp (null if no checkpoints)
    let values = if let Some(first) = checkpoints.first() {
        values.first_checkpoint_ts(first.timestamp)
    } else {
        values.first_checkpoint_ts_null()
    };

    // Add commit subject and body
    let values = if let Ok(commit) = repo.find_commit(commit_sha.to_string()) {
        let subject = commit.summary().unwrap_or_default();
        let values = values.commit_subject(subject);
        let body = commit.body().unwrap_or_default();
        if body.is_empty() {
            values.commit_body_null()
        } else {
            values.commit_body(body)
        }
    } else {
        values.commit_subject_null().commit_body_null()
    };

    let values = values.authorship_note(authorship_note);

    let values = if let Some(hunks) = hunks_json {
        values.hunks(hunks)
    } else {
        values.hunks_null()
    };

    // Build attributes - start with version and extract session_id from first AI checkpoint
    // session_id links this commit to the AI agent conversation that produced it
    // Note: session_id removed from committed events - commits can contain code from multiple AI sessions
    let mut attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"));

    attrs = attrs
        .author(human_author)
        .commit_sha(commit_sha)
        .base_commit_sha(parent_sha);

    // Get repo URL from default remote
    if let Ok(Some(remote_name)) = repo.get_default_remote()
        && let Ok(remotes) = repo.remotes_with_urls()
        && let Some((_, url)) = remotes.into_iter().find(|(n, _)| n == &remote_name)
        && let Ok(normalized) = crate::repo_url::normalize_repo_url(&url)
    {
        attrs = attrs.repo_url(normalized);
    }

    // Get current branch
    if let Ok(head_ref) = repo.head()
        && let Ok(short_branch) = head_ref.shorthand()
    {
        attrs = attrs.branch(short_branch);
    }

    // Attach custom attributes using Config::fresh() to support runtime config updates
    attrs = attrs.custom_attributes_map(Config::fresh().custom_attributes());

    // Record the metric
    record(values, attrs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_line_ranges_handles_scattered_and_contiguous_lines() {
        assert_eq!(count_line_ranges(&[]), 0);
        assert_eq!(count_line_ranges(&[1]), 1);
        assert_eq!(count_line_ranges(&[1, 2, 3]), 1);
        assert_eq!(count_line_ranges(&[1, 3, 5]), 3);
        // Includes unsorted and duplicate values.
        assert_eq!(count_line_ranges(&[5, 3, 3, 4, 10]), 2);
    }

    #[test]
    fn test_should_skip_expensive_post_commit_stats_thresholds() {
        let below_threshold = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS - 1,
            added_lines: STATS_SKIP_MAX_ADDED_LINES - 1,
            hunk_ranges: STATS_SKIP_MAX_HUNKS - 1,
            deleted_lines: STATS_SKIP_MAX_DELETED_LINES - 1,
        };
        assert!(!should_skip_expensive_post_commit_stats(&below_threshold));

        let by_hunks = StatsCostEstimate {
            files_with_additions: 1,
            added_lines: 1,
            hunk_ranges: STATS_SKIP_MAX_HUNKS,
            deleted_lines: 0,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_hunks));

        let by_added_lines = StatsCostEstimate {
            files_with_additions: 1,
            added_lines: STATS_SKIP_MAX_ADDED_LINES,
            hunk_ranges: 1,
            deleted_lines: 0,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_added_lines));

        let by_files = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
            added_lines: 1,
            hunk_ranges: 1,
            deleted_lines: 0,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_files));

        let by_deleted_lines = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: STATS_SKIP_MAX_DELETED_LINES,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_deleted_lines));
    }

    #[test]
    fn test_count_line_ranges_single_element() {
        assert_eq!(count_line_ranges(&[42]), 1);
    }

    #[test]
    fn test_count_line_ranges_all_contiguous() {
        assert_eq!(count_line_ranges(&[1, 2, 3, 4, 5]), 1);
    }

    #[test]
    fn test_count_line_ranges_all_scattered() {
        assert_eq!(count_line_ranges(&[1, 10, 20, 30]), 4);
    }

    #[test]
    fn test_count_line_ranges_duplicates() {
        assert_eq!(count_line_ranges(&[5, 5, 5]), 1);
    }

    #[test]
    fn test_count_line_ranges_unsorted() {
        // After sort+dedup: [1, 2, 5, 6, 10] -> ranges: [1,2], [5,6], [10]
        assert_eq!(count_line_ranges(&[10, 5, 6, 1, 2]), 3);
    }

    #[test]
    fn test_count_line_ranges_two_ranges() {
        assert_eq!(count_line_ranges(&[1, 2, 3, 10, 11, 12]), 2);
    }

    #[test]
    fn test_should_skip_stats_exactly_at_thresholds() {
        // Exactly at the hunks threshold alone should trigger skip.
        let at_hunks = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: STATS_SKIP_MAX_HUNKS,
            deleted_lines: 0,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_hunks),
            "Exactly at hunk threshold should skip"
        );

        // Exactly at added-lines threshold alone should trigger skip.
        let at_added = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: STATS_SKIP_MAX_ADDED_LINES,
            hunk_ranges: 0,
            deleted_lines: 0,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_added),
            "Exactly at added-lines threshold should skip"
        );

        // Exactly at files-with-additions threshold alone should trigger skip.
        let at_files = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: 0,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_files),
            "Exactly at files-with-additions threshold should skip"
        );

        // Exactly at deleted-lines threshold alone should trigger skip.
        let at_deleted = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: STATS_SKIP_MAX_DELETED_LINES,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_deleted),
            "Exactly at deleted-lines threshold should skip"
        );

        // All at zero should NOT skip.
        let all_zero = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: 0,
        };
        assert!(
            !should_skip_expensive_post_commit_stats(&all_zero),
            "All zero values should not skip"
        );
    }

    #[test]
    fn test_checkpoint_input_debug_summary_groups_kinds_and_files() {
        let ai_checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            String::new(),
            "copilot".to_string(),
            vec![WorkingLogEntry::new(
                "src/main.rs".to_string(),
                "sha-ai".to_string(),
                Vec::new(),
                Vec::new(),
            )],
        );
        let known_human_checkpoint = Checkpoint::new(
            CheckpointKind::KnownHuman,
            String::new(),
            "liuwang".to_string(),
            vec![
                WorkingLogEntry::new(
                    ".gitignore".to_string(),
                    "sha-ignore".to_string(),
                    Vec::new(),
                    Vec::new(),
                ),
                WorkingLogEntry::new(
                    "src/lib.rs".to_string(),
                    "sha-lib".to_string(),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
        );
        let human_checkpoint = Checkpoint::new(
            CheckpointKind::Human,
            String::new(),
            "liuwang".to_string(),
            vec![WorkingLogEntry::new(
                "src/lib.rs".to_string(),
                "sha-human".to_string(),
                Vec::new(),
                Vec::new(),
            )],
        );

        let summary = checkpoint_input_debug_summary(&[
            ai_checkpoint,
            known_human_checkpoint,
            human_checkpoint,
        ]);

        assert_eq!(summary["checkpointCount"], serde_json::json!(3));
        assert_eq!(summary["checkpointEntryCount"], serde_json::json!(4));
        assert_eq!(
            summary["checkpointKindCounts"]["ai_agent"],
            serde_json::json!(1)
        );
        assert_eq!(
            summary["checkpointKindCounts"]["known_human"],
            serde_json::json!(1)
        );
        assert_eq!(
            summary["checkpointKindCounts"]["human"],
            serde_json::json!(1)
        );
        assert_eq!(summary["aiCheckpointCount"], serde_json::json!(1));
        assert_eq!(summary["aiEntryCount"], serde_json::json!(1));
        assert_eq!(summary["uniqueFileCount"], serde_json::json!(3));
        assert_eq!(
            summary["kindSummary"]["known_human"]["uniqueFileCount"],
            serde_json::json!(2)
        );
        assert_eq!(summary["onlyKnownHuman"], serde_json::json!(false));
        assert_eq!(summary["onlySingleFile"], serde_json::json!(false));
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
    fn plain_legacy_human_checkpoint_rejects_ai_or_downgraded_metadata() {
        let plain_human = Checkpoint::new(
            CheckpointKind::Human,
            String::new(),
            "tester".to_string(),
            vec![WorkingLogEntry::new(
                "src/plain.ts".to_string(),
                "sha".to_string(),
                Vec::new(),
                Vec::new(),
            )],
        );
        assert!(is_plain_legacy_human_checkpoint(&plain_human));

        let replay_human = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/replay.ts",
            &[("git_ai_replay_checkpoint", "true")],
        );
        assert!(!is_plain_legacy_human_checkpoint(&replay_human));

        let ai_pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/pre.ts",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        assert!(!is_plain_legacy_human_checkpoint(&ai_pre_edit));

        let downgraded_known_human = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/save.ts",
            &[("known_human_downgraded", "true")],
        );
        assert!(!is_plain_legacy_human_checkpoint(&downgraded_known_human));
    }

    #[test]
    fn ai_pre_edit_path_evidence_requires_matching_ai_edited_tool_use_and_file() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/from-pre.ts",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
                ("agent_tool", "github-copilot"),
            ],
        );
        let ai_edit = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/from-post.ts",
            &[("edit_kind", "file_edit"), ("tool_use_id", "call_1")],
        );
        let matching_ai_edit = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/from-pre.ts",
            &[("edit_kind", "file_edit"), ("tool_use_id", "call_1")],
        );
        let entry = pre_edit.entries.first().unwrap();

        let same_tool_different_file = collect_ai_edited_tool_use_files(&[ai_edit]);
        assert!(!checkpoint_has_ai_pre_edit_path_evidence(
            &pre_edit,
            entry,
            &same_tool_different_file
        ));

        let matched_files = collect_ai_edited_tool_use_files(&[matching_ai_edit]);
        assert!(checkpoint_has_ai_pre_edit_path_evidence(
            &pre_edit,
            entry,
            &matched_files
        ));

        let unmatched_files = collect_ai_edited_tool_use_files(&[]);
        assert!(!checkpoint_has_ai_pre_edit_path_evidence(
            &pre_edit,
            entry,
            &unmatched_files
        ));
    }

    #[test]
    fn ai_pre_edit_path_evidence_rejects_human_agent_tool() {
        let human_pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/from-pre.ts",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
                ("agent_tool", "human"),
            ],
        );
        let matching_ai_edit = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/from-pre.ts",
            &[("edit_kind", "file_edit"), ("tool_use_id", "call_1")],
        );
        let matched_files = collect_ai_edited_tool_use_files(&[matching_ai_edit]);
        let entry = human_pre_edit.entries.first().unwrap();

        assert!(!checkpoint_has_ai_pre_edit_path_evidence(
            &human_pre_edit,
            entry,
            &matched_files
        ));
    }

    #[test]
    fn ai_pre_edit_path_evidence_rejects_bash_and_plain_human() {
        let pre_edit = checkpoint_with_metadata(
            CheckpointKind::Human,
            "src/from-pre.ts",
            &[
                ("ai_pre_edit", "true"),
                ("edit_kind", "file_edit"),
                ("tool_use_id", "call_1"),
            ],
        );
        let bash_ai = checkpoint_with_metadata(
            CheckpointKind::AiAgent,
            "src/from-post.ts",
            &[("edit_kind", "bash"), ("tool_use_id", "call_1")],
        );
        let entry = pre_edit.entries.first().unwrap();
        let bash_files = collect_ai_edited_tool_use_files(&[bash_ai]);
        assert!(!checkpoint_has_ai_pre_edit_path_evidence(
            &pre_edit,
            entry,
            &bash_files
        ));

        let plain_human = Checkpoint::new(
            CheckpointKind::Human,
            String::new(),
            "tester".to_string(),
            vec![WorkingLogEntry::new(
                "src/plain.ts".to_string(),
                "sha".to_string(),
                Vec::new(),
                Vec::new(),
            )],
        );
        let mut matched_files = HashSet::new();
        matched_files.insert(("call_1".to_string(), "src/plain.ts".to_string()));
        let plain_entry = plain_human.entries.first().unwrap();
        assert!(!checkpoint_has_ai_pre_edit_path_evidence(
            &plain_human,
            plain_entry,
            &matched_files
        ));
    }

    #[test]
    fn test_attribution_gap_reason_distinguishes_all_and_partial_unknown() {
        let all_unknown = crate::authorship::stats::CommitStats {
            unknown_additions: 849,
            git_diff_added_lines: 849,
            ..Default::default()
        };
        assert_eq!(
            attribution_gap_reason(&all_unknown),
            Some("all_added_lines_unknown")
        );

        let with_human = crate::authorship::stats::CommitStats {
            human_additions: 1,
            unknown_additions: 848,
            git_diff_added_lines: 849,
            ..Default::default()
        };
        assert_eq!(
            attribution_gap_reason(&with_human),
            Some("partial_added_lines_unknown")
        );

        let deletion_only = crate::authorship::stats::CommitStats {
            git_diff_added_lines: 0,
            unknown_additions: 0,
            ..Default::default()
        };
        assert_eq!(attribution_gap_reason(&deletion_only), None);

        let fully_attributed = crate::authorship::stats::CommitStats {
            git_diff_added_lines: 10,
            ai_additions: 10,
            ..Default::default()
        };
        assert_eq!(attribution_gap_reason(&fully_attributed), None);
    }
}
