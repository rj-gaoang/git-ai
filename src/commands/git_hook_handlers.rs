//! Repository-local git hook infrastructure.
//!
//! The legacy symlink-style git core-hooks feature has been sunset. This module
//! still keeps removal helpers for old installs, and now installs a repo-local
//! dispatcher so git-ai can attach to a repository's effective `post-commit`
//! without overwriting existing hooks.

use crate::error::GitAiError;
use crate::git::repository::Repository;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

const CONFIG_KEY_CORE_HOOKS_PATH: &str = "core.hooksPath";
const REPO_HOOK_STATE_FILE: &str = "git_hooks_state.json";
const REBASE_HOOK_MASK_STATE_FILE: &str = "rebase_hook_mask_state.json";
const GIT_HOOKS_DIR_NAME: &str = "hooks";
const REPO_HOOK_STATE_SCHEMA_VERSION: &str = "repo_hooks/2";
const MANAGED_HOOK_MARKER: &str = "git-ai managed repository hook dispatcher";
const REPO_POST_COMMIT_SOURCE: &str = "git-repo-post-commit-hook";
const REPO_POST_MERGE_SOURCE: &str = "git-repo-post-merge-hook";

pub const ENV_SKIP_ALL_HOOKS: &str = "GIT_AI_SKIP_ALL_HOOKS";
// Intentionally avoid a GIT_* prefix so git alias shell-command tests don't
// observe extra GIT_* variables in the environment.
pub const ENV_SKIP_MANAGED_HOOKS: &str = "GITAI_SKIP_MANAGED_HOOKS";

// ---------------------------------------------------------------------------
// Core git hook names (used for binary-name detection)
// ---------------------------------------------------------------------------

const CORE_GIT_HOOK_NAMES: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "push-to-checkout",
    "reference-transaction",
];

// ---------------------------------------------------------------------------
// Serde types (needed to read existing state files during removal)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ForwardMode {
    RepoLocal,
    GlobalFallback,
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct RepoHookState {
    #[serde(default = "repo_hook_state_schema_version")]
    schema_version: String,
    managed_hooks_path: String,
    original_local_hooks_path: Option<String>,
    #[serde(default)]
    forward_mode: ForwardMode,
    #[serde(default, alias = "previous_hooks_path")]
    forward_hooks_path: Option<String>,
    binary_path: String,
}

