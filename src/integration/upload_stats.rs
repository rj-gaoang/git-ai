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
//!   * `GIT_AI_REPORT_IDE_NAME`          - optional IDE/editor name override for `clientContext`
//!   * `GIT_AI_REPORT_IDE_VERSION`       - optional IDE/editor version override for `clientContext`
//!   * `GIT_AI_REPORT_PLUGIN_VERSION`    - optional git-ai plugin/extension version for `clientContext`
//!
//! When the explicit IDE fields are unset, `clientContext.ideName` and
//! `clientContext.ideVersion` fall back to `TERM_PROGRAM` and
//! `TERM_PROGRAM_VERSION` when available.
//!
//! Uploads are best-effort: failures are logged and never propagated to the
//! caller, so a network outage cannot break a `git commit`.

use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};
use crate::authorship::stats::{
    CommitStats, accepted_lines_from_attestations_by_file, stats_for_commit_stats,
};
use crate::authorship::transcript::Message;
use crate::git::refs::{get_authorship, note_blob_oids_for_commits};
use crate::git::repository::{Repository, exec_git};
use crate::http;
use crate::integration::ide_mcp::resolve_x_user_id;
use crate::utils::LockFile;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_UPLOAD_URL: &str =
    "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats";
const UPLOAD_TIMEOUT_SECS: u64 = 20;
const UPLOAD_MAX_ATTEMPTS: usize = 3;
const UPLOAD_RETRY_DELAY_MILLIS: u64 = 1500;
const UPLOAD_ACTIVITY_LOCK_FILE: &str = "upload_activity.lock";
const UPLOAD_ACTIVITY_LOCK_WAIT_SECS_AUTO: u64 = 5;
const UPLOAD_ACTIVITY_LOCK_WAIT_SECS_MANUAL: u64 = 30;
const UPLOAD_ACTIVITY_LOCK_RETRY_MILLIS: u64 = 250;
const UPLOAD_STATUS_FILE: &str = "upload_stats_status.json";
const UPLOAD_STATUS_SCHEMA_VERSION: u32 = 1;
const MAX_STATUS_ERROR_CHARS: usize = 500;
const DEFAULT_UPLOAD_BACKLOG_LIMIT: usize = 25;

fn upload_activity_lock_path_from_internal_dir(internal_dir: &Path) -> PathBuf {
    internal_dir.join(UPLOAD_ACTIVITY_LOCK_FILE)
}

pub(crate) fn upload_activity_lock_path() -> Option<PathBuf> {
    crate::config::internal_dir_path().map(|dir| upload_activity_lock_path_from_internal_dir(&dir))
}

fn acquire_lock_with_retry(
    lock_path: &Path,
    max_wait: Duration,
    retry_interval: Duration,
) -> Option<LockFile> {
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let started_at = Instant::now();
    loop {
        if let Some(lock) = LockFile::try_acquire(lock_path) {
            return Some(lock);
        }

        if started_at.elapsed() >= max_wait {
            return None;
        }

        std::thread::sleep(retry_interval);
    }
}

fn upload_activity_lock_wait_duration(mode: &str) -> Duration {
    match mode {
        "manual" => Duration::from_secs(UPLOAD_ACTIVITY_LOCK_WAIT_SECS_MANUAL),
        _ => Duration::from_secs(UPLOAD_ACTIVITY_LOCK_WAIT_SECS_AUTO),
    }
}

fn acquire_upload_activity_lock(debug_context: &UploadDebugContext) -> Result<LockFile, String> {
    let lock_path = upload_activity_lock_path()
        .ok_or_else(|| "Could not determine git-ai upload activity lock path".to_string())?;
    let max_wait = upload_activity_lock_wait_duration(debug_context.mode.as_str());
    let wait_started_at = Instant::now();

    crate::diagnostics::append_debug_event(
        "upload_stats_activity_lock_wait_started",
        json!({
            "commitSha": debug_context.commit_sha.as_str(),
            "commitShort": debug_context.commit_short.as_str(),
            "source": debug_context.source.as_str(),
            "mode": debug_context.mode.as_str(),
            "lockPath": lock_path.display().to_string(),
            "maxWaitSecs": max_wait.as_secs(),
        }),
    );

    let lock = acquire_lock_with_retry(
        &lock_path,
        max_wait,
        Duration::from_millis(UPLOAD_ACTIVITY_LOCK_RETRY_MILLIS),
    );
    let waited_ms = wait_started_at
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;

    match lock {
        Some(lock) => {
            crate::diagnostics::append_debug_event(
                "upload_stats_activity_lock_acquired",
                json!({
                    "commitSha": debug_context.commit_sha.as_str(),
                    "commitShort": debug_context.commit_short.as_str(),
                    "source": debug_context.source.as_str(),
                    "mode": debug_context.mode.as_str(),
                    "lockPath": lock_path.display().to_string(),
                    "waitedMs": waited_ms,
                }),
            );
            Ok(lock)
        }
        None => {
            crate::diagnostics::append_debug_event(
                "upload_stats_activity_lock_timeout",
                json!({
                    "commitSha": debug_context.commit_sha.as_str(),
                    "commitShort": debug_context.commit_short.as_str(),
                    "source": debug_context.source.as_str(),
                    "mode": debug_context.mode.as_str(),
                    "lockPath": lock_path.display().to_string(),
                    "maxWaitSecs": max_wait.as_secs(),
                    "waitedMs": waited_ms,
                }),
            );
            Err(format!(
                "Timed out waiting {}s for upload activity lock at {}",
                max_wait.as_secs(),
                lock_path.display()
            ))
        }
    }
}

pub(crate) fn wait_for_upload_activity_to_finish(max_wait: Duration) -> bool {
    let Some(lock_path) = upload_activity_lock_path() else {
        return true;
    };

    acquire_lock_with_retry(
        &lock_path,
        max_wait,
        Duration::from_millis(UPLOAD_ACTIVITY_LOCK_RETRY_MILLIS),
    )
    .is_some()
}

#[derive(Debug, Clone)]
pub enum ManualUploadOutcome {
    DryRun {
        commit_sha: String,
        url: String,
        url_source: &'static str,
        payload_summary: Value,
    },
    Uploaded {
        commit_sha: String,
        url: String,
        status_code: u16,
    },
    Skipped {
        commit_sha: String,
        reason: &'static str,
    },
}

