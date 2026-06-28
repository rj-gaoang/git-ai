use crate::git::refs::{
    AI_AUTHORSHIP_PUSH_REFSPEC, copy_ref, fallback_merge_notes_ours, merge_notes_from_ref,
    ref_exists, tracking_ref_for_remote,
};
use crate::{
    authorship::post_commit::post_commit_with_final_state,
    authorship::rebase_authorship::committed_file_snapshot_between_commits,
};
use crate::{
    error::GitAiError,
    git::{cli_parser::ParsedGitInvocation, repository::exec_git},
};

use super::repository::Repository;

#[cfg(windows)]
fn disabled_hooks_config() -> &'static str {
    "core.hooksPath=NUL"
}

#[cfg(not(windows))]
fn disabled_hooks_config() -> &'static str {
    "core.hooksPath=/dev/null"
}

/// Result of checking for authorship notes on a remote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesExistence {
    /// Notes were found and fetched from the remote
    Found,
    /// Confirmed that no notes exist on the remote
    NotFound,
}

pub fn fetch_remote_from_args(
    repository: &Repository,
    parsed_args: &ParsedGitInvocation,
) -> Result<String, GitAiError> {
    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // 2) Fetch authorship refs from the appropriate remote
    // Try to detect remote (named remote, URL, or local path) from args first
    let positional_remote = extract_remote_from_fetch_args(&parsed_args.command_args);
    let specified_remote = positional_remote.or_else(|| {
        parsed_args
            .command_args
            .iter()
            .find(|a| remote_names.iter().any(|r| r == *a))
            .cloned()
    });

    let remote = specified_remote
        .or_else(|| repository.upstream_remote().ok().flatten())
        .or_else(|| repository.get_default_remote().ok().flatten());

    remote.map(|r| r.to_string()).ok_or_else(|| {
        GitAiError::Generic(
            "Could not determine a remote for fetch/push operation. \
                 No remote was specified in args, no upstream is configured, \
                 and no default remote was found."
                .to_string(),
        )
    })
}