fn repo_hook_state_schema_version() -> String {
    REPO_HOOK_STATE_SCHEMA_VERSION.to_string()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` when the binary name matches a recognised git core hook,
/// indicating this process was invoked as a hook symlink.
pub fn is_git_hook_binary_name(binary_name: &str) -> bool {
    CORE_GIT_HOOK_NAMES.contains(&binary_name)
}

/// Report returned by [`remove_repo_hooks`].
#[derive(Debug, Clone, Default)]
pub struct RemoveRepoHooksReport {
    pub changed: bool,
    pub managed_hooks_path: PathBuf,
}

/// Install or refresh the repository-local dispatcher used as the effective
/// `core.hooksPath` for this repo.
pub fn ensure_repo_post_commit_dispatcher(
    repo: &Repository,
    git_ai_exe: &Path,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let managed_hooks_dir = managed_git_hooks_dir_for_repo(repo);
    let state_path = repo_state_path(repo);
    let local_config_path = repo_local_config_path(repo);
    let current_local_hooks =
        read_hooks_path_from_config(&local_config_path, gix_config::Source::Local)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    let current_effective_hooks = repo
        .config_get_str(CONFIG_KEY_CORE_HOOKS_PATH)
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let current_points_to_managed = current_local_hooks.as_deref().is_some_and(|path| {
        paths_equivalent(&hooks_path_value_to_path(repo, path), &managed_hooks_dir)
    });

    let prior_state = read_repo_hook_state(&state_path)?;
    let original_local_hooks_path = if current_points_to_managed {
        prior_state
            .as_ref()
            .and_then(|state| state.original_local_hooks_path.clone())
            .filter(|value| !value.trim().is_empty())
    } else {
        current_local_hooks.clone()
    };
    let forward_hooks_path = if current_points_to_managed {
        prior_state
            .as_ref()
            .and_then(|state| state.forward_hooks_path.clone())
            .or_else(|| {
                prior_state
                    .as_ref()
                    .and_then(|state| state.original_local_hooks_path.clone())
            })
            .filter(|value| !value.trim().is_empty())
    } else {
        current_effective_hooks.clone().or_else(|| {
            Some(path_value_for_git_config(&default_git_hooks_dir_for_repo(
                repo,
            )))
        })
    }
    .and_then(|value| {
        let path = hooks_path_value_to_path(repo, &value);
        if is_disallowed_forward_hooks_path(&path, Some(repo), Some(&managed_hooks_dir)) {
            None
        } else {
            Some(path_value_for_git_config(&path))
        }
    });

    let mut changed = false;
    let managed_hooks_value = path_value_for_git_config(&managed_hooks_dir);
    if !current_points_to_managed {
        changed |= set_hooks_path_in_config(
            &local_config_path,
            gix_config::Source::Local,
            &managed_hooks_value,
            dry_run,
        )?;
    }

    let desired_state = RepoHookState {
        schema_version: REPO_HOOK_STATE_SCHEMA_VERSION.to_string(),
        managed_hooks_path: managed_hooks_value.clone(),
        original_local_hooks_path,
        forward_mode: if forward_hooks_path.is_some() {
            ForwardMode::RepoLocal
        } else {
            ForwardMode::None
        },
        forward_hooks_path: forward_hooks_path.clone(),
        binary_path: path_value_for_git_config(git_ai_exe),
    };

    let state_changed = prior_state.as_ref() != Some(&desired_state);
    if state_changed {
        changed = true;
        if !dry_run {
            write_repo_hook_state(&state_path, &desired_state)?;
        }
    }

    if ensure_dispatcher_scripts(
        &managed_hooks_dir,
        git_ai_exe,
        forward_hooks_path.as_deref(),
        dry_run,
    )? {
        changed = true;
    }

    Ok(changed)
}

/// Remove git-ai managed hooks from a repository, restoring the previous
/// `core.hooksPath` value if one was saved in the hook state file.
pub fn remove_repo_hooks(
    repo: &Repository,
    dry_run: bool,
) -> Result<RemoveRepoHooksReport, GitAiError> {
    let managed_hooks_dir = managed_git_hooks_dir_for_repo(repo);
    let state_path = repo_state_path(repo);
    let rebase_state_path = rebase_hook_mask_state_path(repo);
    let local_config_path = repo_local_config_path(repo);
    let prior_state = read_repo_hook_state(&state_path)?;

    let current_local_hooks =
        read_hooks_path_from_config(&local_config_path, gix_config::Source::Local)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

    let local_points_to_managed = current_local_hooks
        .as_deref()
        .is_some_and(|path| normalize_path(Path::new(path)) == normalize_path(&managed_hooks_dir));

    let restored_local_hooks = prior_state
        .as_ref()
        .and_then(|state| state.original_local_hooks_path.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| {
            normalize_path(path) != normalize_path(&managed_hooks_dir)
                && !is_disallowed_forward_hooks_path(path, Some(repo), Some(&managed_hooks_dir))
        });

    let mut changed = false;
    if local_points_to_managed {
        if let Some(restored_hooks_path) = restored_local_hooks {
            changed |= set_hooks_path_in_config(
                &local_config_path,
                gix_config::Source::Local,
                &restored_hooks_path.to_string_lossy(),
                dry_run,
            )?;
        } else {
            changed |= unset_hooks_path_in_local_config(repo, dry_run)?;
        }
    }

    if managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_ok() {
        changed = true;
        if !dry_run {
            remove_hook_entry(&managed_hooks_dir)?;
        }
    }

    changed |= delete_state_file(&state_path, dry_run)?;
    changed |= delete_state_file(&rebase_state_path, dry_run)?;

    Ok(RemoveRepoHooksReport {
        changed,
        managed_hooks_path: managed_hooks_dir,
    })
}

/// Returns `true` when a hook state file exists for the repo (or from env context),
/// meaning managed hooks were previously installed.
pub fn has_repo_hook_state(repo: Option<&Repository>) -> bool {
    let state_path = repo.map(repo_state_path).or_else(repo_state_path_from_env);
    state_path
        .map(|path| path.exists() || path.symlink_metadata().is_ok())
        .unwrap_or(false)
}

/// Resolve the hooks path that was active before git-ai managed hooks were
/// installed so the wrapper can set it on child git invocations.
pub fn resolve_previous_non_managed_hooks_path(repo: Option<&Repository>) -> Option<PathBuf> {
    should_forward_repo_state_first(repo)
}

// ---------------------------------------------------------------------------
// Internal helpers - path resolution
// ---------------------------------------------------------------------------

fn repo_ai_dir(repo: &Repository) -> PathBuf {
    repo.common_dir().join("ai")
}

fn repo_worktree_ai_dir(repo: &Repository) -> PathBuf {
    repo.path().join("ai")
}

fn repo_local_config_path(repo: &Repository) -> PathBuf {
    repo.common_dir().join("config")
}

fn repo_state_path(repo: &Repository) -> PathBuf {
    repo_ai_dir(repo).join(REPO_HOOK_STATE_FILE)
}

fn rebase_hook_mask_state_path(repo: &Repository) -> PathBuf {
    repo_worktree_ai_dir(repo).join(REBASE_HOOK_MASK_STATE_FILE)
}

fn managed_git_hooks_dir_for_repo(repo: &Repository) -> PathBuf {
    repo_ai_dir(repo).join(GIT_HOOKS_DIR_NAME)
}

fn default_git_hooks_dir_for_repo(repo: &Repository) -> PathBuf {
    repo.common_dir().join(GIT_HOOKS_DIR_NAME)
}

fn path_value_for_git_config(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let raw = raw
        .strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
        .unwrap_or(raw.as_ref());
    crate::utils::normalize_to_posix(raw)
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn hooks_path_value_to_path(repo: &Repository, value: &str) -> PathBuf {
    let trimmed = value.trim();
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        repo.canonical_workdir().join(path)
    }
}

fn managed_git_hooks_dir_from_context() -> Option<PathBuf> {
    if let Some(repo) = find_hook_repository_from_context() {
        return Some(managed_git_hooks_dir_for_repo(&repo));
    }
    git_dir_from_context().map(|git_dir| git_dir.join("ai").join(GIT_HOOKS_DIR_NAME))
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn canonicalize_if_possible(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn is_managed_hooks_path(path: &Path, repo: Option<&Repository>) -> bool {
    if let Some(repo) = repo {
        return normalize_path(path) == normalize_path(&managed_git_hooks_dir_for_repo(repo));
    }
    if let Some(managed_from_context) = managed_git_hooks_dir_from_context() {
        return normalize_path(path) == normalize_path(&managed_from_context);
    }
    false
}

// ---------------------------------------------------------------------------
// Internal helpers - git config operations
// ---------------------------------------------------------------------------

fn global_git_config_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = test_global_git_config_override_path() {
        return path;
    }

    if let Ok(path) = std::env::var("GIT_CONFIG_GLOBAL")
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }
    crate::mdm::utils::home_dir().join(".gitconfig")
}

#[cfg(test)]
fn test_global_git_config_override_path() -> Option<PathBuf> {
    test_global_git_config_override()
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

#[cfg(test)]
fn test_global_git_config_override() -> &'static std::sync::Mutex<Option<PathBuf>> {
    use std::sync::OnceLock;

    static TEST_GLOBAL_CONFIG_OVERRIDE: OnceLock<std::sync::Mutex<Option<PathBuf>>> =
        OnceLock::new();
    TEST_GLOBAL_CONFIG_OVERRIDE.get_or_init(|| std::sync::Mutex::new(None))
}

fn load_config(
    path: &Path,
    source: gix_config::Source,
) -> Result<gix_config::File<'static>, GitAiError> {
    if path.exists() {
        return gix_config::File::from_path_no_includes(path.to_path_buf(), source)
            .map_err(|e| GitAiError::GixError(e.to_string()));
    }
    Ok(gix_config::File::default())
}

fn write_config(path: &Path, cfg: &gix_config::File<'_>) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = cfg.to_bstring();
    fs::write(path, bytes.as_slice())?;
    Ok(())
}

fn write_repo_hook_state(path: &Path, state: &RepoHookState) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content =
        serde_json::to_string_pretty(state).map_err(|e| GitAiError::Generic(e.to_string()))?;
    fs::write(path, content)?;
    Ok(())
}

fn read_hooks_path_from_config(path: &Path, source: gix_config::Source) -> Option<String> {
    load_config(path, source).ok().and_then(|cfg| {
        cfg.string(CONFIG_KEY_CORE_HOOKS_PATH)
            .map(|v| v.to_string())
    })
}

fn set_hooks_path_in_config(
    path: &Path,
    source: gix_config::Source,
    value: &str,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let mut cfg = load_config(path, source)?;
    let current = cfg
        .string(CONFIG_KEY_CORE_HOOKS_PATH)
        .map(|v| v.to_string());
    if current.as_deref() == Some(value) {
        return Ok(false);
    }

    if !dry_run {
        cfg.set_raw_value(&CONFIG_KEY_CORE_HOOKS_PATH, value)
            .map_err(|e| GitAiError::GixError(e.to_string()))?;
        write_config(path, &cfg)?;
    }

    Ok(true)
}

fn unset_hooks_path_in_local_config(repo: &Repository, dry_run: bool) -> Result<bool, GitAiError> {
    let local_config_path = repo_local_config_path(repo);
    if read_hooks_path_from_config(&local_config_path, gix_config::Source::Local).is_none() {
        return Ok(false);
    }

    if !dry_run {
        let mut cfg = load_config(&local_config_path, gix_config::Source::Local)?;
        if let Ok(mut hooks_path_values) = cfg.raw_values_mut_by("core", None, "hooksPath") {
            hooks_path_values.delete_all();
        }
        write_config(&local_config_path, &cfg)?;
    }

    Ok(true)
}

// ---------------------------------------------------------------------------
// Internal helpers - state file I/O
// ---------------------------------------------------------------------------

fn read_repo_hook_state(path: &Path) -> Result<Option<RepoHookState>, GitAiError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    match serde_json::from_str::<RepoHookState>(&content) {
        Ok(state) => Ok(Some(state)),
        Err(err) => {
            tracing::debug!(
                "ignoring invalid repo hook state {}: {}",
                path.display(),
                err
            );
            Ok(None)
        }
    }
}

fn delete_state_file(path: &Path, dry_run: bool) -> Result<bool, GitAiError> {
    if !path.exists() {
        return Ok(false);
    }
    if !dry_run {
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn remove_hook_entry(hook_path: &Path) -> Result<(), GitAiError> {
    let metadata = hook_path.symlink_metadata()?;
    let file_type = metadata.file_type();

    if file_type.is_dir() && !file_type.is_symlink() {
        fs::remove_dir_all(hook_path)?;
    } else {
        fs::remove_file(hook_path)?;
    }
    Ok(())
}

fn managed_hook_script_content(
    hook_name: &str,
    git_ai_exe: &Path,
    previous_hooks_path: Option<&str>,
) -> String {
    let git_ai_exe = shell_single_quote(&path_value_for_git_config(git_ai_exe));
    let hook_name_quoted = shell_single_quote(hook_name);
    let post_commit_source = shell_single_quote(REPO_POST_COMMIT_SOURCE);
    let post_merge_source = shell_single_quote(REPO_POST_MERGE_SOURCE);
    let forward_hooks_path = shell_single_quote(previous_hooks_path.unwrap_or(""));

    format!(
        r#"#!/bin/sh
# {marker}.

GIT_AI_EXE={git_ai_exe}
HOOK_NAME={hook_name}
POST_COMMIT_SOURCE={post_commit_source}
POST_MERGE_SOURCE={post_merge_source}
FORWARD_HOOKS_PATH={forward_hooks_path}

run_forward_hook() {{
  if [ -z "$FORWARD_HOOKS_PATH" ]; then
    return 0
  fi
  FORWARD_HOOK="$FORWARD_HOOKS_PATH/$HOOK_NAME"
  if [ ! -f "$FORWARD_HOOK" ]; then
    return 0
  fi
  if [ "$GIT_AI_MANAGED_HOOK_FORWARD_DEPTH" = "1" ]; then
    return 0
  fi
  GIT_AI_MANAGED_HOOK_FORWARD_DEPTH=1 "$FORWARD_HOOK" "$@"
}}

if [ "${{GIT_AI_SKIP_ALL_HOOKS:-}}" = "1" ] || [ "${{GITAI_SKIP_MANAGED_HOOKS:-}}" = "1" ] || [ -n "${{GIT_AI_WRAPPER_INVOCATION_ID:-}}" ]; then
  run_forward_hook "$@"
  exit $?
fi

if [ "$HOOK_NAME" = "post-commit" ] && ([ -x "$GIT_AI_EXE" ] || [ -f "$GIT_AI_EXE" ]); then
  (
    GIT_AI_SKIP_ALL_HOOKS=1 \
    GIT_AI_POST_COMMIT_FALLBACK_UPLOAD_SPAWNED=1 \
    "$GIT_AI_EXE" repair-authorship-note HEAD --write

    GIT_AI_SKIP_ALL_HOOKS=1 \
    GIT_AI_POST_COMMIT_FALLBACK_UPLOAD_SPAWNED=1 \
    "$GIT_AI_EXE" upload-stats HEAD --source "$POST_COMMIT_SOURCE" --wait-for-authorship-note-ms 15000 --skip-if-already-uploaded
  ) >/dev/null 2>&1 &
fi

if [ "$HOOK_NAME" = "post-merge" ] && ([ -x "$GIT_AI_EXE" ] || [ -f "$GIT_AI_EXE" ]); then
  GIT_AI_SKIP_ALL_HOOKS=1 \
  "$GIT_AI_EXE" sync-working-log-base --source "$POST_MERGE_SOURCE" >/dev/null 2>&1 || true
fi

run_forward_hook "$@"
exit $?
"#,
        marker = MANAGED_HOOK_MARKER,
        git_ai_exe = git_ai_exe,
        hook_name = hook_name_quoted,
        post_commit_source = post_commit_source,
        post_merge_source = post_merge_source,
        forward_hooks_path = forward_hooks_path
    )
}

fn write_executable_script(path: &Path, content: &str, dry_run: bool) -> Result<bool, GitAiError> {
    let existing = fs::read_to_string(path).ok();
    if existing.as_deref() == Some(content) {
        return Ok(false);
    }

    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions)?;
        }
    }

    Ok(true)
}

fn ensure_dispatcher_scripts(
    managed_hooks_dir: &Path,
    git_ai_exe: &Path,
    previous_hooks_path: Option<&str>,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let mut changed = false;
    if !dry_run {
        fs::create_dir_all(managed_hooks_dir)?;
    }

    for hook_name in CORE_GIT_HOOK_NAMES {
        let content = managed_hook_script_content(hook_name, git_ai_exe, previous_hooks_path);
        changed |= write_executable_script(&managed_hooks_dir.join(hook_name), &content, dry_run)?;
    }

    Ok(changed)
}

// ---------------------------------------------------------------------------
// Internal helpers - forward path validation
// ---------------------------------------------------------------------------

fn is_path_inside_component(path: &Path, component: &str) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(component)
    })
}

fn is_path_inside_any_git_ai_dir(path: &Path) -> bool {
    let mut previous_was_git_dir = false;
    for part in path.components() {
        let part = part.as_os_str().to_string_lossy();
        if previous_was_git_dir && part.eq_ignore_ascii_case("ai") {
            return true;
        }
        previous_was_git_dir = part.eq_ignore_ascii_case(".git");
    }
    false
}

fn is_disallowed_forward_hooks_path(
    path: &Path,
    repo: Option<&Repository>,
    managed_hooks_path: Option<&Path>,
) -> bool {
    if is_path_inside_component(path, ".git-ai") {
        return true;
    }
    if is_path_inside_any_git_ai_dir(path) {
        return true;
    }

    if let Some(repo) = repo {
        let repo_ai_dir = repo_ai_dir(repo);
        if normalize_path(path).starts_with(normalize_path(&repo_ai_dir)) {
            return true;
        }
    }

    if let Some(managed_hooks_path) = managed_hooks_path
        && normalize_path(path) == normalize_path(managed_hooks_path)
    {
        return true;
    }

    is_managed_hooks_path(path, repo)
}

// ---------------------------------------------------------------------------
// Internal helpers - repository / git-dir context discovery
// ---------------------------------------------------------------------------

fn repo_state_path_from_env() -> Option<PathBuf> {
    if let Some(repo) = find_hook_repository_from_context() {
        return Some(repo_state_path(&repo));
    }
    git_dir_from_context().map(|git_dir| git_dir.join("ai").join(REPO_HOOK_STATE_FILE))
}

fn git_dir_from_env() -> Option<PathBuf> {
    let git_dir = std::env::var("GIT_DIR").ok()?;
    let git_dir = git_dir.trim();
    if git_dir.is_empty() {
        return None;
    }

    let git_dir = PathBuf::from(git_dir);
    if git_dir.is_absolute() {
        Some(git_dir)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(git_dir))
    }
}

fn git_dir_from_context() -> Option<PathBuf> {
    if let Some(from_env) = git_dir_from_env() {
        return Some(from_env);
    }

    // In some wrapper-internal invocations Git may not export GIT_DIR to hooks.
    // For normal non-bare hooks, the working directory is the repo root.
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join(".git");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

fn worktree_root_from_git_dir(git_dir: &Path) -> Option<PathBuf> {
    let gitdir_file = git_dir.join("gitdir");
    let gitdir_target = fs::read_to_string(gitdir_file).ok()?;
    let gitdir_target = gitdir_target.trim();
    if gitdir_target.is_empty() {
        return None;
    }

    let gitdir_path = PathBuf::from(gitdir_target);
    let gitdir_path = if gitdir_path.is_absolute() {
        gitdir_path
    } else {
        git_dir.join(gitdir_path)
    };

    let gitdir_path = canonicalize_if_possible(gitdir_path);
    gitdir_path.parent().map(Path::to_path_buf)
}

fn hook_repository_lookup_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(git_dir) = git_dir_from_context() {
        if let Some(worktree_root) = worktree_root_from_git_dir(&git_dir)
            && !paths
                .iter()
                .any(|existing| normalize_path(existing) == normalize_path(&worktree_root))
        {
            paths.push(worktree_root);
        }

        if !paths
            .iter()
            .any(|existing| normalize_path(existing) == normalize_path(&git_dir))
        {
            paths.push(git_dir.clone());
        }

        if git_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.eq_ignore_ascii_case(".git"))
            .unwrap_or(false)
            && let Some(parent) = git_dir.parent()
        {
            let parent = parent.to_path_buf();
            if !paths
                .iter()
                .any(|existing| normalize_path(existing) == normalize_path(&parent))
            {
                paths.push(parent);
            }
        }
    }

    if let Ok(current_dir) = std::env::current_dir()
        && !paths
            .iter()
            .any(|existing| normalize_path(existing) == normalize_path(&current_dir))
    {
        paths.push(current_dir);
    }

    paths
}

fn find_hook_repository_from_context() -> Option<Repository> {
    hook_repository_lookup_paths()
        .into_iter()
        .find_map(|path| crate::git::find_repository_in_path(&path.to_string_lossy()).ok())
}

fn context_repo_ai_dir() -> Option<PathBuf> {
    if let Some(repo) = find_hook_repository_from_context() {
        return Some(repo_ai_dir(&repo));
    }
    git_dir_from_context().map(|git_dir| git_dir.join("ai"))
}

// ---------------------------------------------------------------------------
// Internal helpers - forward resolution (used by wrapper during transition)
// ---------------------------------------------------------------------------

fn should_forward_repo_state_first(repo: Option<&Repository>) -> Option<PathBuf> {
    let state_path = repo
        .map(repo_state_path)
        .or_else(repo_state_path_from_env)?;
    let state = read_repo_hook_state(&state_path).ok().flatten()?;

    let managed_hooks_dir = if !state.managed_hooks_path.trim().is_empty() {
        Some(PathBuf::from(state.managed_hooks_path.trim()))
    } else if let Some(repo) = repo {
        Some(managed_git_hooks_dir_for_repo(repo))
    } else {
        managed_git_hooks_dir_from_context()
    };

    let fallback_repo = repo;
    let candidate = match state.forward_mode {
        ForwardMode::RepoLocal => state
            .forward_hooks_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        ForwardMode::GlobalFallback => {
            read_hooks_path_from_config(&global_git_config_path(), gix_config::Source::User)
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        }
        ForwardMode::None => None,
    }?;

    if is_disallowed_forward_hooks_path(&candidate, fallback_repo, managed_hooks_dir.as_deref()) {
        return None;
    }
    if let Some(context_repo_ai_dir) = context_repo_ai_dir()
        && normalize_path(&candidate).starts_with(normalize_path(&context_repo_ai_dir))
    {
        return None;
    }

    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_script_uploads_post_commit_and_forwards_existing_hooks_path() {
        let dispatcher = managed_hook_script_content(
            "post-commit",
            Path::new("C:/Users/admin/.git-ai/launcher/git-ai.exe"),
            Some("D:/product/app/node_modules/@one/rscli"),
        );

        assert!(dispatcher.contains(MANAGED_HOOK_MARKER));
        assert!(dispatcher.contains("FORWARD_HOOKS_PATH='D:/product/app/node_modules/@one/rscli'"));
        assert!(dispatcher.contains("run_forward_hook \"$@\""));
        assert!(dispatcher.contains("repair-authorship-note HEAD --write"));
        assert!(dispatcher.contains(
            "upload-stats HEAD --source \"$POST_COMMIT_SOURCE\" --wait-for-authorship-note-ms 15000 --skip-if-already-uploaded"
        ));
        assert!(dispatcher.contains("GIT_AI_WRAPPER_INVOCATION_ID"));
    }

    #[test]
    fn dispatcher_script_syncs_working_log_base_on_post_merge() {
        let dispatcher = managed_hook_script_content(
            "post-merge",
            Path::new("C:/Users/admin/.git-ai/launcher/git-ai.exe"),
            Some("D:/product/app/node_modules/@one/rscli"),
        );

        assert!(dispatcher.contains("HOOK_NAME='post-merge'"));
        assert!(dispatcher.contains("POST_MERGE_SOURCE='git-repo-post-merge-hook'"));
        assert!(dispatcher.contains("sync-working-log-base --source \"$POST_MERGE_SOURCE\""));
        assert!(dispatcher.contains("run_forward_hook \"$@\""));
    }
}