#[derive(Debug, Clone)]
struct UploadDebugContext {
    commit_sha: String,
    commit_short: String,
    source: String,
    mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NoteUploadStatus {
    NotUploaded,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadNoteRecord {
    upload_status: NoteUploadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note_blob_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_seen_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_success_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    upload_attempts: u32,
}

impl UploadNoteRecord {
    fn new_not_uploaded(note_blob_oid: Option<String>, now_ms: u64) -> Self {
        Self {
            upload_status: NoteUploadStatus::NotUploaded,
            note_blob_oid,
            first_seen_at_ms: Some(now_ms),
            last_attempt_at_ms: None,
            last_success_at_ms: None,
            last_failure_at_ms: None,
            last_status_code: None,
            last_error: None,
            upload_attempts: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadStatusIndex {
    #[serde(default = "upload_status_schema_version")]
    schema_version: u32,
    #[serde(default)]
    notes: BTreeMap<String, UploadNoteRecord>,
}

impl Default for UploadStatusIndex {
    fn default() -> Self {
        Self {
            schema_version: UPLOAD_STATUS_SCHEMA_VERSION,
            notes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct UploadCandidateSeed {
    commit_sha: String,
    authorship_log: Option<AuthorshipLog>,
    stats: Option<CommitStats>,
}

#[derive(Debug, Clone)]
struct PreparedUploadBatch {
    payload: Value,
    commit_shas: Vec<String>,
    payload_summary: Value,
}

fn upload_status_schema_version() -> u32 {
    UPLOAD_STATUS_SCHEMA_VERSION
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn upload_status_path(repo: &Repository) -> PathBuf {
    repo.storage.ai_dir.join(UPLOAD_STATUS_FILE)
}

fn load_upload_status_index(repo: &Repository) -> UploadStatusIndex {
    let path = upload_status_path(repo);
    let Ok(content) = fs::read_to_string(&path) else {
        return UploadStatusIndex::default();
    };

    match serde_json::from_str::<UploadStatusIndex>(&content) {
        Ok(mut index) => {
            index.schema_version = UPLOAD_STATUS_SCHEMA_VERSION;
            index
        }
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "upload_stats_status_load_failed",
                json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "path": path.display().to_string(),
                    "error": error.to_string(),
                }),
            );
            UploadStatusIndex::default()
        }
    }
}

fn save_upload_status_index(repo: &Repository, index: &UploadStatusIndex) -> Result<(), String> {
    let path = upload_status_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let content = serde_json::to_string_pretty(index).map_err(|error| error.to_string())?;
    fs::write(&path, content).map_err(|error| error.to_string())?;
    Ok(())
}

fn truncate_status_error(error: &str) -> String {
    error.chars().take(MAX_STATUS_ERROR_CHARS).collect()
}

fn ensure_note_status_record<'a>(
    index: &'a mut UploadStatusIndex,
    commit_sha: &str,
    note_blob_oid: Option<String>,
    now_ms: u64,
) -> &'a mut UploadNoteRecord {
    let record = index
        .notes
        .entry(commit_sha.to_string())
        .or_insert_with(|| UploadNoteRecord::new_not_uploaded(note_blob_oid.clone(), now_ms));

    if record.first_seen_at_ms.is_none() {
        record.first_seen_at_ms = Some(now_ms);
    }

    if let Some(note_blob_oid) = note_blob_oid
        && record.note_blob_oid.as_deref() != Some(note_blob_oid.as_str())
    {
        record.note_blob_oid = Some(note_blob_oid);
        record.upload_status = NoteUploadStatus::NotUploaded;
        record.last_status_code = None;
        record.last_error = None;
    }

    record
}

fn mark_upload_attempt_started(index: &mut UploadStatusIndex, commit_shas: &[String], now_ms: u64) {
    for commit_sha in commit_shas {
        let record = ensure_note_status_record(index, commit_sha, None, now_ms);
        record.last_attempt_at_ms = Some(now_ms);
        record.upload_attempts = record.upload_attempts.saturating_add(1);
    }
}

fn mark_upload_succeeded(
    index: &mut UploadStatusIndex,
    commit_shas: &[String],
    status_code: u16,
    now_ms: u64,
) {
    for commit_sha in commit_shas {
        let record = ensure_note_status_record(index, commit_sha, None, now_ms);
        record.upload_status = NoteUploadStatus::Succeeded;
        record.last_success_at_ms = Some(now_ms);
        record.last_status_code = Some(status_code);
        record.last_error = None;
    }
}

fn mark_upload_failed(
    index: &mut UploadStatusIndex,
    commit_shas: &[String],
    error: &str,
    now_ms: u64,
) {
    let error = truncate_status_error(error);
    for commit_sha in commit_shas {
        let record = ensure_note_status_record(index, commit_sha, None, now_ms);
        record.upload_status = NoteUploadStatus::Failed;
        record.last_failure_at_ms = Some(now_ms);
        record.last_status_code = None;
        record.last_error = Some(error.clone());
    }
}

fn mark_note_not_uploaded_best_effort(repo: &Repository, commit_sha: &str, reason: &str) {
    let now_ms = now_epoch_ms();
    let mut index = load_upload_status_index(repo);
    let note_blob_oid = note_blob_oid_for_commit(repo, commit_sha);
    ensure_note_status_record(&mut index, commit_sha, note_blob_oid, now_ms);

    if let Err(error) = save_upload_status_index(repo, &index) {
        crate::diagnostics::append_debug_event(
            "upload_stats_status_save_failed",
            json!({
                "commitSha": commit_sha,
                "commitShort": short_sha(commit_sha),
                "source": "auto",
                "mode": "not_uploaded_marker",
                "reason": reason,
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "error": error,
            }),
        );
    }
}

impl ManualUploadOutcome {
    pub fn commit_sha(&self) -> &str {
        match self {
            Self::DryRun { commit_sha, .. }
            | Self::Uploaded { commit_sha, .. }
            | Self::Skipped { commit_sha, .. } => commit_sha,
        }
    }
}

fn list_local_authorship_notes(repo: &Repository) -> Result<BTreeMap<String, String>, String> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("list".to_string());

    let output = match exec_git(&args) {
        Ok(output) => output,
        Err(crate::error::GitAiError::GitCliError { code: Some(1), .. }) => {
            return Ok(BTreeMap::new());
        }
        Err(error) => return Err(error.to_string()),
    };

    let stdout = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let mut notes = BTreeMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(note_blob_oid) = parts.next() else {
            continue;
        };
        let Some(commit_sha) = parts.next() else {
            continue;
        };
        notes.insert(commit_sha.to_string(), note_blob_oid.to_string());
    }

    Ok(notes)
}

fn list_head_reachable_commits(repo: &Repository) -> Result<BTreeSet<String>, String> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("HEAD".to_string());

    let output = exec_git(&args).map_err(|error| error.to_string())?;
    let stdout = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;

    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn note_blob_oid_for_commit(repo: &Repository, commit_sha: &str) -> Option<String> {
    note_blob_oids_for_commits(repo, &[commit_sha.to_string()])
        .ok()
        .and_then(|mut notes| notes.remove(commit_sha))
}

fn sync_reachable_note_statuses(
    repo: &Repository,
    index: &mut UploadStatusIndex,
    explicit_commits: &[String],
    now_ms: u64,
) -> BTreeMap<String, String> {
    let all_notes = match list_local_authorship_notes(repo) {
        Ok(notes) => notes,
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "upload_stats_status_note_list_failed",
                json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "error": error,
                }),
            );
            BTreeMap::new()
        }
    };

    let reachable_commits = match list_head_reachable_commits(repo) {
        Ok(commits) => commits,
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "upload_stats_status_rev_list_failed",
                json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "error": error,
                }),
            );
            BTreeSet::new()
        }
    };

    let explicit_set = explicit_commits.iter().cloned().collect::<BTreeSet<_>>();
    let mut eligible_notes = BTreeMap::new();
    for (commit_sha, note_blob_oid) in all_notes {
        if reachable_commits.contains(&commit_sha) || explicit_set.contains(&commit_sha) {
            ensure_note_status_record(index, &commit_sha, Some(note_blob_oid.clone()), now_ms);
            eligible_notes.insert(commit_sha, note_blob_oid);
        }
    }

    for commit_sha in explicit_commits {
        if !eligible_notes.contains_key(commit_sha)
            && let Some(note_blob_oid) = note_blob_oid_for_commit(repo, commit_sha)
        {
            ensure_note_status_record(index, commit_sha, Some(note_blob_oid.clone()), now_ms);
            eligible_notes.insert(commit_sha.clone(), note_blob_oid);
        }
    }

    eligible_notes
}

fn ordered_upload_candidates(
    index: &UploadStatusIndex,
    eligible_notes: &BTreeMap<String, String>,
    previously_tracked: &HashSet<String>,
    seeds: &[UploadCandidateSeed],
    backlog_limit: usize,
) -> Vec<String> {
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();

    for seed in seeds {
        if eligible_notes.contains_key(&seed.commit_sha) && seen.insert(seed.commit_sha.clone()) {
            ordered.push(seed.commit_sha.clone());
        }
    }

    let mut backlog_count = 0usize;
    for status in [NoteUploadStatus::Failed, NoteUploadStatus::NotUploaded] {
        for (commit_sha, record) in &index.notes {
            if backlog_count >= backlog_limit {
                return ordered;
            }
            if record.upload_status == status
                && previously_tracked.contains(commit_sha)
                && eligible_notes.contains_key(commit_sha)
                && seen.insert(commit_sha.clone())
            {
                ordered.push(commit_sha.clone());
                backlog_count += 1;
            }
        }
    }

    ordered
}