/// Try to fetch authorship notes from all remotes for source commits that are missing
/// local notes.  This is a best-effort operation used before cherry-pick attribution
/// rewriting to ensure notes from remote repos are available locally.
///
/// Uses the safe fetch pattern (tracking ref + merge with `-s ours`) so local notes
/// are never overwritten.  Silently ignores any fetch errors.
pub fn fetch_missing_notes_for_commits(repository: &Repository, source_commits: &[String]) {
    use std::collections::HashSet;

    // Fetch the full set of locally-noted commits in one subprocess call.
    // `git notes --ref=refs/notes/ai list` outputs "<note-sha> <commit-sha>" per line.
    let mut args = repository.global_args_for_exec();
    args.extend(
        ["notes", "--ref=refs/notes/ai", "list"]
            .iter()
            .map(|s| s.to_string()),
    );
    let noted_commits: HashSet<String> = exec_git(&args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|line| line.split_whitespace().nth(1).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let missing: Vec<&String> = source_commits
        .iter()
        .filter(|sha| !noted_commits.contains(sha.as_str()))
        .collect();

    if missing.is_empty() {
        return;
    }

    tracing::debug!(
        "Source commits missing notes: {:?}, trying to fetch from remotes",
        missing
    );

    if let Ok(remotes) = repository.remotes_with_urls() {
        for (remote_name, _) in remotes {
            tracing::debug!("Attempting safe notes fetch from remote {}", remote_name);
            match fetch_authorship_notes(repository, &remote_name) {
                Ok(_) => tracing::debug!("Fetched and merged notes from remote {}", remote_name),
                Err(e) => tracing::debug!(
                    "Notes fetch from remote {} failed (best-effort): {}",
                    remote_name,
                    e
                ),
            }
        }
    }
}

// for use with post-fetch and post-pull and post-clone hooks
// Returns Ok(NotesExistence::Found) if notes were found and fetched,
// Ok(NotesExistence::NotFound) if confirmed no notes exist on remote,
// Err(...) for actual errors (network, permissions, etc.)
pub fn fetch_authorship_notes(
    repository: &Repository,
    remote_name: &str,
) -> Result<NotesExistence, GitAiError> {
    // Generate tracking ref for this remote
    let tracking_ref = tracking_ref_for_remote(remote_name);

    tracing::debug!(
        "fetching authorship notes for remote '{}' to tracking ref '{}'",
        remote_name,
        tracking_ref
    );

    // Fetch notes to tracking ref with explicit refspec.
    // If the remote does not have refs/notes/ai yet, treat that as NotFound.
    let fetch_refspec = format!("+refs/notes/ai:{}", tracking_ref);

    // Build the internal authorship fetch with explicit flags and disabled hooks.
    // IMPORTANT: use repository.global_args_for_exec() to ensure -C flag is present for bare repos.
    let fetch_authorship = build_authorship_fetch_args(
        repository.global_args_for_exec(),
        remote_name,
        &fetch_refspec,
    );

    tracing::debug!("fetch command: {:?}", fetch_authorship);

    match exec_git(&fetch_authorship) {
        Ok(output) => {
            tracing::debug!(
                "fetch stdout: '{}'",
                String::from_utf8_lossy(&output.stdout)
            );
            tracing::debug!(
                "fetch stderr: '{}'",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            if is_missing_remote_notes_ref_error(&e) {
                tracing::debug!(
                    "no authorship notes found on remote '{}', nothing to sync",
                    remote_name
                );
                return Ok(NotesExistence::NotFound);
            }
            tracing::debug!("authorship fetch failed: {}", e);
            return Err(e);
        }
    }

    // After successful fetch, merge the tracking ref into refs/notes/ai
    let local_notes_ref = "refs/notes/ai";

    if crate::git::refs::ref_exists(repository, &tracking_ref) {
        if crate::git::refs::ref_exists(repository, local_notes_ref) {
            // Both exist - merge them
            tracing::debug!(
                "merging authorship notes from {} into {}",
                tracking_ref,
                local_notes_ref
            );
            if let Err(e) = merge_notes_from_ref(repository, &tracking_ref) {
                tracing::debug!("notes merge failed: {}", e);
                // Fallback: manually merge notes when git notes merge crashes
                if let Err(e2) = fallback_merge_notes_ours(repository, &tracking_ref) {
                    tracing::debug!("fallback merge also failed: {}", e2);
                }
            }
        } else {
            // Only tracking ref exists - copy it to local
            tracing::debug!(
                "initializing {} from tracking ref {}",
                local_notes_ref,
                tracking_ref
            );
            if let Err(e) = copy_ref(repository, &tracking_ref, local_notes_ref) {
                tracing::debug!("notes copy failed: {}", e);
                // Don't fail on copy errors, just log and continue
            }
        }
    } else {
        tracing::debug!("tracking ref {} was not created after fetch", tracking_ref);
    }

    Ok(NotesExistence::Found)
}

fn is_missing_remote_notes_ref_error(error: &GitAiError) -> bool {
    let GitAiError::GitCliError { stderr, .. } = error else {
        return false;
    };

    let stderr_lower = stderr.to_ascii_lowercase();
    stderr_lower.contains("refs/notes/ai")
        && (stderr_lower.contains("couldn't find remote ref")
            || stderr_lower.contains("could not find remote ref")
            || stderr_lower.contains("remote ref does not exist")
            || stderr_lower.contains("not our ref"))
}
/// Maximum number of fetch-merge-push attempts before giving up.
/// On busy monorepos, concurrent pushers can cause non-fast-forward rejections
/// even after a successful merge, so we retry the full cycle.
const PUSH_NOTES_MAX_ATTEMPTS: usize = 3;
const PUSH_BACKFILL_MAX_COMMITS: usize = 32;

/// Safety net for commits created outside git-ai's normal post-commit path.
///
/// This path is intentionally narrower than push backfill: it only materializes
/// a missing note when the commit's parent working log already contains
/// checkpoint or INITIAL evidence. Commits with no local evidence remain
/// unmaterialized so historical unknowns are not silently turned into AI/human.
pub fn backfill_missing_authorship_for_observed_commit(
    repository: &Repository,
    commit_sha: &str,
    source: &str,
) -> Result<bool, GitAiError> {
    if !is_push_backfill_candidate_oid(commit_sha)
        || commit_has_authorship_log(repository, commit_sha)
    {
        return Ok(false);
    }

    let author = repository.git_author_identity().formatted_or_unknown();
    match commit_has_materializable_parent_state(repository, commit_sha) {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "observed_head_authorship_backfill_failed",
                serde_json::json!({
                    "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "source": source,
                    "error": error.to_string(),
                }),
            );
            return Err(error);
        }
    }

    match materialize_missing_commit_authorship(repository, commit_sha, &author) {
        Ok(true) => {
            crate::diagnostics::append_debug_event(
                "observed_head_authorship_backfill_materialized",
                serde_json::json!({
                    "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "source": source,
                }),
            );
            Ok(true)
        }
        Ok(false) => Ok(false),
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "observed_head_authorship_backfill_failed",
                serde_json::json!({
                    "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "source": source,
                    "error": error.to_string(),
                }),
            );
            Err(error)
        }
    }
}

/// Backfill recent missing notes when a later git-ai-observed command proves
/// the worktree HEAD has advanced, even if the original commit bypassed hooks.
pub fn backfill_missing_authorship_from_observed_head(
    repository: &Repository,
    source: &str,
) -> Result<usize, GitAiError> {
    let head = match repository.head().and_then(|head| head.target()) {
        Ok(head) if is_push_backfill_candidate_oid(&head) => head,
        _ => return Ok(0),
    };

    let mut current = head;
    let mut inspected = 0usize;
    let mut candidates = Vec::new();

    while inspected < PUSH_BACKFILL_MAX_COMMITS && is_push_backfill_candidate_oid(&current) {
        inspected += 1;

        if !commit_has_authorship_log(repository, &current)
            && commit_has_materializable_parent_state(repository, &current)?
        {
            candidates.push(current.clone());
        }

        let commit = repository.find_commit(current.clone())?;
        if commit.parent_count()? != 1 {
            break;
        }
        current = commit.parent(0)?.id();
    }

    candidates.reverse();
    let author = repository.git_author_identity().formatted_or_unknown();
    let mut materialized = 0usize;

    for commit_sha in candidates {
        match materialize_missing_commit_authorship(repository, &commit_sha, &author) {
            Ok(true) => {
                materialized += 1;
                crate::diagnostics::append_debug_event(
                    "observed_head_authorship_backfill_materialized",
                    serde_json::json!({
                        "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                        "commitSha": commit_sha,
                        "source": source,
                        "inspectedCount": inspected,
                        "maxCommits": PUSH_BACKFILL_MAX_COMMITS,
                    }),
                );
            }
            Ok(false) => {}
            Err(error) => {
                crate::diagnostics::append_debug_event(
                    "observed_head_authorship_backfill_failed",
                    serde_json::json!({
                        "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                        "commitSha": commit_sha,
                        "source": source,
                        "inspectedCount": inspected,
                        "maxCommits": PUSH_BACKFILL_MAX_COMMITS,
                        "error": error.to_string(),
                    }),
                );
                return Err(error);
            }
        }
    }

    if materialized > 0 {
        crate::diagnostics::append_debug_event(
            "observed_head_authorship_backfill_completed",
            serde_json::json!({
                "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                "materializedCount": materialized,
                "inspectedCount": inspected,
                "maxCommits": PUSH_BACKFILL_MAX_COMMITS,
                "source": source,
            }),
        );
    }

    Ok(materialized)
}