fn prepare_upload_batch(
    repo: &Repository,
    index: &mut UploadStatusIndex,
    seeds: &[UploadCandidateSeed],
    ignore_patterns: &[String],
    source: &str,
) -> Result<PreparedUploadBatch, String> {
    let now_ms = now_epoch_ms();
    let explicit_commits = seeds
        .iter()
        .map(|seed| seed.commit_sha.clone())
        .collect::<Vec<_>>();
    let previously_tracked = index.notes.keys().cloned().collect::<HashSet<_>>();
    let seed_map = seeds
        .iter()
        .map(|seed| (seed.commit_sha.as_str(), seed))
        .collect::<HashMap<_, _>>();

    let eligible_notes = sync_reachable_note_statuses(repo, index, &explicit_commits, now_ms);
    let ordered_commits = ordered_upload_candidates(
        index,
        &eligible_notes,
        &previously_tracked,
        seeds,
        upload_backlog_limit(),
    );

    let mut commit_entries = Vec::new();
    let mut commit_shas = Vec::new();
    for commit_sha in ordered_commits {
        let seed = seed_map.get(commit_sha.as_str()).copied();
        let authorship_log = seed
            .and_then(|seed| seed.authorship_log.clone())
            .or_else(|| get_authorship(repo, &commit_sha));
        let Some(authorship_log) = authorship_log else {
            let error = "authorship note disappeared before upload".to_string();
            mark_upload_failed(
                index,
                std::slice::from_ref(&commit_sha),
                &error,
                now_epoch_ms(),
            );
            crate::diagnostics::append_debug_event(
                "upload_stats_candidate_skipped",
                json!({
                    "commitSha": commit_sha.as_str(),
                    "commitShort": short_sha(&commit_sha),
                    "source": source,
                    "reason": "authorship_note_missing",
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                }),
            );
            continue;
        };

        let stats = match seed.and_then(|seed| seed.stats.clone()) {
            Some(stats) => stats,
            None => match stats_for_commit_stats(repo, &commit_sha, ignore_patterns) {
                Ok(stats) => stats,
                Err(error) => {
                    let error = error.to_string();
                    mark_upload_failed(
                        index,
                        std::slice::from_ref(&commit_sha),
                        &format!("stats build failed: {}", error),
                        now_epoch_ms(),
                    );
                    crate::diagnostics::append_debug_event(
                        "upload_stats_candidate_skipped",
                        json!({
                            "commitSha": commit_sha.as_str(),
                            "commitShort": short_sha(&commit_sha),
                            "source": source,
                            "reason": "stats_build_failed",
                            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                            "error": error,
                        }),
                    );
                    continue;
                }
            },
        };

        match build_commit_entry(repo, &commit_sha, &authorship_log, &stats) {
            Ok(commit_entry) => {
                commit_entries.push(Value::Object(commit_entry));
                commit_shas.push(commit_sha);
            }
            Err(error) => {
                mark_upload_failed(
                    index,
                    std::slice::from_ref(&commit_sha),
                    &format!("payload build failed: {}", error),
                    now_epoch_ms(),
                );
                crate::diagnostics::append_debug_event(
                    "upload_stats_candidate_skipped",
                    json!({
                        "commitSha": commit_sha.as_str(),
                        "commitShort": short_sha(&commit_sha),
                        "source": source,
                        "reason": "payload_build_failed",
                        "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                        "error": error,
                    }),
                );
            }
        }
    }

    if commit_entries.is_empty() {
        return Err("no uploadable authorship notes found".to_string());
    }

    let payload = build_payload_from_commit_entries(repo, commit_entries, source)?;
    let payload_summary = upload_payload_summary(&payload);
    Ok(PreparedUploadBatch {
        payload,
        commit_shas,
        payload_summary,
    })
}

/// Resolve the upload endpoint URL from the environment, falling back to the
/// team-managed default when no override is provided.
#[cfg(test)]
fn resolve_upload_url() -> String {
    resolve_upload_url_with_source().0
}

fn resolve_upload_url_with_source() -> (String, &'static str) {
    if let Some(url) = env_non_empty("GIT_AI_REPORT_REMOTE_URL") {
        return (url, "GIT_AI_REPORT_REMOTE_URL");
    }

    let endpoint = env_non_empty("GIT_AI_REPORT_REMOTE_ENDPOINT");
    let path = env_non_empty("GIT_AI_REPORT_REMOTE_PATH");
    if let (Some(endpoint), Some(path)) = (endpoint.as_ref(), path.as_ref()) {
        let endpoint = endpoint.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        return (
            format!("{}/{}", endpoint, path),
            "GIT_AI_REPORT_REMOTE_ENDPOINT+GIT_AI_REPORT_REMOTE_PATH",
        );
    }

    (DEFAULT_UPLOAD_URL.to_string(), "built_in_default")
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

fn env_first_non_empty(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| env_non_empty(name))
}

fn upload_backlog_limit() -> usize {
    env_non_empty("GIT_AI_UPLOAD_BACKLOG_LIMIT")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_UPLOAD_BACKLOG_LIMIT)
}

fn git_ai_cli_version() -> String {
    if cfg!(debug_assertions) {
        format!("{} (debug)", env!("CARGO_PKG_VERSION"))
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

fn git_version_string() -> Option<String> {
    let output = crate::git::repository::exec_git(&["--version".to_string()]).ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(
        trimmed
            .strip_prefix("git version ")
            .unwrap_or(trimmed)
            .trim()
            .to_string(),
    )
}

fn normalize_ide_name(name: &str) -> String {
    match name.trim().to_ascii_lowercase().as_str() {
        "vscode" | "code" | "visual studio code" => "VS Code".to_string(),
        "cursor" => "Cursor".to_string(),
        "windsurf" => "Windsurf".to_string(),
        "intellij" | "idea" | "intellij idea" => "IntelliJ IDEA".to_string(),
        _ => name.trim().to_string(),
    }
}

fn ide_name() -> Option<String> {
    env_first_non_empty(&[
        "GIT_AI_REPORT_IDE_NAME",
        "GIT_AI_IDE_NAME",
        "GIT_AI_EDITOR_NAME",
        "GIT_AI_EDITOR",
    ])
    .or_else(|| env_non_empty("TERM_PROGRAM"))
    .or_else(|| std::env::var_os("VSCODE_GIT_IPC_HANDLE").map(|_| "VS Code".to_string()))
    .map(|value| normalize_ide_name(&value))
}

fn ide_version() -> Option<String> {
    env_first_non_empty(&[
        "GIT_AI_REPORT_IDE_VERSION",
        "GIT_AI_IDE_VERSION",
        "GIT_AI_EDITOR_VERSION",
    ])
    .or_else(|| env_non_empty("TERM_PROGRAM_VERSION"))
}

fn plugin_version() -> Option<String> {
    env_first_non_empty(&[
        "GIT_AI_REPORT_PLUGIN_VERSION",
        "GIT_AI_PLUGIN_VERSION",
        "GIT_AI_REPORT_EXTENSION_VERSION",
        "GIT_AI_EXTENSION_VERSION",
    ])
}

fn build_client_context() -> Value {
    json!({
        "gitAiCliVersion": git_ai_cli_version(),
        "gitAiPluginVersion": plugin_version(),
        "ideName": ide_name(),
        "ideVersion": ide_version(),
        "gitVersion": git_version_string(),
    })
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
    let commit_short = short_sha(commit_sha).to_string();
    crate::diagnostics::append_debug_event(
        "upload_stats_auto_entered",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": "auto",
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "autoUploadEnabled": feature_flags.auto_upload_ai_stats,
            "asyncMode": feature_flags.async_mode,
            "hasStats": stats.is_some(),
            "promptCount": authorship_log.metadata.prompts.len(),
            "attestationFileCount": authorship_log.attestations.len(),
        }),
    );

    if !feature_flags.auto_upload_ai_stats {
        crate::diagnostics::append_debug_event(
            "upload_stats_skipped",
            json!({
                "reason": "feature_flag_disabled",
                "source": "auto",
                "commitSha": commit_sha,
                "commitShort": short_sha(commit_sha),
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "envAutoUploadAiStats": std::env::var("GIT_AI_AUTO_UPLOAD_AI_STATS").ok(),
                "asyncMode": feature_flags.async_mode,
            }),
        );
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
        crate::diagnostics::append_debug_event(
            "upload_stats_skipped",
            json!({
                "reason": "stats_unavailable",
                "source": "auto",
                "commitSha": commit_sha,
                "commitShort": short_sha(commit_sha),
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "hint": "post_commit_stats_skipped event should explain whether this was a merge commit or an expensive commit",
            }),
        );
        mark_note_not_uploaded_best_effort(repo, commit_sha, "stats_unavailable");
        log_debug("stats unavailable for this commit; skipping upload");
        return;
    };

    crate::diagnostics::append_debug_event(
        "upload_stats_payload_build_started",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": "auto",
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "statsSummary": upload_stats_summary(stats),
            "promptCount": authorship_log.metadata.prompts.len(),
            "attestationFileCount": authorship_log.attestations.len(),
        }),
    );

    let (url, url_source) = resolve_upload_url_with_source();
    let api_key = env_non_empty("GIT_AI_REPORT_REMOTE_API_KEY");
    let explicit_user_id = env_non_empty("GIT_AI_REPORT_REMOTE_USER_ID");
    let user_id = explicit_user_id
        .clone()
        .or_else(|| resolve_x_user_id(Some(repo.canonical_workdir())));
    crate::diagnostics::append_debug_event(
        "upload_stats_ready",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": "auto",
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "mode": if feature_flags.async_mode { "background" } else { "inline" },
            "url": url,
            "urlSource": url_source,
            "hasApiKey": api_key.is_some(),
            "hasUserId": user_id.is_some(),
            "userIdSource": if explicit_user_id.is_some() { "GIT_AI_REPORT_REMOTE_USER_ID" } else if user_id.is_some() { "ide_mcp_config" } else { "missing" },
            "pendingBatch": true,
        }),
    );
    dispatch_upload(UploadDispatch {
        run_in_background: feature_flags.async_mode,
        repo: repo.clone(),
        url,
        api_key,
        user_id,
        seeds: vec![UploadCandidateSeed {
            commit_sha: commit_sha.to_string(),
            authorship_log: Some(authorship_log.clone()),
            stats: Some(stats.clone()),
        }],
        ignore_patterns: Vec::new(),
        commit_short,
        source: "auto".to_string(),
    });
}