/// Best-effort safety net for commits created outside git-ai's normal commit
/// capture path. If a later push is captured, materialize authorship/stat notes
/// for the recent first-parent HEAD chain before syncing notes or returning for
/// the HTTP notes backend.
pub fn backfill_missing_authorship_before_push(
    repository: &Repository,
) -> Result<usize, GitAiError> {
    let head = match repository.head().and_then(|head| head.target()) {
        Ok(head) if is_push_backfill_candidate_oid(&head) => head,
        _ => return Ok(0),
    };

    let author = repository.git_author_identity().formatted_or_unknown();
    let mut current = head;
    let mut materialized = 0usize;
    let mut inspected = 0usize;

    while inspected < PUSH_BACKFILL_MAX_COMMITS && is_push_backfill_candidate_oid(&current) {
        inspected += 1;

        if !commit_has_authorship_log(repository, &current) {
            match materialize_missing_commit_authorship(repository, &current, &author) {
                Ok(true) => {
                    materialized += 1;
                    crate::diagnostics::append_debug_event(
                        "push_authorship_backfill_materialized",
                        serde_json::json!({
                            "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                            "commitSha": current,
                            "inspectedCount": inspected,
                            "maxCommits": PUSH_BACKFILL_MAX_COMMITS,
                        }),
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    crate::diagnostics::append_debug_event(
                        "push_authorship_backfill_failed",
                        serde_json::json!({
                            "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                            "commitSha": current,
                            "inspectedCount": inspected,
                            "maxCommits": PUSH_BACKFILL_MAX_COMMITS,
                            "error": error.to_string(),
                        }),
                    );
                    return Err(error);
                }
            }
        }

        let commit = repository.find_commit(current.clone())?;
        if commit.parent_count()? != 1 {
            break;
        }
        current = commit.parent(0)?.id();
    }

    if materialized > 0 || inspected > 0 {
        crate::diagnostics::append_debug_event(
            "push_authorship_backfill_completed",
            serde_json::json!({
                "repo": repository.canonical_workdir().to_string_lossy().to_string(),
                "materializedCount": materialized,
                "inspectedCount": inspected,
                "maxCommits": PUSH_BACKFILL_MAX_COMMITS,
            }),
        );
    }

    Ok(materialized)
}

fn materialize_missing_commit_authorship(
    repository: &Repository,
    commit_sha: &str,
    author: &str,
) -> Result<bool, GitAiError> {
    if commit_has_authorship_log(repository, commit_sha) {
        return Ok(false);
    }

    let parent_sha = commit_parent(repository, commit_sha)?;
    let final_state =
        committed_file_snapshot_between_commits(repository, parent_sha.as_deref(), commit_sha)?;

    post_commit_with_final_state(
        repository,
        parent_sha,
        commit_sha.to_string(),
        author.to_string(),
        true,
        Some(&final_state),
    )?;

    Ok(true)
}

fn commit_has_materializable_parent_state(
    repository: &Repository,
    commit_sha: &str,
) -> Result<bool, GitAiError> {
    let parent_sha =
        commit_parent(repository, commit_sha)?.unwrap_or_else(|| "initial".to_string());
    if !repository.storage.has_working_log(&parent_sha) {
        return Ok(false);
    }

    let working_log = repository
        .storage
        .working_log_for_base_commit(&parent_sha)?;
    let has_checkpoints = !working_log.read_all_checkpoints()?.is_empty();
    let initial = working_log.read_initial_attributions();
    Ok(has_checkpoints || !initial.files.is_empty())
}

fn commit_has_authorship_log(repository: &Repository, commit_sha: &str) -> bool {
    crate::git::notes_api::read_authorship_v3(repository, commit_sha).is_ok()
}

fn commit_parent(repository: &Repository, commit_sha: &str) -> Result<Option<String>, GitAiError> {
    let commit = repository.find_commit(commit_sha.to_string())?;
    if commit.parent_count()? == 0 {
        return Ok(None);
    }
    Ok(Some(commit.parent(0)?.id()))
}

fn is_push_backfill_candidate_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid.chars().all(|c| c.is_ascii_hexdigit())
        && oid.chars().any(|c| c != '0')
}

// for use with post-push hook
pub fn push_authorship_notes(repository: &Repository, remote_name: &str) -> Result<(), GitAiError> {
    // Belt-and-suspenders: when the HTTP backend is active, notes are not stored
    // in refs/notes/ai so there is nothing to push.
    if crate::config::Config::get().notes_backend_kind() == crate::config::NotesBackendKind::Http {
        tracing::debug!("push_authorship_notes: skipping refs/notes/ai push (Http backend active)");
        return Ok(());
    }

    let mut last_error = None;

    for attempt in 0..PUSH_NOTES_MAX_ATTEMPTS {
        if attempt > 0 {
            tracing::debug!(
                "retrying notes push (attempt {}/{})",
                attempt + 1,
                PUSH_NOTES_MAX_ATTEMPTS
            );
        }

        fetch_and_merge_tracking_notes(repository, remote_name);

        // Push notes without force (requires fast-forward)
        let push_args = build_authorship_push_args(repository.global_args_for_exec(), remote_name);

        tracing::debug!("pushing authorship refs (no force): {:?}", &push_args);

        match exec_git(&push_args) {
            Ok(_) => return Ok(()),
            Err(e) => {
                tracing::debug!("authorship push failed: {}", e);
                if is_non_fast_forward_error(&e) && attempt + 1 < PUSH_NOTES_MAX_ATTEMPTS {
                    // Another pusher updated remote notes between our merge and push.
                    // Retry the full fetch-merge-push cycle.
                    last_error = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| GitAiError::Generic("notes push exhausted retries".to_string())))
}

/// Fetch remote notes into a tracking ref and merge into local refs/notes/ai.
fn fetch_and_merge_tracking_notes(repository: &Repository, remote_name: &str) {
    let tracking_ref = tracking_ref_for_remote(remote_name);
    let fetch_refspec = format!("+refs/notes/ai:{}", tracking_ref);

    let fetch_args = build_authorship_fetch_args(
        repository.global_args_for_exec(),
        remote_name,
        &fetch_refspec,
    );

    tracing::debug!("pre-push authorship fetch: {:?}", &fetch_args);

    // Fetch is best-effort; if it fails (e.g., no remote notes yet), continue
    if exec_git(&fetch_args).is_err() {
        return;
    }

    let local_notes_ref = "refs/notes/ai";

    if !ref_exists(repository, &tracking_ref) {
        return;
    }

    if !ref_exists(repository, local_notes_ref) {
        // Only tracking ref exists - copy it to local
        tracing::debug!(
            "pre-push: initializing {} from {}",
            local_notes_ref,
            tracking_ref
        );
        if let Err(e) = copy_ref(repository, &tracking_ref, local_notes_ref) {
            tracing::debug!("pre-push notes copy failed: {}", e);
        }
        return;
    }

    // Both exist - merge them
    tracing::debug!(
        "pre-push: merging {} into {}",
        tracking_ref,
        local_notes_ref
    );
    if let Err(e) = merge_notes_from_ref(repository, &tracking_ref) {
        tracing::debug!("pre-push notes merge failed: {}", e);
        // Fallback: manually merge notes when git notes merge crashes
        // (e.g., due to corrupted/mixed-fanout notes trees, or git bugs
        // with fanout-level mismatches on older git versions like macOS)
        if let Err(e2) = fallback_merge_notes_ours(repository, &tracking_ref) {
            tracing::debug!("pre-push fallback merge also failed: {}", e2);
        }
    }
}

fn is_non_fast_forward_error(error: &GitAiError) -> bool {
    let GitAiError::GitCliError { stderr, .. } = error else {
        return false;
    };
    stderr.contains("non-fast-forward")
}

fn extract_remote_from_fetch_args(args: &[String]) -> Option<String> {
    let mut after_double_dash = false;

    for arg in args {
        if !after_double_dash {
            if arg == "--" {
                after_double_dash = true;
                continue;
            }
            if arg.starts_with('-') {
                // Option; skip
                continue;
            }
        }

        // Candidate positional arg; determine if it's a repository URL/path
        let s = arg.as_str();

        // 1) URL forms (https://, ssh://, file://, git://, etc.)
        if s.contains("://") || s.starts_with("file://") {
            return Some(arg.clone());
        }

        // 2) SCP-like syntax: user@host:path
        if s.contains('@') && s.contains(':') && !s.contains("://") {
            return Some(arg.clone());
        }

        // 3) Local path forms
        if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with("~/")
        {
            return Some(arg.clone());
        }

        // Heuristic: bare repo directories often end with .git
        if s.ends_with(".git") {
            return Some(arg.clone());
        }

        // 4) As a last resort, if the path exists on disk, treat as local path
        if std::path::Path::new(s).exists() {
            return Some(arg.clone());
        }

        // Otherwise, do not treat this positional token as a repository; likely a refspec
        break;
    }

    None
}

fn with_disabled_hooks(mut args: Vec<String>) -> Vec<String> {
    args.push("-c".to_string());
    args.push(disabled_hooks_config().to_string());
    args
}

fn build_authorship_fetch_args(
    global_args: Vec<String>,
    remote_name: &str,
    fetch_refspec: &str,
) -> Vec<String> {
    let mut args = with_disabled_hooks(global_args);
    args.push("fetch".to_string());
    args.push("--no-tags".to_string());
    args.push("--recurse-submodules=no".to_string());
    args.push("--no-write-fetch-head".to_string());
    args.push("--no-write-commit-graph".to_string());
    args.push("--no-auto-maintenance".to_string());
    args.push(remote_name.to_string());
    args.push(fetch_refspec.to_string());
    args
}

fn build_authorship_push_args(global_args: Vec<String>, remote_name: &str) -> Vec<String> {
    let mut args = with_disabled_hooks(global_args);
    args.push("push".to_string());
    args.push("--quiet".to_string());
    args.push("--no-recurse-submodules".to_string());
    args.push("--no-verify".to_string());
    args.push("--no-signed".to_string());
    args.push(remote_name.to_string());
    args.push(AI_AUTHORSHIP_PUSH_REFSPEC.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorship_fetch_args_always_disable_hooks() {
        let disabled_hooks = disabled_hooks_config();
        let args = build_authorship_fetch_args(
            vec!["-C".to_string(), "/tmp/repo".to_string()],
            "origin",
            "+refs/notes/ai:refs/notes/ai-remote/origin",
        );

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == disabled_hooks)
        );
        assert!(args.contains(&"fetch".to_string()));
    }

    #[test]
    fn authorship_push_args_always_disable_hooks() {
        let disabled_hooks = disabled_hooks_config();
        let args =
            build_authorship_push_args(vec!["-C".to_string(), "/tmp/repo".to_string()], "origin");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == disabled_hooks)
        );
        assert!(args.contains(&"push".to_string()));
    }

    #[test]
    fn missing_remote_notes_ref_error_is_detected() {
        let err = GitAiError::GitCliError {
            code: Some(128),
            stderr: "fatal: couldn't find remote ref refs/notes/ai".to_string(),
            args: vec!["fetch".to_string(), "origin".to_string()],
        };
        assert!(is_missing_remote_notes_ref_error(&err));
    }

    #[test]
    fn missing_remote_notes_ref_error_ignores_unrelated_git_errors() {
        let err = GitAiError::GitCliError {
            code: Some(128),
            stderr: "fatal: Authentication failed for 'https://github.com/org/repo.git/'"
                .to_string(),
            args: vec!["fetch".to_string(), "origin".to_string()],
        };
        assert!(!is_missing_remote_notes_ref_error(&err));
    }
}