pub fn upload_local_commit_stats(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
    dry_run: bool,
    source: &str,
) -> Result<ManualUploadOutcome, String> {
    let commit_short = short_sha(commit_sha).to_string();
    crate::diagnostics::append_debug_event(
        "upload_stats_manual_entered",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "dryRun": dry_run,
            "ignorePatternCount": ignore_patterns.len(),
        }),
    );

    let Some(authorship_log) = get_authorship(repo, commit_sha) else {
        crate::diagnostics::append_debug_event(
            "upload_stats_skipped",
            json!({
                "reason": "manual_no_authorship_note",
                "source": source,
                "commitSha": commit_sha,
                "commitShort": commit_short,
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            }),
        );
        return Ok(ManualUploadOutcome::Skipped {
            commit_sha: commit_sha.to_string(),
            reason: "no_authorship_note",
        });
    };

    crate::diagnostics::append_debug_event(
        "upload_stats_manual_authorship_note_loaded",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "promptCount": authorship_log.metadata.prompts.len(),
            "attestationFileCount": authorship_log.attestations.len(),
        }),
    );

    let stats = stats_for_commit_stats(repo, commit_sha, ignore_patterns).map_err(|err| {
        let error = err.to_string();
        crate::diagnostics::append_debug_event(
            "upload_stats_skipped",
            json!({
                "reason": "manual_stats_build_failed",
                "source": source,
                "commitSha": commit_sha,
                "commitShort": commit_short,
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "error": error,
            }),
        );
        error
    })?;

    crate::diagnostics::append_debug_event(
        "upload_stats_manual_stats_computed",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "statsSummary": upload_stats_summary(&stats),
        }),
    );

    crate::diagnostics::append_debug_event(
        "upload_stats_payload_build_started",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "statsSummary": upload_stats_summary(&stats),
            "promptCount": authorship_log.metadata.prompts.len(),
            "attestationFileCount": authorship_log.attestations.len(),
        }),
    );

    let (url, url_source) = resolve_upload_url_with_source();
    let api_key = env_non_empty("GIT_AI_REPORT_REMOTE_API_KEY");
    let explicit_user_id = env_non_empty("GIT_AI_REPORT_REMOTE_USER_ID");
    let user_id = explicit_user_id
        .clone()
        .or_else(|| resolve_x_user_id(Some(repo.canonical_workdir())));

    let debug_context = UploadDebugContext {
        commit_sha: commit_sha.to_string(),
        commit_short: commit_short.clone(),
        source: source.to_string(),
        mode: if dry_run {
            "manual_dry_run".to_string()
        } else {
            "manual".to_string()
        },
    };

    let _upload_activity_lock = acquire_upload_activity_lock(&debug_context)?;
    let mut status_index = load_upload_status_index(repo);
    let seeds = vec![UploadCandidateSeed {
        commit_sha: commit_sha.to_string(),
        authorship_log: Some(authorship_log.clone()),
        stats: Some(stats.clone()),
    }];
    let batch = prepare_upload_batch(repo, &mut status_index, &seeds, ignore_patterns, source)
        .map_err(|error| {
            let _ = save_upload_status_index(repo, &status_index);
            crate::diagnostics::append_debug_event(
                "upload_stats_skipped",
                json!({
                    "reason": "payload_build_failed",
                    "source": source,
                    "commitSha": commit_sha,
                    "commitShort": commit_short,
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "error": error,
                }),
            );
            error
        })?;
    crate::diagnostics::append_debug_event(
        "upload_stats_payload_build_succeeded",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "payloadSummary": batch.payload_summary.clone(),
        }),
    );

    crate::diagnostics::append_debug_event(
        "upload_stats_ready",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
            "mode": if dry_run { "manual_dry_run" } else { "manual" },
            "url": url,
            "urlSource": url_source,
            "hasApiKey": api_key.is_some(),
            "hasUserId": user_id.is_some(),
            "userIdSource": if explicit_user_id.is_some() { "GIT_AI_REPORT_REMOTE_USER_ID" } else if user_id.is_some() { "ide_mcp_config" } else { "missing" },
            "payloadSummary": batch.payload_summary.clone(),
        }),
    );

    if dry_run {
        log_info(&format!(
            "prepared dry-run upload for {} source={} url={}",
            commit_short, source, url
        ));
        crate::diagnostics::append_debug_event(
            "upload_stats_dry_run_prepared",
            json!({
                "commitSha": commit_sha,
                "commitShort": commit_short,
                "source": source,
                "mode": "manual_dry_run",
                "url": url,
                "urlSource": url_source,
                "payloadSummary": batch.payload_summary.clone(),
            }),
        );
        return Ok(ManualUploadOutcome::DryRun {
            commit_sha: commit_sha.to_string(),
            url,
            url_source,
            payload_summary: batch.payload_summary.clone(),
        });
    }

    mark_upload_attempt_started(&mut status_index, &batch.commit_shas, now_epoch_ms());
    if let Err(error) = save_upload_status_index(repo, &status_index) {
        crate::diagnostics::append_debug_event(
            "upload_stats_status_save_failed",
            json!({
                "commitSha": commit_sha,
                "commitShort": commit_short,
                "source": source,
                "mode": "manual",
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "error": error,
            }),
        );
    }

    log_info(&format!(
        "starting manual upload for {} source={} url={} has_api_key={} has_user_id={}",
        commit_short,
        source,
        url,
        api_key.is_some(),
        user_id.is_some()
    ));
    crate::diagnostics::append_debug_event(
        "upload_stats_started",
        json!({
            "commitSha": commit_sha,
            "commitShort": commit_short,
            "source": source,
            "mode": "manual",
            "url": url,
            "hasApiKey": api_key.is_some(),
            "hasUserId": user_id.is_some(),
            "payloadSummary": batch.payload_summary.clone(),
        }),
    );

    match perform_upload_with_lock_held(
        &url,
        &batch.payload,
        api_key.as_deref(),
        user_id.as_deref(),
        &debug_context,
    ) {
        Ok(status_code) => {
            crate::diagnostics::append_debug_event(
                "upload_stats_succeeded",
                json!({
                    "commitSha": commit_sha,
                    "commitShort": commit_short,
                    "source": source,
                    "mode": "manual",
                    "url": url,
                    "statusCode": status_code,
                }),
            );
            mark_upload_succeeded(
                &mut status_index,
                &batch.commit_shas,
                status_code,
                now_epoch_ms(),
            );
            if let Err(error) = save_upload_status_index(repo, &status_index) {
                crate::diagnostics::append_debug_event(
                    "upload_stats_status_save_failed",
                    json!({
                        "commitSha": commit_sha,
                        "commitShort": commit_short,
                        "source": source,
                        "mode": "manual",
                        "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                        "error": error,
                    }),
                );
            }
            log_info(&format!(
                "uploaded manual stats for {} source={} status={}",
                commit_short, source, status_code
            ));
            Ok(ManualUploadOutcome::Uploaded {
                commit_sha: commit_sha.to_string(),
                url,
                status_code,
            })
        }
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "upload_stats_failed",
                json!({
                    "commitSha": commit_sha,
                    "commitShort": commit_short,
                    "source": source,
                    "mode": "manual",
                    "url": url,
                    "error": &error,
                    "hasApiKey": api_key.is_some(),
                    "hasUserId": user_id.is_some(),
                }),
            );
            mark_upload_failed(
                &mut status_index,
                &batch.commit_shas,
                &error,
                now_epoch_ms(),
            );
            if let Err(save_error) = save_upload_status_index(repo, &status_index) {
                crate::diagnostics::append_debug_event(
                    "upload_stats_status_save_failed",
                    json!({
                        "commitSha": commit_sha,
                        "commitShort": commit_short,
                        "source": source,
                        "mode": "manual",
                        "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                        "error": save_error,
                    }),
                );
            }
            log_warn(&format!(
                "manual upload failed for {} source={}: {}",
                commit_short, source, error
            ));
            Err(error)
        }
    }
}

struct UploadDispatch {
    run_in_background: bool,
    repo: Repository,
    url: String,
    api_key: Option<String>,
    user_id: Option<String>,
    seeds: Vec<UploadCandidateSeed>,
    ignore_patterns: Vec<String>,
    commit_short: String,
    source: String,
}

fn dispatch_upload(request: UploadDispatch) {
    let UploadDispatch {
        run_in_background,
        repo,
        url,
        api_key,
        user_id,
        seeds,
        ignore_patterns,
        commit_short,
        source,
    } = request;
    let upload_mode = if run_in_background {
        "background"
    } else {
        "inline"
    };
    let commit_sha = seeds
        .first()
        .map(|seed| seed.commit_sha.clone())
        .unwrap_or_default();
    let debug_context = UploadDebugContext {
        commit_sha: commit_sha.clone(),
        commit_short: commit_short.clone(),
        source: source.clone(),
        mode: upload_mode.to_string(),
    };

    run_upload_task(run_in_background, move || {
        let _upload_activity_lock = match acquire_upload_activity_lock(&debug_context) {
            Ok(lock) => lock,
            Err(error) => {
                crate::diagnostics::append_debug_event(
                    "upload_stats_failed",
                    json!({
                        "commitSha": commit_sha.as_str(),
                        "commitShort": commit_short.as_str(),
                        "source": source.as_str(),
                        "mode": upload_mode,
                        "url": url.as_str(),
                        "error": error,
                        "hasApiKey": api_key.is_some(),
                        "hasUserId": user_id.is_some(),
                    }),
                );
                return;
            }
        };

        let mut status_index = load_upload_status_index(&repo);
        let batch =
            match prepare_upload_batch(&repo, &mut status_index, &seeds, &ignore_patterns, &source)
            {
                Ok(batch) => batch,
                Err(error) => {
                    let _ = save_upload_status_index(&repo, &status_index);
                    crate::diagnostics::append_debug_event(
                        "upload_stats_skipped",
                        json!({
                            "reason": "payload_build_failed",
                            "commitSha": commit_sha.as_str(),
                            "commitShort": commit_short.as_str(),
                            "source": source.as_str(),
                            "mode": upload_mode,
                            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                            "error": error,
                        }),
                    );
                    log_warn(&format!(
                        "skipping upload for {} because payload build failed: {}",
                        commit_short, error
                    ));
                    return;
                }
            };

        crate::diagnostics::append_debug_event(
            "upload_stats_payload_build_succeeded",
            json!({
                "commitSha": commit_sha.as_str(),
                "commitShort": commit_short.as_str(),
                "source": source.as_str(),
                "mode": upload_mode,
                "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                "payloadSummary": batch.payload_summary.clone(),
            }),
        );

        mark_upload_attempt_started(&mut status_index, &batch.commit_shas, now_epoch_ms());
        if let Err(error) = save_upload_status_index(&repo, &status_index) {
            crate::diagnostics::append_debug_event(
                "upload_stats_status_save_failed",
                json!({
                    "commitSha": commit_sha.as_str(),
                    "commitShort": commit_short.as_str(),
                    "source": source.as_str(),
                    "mode": upload_mode,
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "error": error,
                }),
            );
        }

        log_info(&format!(
            "starting upload for {} mode={} url={} has_api_key={} has_user_id={}",
            commit_short,
            upload_mode,
            url,
            api_key.is_some(),
            user_id.is_some()
        ));
        crate::diagnostics::append_debug_event(
            "upload_stats_started",
            json!({
                "commitSha": commit_sha.as_str(),
                "commitShort": commit_short.as_str(),
                "source": source.as_str(),
                "mode": upload_mode,
                "url": url.as_str(),
                "hasApiKey": api_key.is_some(),
                "hasUserId": user_id.is_some(),
                "payloadSummary": batch.payload_summary.clone(),
            }),
        );
        match perform_upload_with_lock_held(
            &url,
            &batch.payload,
            api_key.as_deref(),
            user_id.as_deref(),
            &debug_context,
        ) {
            Err(err) => {
                crate::diagnostics::append_debug_event(
                    "upload_stats_failed",
                    json!({
                        "commitSha": commit_sha.as_str(),
                        "commitShort": commit_short.as_str(),
                        "source": source.as_str(),
                        "mode": upload_mode,
                        "url": url.as_str(),
                        "error": err.as_str(),
                        "hasApiKey": api_key.is_some(),
                        "hasUserId": user_id.is_some(),
                    }),
                );
                mark_upload_failed(&mut status_index, &batch.commit_shas, &err, now_epoch_ms());
                if let Err(error) = save_upload_status_index(&repo, &status_index) {
                    crate::diagnostics::append_debug_event(
                        "upload_stats_status_save_failed",
                        json!({
                            "commitSha": commit_sha.as_str(),
                            "commitShort": commit_short.as_str(),
                            "source": source.as_str(),
                            "mode": upload_mode,
                            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                            "error": error,
                        }),
                    );
                }
                log_warn(&format!("upload failed for {}: {}", commit_short, err));
            }
            Ok(status_code) => {
                crate::diagnostics::append_debug_event(
                    "upload_stats_succeeded",
                    json!({
                        "commitSha": commit_sha.as_str(),
                        "commitShort": commit_short.as_str(),
                        "source": source.as_str(),
                        "mode": upload_mode,
                        "url": url.as_str(),
                        "statusCode": status_code,
                    }),
                );
                mark_upload_succeeded(
                    &mut status_index,
                    &batch.commit_shas,
                    status_code,
                    now_epoch_ms(),
                );
                if let Err(error) = save_upload_status_index(&repo, &status_index) {
                    crate::diagnostics::append_debug_event(
                        "upload_stats_status_save_failed",
                        json!({
                            "commitSha": commit_sha.as_str(),
                            "commitShort": commit_short.as_str(),
                            "source": source.as_str(),
                            "mode": upload_mode,
                            "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                            "error": error,
                        }),
                    );
                }
                log_info(&format!("uploaded stats for {}", commit_short));
            }
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
    if crate::diagnostics::debug_stderr_enabled() {
        eprintln!("[git-ai] upload-ai-stats: {}", message);
    }
}

fn upload_stats_summary(stats: &CommitStats) -> Value {
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
        "toolModelBreakdownCount": stats.tool_model_breakdown.len(),
    })
}

fn upload_payload_summary(payload: &Value) -> Value {
    let commits = payload
        .get("commits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let first_commit = commits.first();
    let stats = first_commit.and_then(|commit| commit.get("stats"));
    let prompts = first_commit
        .and_then(|commit| commit.get("prompts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let files = stats
        .and_then(|stats| stats.get("files"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prompt_text_count = prompts
        .iter()
        .filter(|prompt| {
            prompt
                .get("promptText")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty())
        })
        .count();

    json!({
        "commitCount": commits.len(),
        "firstCommitSha": first_commit.and_then(|commit| commit.get("commitSha")).and_then(Value::as_str),
        "hasAuthorshipNote": first_commit.and_then(|commit| commit.get("hasAuthorshipNote")).and_then(Value::as_bool),
        "fileCount": files.len(),
        "promptCount": prompts.len(),
        "promptsWithText": prompt_text_count,
        "stats": stats.map(|stats| json!({
            "humanAdditions": stats.get("humanAdditions").and_then(Value::as_u64).unwrap_or(0),
            "unknownAdditions": stats.get("unknownAdditions").and_then(Value::as_u64).unwrap_or(0),
            "aiAdditions": stats.get("aiAdditions").and_then(Value::as_u64).unwrap_or(0),
            "gitDiffAddedLines": stats.get("gitDiffAddedLines").and_then(Value::as_u64).unwrap_or(0),
            "gitDiffDeletedLines": stats.get("gitDiffDeletedLines").and_then(Value::as_u64).unwrap_or(0),
        })),
    })
}

fn perform_upload_with_lock_held(
    url: &str,
    payload: &Value,
    api_key: Option<&str>,
    user_id: Option<&str>,
    debug_context: &UploadDebugContext,
) -> Result<u16, String> {
    let mut last_error = None;

    for attempt in 1..=UPLOAD_MAX_ATTEMPTS {
        crate::diagnostics::append_debug_event(
            "upload_stats_attempt_started",
            json!({
                "commitSha": debug_context.commit_sha.as_str(),
                "commitShort": debug_context.commit_short.as_str(),
                "source": debug_context.source.as_str(),
                "mode": debug_context.mode.as_str(),
                "url": url,
                "attempt": attempt,
                "maxAttempts": UPLOAD_MAX_ATTEMPTS,
            }),
        );

        match perform_upload_once(url, payload, api_key, user_id, debug_context) {
            Ok(status_code) => {
                if attempt > 1 {
                    crate::diagnostics::append_debug_event(
                        "upload_stats_retry_recovered",
                        json!({
                            "commitSha": debug_context.commit_sha.as_str(),
                            "commitShort": debug_context.commit_short.as_str(),
                            "source": debug_context.source.as_str(),
                            "mode": debug_context.mode.as_str(),
                            "url": url,
                            "attempt": attempt,
                            "statusCode": status_code,
                        }),
                    );
                }
                return Ok(status_code);
            }
            Err(error) => {
                let retryable = is_retryable_upload_error(&error);
                crate::diagnostics::append_debug_event(
                    "upload_stats_attempt_failed",
                    json!({
                        "commitSha": debug_context.commit_sha.as_str(),
                        "commitShort": debug_context.commit_short.as_str(),
                        "source": debug_context.source.as_str(),
                        "mode": debug_context.mode.as_str(),
                        "url": url,
                        "attempt": attempt,
                        "maxAttempts": UPLOAD_MAX_ATTEMPTS,
                        "retryable": retryable,
                        "error": error,
                    }),
                );
                last_error = Some(error);

                if !retryable || attempt == UPLOAD_MAX_ATTEMPTS {
                    break;
                }

                std::thread::sleep(Duration::from_millis(UPLOAD_RETRY_DELAY_MILLIS));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "upload failed without an error message".to_string()))
}

fn perform_upload_once(
    url: &str,
    payload: &Value,
    api_key: Option<&str>,
    user_id: Option<&str>,
    debug_context: &UploadDebugContext,
) -> Result<u16, String> {
    log_debug(&format!(
        "perform_upload url={} has_api_key={} has_user_id={}",
        url,
        api_key.is_some(),
        user_id.is_some()
    ));
    crate::diagnostics::append_debug_event(
        "upload_stats_http_prepare_started",
        json!({
            "commitSha": debug_context.commit_sha.as_str(),
            "commitShort": debug_context.commit_short.as_str(),
            "source": debug_context.source.as_str(),
            "mode": debug_context.mode.as_str(),
            "url": url,
            "timeoutSecs": UPLOAD_TIMEOUT_SECS,
            "hasApiKey": api_key.is_some(),
            "hasUserId": user_id.is_some(),
        }),
    );

    let agent = http::build_agent(Some(UPLOAD_TIMEOUT_SECS));
    let mut request = agent.post(url).set("Content-Type", "application/json");
    if let Some(key) = api_key {
        request = request.set("Authorization", &format!("Bearer {}", key));
    }
    if let Some(id) = user_id {
        request = request.set("X-USER-ID", id);
    }

    let body = serde_json::to_string(payload).map_err(|e| {
        let error = e.to_string();
        crate::diagnostics::append_debug_event(
            "upload_stats_http_body_serialize_failed",
            json!({
                "commitSha": debug_context.commit_sha.as_str(),
                "commitShort": debug_context.commit_short.as_str(),
                "source": debug_context.source.as_str(),
                "mode": debug_context.mode.as_str(),
                "url": url,
                "error": error,
            }),
        );
        error
    })?;
    crate::diagnostics::append_debug_event(
        "upload_stats_http_request_ready",
        json!({
            "commitSha": debug_context.commit_sha.as_str(),
            "commitShort": debug_context.commit_short.as_str(),
            "source": debug_context.source.as_str(),
            "mode": debug_context.mode.as_str(),
            "url": url,
            "bodyBytes": body.len(),
            "hasApiKey": api_key.is_some(),
            "hasUserId": user_id.is_some(),
        }),
    );
    let response = http::send_with_body(request, &body).map_err(|error| {
        crate::diagnostics::append_debug_event(
            "upload_stats_http_send_failed",
            json!({
                "commitSha": debug_context.commit_sha.as_str(),
                "commitShort": debug_context.commit_short.as_str(),
                "source": debug_context.source.as_str(),
                "mode": debug_context.mode.as_str(),
                "url": url,
                "error": error,
            }),
        );
        error
    })?;
    log_debug(&format!(
        "perform_upload response status={} url={}",
        response.status_code, url
    ));
    crate::diagnostics::append_debug_event(
        "upload_stats_http_response_received",
        json!({
            "commitSha": debug_context.commit_sha.as_str(),
            "commitShort": debug_context.commit_short.as_str(),
            "source": debug_context.source.as_str(),
            "mode": debug_context.mode.as_str(),
            "url": url,
            "statusCode": response.status_code,
            "successStatus": (200..300).contains(&response.status_code),
        }),
    );
    if (200..300).contains(&response.status_code) {
        match inspect_backend_response_body(response.as_bytes()) {
            Ok(Some((backend_code, backend_msg))) => {
                crate::diagnostics::append_debug_event(
                    "upload_stats_http_body_checked",
                    json!({
                        "commitSha": debug_context.commit_sha.as_str(),
                        "commitShort": debug_context.commit_short.as_str(),
                        "source": debug_context.source.as_str(),
                        "mode": debug_context.mode.as_str(),
                        "url": url,
                        "statusCode": response.status_code,
                        "backendCode": backend_code,
                        "backendMsg": backend_msg,
                    }),
                );
            }
            Ok(None) => {}
            Err(error) => {
                crate::diagnostics::append_debug_event(
                    "upload_stats_http_body_rejected",
                    json!({
                        "commitSha": debug_context.commit_sha.as_str(),
                        "commitShort": debug_context.commit_short.as_str(),
                        "source": debug_context.source.as_str(),
                        "mode": debug_context.mode.as_str(),
                        "url": url,
                        "statusCode": response.status_code,
                        "error": error,
                        "bodyExcerpt": response_body_excerpt(response.as_bytes()),
                    }),
                );
                return Err(error);
            }
        }
        Ok(response.status_code)
    } else {
        let body_excerpt = response_body_excerpt(response.as_bytes());
        crate::diagnostics::append_debug_event(
            "upload_stats_http_non_success",
            json!({
                "commitSha": debug_context.commit_sha.as_str(),
                "commitShort": debug_context.commit_short.as_str(),
                "source": debug_context.source.as_str(),
                "mode": debug_context.mode.as_str(),
                "url": url,
                "statusCode": response.status_code,
                "bodyExcerpt": body_excerpt,
            }),
        );
        Err(format!("HTTP {}: {}", response.status_code, body_excerpt))
    }
}

fn is_retryable_upload_error(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();

    lowered.contains("connection failed")
        || lowered.contains("connect error")
        || lowered.contains("timed out")
        || lowered.contains("timeout")
        || lowered.contains("temporarily unavailable")
        || lowered.contains("http 408")
        || lowered.contains("http 425")
        || lowered.contains("http 429")
        || lowered.contains("http 500")
        || lowered.contains("http 502")
        || lowered.contains("http 503")
        || lowered.contains("http 504")
}

fn inspect_backend_response_body(body: &[u8]) -> Result<Option<(i64, Option<String>)>, String> {
    if body.is_empty() {
        return Ok(None);
    }

    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };

    let Some(code) = value.get("code").and_then(Value::as_i64) else {
        return Ok(None);
    };
    let msg = value
        .get("msg")
        .and_then(Value::as_str)
        .and_then(trim_non_empty);

    if code == 200 {
        Ok(Some((code, msg)))
    } else if let Some(message) = msg {
        Err(format!("backend returned code {}: {}", code, message))
    } else {
        Err(format!("backend returned code {}", code))
    }
}

fn response_body_excerpt(body: &[u8]) -> String {
    std::str::from_utf8(body)
        .map(|text| text.chars().take(200).collect::<String>())
        .unwrap_or_else(|_| format!("<{} bytes non-utf8>", body.len()))
}

#[cfg(test)]
fn build_payload_with_source(
    repo: &Repository,
    commit_sha: &str,
    authorship_log: &AuthorshipLog,
    stats: &CommitStats,
    source: &str,
) -> Result<Value, String> {
    let commit_entry = build_commit_entry(repo, commit_sha, authorship_log, stats)?;
    build_payload_from_commit_entries(repo, vec![Value::Object(commit_entry)], source)
}

fn build_commit_entry(
    repo: &Repository,
    commit_sha: &str,
    authorship_log: &AuthorshipLog,
    stats: &CommitStats,
) -> Result<Map<String, Value>, String> {
    let workdir = repo.canonical_workdir().to_path_buf();
    let (commit_message, commit_author, commit_timestamp) =
        git_commit_metadata(&workdir, commit_sha)
            .ok_or_else(|| "failed to read commit metadata".to_string())?;

    let file_stats = build_file_stats(repo, commit_sha, authorship_log);
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

    Ok(commit_entry)
}

fn build_payload_from_commit_entries(
    repo: &Repository,
    commit_entries: Vec<Value>,
    source: &str,
) -> Result<Value, String> {
    if commit_entries.is_empty() {
        return Err("payload must contain at least one commit".to_string());
    }

    let workdir = repo.canonical_workdir().to_path_buf();
    let repo_url = git_repo_url(&workdir);
    let project_name = derive_project_name(repo_url.as_deref(), &workdir);
    let branch = git_current_branch(&workdir);

    let payload = json!({
        "repoUrl": repo_url.unwrap_or_default(),
        "projectName": project_name,
        "branch": branch.unwrap_or_default(),
        "source": source,
        "reviewDocumentId": Value::Null,
        "authorshipSchemaVersion": AUTHORSHIP_LOG_VERSION,
        "clientContext": build_client_context(),
        "commits": commit_entries,
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
    let user_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::User { .. }))
        .cloned()
        .collect::<Vec<_>>();
    serde_json::to_value(user_messages).unwrap_or_else(|_| Value::Array(Vec::new()))
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
                        || model_text
                            .eq_ignore_ascii_case(&format!("{}/{}", tool_value, model_value))
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
    repo: &Repository,
    commit_sha: &str,
    authorship_log: &AuthorshipLog,
) -> Vec<Value> {
    let workdir = repo.canonical_workdir().to_path_buf();
    let numstat = git_diff_tree_numstat(&workdir, commit_sha);
    if numstat.is_empty() {
        return Vec::new();
    }

    let accepted_by_file = build_added_lines_by_file(repo, commit_sha)
        .map(|added_lines_by_file| {
            accepted_lines_from_attestations_by_file(
                repo,
                commit_sha,
                Some(authorship_log),
                &added_lines_by_file,
                false,
            )
        })
        .unwrap_or_default();

    let mut files = Vec::with_capacity(numstat.len());
    for (file_path, added, deleted) in numstat {
        let accepted = accepted_by_file.get(&file_path);
        let ai_attr = accepted.map(|value| value.ai_accepted).unwrap_or(0);
        let human_attr = accepted
            .map(|value| value.known_human_accepted)
            .unwrap_or(0);

        let ai_add = ai_attr.min(added);
        let human_add = human_attr.min(added.saturating_sub(ai_add));
        let unknown_add = added.saturating_sub(ai_add).saturating_sub(human_add);

        let breakdown = accepted
            .map(|value| &value.ai_accepted_by_tool)
            .map(|map| {
                map.iter()
                    .map(|(key, count)| {
                        let (split_tool, split_model) = split_tool_model(key);
                        let (tool, model) =
                            normalize_tool_model(Some(split_tool.as_str()), split_model.as_deref());
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

fn build_added_lines_by_file(
    repo: &Repository,
    commit_sha: &str,
) -> Result<HashMap<String, Vec<u32>>, crate::error::GitAiError> {
    let commit_obj = repo.revparse_single(commit_sha)?.peel_to_commit()?;
    let parent_count = commit_obj.parent_count()?;

    if parent_count > 1 {
        return Ok(HashMap::new());
    }

    let from_ref = if parent_count == 0 {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
    } else {
        commit_obj.parent(0)?.id()
    };

    repo.diff_added_lines(&from_ref, commit_sha, None)
}

fn git_diff_tree_numstat(workdir: &Path, commit_sha: &str) -> Vec<(String, u32, u32)> {
    let output = std::process::Command::new("git")
        .arg("-c")
        .arg("core.quotepath=false")
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
    use crate::git::test_utils::TmpRepo;
    use std::collections::HashMap;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, mpsc};
    use std::time::Duration;

    static ENV_GUARD_LOCK: Mutex<()> = Mutex::new(());

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
        assert_eq!(prompt["promptText"], "first prompt\n\nsecond prompt");
        assert_eq!(prompt["messagesUrl"], "https://cas.example/prompt-123");
        assert_eq!(prompt["acceptedLines"], 7);
        assert_eq!(prompt["customAttributes"]["language"], "rust");
        assert!(prompt["messages"].is_array());
        assert_eq!(prompt["messages"].as_array().map(Vec::len), Some(2));
        assert!(
            prompt["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|message| message["type"] == "user")
        );
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
    fn inspect_backend_response_body_accepts_success_code() {
        let result =
            inspect_backend_response_body("{\"code\":200,\"msg\":\"操作成功\"}".as_bytes())
                .expect("success body should pass");

        assert_eq!(result, Some((200, Some("操作成功".to_string()))));
    }

    #[test]
    fn inspect_backend_response_body_rejects_failure_code() {
        let error = inspect_backend_response_body(
            "{\"code\":500,\"msg\":\"数据库中已存在该记录，请联系管理员确认\"}".as_bytes(),
        )
        .expect_err("non-200 backend code should fail upload");

        assert_eq!(
            error,
            "backend returned code 500: 数据库中已存在该记录，请联系管理员确认"
        );
    }

    #[test]
    fn inspect_backend_response_body_ignores_non_standard_success_body() {
        let result =
            inspect_backend_response_body(b"ok").expect("non-json success body should not fail");

        assert_eq!(result, None);
    }

    #[test]
    fn retryable_upload_error_detects_transient_network_and_gateway_failures() {
        assert!(is_retryable_upload_error(
            "Connection Failed: Connect error: os error 10060"
        ));
        assert!(is_retryable_upload_error("HTTP 503: service unavailable"));
        assert!(is_retryable_upload_error("HTTP 429: too many requests"));
        assert!(!is_retryable_upload_error(
            "backend returned code 500: duplicate commit"
        ));
        assert!(!is_retryable_upload_error("HTTP 400: bad request"));
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

    #[test]
    fn upload_activity_lock_path_uses_internal_dir() {
        let internal_dir = Path::new("C:/tmp/.git-ai/internal");

        let lock_path = upload_activity_lock_path_from_internal_dir(internal_dir);

        assert_eq!(
            lock_path,
            Path::new("C:/tmp/.git-ai/internal/upload_activity.lock")
        );
    }

    #[test]
    fn upload_activity_lock_wait_duration_is_short_for_non_manual_uploads() {
        assert_eq!(
            upload_activity_lock_wait_duration("background"),
            Duration::from_secs(UPLOAD_ACTIVITY_LOCK_WAIT_SECS_AUTO)
        );
        assert_eq!(
            upload_activity_lock_wait_duration("inline"),
            Duration::from_secs(UPLOAD_ACTIVITY_LOCK_WAIT_SECS_AUTO)
        );
    }

    #[test]
    fn upload_activity_lock_wait_duration_is_longer_for_manual_uploads() {
        assert_eq!(
            upload_activity_lock_wait_duration("manual"),
            Duration::from_secs(UPLOAD_ACTIVITY_LOCK_WAIT_SECS_MANUAL)
        );
    }

    #[test]
    fn acquire_lock_with_retry_waits_for_existing_upload_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = upload_activity_lock_path_from_internal_dir(dir.path());
        let lock_path_for_thread = lock_path.clone();
        let (ready_tx, ready_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            let _lock = LockFile::try_acquire(&lock_path_for_thread).expect("worker lock");
            let _ = ready_tx.send(());
            std::thread::sleep(Duration::from_millis(120));
        });

        ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should acquire lock");

        let acquired = acquire_lock_with_retry(
            &lock_path,
            Duration::from_secs(1),
            Duration::from_millis(10),
        );

        assert!(
            acquired.is_some(),
            "lock should become available after worker exits"
        );
        worker.join().expect("worker join");
    }

    #[test]
    fn note_status_resets_to_not_uploaded_when_note_blob_changes() {
        let mut index = UploadStatusIndex::default();
        let commit_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        ensure_note_status_record(&mut index, commit_sha, Some("old-blob".to_string()), 1);
        mark_upload_succeeded(&mut index, &[commit_sha.to_string()], 200, 2);

        let record =
            ensure_note_status_record(&mut index, commit_sha, Some("new-blob".to_string()), 3);

        assert_eq!(record.upload_status, NoteUploadStatus::NotUploaded);
        assert_eq!(record.note_blob_oid.as_deref(), Some("new-blob"));
        assert_eq!(record.last_status_code, None);
        assert_eq!(record.last_error, None);
    }

    #[test]
    fn prepare_upload_batch_includes_failed_note_with_current_commit() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");

        tmp_repo
            .write_file("first.txt", "first\n", true)
            .expect("write first file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("first checkpoint");
        tmp_repo
            .commit_with_message("first commit")
            .expect("first commit");
        let first_sha = tmp_repo.get_head_commit_sha().expect("first sha");

        let mut status_index = UploadStatusIndex::default();
        let first_blob = note_blob_oid_for_commit(tmp_repo.gitai_repo(), &first_sha);
        ensure_note_status_record(&mut status_index, &first_sha, first_blob, 1);
        mark_upload_failed(&mut status_index, &[first_sha.clone()], "network down", 2);

        tmp_repo
            .write_file("second.txt", "second\n", true)
            .expect("write second file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("second checkpoint");
        let second_authorship = tmp_repo
            .commit_with_message("second commit")
            .expect("second commit");
        let second_sha = tmp_repo.get_head_commit_sha().expect("second sha");
        let second_stats =
            stats_for_commit_stats(tmp_repo.gitai_repo(), &second_sha, &[]).expect("second stats");

        let batch = prepare_upload_batch(
            tmp_repo.gitai_repo(),
            &mut status_index,
            &[UploadCandidateSeed {
                commit_sha: second_sha.clone(),
                authorship_log: Some(second_authorship),
                stats: Some(second_stats),
            }],
            &[],
            "auto",
        )
        .expect("batch should be prepared");

        assert_eq!(
            batch.commit_shas,
            vec![second_sha.clone(), first_sha.clone()]
        );
        let commits = batch.payload["commits"].as_array().expect("commits array");
        let commit_shas = commits
            .iter()
            .map(|commit| commit["commitSha"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(commit_shas, vec![second_sha, first_sha]);
    }

    #[test]
    fn prepare_upload_batch_registers_new_notes_without_immediate_backlog_upload() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");

        tmp_repo
            .write_file("first.txt", "first\n", true)
            .expect("write first file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("first checkpoint");
        tmp_repo
            .commit_with_message("first commit")
            .expect("first commit");
        let first_sha = tmp_repo.get_head_commit_sha().expect("first sha");

        tmp_repo
            .write_file("second.txt", "second\n", true)
            .expect("write second file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("second checkpoint");
        let second_authorship = tmp_repo
            .commit_with_message("second commit")
            .expect("second commit");
        let second_sha = tmp_repo.get_head_commit_sha().expect("second sha");
        let second_stats =
            stats_for_commit_stats(tmp_repo.gitai_repo(), &second_sha, &[]).expect("second stats");

        let mut status_index = UploadStatusIndex::default();
        let batch = prepare_upload_batch(
            tmp_repo.gitai_repo(),
            &mut status_index,
            &[UploadCandidateSeed {
                commit_sha: second_sha.clone(),
                authorship_log: Some(second_authorship),
                stats: Some(second_stats),
            }],
            &[],
            "auto",
        )
        .expect("batch should be prepared");

        assert_eq!(batch.commit_shas, vec![second_sha]);
        assert_eq!(
            status_index
                .notes
                .get(&first_sha)
                .map(|record| record.upload_status),
            Some(NoteUploadStatus::NotUploaded)
        );
    }

    #[test]
    fn git_diff_tree_numstat_returns_unquoted_utf8_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_path = temp.path();

        run_git(repo_path, &["init"]);
        run_git(repo_path, &["config", "user.name", "Test User"]);
        run_git(repo_path, &["config", "user.email", "test@example.com"]);
        run_git(repo_path, &["config", "core.autocrlf", "false"]);

        let seed_file = repo_path.join("README.md");
        fs::write(&seed_file, "seed\n").expect("write seed file");
        run_git(repo_path, &["add", "README.md"]);
        run_git(repo_path, &["commit", "-m", "seed repo"]);

        let file_path = repo_path
            .join("docs")
            .join("design-doc")
            .join("git-ai环境变量与配置说明.md");
        fs::create_dir_all(file_path.parent().expect("parent dir")).expect("create dir tree");
        fs::write(&file_path, "line 1\nline 2\n").expect("write utf8 file");

        run_git(repo_path, &["add", "."]);
        run_git(repo_path, &["commit", "-m", "add utf8 file"]);

        let commit_sha = git_stdout(repo_path, &["rev-parse", "HEAD"]);
        let numstat = git_diff_tree_numstat(repo_path, commit_sha.trim());

        assert_eq!(numstat.len(), 1);
        assert_eq!(numstat[0].0, "docs/design-doc/git-ai环境变量与配置说明.md");
        assert_eq!(numstat[0].1, 2);
        assert_eq!(numstat[0].2, 0);
    }

    #[test]
    fn build_file_stats_attributes_trailing_blank_line_to_neighboring_human_block() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");

        let mut file = tmp_repo
            .write_file("test.txt", "seed\n", true)
            .expect("seed file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("seed checkpoint");
        tmp_repo
            .commit_with_message("seed commit")
            .expect("seed commit");

        file.append("\nHuman line 1\nHuman line 2\n\n")
            .expect("append human block");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("human checkpoint");
        tmp_repo
            .commit_with_message("human block")
            .expect("human commit");

        let head_sha = tmp_repo.get_head_commit_sha().expect("head sha");
        let authorship_log =
            get_authorship(tmp_repo.gitai_repo(), &head_sha).expect("authorship note");
        let file_stats = build_file_stats(tmp_repo.gitai_repo(), &head_sha, &authorship_log);

        assert_eq!(file_stats.len(), 1);
        assert_eq!(file_stats[0]["filePath"], "test.txt");
        assert_eq!(file_stats[0]["gitDiffAddedLines"], 4);
        assert_eq!(file_stats[0]["humanAdditions"], 4);
        assert_eq!(file_stats[0]["unknownAdditions"], 0);
        assert_eq!(file_stats[0]["aiAdditions"], 0);
    }

    #[test]
    fn build_payload_includes_client_context() {
        let _g = EnvGuard::new();
        unsafe {
            std::env::set_var("GIT_AI_REPORT_IDE_NAME", "VS Code");
            std::env::set_var("GIT_AI_REPORT_IDE_VERSION", "1.100.2");
            std::env::set_var("GIT_AI_REPORT_PLUGIN_VERSION", "0.9.2");
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("TERM_PROGRAM_VERSION");
        }

        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file("test.txt", "seed\n", true)
            .expect("seed file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("known human checkpoint");
        let authorship_log = tmp_repo
            .commit_with_message("seed commit")
            .expect("seed commit");
        let head_sha = tmp_repo.get_head_commit_sha().expect("head sha");
        let stats = stats_for_commit_stats(tmp_repo.gitai_repo(), &head_sha, &[])
            .expect("stats for commit");

        let payload = build_payload_with_source(
            tmp_repo.gitai_repo(),
            &head_sha,
            &authorship_log,
            &stats,
            "manual",
        )
        .expect("payload");

        assert_eq!(
            payload["clientContext"]["gitAiCliVersion"],
            git_ai_cli_version()
        );
        assert_eq!(payload["clientContext"]["gitAiPluginVersion"], "0.9.2");
        assert_eq!(payload["clientContext"]["ideName"], "VS Code");
        assert_eq!(payload["clientContext"]["ideVersion"], "1.100.2");
        assert_eq!(
            payload["clientContext"]["gitVersion"],
            json!(git_version_string())
        );
    }

    fn run_git(repo_path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .expect("run git command");

        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(repo_path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .expect("run git command");

        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8(output.stdout).expect("git stdout should be utf8")
    }

    /// Serialize and restore upload-related env vars so URL resolution tests do
    /// not race under the default parallel test runner.
    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        url: Option<String>,
        endpoint: Option<String>,
        path: Option<String>,
        ide_name: Option<String>,
        ide_version: Option<String>,
        plugin_version: Option<String>,
        term_program: Option<String>,
        term_program_version: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_GUARD_LOCK.lock().expect("env guard lock poisoned");
            Self {
                _lock: lock,
                url: std::env::var("GIT_AI_REPORT_REMOTE_URL").ok(),
                endpoint: std::env::var("GIT_AI_REPORT_REMOTE_ENDPOINT").ok(),
                path: std::env::var("GIT_AI_REPORT_REMOTE_PATH").ok(),
                ide_name: std::env::var("GIT_AI_REPORT_IDE_NAME").ok(),
                ide_version: std::env::var("GIT_AI_REPORT_IDE_VERSION").ok(),
                plugin_version: std::env::var("GIT_AI_REPORT_PLUGIN_VERSION").ok(),
                term_program: std::env::var("TERM_PROGRAM").ok(),
                term_program_version: std::env::var("TERM_PROGRAM_VERSION").ok(),
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
                match &self.ide_name {
                    Some(v) => std::env::set_var("GIT_AI_REPORT_IDE_NAME", v),
                    None => std::env::remove_var("GIT_AI_REPORT_IDE_NAME"),
                }
                match &self.ide_version {
                    Some(v) => std::env::set_var("GIT_AI_REPORT_IDE_VERSION", v),
                    None => std::env::remove_var("GIT_AI_REPORT_IDE_VERSION"),
                }
                match &self.plugin_version {
                    Some(v) => std::env::set_var("GIT_AI_REPORT_PLUGIN_VERSION", v),
                    None => std::env::remove_var("GIT_AI_REPORT_PLUGIN_VERSION"),
                }
                match &self.term_program {
                    Some(v) => std::env::set_var("TERM_PROGRAM", v),
                    None => std::env::remove_var("TERM_PROGRAM"),
                }
                match &self.term_program_version {
                    Some(v) => std::env::set_var("TERM_PROGRAM_VERSION", v),
                    None => std::env::remove_var("TERM_PROGRAM_VERSION"),
                }
            }
        }
    }
}
