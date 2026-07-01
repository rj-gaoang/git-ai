use crate::daemon::DaemonConfig;
use crate::error::GitAiError;
use crate::mdm::agents::get_all_installers;
use crate::mdm::hook_installer::HookInstallerParams;
use crate::mdm::skills_installer;
use crate::mdm::spinner::{Spinner, print_diff};
use crate::mdm::utils::{get_current_binary_path, home_dir};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CORE_HOOKS_PATH_KEY: &str = "core.hooksPath";
const VISUAL_STUDIO_INSTALLER_ID: &str = "visual-studio";
const GLOBAL_POST_COMMIT_HOOK_ID: &str = "git-global-post-commit-hook";
const MANAGED_GLOBAL_HOOKS_DIR: &str = "managed-git-hooks";
const MANAGED_GLOBAL_HOOK_MARKER: &str = "git-ai managed global post-commit hook";
const GLOBAL_POST_COMMIT_SOURCE: &str = "git-global-post-commit-hook";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InstallOptions {
    dry_run: bool,
    verbose: bool,
    install_skills: bool,
    include_visual_studio_extension: bool,
}

/// Installation status for a tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    /// Tool was not detected on the machine
    NotFound,
    /// Hooks/extensions were successfully installed or updated
    Installed,
    /// Hooks/extensions were already up to date
    AlreadyInstalled,
    /// Installation attempted but failed
    Failed,
}

impl InstallStatus {
    /// Convert status to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallStatus::NotFound => "not_found",
            InstallStatus::Installed => "installed",
            InstallStatus::AlreadyInstalled => "already_installed",
            InstallStatus::Failed => "failed",
        }
    }
}

/// Detailed install result for metrics tracking
#[derive(Debug, Clone)]
pub struct InstallResult {
    pub status: InstallStatus,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl InstallResult {
    pub fn installed() -> Self {
        Self {
            status: InstallStatus::Installed,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn already_installed() -> Self {
        Self {
            status: InstallStatus::AlreadyInstalled,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn not_found() -> Self {
        Self {
            status: InstallStatus::NotFound,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            status: InstallStatus::Failed,
            error: Some(msg.into()),
            warnings: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Get message for ClickHouse (error if failed, else joined warnings)
    pub fn message_for_metrics(&self) -> Option<String> {
        if let Some(err) = &self.error {
            Some(err.clone())
        } else if !self.warnings.is_empty() {
            Some(self.warnings.join("; "))
        } else {
            None
        }
    }
}

/// Convert a HashMap of tool statuses to string keys and values
pub fn to_hashmap(statuses: HashMap<String, InstallStatus>) -> HashMap<String, String> {
    statuses
        .into_iter()
        .map(|(k, v)| (k, v.as_str().to_string()))
        .collect()
}

fn print_amp_plugins_note(installer_id: &str) {
    if installer_id == "amp" {
        println!("  Note: Amp plugins are experimental. Run amp with `PLUGINS=all amp`.");
    }
}

/// Find PIDs of running processes that match any of the given process names.
/// Returns a list of (pid, process_name) tuples for each match found.
fn find_running_pids(process_names: &[&str]) -> Vec<(u32, String)> {
    if process_names.is_empty() {
        return vec![];
    }

    let output = {
        #[cfg(unix)]
        {
            Command::new("ps")
                .args(["axo", "pid,comm"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
        }
        #[cfg(windows)]
        {
            Command::new("tasklist")
                .args(["/FO", "CSV", "/NH"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
        }
    };

    let Ok(output) = output else {
        return vec![];
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results: Vec<(u32, String)> = Vec::new();

    for line in stdout.lines() {
        #[cfg(unix)]
        {
            let trimmed = line.trim();
            // ps output: "  PID COMM" — split on whitespace
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let pid_str = parts.next().unwrap_or("").trim();
            let comm = parts.next().unwrap_or("").trim();
            // comm may be a full path; extract the basename
            let base = comm.rsplit('/').next().unwrap_or(comm);
            if let Ok(pid) = pid_str.parse::<u32>() {
                for &name in process_names {
                    if base.eq_ignore_ascii_case(name) {
                        results.push((pid, base.to_string()));
                        break;
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            // tasklist CSV: "Image Name","PID",...
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() >= 2 {
                let image = fields[0].trim_matches('"');
                let pid_str = fields[1].trim_matches('"');
                let base = image.strip_suffix(".exe").unwrap_or(image);
                if let Ok(pid) = pid_str.parse::<u32>() {
                    for &name in process_names {
                        if base.eq_ignore_ascii_case(name) {
                            results.push((pid, base.to_string()));
                            break;
                        }
                    }
                }
            }
        }
    }

    results
}

fn ensure_global_git_config_dirs() -> Result<(), GitAiError> {
    if let Ok(path) = std::env::var("GIT_CONFIG_GLOBAL") {
        let config_path = PathBuf::from(path);
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        fs::create_dir_all(home)?;
    }

    Ok(())
}

fn global_git_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("GIT_CONFIG_GLOBAL")
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }
    home_dir().join(".gitconfig")
}

fn load_global_git_config(path: &Path) -> Result<gix_config::File<'static>, GitAiError> {
    if path.exists() {
        return gix_config::File::from_path_no_includes(
            path.to_path_buf(),
            gix_config::Source::User,
        )
        .map_err(|e| GitAiError::GixError(e.to_string()));
    }
    Ok(gix_config::File::default())
}

fn write_global_git_config(path: &Path, cfg: &gix_config::File<'_>) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = cfg.to_bstring();
    fs::write(path, bytes.as_slice())?;
    Ok(())
}

fn remove_global_git_config_section(section: &str) -> Result<(), GitAiError> {
    let config_path = global_git_config_path();
    if !config_path.exists() {
        return Ok(());
    }
    let mut cfg = load_global_git_config(&config_path)?;
    while cfg.remove_section(section, None).is_some() {}
    write_global_git_config(&config_path, &cfg)
}

fn cleanup_daemon_trace2(dry_run: bool) -> Result<(), GitAiError> {
    if dry_run {
        return Ok(());
    }

    ensure_global_git_config_dirs()?;
    remove_global_git_config_section("trace2")
}

#[cfg(windows)]
fn sync_windows_git_proxy_entrypoint(
    git_ai_exe: &Path,
    git_proxy: &Path,
    refresh_existing: bool,
) -> Result<(), GitAiError> {
    if !git_ai_exe.exists() {
        return Ok(());
    }

    if git_proxy.exists() && !refresh_existing {
        return Ok(());
    }

    if paths_refer_to_same_file(git_ai_exe, git_proxy) {
        return Ok(());
    }

    if let Some(parent) = git_proxy.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(git_ai_exe, git_proxy)?;
    Ok(())
}

#[cfg(windows)]
fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }

    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(windows)]
fn is_windows_git_ai_entrypoint(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    name.eq_ignore_ascii_case("git-ai.exe") || name.eq_ignore_ascii_case("git.exe")
}

#[cfg(windows)]
fn windows_git_ai_root_from_entrypoint(current_exe: &Path) -> Option<PathBuf> {
    let entrypoint_dir = current_exe.parent()?;
    let entrypoint_dir_name = entrypoint_dir.file_name()?.to_str()?;

    if !entrypoint_dir_name.eq_ignore_ascii_case("launcher")
        && !entrypoint_dir_name.eq_ignore_ascii_case("bin")
    {
        return None;
    }

    let root = entrypoint_dir.parent()?;
    let root_name = root.file_name()?.to_str()?;
    if root_name.eq_ignore_ascii_case(".git-ai") {
        Some(root.to_path_buf())
    } else {
        None
    }
}

#[cfg(windows)]
fn sync_windows_git_ai_entrypoint_tree(root: &Path, current_exe: &Path) -> Result<(), GitAiError> {
    let launcher_git_ai = root.join("launcher").join("git-ai.exe");
    let bin_git_ai = root.join("bin").join("git-ai.exe");

    // The launcher is the authoritative entrypoint for normal Windows PATH
    // usage.  If install-hooks is invoked through a compatibility bin copy,
    // still use launcher\git-ai.exe as the source when it exists, so a stale
    // bin hook cannot downgrade the stable launcher.
    let source = if launcher_git_ai.exists() {
        launcher_git_ai.clone()
    } else if current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("git-ai.exe"))
    {
        current_exe.to_path_buf()
    } else if bin_git_ai.exists() {
        bin_git_ai.clone()
    } else {
        return Ok(());
    };

    for target in [
        launcher_git_ai,
        root.join("launcher").join("git.exe"),
        bin_git_ai,
        root.join("bin").join("git.exe"),
    ] {
        // Do not try to overwrite the executable image of the current process.
        // Windows commonly rejects that, and install.ps1 handles active binary
        // replacement with its rename fallback.
        if paths_refer_to_same_file(&target, current_exe) {
            continue;
        }

        sync_windows_git_proxy_entrypoint(&source, &target, true)?;
    }

    Ok(())
}

#[cfg(windows)]
fn repair_git_proxy_entrypoint_for_current_exe(current_exe: &Path) -> Result<(), GitAiError> {
    if !is_windows_git_ai_entrypoint(current_exe) {
        return Ok(());
    }

    if let Some(root) = windows_git_ai_root_from_entrypoint(current_exe) {
        return sync_windows_git_ai_entrypoint_tree(&root, current_exe);
    }

    let Some(install_dir) = current_exe.parent() else {
        return Ok(());
    };

    let current_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let git_ai_exe = install_dir.join("git-ai.exe");
    let git_proxy = install_dir.join("git.exe");
    let refresh_existing = current_name.eq_ignore_ascii_case("git-ai.exe");
    sync_windows_git_proxy_entrypoint(&git_ai_exe, &git_proxy, refresh_existing)
}

#[cfg(windows)]
fn repair_git_proxy_entrypoint(dry_run: bool) -> Result<(), GitAiError> {
    if dry_run {
        return Ok(());
    }

    let Ok(current_exe) = std::env::current_exe() else {
        return Ok(());
    };

    repair_git_proxy_entrypoint_for_current_exe(&current_exe)
}

#[cfg(unix)]
fn repair_git_proxy_entrypoint(dry_run: bool) -> Result<(), GitAiError> {
    if dry_run {
        return Ok(());
    }

    let Ok(current_exe) = std::env::current_exe() else {
        return Ok(());
    };
    let Some(install_dir) = current_exe.parent() else {
        return Ok(());
    };

    let current_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if current_name != "git-ai" && current_name != "git" {
        return Ok(());
    }

    let git_ai_exe = install_dir.join("git-ai");
    let git_proxy = install_dir.join("git");
    if !git_ai_exe.exists() || git_proxy.exists() {
        return Ok(());
    }

    std::os::unix::fs::symlink(&git_ai_exe, &git_proxy)?;
    Ok(())
}

#[cfg(not(any(windows, unix)))]
fn repair_git_proxy_entrypoint(_dry_run: bool) -> Result<(), GitAiError> {
    Ok(())
}

fn expanded_hooks_path(value: &str) -> PathBuf {
    let trimmed = value.trim();
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(trimmed)
}

fn stale_global_hooks_path_should_be_removed(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    let candidate = expanded_hooks_path(trimmed);
    if candidate.exists() || candidate.symlink_metadata().is_ok() {
        return false;
    }

    let normalized = trimmed.replace('/', "\\").to_ascii_lowercase();
    let known_managed_legacy_path = normalized.contains("ai-contribution-tracker")
        || normalized.contains("\\.git-ai\\")
        || normalized.contains("\\.git\\ai\\hooks");

    candidate.is_absolute() || known_managed_legacy_path
}

fn repair_stale_global_hooks_path(dry_run: bool) -> Result<Option<String>, GitAiError> {
    let config_path = global_git_config_path();
    if !config_path.exists() {
        return Ok(None);
    }

    let mut cfg = load_global_git_config(&config_path)?;
    let current = cfg
        .string(CORE_HOOKS_PATH_KEY)
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty());

    let Some(current) = current else {
        return Ok(None);
    };

    if !stale_global_hooks_path_should_be_removed(&current) {
        return Ok(None);
    }

    if !dry_run {
        if let Ok(mut hooks_path_values) = cfg.raw_values_mut_by("core", None, "hooksPath") {
            hooks_path_values.delete_all();
        }
        write_global_git_config(&config_path, &cfg)?;
    }

    Ok(Some(current))
}

#[derive(Debug, Clone)]
struct ManagedGlobalPostCommitHookResult {
    status: InstallStatus,
    message: Option<String>,
}

impl ManagedGlobalPostCommitHookResult {
    fn installed() -> Self {
        Self {
            status: InstallStatus::Installed,
            message: None,
        }
    }

    fn already_installed() -> Self {
        Self {
            status: InstallStatus::AlreadyInstalled,
            message: None,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: InstallStatus::Failed,
            message: Some(message.into()),
        }
    }
}

fn managed_global_hooks_dir() -> PathBuf {
    home_dir().join(".git-ai").join(MANAGED_GLOBAL_HOOKS_DIR)
}

fn managed_global_hooks_path_value() -> String {
    crate::utils::normalize_to_posix(&managed_global_hooks_dir().to_string_lossy())
}

fn path_value_for_git_config(path: &Path) -> String {
    crate::utils::normalize_to_posix(&path.to_string_lossy())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn managed_global_post_commit_hook_content(git_ai_exe: &Path) -> String {
    let git_ai_exe = shell_single_quote(&path_value_for_git_config(git_ai_exe));
    let source = shell_single_quote(GLOBAL_POST_COMMIT_SOURCE);

    format!(
        r#"#!/bin/sh
# {marker}.
# Best-effort only. This hook must never block or break user commits.

GIT_AI_EXE={git_ai_exe}
SOURCE={source}

if [ ! -x "$GIT_AI_EXE" ] && [ ! -f "$GIT_AI_EXE" ]; then
  exit 0
fi

if [ "${{GITAI_SKIP_MANAGED_HOOKS:-}}" = "1" ] || [ -n "${{GIT_AI_WRAPPER_INVOCATION_ID:-}}" ]; then
  exit 0
fi

(
  GIT_AI_SKIP_ALL_HOOKS=1 \
  GIT_AI_POST_COMMIT_FALLBACK_UPLOAD_SPAWNED=1 \
  "$GIT_AI_EXE" repair-authorship-note HEAD --write

  GIT_AI_SKIP_ALL_HOOKS=1 \
  GIT_AI_POST_COMMIT_FALLBACK_UPLOAD_SPAWNED=1 \
  "$GIT_AI_EXE" upload-stats HEAD --source "$SOURCE" --skip-if-already-uploaded
) >/dev/null 2>&1 &

exit 0
"#,
        marker = MANAGED_GLOBAL_HOOK_MARKER,
        git_ai_exe = git_ai_exe,
        source = source
    )
}

fn is_managed_global_hook_content(content: &str) -> bool {
    content.contains(MANAGED_GLOBAL_HOOK_MARKER)
}

fn current_global_hooks_path() -> Result<Option<String>, GitAiError> {
    let config_path = global_git_config_path();
    if !config_path.exists() {
        return Ok(None);
    }

    let cfg = load_global_git_config(&config_path)?;
    Ok(cfg
        .string(CORE_HOOKS_PATH_KEY)
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty()))
}

fn set_global_hooks_path(value: &str) -> Result<(), GitAiError> {
    ensure_global_git_config_dirs()?;
    let config_path = global_git_config_path();
    let mut cfg = load_global_git_config(&config_path)?;
    cfg.set_raw_value(&CORE_HOOKS_PATH_KEY, value)
        .map_err(|e| GitAiError::GixError(e.to_string()))?;
    write_global_git_config(&config_path, &cfg)
}

fn should_use_managed_global_hooks_dir(current_hooks_path: Option<&str>) -> bool {
    let Some(current) = current_hooks_path else {
        return true;
    };
    let trimmed = current.trim();
    if trimmed.is_empty() {
        return true;
    }

    let managed = managed_global_hooks_dir();
    let expanded = expanded_hooks_path(trimmed);
    if expanded == managed {
        return true;
    }

    match (fs::canonicalize(&expanded), fs::canonicalize(&managed)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn managed_global_hook_target_dir(current_hooks_path: Option<&str>) -> PathBuf {
    if should_use_managed_global_hooks_dir(current_hooks_path) {
        managed_global_hooks_dir()
    } else {
        expanded_hooks_path(current_hooks_path.unwrap_or_default())
    }
}

fn can_overwrite_post_commit_hook(path: &Path) -> Result<bool, GitAiError> {
    if !path.exists() {
        return Ok(true);
    }

    let content = fs::read_to_string(path)?;
    Ok(is_managed_global_hook_content(&content))
}

fn install_managed_global_post_commit_hook(
    git_ai_exe: &Path,
    dry_run: bool,
) -> Result<ManagedGlobalPostCommitHookResult, GitAiError> {
    let current_hooks_path = current_global_hooks_path()?;
    let target_dir = managed_global_hook_target_dir(current_hooks_path.as_deref());
    let post_commit_path = target_dir.join("post-commit");
    let use_managed_dir = should_use_managed_global_hooks_dir(current_hooks_path.as_deref());
    let desired_hooks_path = managed_global_hooks_path_value();
    let desired_content = managed_global_post_commit_hook_content(git_ai_exe);

    if !use_managed_dir
        && post_commit_path.exists()
        && !can_overwrite_post_commit_hook(&post_commit_path)?
    {
        return Ok(ManagedGlobalPostCommitHookResult::failed(format!(
            "global core.hooksPath already has a non-git-ai post-commit hook: {}",
            post_commit_path.display()
        )));
    }

    let existing_content = fs::read_to_string(&post_commit_path).ok();
    let hook_changed = existing_content.as_deref() != Some(desired_content.as_str());
    let config_changed = if use_managed_dir {
        current_hooks_path.as_deref().map(str::trim) != Some(desired_hooks_path.as_str())
    } else {
        false
    };

    if dry_run {
        return Ok(if hook_changed || config_changed {
            ManagedGlobalPostCommitHookResult::installed()
        } else {
            ManagedGlobalPostCommitHookResult::already_installed()
        });
    }

    fs::create_dir_all(&target_dir)?;
    if hook_changed {
        fs::write(&post_commit_path, desired_content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&post_commit_path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&post_commit_path, permissions)?;
        }
    }

    if config_changed {
        set_global_hooks_path(&desired_hooks_path)?;
    }

    Ok(if hook_changed || config_changed {
        ManagedGlobalPostCommitHookResult::installed()
    } else {
        ManagedGlobalPostCommitHookResult::already_installed()
    })
}

#[allow(dead_code)]
pub(crate) fn configure_async_mode_daemon_trace2_for_config(
    _daemon_config: &DaemonConfig,
) -> Result<(), GitAiError> {
    ensure_global_git_config_dirs()?;
    remove_global_git_config_section("trace2")
}

fn ensure_daemon(dry_run: bool) {
    if dry_run {
        return;
    }

    if std::env::var_os("GIT_AI_SKIP_DAEMON_RESTART").is_some() {
        return;
    }

    // Don't touch daemon inside test harnesses
    if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
    {
        return;
    }

    let Ok(daemon_config) = DaemonConfig::from_env_or_default_paths() else {
        return;
    };

    stop_orphaned_managed_daemon_processes();

    // Restart daemon so it picks up the freshly-written trace2 config.
    // Uses soft shutdown → hard kill escalation if needed.
    if let Err(e) = crate::commands::daemon::restart_daemon(&daemon_config) {
        eprintln!(
            "[git-ai] warning: failed to restart background service: {}",
            e
        );
    }
}

#[cfg(windows)]
fn stop_orphaned_managed_daemon_processes() {
    let Ok(current_exe) = std::env::current_exe() else {
        return;
    };

    let script = r#"
$target = [Environment]::GetEnvironmentVariable('GIT_AI_INSTALL_HOOKS_CURRENT_EXE')
if ([string]::IsNullOrWhiteSpace($target)) { exit 0 }
try {
  $target = [IO.Path]::GetFullPath($target).TrimEnd('\').ToLowerInvariant()
} catch {
  $target = $target.TrimEnd('\').ToLowerInvariant()
}
Get-CimInstance Win32_Process -Filter "name = 'git-ai.exe'" -ErrorAction SilentlyContinue |
  Where-Object {
    $_.CommandLine -match '\bbg\s+run\b' -and
    $_.ExecutablePath -and
    ([IO.Path]::GetFullPath($_.ExecutablePath).TrimEnd('\').ToLowerInvariant() -eq $target)
  } |
  ForEach-Object {
    try { Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop } catch { }
  }
"#;

    let _ = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("GIT_AI_INSTALL_HOOKS_CURRENT_EXE", current_exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn stop_orphaned_managed_daemon_processes() {}

/// Main entry point for install-hooks command
pub fn run(args: &[String]) -> Result<HashMap<String, String>, GitAiError> {
    let options = parse_install_options(args);

    // Global trace2 makes every Git invocation on the machine hit git-ai's daemon.
    // Keep trace2 scoped to the git-ai proxy process instead.
    if let Err(e) = cleanup_daemon_trace2(options.dry_run) {
        eprintln!("Warning: could not clean up trace2 config (non-fatal): {e}");
    }
    match repair_stale_global_hooks_path(options.dry_run) {
        Ok(Some(path)) if options.dry_run => {
            println!("Would remove stale global core.hooksPath: {}", path);
        }
        Ok(Some(path)) => {
            println!("Removed stale global core.hooksPath: {}", path);
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("Warning: could not repair core.hooksPath (non-fatal): {e}");
        }
    }
    if let Err(e) = repair_git_proxy_entrypoint(options.dry_run) {
        eprintln!("Warning: could not repair git proxy entrypoint (non-fatal): {e}");
    }
    ensure_daemon(options.dry_run);

    // Now that the daemon is (re)started, initialize the telemetry handle so
    // that install-hooks metrics and observability events route through it.
    if !options.dry_run {
        let _ = crate::daemon::telemetry_handle::init_daemon_telemetry_handle();
    }

    // Get absolute path to the current binary. Do not let canonicalization or
    // install-time config persistence block hook/trace2 setup.
    let binary_path = current_binary_path_for_install();
    persist_install_config_best_effort(&binary_path, options.dry_run);
    let params = HookInstallerParams { binary_path };

    // Run async operations with smol and convert result before emitting the
    // install probe, so success telemetry only means hook setup completed.
    let statuses = smol::block_on(async_run_install(&params, &options));

    // Clean up legacy envelope logs directory and related artifacts.
    // These are no longer used — all telemetry now routes through the daemon.
    if !options.dry_run {
        cleanup_legacy_envelope_logs();
    }

    let defer_install_probe =
        std::env::var("GIT_AI_DEFER_INSTALL_HOOKS_PROBE").as_deref() == Ok("1");

    let statuses = match statuses {
        Ok(statuses) => {
            if !options.dry_run && !defer_install_probe {
                crate::integration::install_test_upload::maybe_upload_install_success();
            }
            statuses
        }
        Err(error) => {
            if !options.dry_run && !defer_install_probe {
                crate::integration::install_test_upload::maybe_upload_install_failure(
                    "install-hooks",
                    error.to_string(),
                );
            }
            return Err(error);
        }
    };
    Ok(to_hashmap(statuses))
}

fn parse_install_options(args: &[String]) -> InstallOptions {
    let mut options = InstallOptions::default();

    for arg in args {
        match arg.as_str() {
            "--dry-run" | "--dry-run=true" => options.dry_run = true,
            "--verbose" | "-v" => options.verbose = true,
            "--skills" => options.install_skills = true,
            "--visual-studio-extension" => options.include_visual_studio_extension = true,
            _ => {}
        }
    }

    options
}

fn should_include_installer(id: &str, options: &InstallOptions) -> bool {
    options.include_visual_studio_extension || id != VISUAL_STUDIO_INSTALLER_ID
}

fn current_binary_path_for_install() -> PathBuf {
    match get_current_binary_path() {
        Ok(path) => path,
        Err(error) => {
            let fallback = std::env::current_exe().unwrap_or_else(|_| {
                PathBuf::from(if cfg!(windows) {
                    "git-ai.exe"
                } else {
                    "git-ai"
                })
            });
            eprintln!(
                "Warning: could not canonicalize git-ai binary path (non-fatal): {error}; using {}",
                fallback.display()
            );
            fallback
        }
    }
}

fn persist_install_config_best_effort(binary_path: &Path, dry_run: bool) -> bool {
    match persist_install_config(binary_path, dry_run) {
        Ok(changed) => changed,
        Err(error) => {
            eprintln!("Warning: could not persist install config (non-fatal): {error}");
            false
        }
    }
}

fn persist_install_config(binary_path: &Path, dry_run: bool) -> Result<bool, GitAiError> {
    if dry_run {
        return Ok(false);
    }

    let api_base = std::env::var("API_BASE").ok().filter(|s| !s.is_empty());
    let api_key = std::env::var("API_KEY").ok().filter(|s| !s.is_empty());

    if api_base.is_none() && api_key.is_none() {
        return Ok(false);
    }

    let mut file_config = crate::config::load_file_config_public().map_err(GitAiError::Generic)?;
    let mut changed = false;

    if let Some(ref api_base) = api_base
        && file_config.api_base_url.as_deref() != Some(api_base.as_str())
    {
        file_config.api_base_url = Some(api_base.clone());
        changed = true;
    }

    if let Some(ref api_key) = api_key
        && file_config.api_key.as_deref() != Some(api_key.as_str())
    {
        file_config.api_key = Some(api_key.clone());
        changed = true;
    }

    if api_base.is_some() {
        let git_path_missing = file_config
            .git_path
            .as_ref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true);
        if git_path_missing && let Some(git_path) = detect_install_git_path(binary_path) {
            file_config.git_path = Some(git_path);
            changed = true;
        }
    }

    if !changed {
        return Ok(false);
    }

    crate::config::save_file_config(&file_config).map_err(GitAiError::Generic)?;
    Ok(true)
}

fn detect_install_git_path(binary_path: &Path) -> Option<String> {
    let install_dir = binary_path.parent()?;

    #[cfg(windows)]
    {
        parse_git_og_cmd_path(&fs::read_to_string(install_dir.join("git-og.cmd")).ok()?)
    }

    #[cfg(not(windows))]
    {
        let target = fs::read_link(install_dir.join("git-og")).ok()?;
        let resolved = if target.is_absolute() {
            target
        } else {
            install_dir.join(target)
        };
        Some(resolved.to_string_lossy().to_string())
    }
}

#[cfg(windows)]
fn parse_git_og_cmd_path(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let start = line.find('"')?;
        let rest = &line[start + 1..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    })
}

/// Main entry point for uninstall-hooks command
pub fn run_uninstall(args: &[String]) -> Result<HashMap<String, String>, GitAiError> {
    // Parse flags
    let mut dry_run = false;
    let mut verbose = false;
    for arg in args {
        if arg == "--dry-run" || arg == "--dry-run=true" {
            dry_run = true;
        }
        if arg == "--verbose" || arg == "-v" {
            verbose = true;
        }
    }

    // Get absolute path to the current binary
    let binary_path = get_current_binary_path()?;
    let params = HookInstallerParams { binary_path };

    // Run async operations with smol and convert result
    let statuses = smol::block_on(async_run_uninstall(&params, dry_run, verbose))?;
    Ok(to_hashmap(statuses))
}

async fn async_run_install(
    params: &HookInstallerParams,
    options: &InstallOptions,
) -> Result<HashMap<String, InstallStatus>, GitAiError> {
    let mut any_checked = true;
    let mut has_changes = false;
    let mut statuses: HashMap<String, InstallStatus> = HashMap::new();
    // Track detailed results for metrics (tool_id, result)
    let mut detailed_results: Vec<(String, InstallResult)> = Vec::new();

    // === Git Client ===
    println!("\n\x1b[1mGit Client\x1b[0m");
    let spinner = Spinner::new("Global post-commit hook: checking");
    spinner.start();
    match install_managed_global_post_commit_hook(&params.binary_path, options.dry_run) {
        Ok(result) => {
            statuses.insert(GLOBAL_POST_COMMIT_HOOK_ID.to_string(), result.status);
            match result.status {
                InstallStatus::Installed => {
                    has_changes = true;
                    if options.dry_run {
                        spinner.pending("Global post-commit hook: Pending updates");
                    } else {
                        spinner.success("Global post-commit hook: Installed");
                    }
                    detailed_results.push((
                        GLOBAL_POST_COMMIT_HOOK_ID.to_string(),
                        InstallResult::installed(),
                    ));
                }
                InstallStatus::AlreadyInstalled => {
                    spinner.success("Global post-commit hook: Already installed");
                    detailed_results.push((
                        GLOBAL_POST_COMMIT_HOOK_ID.to_string(),
                        InstallResult::already_installed(),
                    ));
                }
                InstallStatus::Failed => {
                    let message = result
                        .message
                        .unwrap_or_else(|| "failed to install global post-commit hook".to_string());
                    spinner.error("Global post-commit hook: Failed");
                    eprintln!("  Warning: {}", message);
                    detailed_results.push((
                        GLOBAL_POST_COMMIT_HOOK_ID.to_string(),
                        InstallResult::failed(message),
                    ));
                }
                InstallStatus::NotFound => {
                    spinner.pending("Global post-commit hook: Not found");
                    detailed_results.push((
                        GLOBAL_POST_COMMIT_HOOK_ID.to_string(),
                        InstallResult::not_found(),
                    ));
                }
            }
        }
        Err(error) => {
            let message = error.to_string();
            spinner.error("Global post-commit hook: Failed");
            eprintln!("  Warning: {}", message);
            statuses.insert(
                GLOBAL_POST_COMMIT_HOOK_ID.to_string(),
                InstallStatus::Failed,
            );
            detailed_results.push((
                GLOBAL_POST_COMMIT_HOOK_ID.to_string(),
                InstallResult::failed(message),
            ));
        }
    }

    // === Coding Agents ===
    println!("\n\x1b[1mCoding Agents\x1b[0m");

    let installers = get_all_installers();
    let mut installed_tools: HashSet<String> = HashSet::new();
    // Track agents whose hooks were updated (name, process_names) for restart warnings
    let mut updated_agents: Vec<(String, Vec<String>)> = Vec::new();

    for installer in &installers {
        let name = installer.name();
        let id = installer.id();

        if !should_include_installer(id, options) {
            continue;
        }

        // Check if tool is installed and hooks status
        match installer.check_hooks(params) {
            Ok(check_result) => {
                if !check_result.tool_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    detailed_results.push((id.to_string(), InstallResult::not_found()));
                    continue;
                }

                installed_tools.insert(id.to_string());
                any_checked = true;

                // Install/update hooks (only for tools that use config file hooks)
                if installer.uses_config_hooks() {
                    let spinner = Spinner::new(&format!("{}: checking hooks", name));
                    spinner.start();

                    match installer.install_hooks(params, options.dry_run) {
                        Ok(Some(diff)) => {
                            if options.dry_run {
                                spinner.pending(&format!("{}: Pending updates", name));
                            } else {
                                spinner.success(&format!("{}: Hooks updated", name));
                                print_amp_plugins_note(id);
                            }
                            if options.verbose {
                                println!();
                                print_diff(&diff);
                            }
                            has_changes = true;
                            statuses.insert(id.to_string(), InstallStatus::Installed);
                            detailed_results.push((id.to_string(), InstallResult::installed()));

                            // Track this agent for restart detection (skip in dry-run)
                            if !options.dry_run {
                                let pnames: Vec<String> = installer
                                    .process_names()
                                    .iter()
                                    .map(|s| s.to_string())
                                    .collect();
                                if !pnames.is_empty() {
                                    updated_agents.push((name.to_string(), pnames));
                                }
                            }
                        }
                        Ok(None) => {
                            spinner.success(&format!("{}: Hooks already up to date", name));
                            print_amp_plugins_note(id);
                            statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                            detailed_results
                                .push((id.to_string(), InstallResult::already_installed()));
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            spinner.error(&format!("{}: Failed to update hooks", name));
                            eprintln!("  Error: {}", error_msg);
                            statuses.insert(id.to_string(), InstallStatus::NotFound);
                            detailed_results
                                .push((id.to_string(), InstallResult::failed(error_msg)));
                        }
                    }
                }

                // Install extras (extensions, git.path, etc.)
                match installer.install_extras(params, options.dry_run) {
                    Ok(results) => {
                        let mut extras_changed = false;
                        for result in results {
                            if result.changed {
                                has_changes = true;
                                extras_changed = true;
                            }
                            if result.changed && !options.dry_run {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.success(&result.message);
                            } else if result.changed && options.dry_run {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.pending(&result.message);
                            } else if result.message.contains("already") {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.success(&result.message);
                            } else if result.message.contains("Unable")
                                || result.message.contains("manually")
                            {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.pending(&result.message);
                            }
                            if options.verbose
                                && let Some(diff) = result.diff
                            {
                                println!();
                                print_diff(&diff);
                            }

                            // Capture warning-like messages for metrics
                            if (result.message.contains("Unable")
                                || result.message.contains("manually")
                                || result.message.contains("Failed"))
                                && let Some((_, detail)) = detailed_results
                                    .iter_mut()
                                    .find(|(tool_id, _)| tool_id == id)
                            {
                                detail.warnings.push(result.message.clone());
                            }
                        }

                        // Track restart detection for extras-only agents (e.g. JetBrains, VS Code)
                        if extras_changed
                            && !options.dry_run
                            && !updated_agents.iter().any(|(n, _)| n == name)
                        {
                            let pnames: Vec<String> = installer
                                .process_names()
                                .iter()
                                .map(|s| s.to_string())
                                .collect();
                            if !pnames.is_empty() {
                                updated_agents.push((name.to_string(), pnames));
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  Error installing extras for {}: {}", name, e);
                        // Capture extras error as a warning on the tool's result
                        if let Some((_, detail)) = detailed_results
                            .iter_mut()
                            .find(|(tool_id, _)| tool_id == id)
                        {
                            detail.warnings.push(format!("Extras install error: {}", e));
                        }
                    }
                }
            }
            Err(version_error) => {
                let error_msg = version_error.to_string();
                any_checked = true;
                let spinner = Spinner::new(&format!("{}: checking version", name));
                spinner.start();
                spinner.error(&format!("{}: Version check failed", name));
                eprintln!("  Error: {}", error_msg);
                eprintln!("  Please update {} to continue using git-ai hooks", name);
                statuses.insert(id.to_string(), InstallStatus::NotFound);
                detailed_results.push((id.to_string(), InstallResult::failed(error_msg)));
            }
        }
    }

    if options.install_skills {
        if let Ok(result) =
            skills_installer::install_skills(options.dry_run, options.verbose, &installed_tools)
            && result.changed
        {
            has_changes = true;
        }
    } else if let Ok(result) = skills_installer::uninstall_skills(options.dry_run, options.verbose)
        && result.changed
    {
        has_changes = true;
    }

    if !any_checked {
        println!("No compatible IDEs or agent configurations detected. Nothing to install.");
    } else if has_changes && options.dry_run {
        println!("\n\x1b[33m⚠ Dry-run mode (default). No changes were made.\x1b[0m");
        println!("To apply these changes, run:");
        println!("\x1b[1m  git-ai install-hooks --dry-run=false\x1b[0m");
    }

    // Check for running agents that had hooks updated and warn about restart
    if !options.dry_run && !updated_agents.is_empty() {
        let mut any_running = false;

        for (agent_name, pnames) in &updated_agents {
            let refs: Vec<&str> = pnames.iter().map(|s| s.as_str()).collect();
            let pids = find_running_pids(&refs);
            if !pids.is_empty() {
                if !any_running {
                    println!(
                        "\n\x1b[33m⚠ The following agents are currently running and must be restarted:\x1b[0m"
                    );
                    any_running = true;
                }
                let pid_list: Vec<String> = pids.iter().map(|(pid, _)| pid.to_string()).collect();
                println!(
                    "  \x1b[1m{}\x1b[0m (PID: {})",
                    agent_name,
                    pid_list.join(", ")
                );
            }
        }

        if any_running {
            println!();
            println!(
                "\x1b[33mRestart the agents listed above for git-ai attribution to take effect.\x1b[0m"
            );
            println!(
                "Any work done before installing git-ai (or before restarting) will be attributed as human."
            );
            println!(
                "This is expected — once you commit and start a fresh session, attribution will work correctly."
            );
            println!(
                "If the issue persists, please open an issue at https://github.com/git-ai-project/git-ai/issues"
            );
        }
    }

    // Emit metrics for each agent/git_client result (only if not dry-run)
    if !options.dry_run {
        emit_install_hooks_metrics(&detailed_results);
    }

    // Warn if git version is below the minimum required for full functionality
    warn_if_git_version_too_old();

    Ok(statuses)
}

/// Minimum git version required for git-ai to function correctly.
/// git 2.22.0 introduced `git worktree list --porcelain` output format improvements
/// and trace2 event logging used by git-ai for attribution.
const MIN_GIT_VERSION: (u32, u32, u32) = (2, 22, 0);

/// Parse a git version string like "git version 2.39.1" into (major, minor, patch).
fn parse_git_version(output: &str) -> Option<(u32, u32, u32)> {
    // Strip the "git version " prefix and any platform suffix (e.g. "(Apple Git-140)")
    let version_str = output.trim().strip_prefix("git version ")?;
    let version_str = version_str.split_whitespace().next()?;
    let mut parts = version_str.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Print a loud warning if the installed git version is older than MIN_GIT_VERSION.
fn warn_if_git_version_too_old() {
    let output = Command::new("git")
        .args(["--version"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let version = match output {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            parse_git_version(&text)
        }
        Err(_) => None,
    };

    if let Some(v) = version {
        let (maj, min, patch) = MIN_GIT_VERSION;
        if v < (maj, min, patch) {
            let (vmaj, vmin, vpatch) = v;
            eprintln!();
            eprintln!(
                "\x1b[1;31m╔══════════════════════════════════════════════════════════════╗\x1b[0m"
            );
            eprintln!(
                "\x1b[1;31m║  WARNING: git version too old — git-ai will not work         ║\x1b[0m"
            );
            eprintln!(
                "\x1b[1;31m╚══════════════════════════════════════════════════════════════╝\x1b[0m"
            );
            eprintln!(
                "\x1b[1;31mDetected git {}.{}.{} — git-ai requires git >= {}.{}.{}\x1b[0m",
                vmaj, vmin, vpatch, maj, min, patch
            );
            eprintln!("\x1b[33mPlease upgrade git before using git-ai:\x1b[0m");
            eprintln!("  macOS:   brew install git");
            eprintln!(
                "  Ubuntu:  sudo add-apt-repository ppa:git-core/ppa && sudo apt-get update && sudo apt-get install git"
            );
            eprintln!("  Windows: https://git-scm.com/download/win");
            eprintln!();
        }
    }
}

/// Emit metrics events for install-hooks results
fn emit_install_hooks_metrics(results: &[(String, InstallResult)]) {
    use crate::metrics::{EventAttributes, InstallHooksValues};

    let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"));

    for (tool_id, result) in results {
        let mut values = InstallHooksValues::new()
            .tool_id(tool_id.clone())
            .status(result.status.as_str().to_string());

        if let Some(msg) = result.message_for_metrics() {
            values = values.message(msg);
        } else {
            values = values.message_null();
        }

        crate::metrics::record(values, attrs.clone());
    }
}

async fn async_run_uninstall(
    params: &HookInstallerParams,
    dry_run: bool,
    verbose: bool,
) -> Result<HashMap<String, InstallStatus>, GitAiError> {
    let mut any_checked = false;
    let mut has_changes = false;
    let mut statuses: HashMap<String, InstallStatus> = HashMap::new();

    // Uninstall skills first (these are global, not per-agent, silently)
    if let Ok(result) = skills_installer::uninstall_skills(dry_run, verbose) {
        if result.changed {
            has_changes = true;
            statuses.insert("skills".to_string(), InstallStatus::Installed);
        } else {
            statuses.insert("skills".to_string(), InstallStatus::AlreadyInstalled);
        }
    }

    // === Coding Agents ===
    println!("\n\x1b[1mCoding Agents\x1b[0m");

    let installers = get_all_installers();

    for installer in installers {
        let name = installer.name();
        let id = installer.id();

        // Check if tool is installed
        match installer.check_hooks(params) {
            Ok(check_result) => {
                if !check_result.tool_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    continue;
                }

                if !check_result.hooks_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    continue;
                }

                any_checked = true;

                // Uninstall hooks
                let spinner = Spinner::new(&format!("{}: removing hooks", name));
                spinner.start();

                match installer.uninstall_hooks(params, dry_run) {
                    Ok(Some(diff)) => {
                        if dry_run {
                            spinner.pending(&format!("{}: Pending removal", name));
                        } else {
                            spinner.success(&format!("{}: Hooks removed", name));
                        }
                        if verbose {
                            println!();
                            print_diff(&diff);
                        }
                        has_changes = true;
                        statuses.insert(id.to_string(), InstallStatus::Installed);
                    }
                    Ok(None) => {
                        spinner.success(&format!("{}: No hooks to remove", name));
                        statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                    }
                    Err(e) => {
                        spinner.error(&format!("{}: Failed to remove hooks", name));
                        eprintln!("  Error: {}", e);
                        statuses.insert(id.to_string(), InstallStatus::NotFound);
                    }
                }

                // Uninstall extras
                match installer.uninstall_extras(params, dry_run) {
                    Ok(results) => {
                        for result in results {
                            if result.changed {
                                has_changes = true;
                            }
                            if !result.message.is_empty() {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                if result.changed {
                                    extra_spinner.success(&result.message);
                                } else {
                                    extra_spinner.pending(&result.message);
                                }
                            }
                            if verbose && let Some(diff) = result.diff {
                                println!();
                                print_diff(&diff);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  Error uninstalling extras for {}: {}", name, e);
                    }
                }
            }
            Err(e) => {
                eprintln!("  Error checking {}: {}", name, e);
                statuses.insert(id.to_string(), InstallStatus::NotFound);
            }
        }
    }

    if !any_checked {
        println!("No git-ai hooks found to uninstall.");
    } else if has_changes && dry_run {
        println!("\n\x1b[33m⚠ Dry-run mode (default). No changes were made.\x1b[0m");
        println!("To apply these changes, run:");
        println!("\x1b[1m  git-ai uninstall-hooks --dry-run=false\x1b[0m");
    } else if !has_changes {
        println!("All git-ai hooks have been removed.");
    }

    Ok(statuses)
}

/// Remove the legacy envelope logs directory and related lock/marker files.
///
/// All telemetry now flows through the daemon control socket, so the per-PID
/// log file system under `~/.git-ai/internal/logs/` is no longer needed.
fn cleanup_legacy_envelope_logs() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let internal = home.join(".git-ai").join("internal");

    // Remove the entire logs directory
    let logs_dir = internal.join("logs");
    if logs_dir.is_dir() {
        let _ = fs::remove_dir_all(&logs_dir);
    }

    // Remove the flush-logs lock file
    let _ = fs::remove_file(internal.join("flush-logs.lock"));

    // Remove the debounce marker file
    let _ = fs::remove_file(internal.join("last_flush_trigger_ts"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::PathBuf;
    use tempfile::tempdir;

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: tests marked `serial` avoid concurrent env mutation.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: tests marked `serial` avoid concurrent env mutation.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: tests marked `serial` avoid concurrent env mutation.
            unsafe {
                if let Some(old) = &self.old {
                    std::env::set_var(self.key, old);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn test_binary_path(install_dir: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            install_dir.join("git-ai.exe")
        }

        #[cfg(not(windows))]
        {
            install_dir.join("git-ai")
        }
    }

    fn write_install_git_marker(install_dir: &Path, git_path: &str) {
        #[cfg(windows)]
        {
            fs::write(
                install_dir.join("git-og.cmd"),
                format!("@echo off\r\n\"{}\" %*\r\n", git_path),
            )
            .unwrap();
        }

        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(git_path, install_dir.join("git-og")).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_git_proxy_entrypoint_refreshes_existing_stale_proxy() {
        let temp = tempdir().unwrap();
        let git_ai_exe = temp.path().join("git-ai.exe");
        let git_proxy = temp.path().join("git.exe");
        fs::write(&git_ai_exe, b"new runtime").unwrap();
        fs::write(&git_proxy, b"old runtime").unwrap();

        sync_windows_git_proxy_entrypoint(&git_ai_exe, &git_proxy, true).unwrap();

        assert_eq!(fs::read(&git_proxy).unwrap(), b"new runtime");
    }

    #[cfg(windows)]
    #[test]
    fn windows_git_proxy_entrypoint_leaves_existing_proxy_when_not_refreshing() {
        let temp = tempdir().unwrap();
        let git_ai_exe = temp.path().join("git-ai.exe");
        let git_proxy = temp.path().join("git.exe");
        fs::write(&git_ai_exe, b"new runtime").unwrap();
        fs::write(&git_proxy, b"old runtime").unwrap();

        sync_windows_git_proxy_entrypoint(&git_ai_exe, &git_proxy, false).unwrap();

        assert_eq!(fs::read(&git_proxy).unwrap(), b"old runtime");
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_hooks_from_bin_refreshes_stale_launcher_git_proxy() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".git-ai");
        let launcher = root.join("launcher");
        let bin = root.join("bin");
        fs::create_dir_all(&launcher).unwrap();
        fs::create_dir_all(&bin).unwrap();

        let launcher_git_ai = launcher.join("git-ai.exe");
        let launcher_git = launcher.join("git.exe");
        let bin_git_ai = bin.join("git-ai.exe");
        let bin_git = bin.join("git.exe");

        fs::write(&launcher_git_ai, b"new launcher runtime").unwrap();
        fs::write(&launcher_git, b"old launcher proxy").unwrap();
        fs::write(&bin_git_ai, b"old bin runtime").unwrap();
        fs::write(&bin_git, b"old bin proxy").unwrap();

        repair_git_proxy_entrypoint_for_current_exe(&bin_git_ai).unwrap();

        assert_eq!(fs::read(&launcher_git).unwrap(), b"new launcher runtime");
        assert_eq!(fs::read(&bin_git).unwrap(), b"new launcher runtime");
        assert_eq!(fs::read(&launcher_git_ai).unwrap(), b"new launcher runtime");
        assert_eq!(
            fs::read(&bin_git_ai).unwrap(),
            b"old bin runtime",
            "the currently running compatibility git-ai.exe must not be overwritten by install-hooks"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_hooks_from_launcher_git_refreshes_bin_without_overwriting_running_proxy() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".git-ai");
        let launcher = root.join("launcher");
        let bin = root.join("bin");
        fs::create_dir_all(&launcher).unwrap();
        fs::create_dir_all(&bin).unwrap();

        let launcher_git_ai = launcher.join("git-ai.exe");
        let launcher_git = launcher.join("git.exe");
        let bin_git_ai = bin.join("git-ai.exe");
        let bin_git = bin.join("git.exe");

        fs::write(&launcher_git_ai, b"new launcher runtime").unwrap();
        fs::write(&launcher_git, b"old running proxy").unwrap();
        fs::write(&bin_git_ai, b"old bin runtime").unwrap();
        fs::write(&bin_git, b"old bin proxy").unwrap();

        repair_git_proxy_entrypoint_for_current_exe(&launcher_git).unwrap();

        assert_eq!(
            fs::read(&launcher_git).unwrap(),
            b"old running proxy",
            "install-hooks must not overwrite the git.exe image that is currently executing"
        );
        assert_eq!(fs::read(&bin_git_ai).unwrap(), b"new launcher runtime");
        assert_eq!(fs::read(&bin_git).unwrap(), b"new launcher runtime");
    }

    #[test]
    fn parse_install_options_defaults_visual_studio_extension_to_disabled() {
        let options = parse_install_options(&[]);

        assert!(!options.include_visual_studio_extension);
        assert!(!should_include_installer(
            VISUAL_STUDIO_INSTALLER_ID,
            &options
        ));
        assert!(should_include_installer("vscode", &options));
    }

    #[test]
    fn parse_install_options_enables_visual_studio_extension_flag() {
        let args = vec![
            "--dry-run".to_string(),
            "--visual-studio-extension".to_string(),
            "--skills".to_string(),
            "-v".to_string(),
        ];
        let options = parse_install_options(&args);

        assert!(options.dry_run);
        assert!(options.verbose);
        assert!(options.install_skills);
        assert!(options.include_visual_studio_extension);
        assert!(should_include_installer(
            VISUAL_STUDIO_INSTALLER_ID,
            &options
        ));
    }

    #[test]
    #[serial]
    fn cleanup_daemon_trace2_removes_existing_trace2_section() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join(".gitconfig");
        fs::write(
            &config_path,
            "[user]\n\tname = Test User\n[trace2]\n\teventTarget = old-target\n\tnormalTarget = old-normal\n",
        )
        .unwrap();

        let _global_config = EnvVarGuard::set("GIT_CONFIG_GLOBAL", config_path.to_str().unwrap());

        cleanup_daemon_trace2(false).unwrap();

        let cfg = load_global_git_config(&config_path).unwrap();
        assert_eq!(
            cfg.string("user.name").map(|value| value.to_string()),
            Some("Test User".to_string())
        );
        assert!(cfg.string("trace2.eventTarget").is_none());
        assert!(cfg.string("trace2.normalTarget").is_none());
    }

    #[test]
    #[serial]
    fn repair_stale_global_hooks_path_removes_missing_absolute_path() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join(".gitconfig");
        let missing_hooks = temp.path().join("missing-hooks");
        fs::write(
            &config_path,
            format!(
                "[core]\n\thooksPath = {}\n[user]\n\tname = Test User\n",
                missing_hooks.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();

        let _global_config = EnvVarGuard::set("GIT_CONFIG_GLOBAL", config_path.to_str().unwrap());

        let removed = repair_stale_global_hooks_path(false).unwrap();

        assert_eq!(
            removed.as_deref(),
            Some(missing_hooks.to_string_lossy().replace('\\', "/").as_str())
        );
        let cfg = load_global_git_config(&config_path).unwrap();
        assert!(cfg.string(CORE_HOOKS_PATH_KEY).is_none());
        assert_eq!(
            cfg.string("user.name").map(|value| value.to_string()),
            Some("Test User".to_string())
        );
    }

    #[test]
    #[serial]
    fn repair_stale_global_hooks_path_preserves_existing_path() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join(".gitconfig");
        let hooks_dir = temp.path().join("hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(
            &config_path,
            format!(
                "[core]\n\thooksPath = {}\n",
                hooks_dir.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();

        let _global_config = EnvVarGuard::set("GIT_CONFIG_GLOBAL", config_path.to_str().unwrap());

        let removed = repair_stale_global_hooks_path(false).unwrap();

        assert!(removed.is_none());
        let cfg = load_global_git_config(&config_path).unwrap();
        assert_eq!(
            cfg.string(CORE_HOOKS_PATH_KEY)
                .map(|value| value.to_string()),
            Some(hooks_dir.to_string_lossy().replace('\\', "/"))
        );
    }

    #[test]
    #[serial]
    fn install_managed_global_post_commit_hook_sets_hooks_path_when_unset() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join(".gitconfig");
        let git_ai_exe = test_binary_path(&temp.path().join("launcher"));
        fs::create_dir_all(git_ai_exe.parent().unwrap()).unwrap();
        fs::write(&git_ai_exe, b"runtime").unwrap();

        let _global_config = EnvVarGuard::set("GIT_CONFIG_GLOBAL", config_path.to_str().unwrap());
        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());

        let result = install_managed_global_post_commit_hook(&git_ai_exe, false).unwrap();

        assert_eq!(result.status, InstallStatus::Installed);
        let cfg = load_global_git_config(&config_path).unwrap();
        assert_eq!(
            cfg.string(CORE_HOOKS_PATH_KEY)
                .map(|value| value.to_string()),
            Some(managed_global_hooks_path_value())
        );

        let hook = managed_global_hooks_dir().join("post-commit");
        let content = fs::read_to_string(hook).unwrap();
        assert!(content.contains(MANAGED_GLOBAL_HOOK_MARKER));
        assert!(content.contains("GITAI_SKIP_MANAGED_HOOKS"));
        assert!(content.contains("GIT_AI_WRAPPER_INVOCATION_ID"));
        assert!(content.contains("repair-authorship-note HEAD --write"));
        assert!(
            content.contains("upload-stats HEAD --source \"$SOURCE\" --skip-if-already-uploaded")
        );
        assert!(content.contains(") >/dev/null 2>&1 &"));
        assert!(content.contains("GIT_AI_SKIP_ALL_HOOKS=1"));
        assert!(content.contains("GIT_AI_POST_COMMIT_FALLBACK_UPLOAD_SPAWNED=1"));
    }

    #[test]
    #[serial]
    fn install_managed_global_post_commit_hook_is_idempotent() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join(".gitconfig");
        let git_ai_exe = test_binary_path(&temp.path().join("launcher"));
        fs::create_dir_all(git_ai_exe.parent().unwrap()).unwrap();
        fs::write(&git_ai_exe, b"runtime").unwrap();

        let _global_config = EnvVarGuard::set("GIT_CONFIG_GLOBAL", config_path.to_str().unwrap());
        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());

        install_managed_global_post_commit_hook(&git_ai_exe, false).unwrap();
        let result = install_managed_global_post_commit_hook(&git_ai_exe, false).unwrap();

        assert_eq!(result.status, InstallStatus::AlreadyInstalled);
    }

    #[test]
    #[serial]
    fn install_managed_global_post_commit_hook_preserves_foreign_post_commit() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join(".gitconfig");
        let hooks_dir = temp.path().join("foreign-hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("post-commit"), "#!/bin/sh\necho foreign\n").unwrap();
        fs::write(
            &config_path,
            format!(
                "[core]\n\thooksPath = {}\n",
                hooks_dir.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        let git_ai_exe = test_binary_path(&temp.path().join("launcher"));
        fs::create_dir_all(git_ai_exe.parent().unwrap()).unwrap();
        fs::write(&git_ai_exe, b"runtime").unwrap();

        let _global_config = EnvVarGuard::set("GIT_CONFIG_GLOBAL", config_path.to_str().unwrap());
        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());

        let result = install_managed_global_post_commit_hook(&git_ai_exe, false).unwrap();

        assert_eq!(result.status, InstallStatus::Failed);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("non-git-ai post-commit hook")
        );
        assert_eq!(
            fs::read_to_string(hooks_dir.join("post-commit")).unwrap(),
            "#!/bin/sh\necho foreign\n"
        );
        let cfg = load_global_git_config(&config_path).unwrap();
        assert_eq!(
            cfg.string(CORE_HOOKS_PATH_KEY)
                .map(|value| value.to_string()),
            Some(hooks_dir.to_string_lossy().replace('\\', "/"))
        );
    }

    #[test]
    fn managed_global_post_commit_hook_skips_wrapper_invocations() {
        let content = managed_global_post_commit_hook_content(Path::new("C:/git-ai/git-ai.exe"));

        assert!(content.contains("GITAI_SKIP_MANAGED_HOOKS"));
        assert!(content.contains("GIT_AI_WRAPPER_INVOCATION_ID"));
        assert!(
            content.find("GITAI_SKIP_MANAGED_HOOKS").unwrap()
                < content.find("repair-authorship-note HEAD --write").unwrap()
        );
        assert!(
            content.find("GIT_AI_WRAPPER_INVOCATION_ID").unwrap()
                < content
                    .find("upload-stats HEAD --source \"$SOURCE\" --skip-if-already-uploaded")
                    .unwrap()
        );
    }

    #[test]
    #[serial]
    fn persist_install_config_updates_api_base_and_backfills_git_path() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let expected_git_path = if cfg!(windows) {
            r"C:\Program Files\Git\bin\git.exe"
        } else {
            "/opt/custom/bin/git"
        };
        write_install_git_marker(&install_dir, expected_git_path);

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");
        let _api_key = EnvVarGuard::remove("API_KEY");

        let changed = persist_install_config(&test_binary_path(&install_dir), false).unwrap();

        assert!(changed);

        let config = crate::config::load_file_config_public().unwrap();
        assert_eq!(
            config.api_base_url.as_deref(),
            Some("https://enterprise.example")
        );
        assert_eq!(config.git_path.as_deref(), Some(expected_git_path));
        assert_eq!(config.api_key, None);
    }

    #[test]
    #[serial]
    fn persist_install_config_preserves_existing_git_path() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();
        write_install_git_marker(
            &install_dir,
            if cfg!(windows) {
                r"C:\Program Files\Git\bin\git.exe"
            } else {
                "/opt/custom/bin/git"
            },
        );

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");
        let _api_key = EnvVarGuard::remove("API_KEY");

        let existing_git_path = if cfg!(windows) {
            r"D:\PortableGit\bin\git.exe"
        } else {
            "/usr/local/bin/git"
        };
        crate::config::save_file_config(&crate::config::FileConfig {
            git_path: Some(existing_git_path.to_string()),
            ..Default::default()
        })
        .unwrap();

        persist_install_config(&test_binary_path(&install_dir), false).unwrap();

        let config = crate::config::load_file_config_public().unwrap();
        assert_eq!(
            config.api_base_url.as_deref(),
            Some("https://enterprise.example")
        );
        assert_eq!(config.git_path.as_deref(), Some(existing_git_path));
    }

    #[test]
    #[serial]
    fn persist_install_config_skips_without_env_or_in_dry_run() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::remove("API_BASE");
        let _api_key = EnvVarGuard::remove("API_KEY");

        let changed = persist_install_config(&test_binary_path(&install_dir), false).unwrap();
        assert!(!changed);
        assert!(!temp.path().join(".git-ai").join("config.json").exists());

        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");
        let changed = persist_install_config(&test_binary_path(&install_dir), true).unwrap();
        assert!(!changed);
        assert!(!temp.path().join(".git-ai").join("config.json").exists());
    }

    #[test]
    #[serial]
    fn persist_install_config_persists_api_key() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::remove("API_BASE");
        let _api_key = EnvVarGuard::set("API_KEY", "sk-enterprise-key-12345");

        let changed = persist_install_config(&test_binary_path(&install_dir), false).unwrap();

        assert!(changed);

        let config = crate::config::load_file_config_public().unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-enterprise-key-12345"));
        assert_eq!(config.api_base_url, None);
    }

    #[test]
    #[serial]
    fn persist_install_config_persists_both_api_base_and_api_key() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");
        let _api_key = EnvVarGuard::set("API_KEY", "sk-enterprise-key-12345");

        let changed = persist_install_config(&test_binary_path(&install_dir), false).unwrap();

        assert!(changed);

        let config = crate::config::load_file_config_public().unwrap();
        assert_eq!(
            config.api_base_url.as_deref(),
            Some("https://enterprise.example")
        );
        assert_eq!(config.api_key.as_deref(), Some("sk-enterprise-key-12345"));
    }

    #[test]
    #[serial]
    fn persist_install_config_best_effort_ignores_invalid_existing_config() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let config_dir = temp.path().join(".git-ai");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("config.json"), b"{not valid json").unwrap();

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");
        let _api_key = EnvVarGuard::remove("API_KEY");

        assert!(
            persist_install_config(&test_binary_path(&install_dir), false).is_err(),
            "invalid existing config should still reproduce the underlying persistence failure"
        );
        assert!(
            !persist_install_config_best_effort(&test_binary_path(&install_dir), false),
            "install-hooks should ignore non-critical config persistence failures"
        );
    }

    #[cfg(windows)]
    #[test]
    fn parse_git_og_cmd_path_extracts_wrapped_git_path() {
        assert_eq!(
            parse_git_og_cmd_path("@echo off\r\n\"C:\\Program Files\\Git\\bin\\git.exe\" %*\r\n"),
            Some("C:\\Program Files\\Git\\bin\\git.exe".to_string())
        );
    }

    #[test]
    fn parse_git_version_standard() {
        assert_eq!(parse_git_version("git version 2.39.1"), Some((2, 39, 1)));
    }

    #[test]
    fn parse_git_version_apple_suffix() {
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39, 3))
        );
    }

    #[test]
    fn parse_git_version_no_patch() {
        assert_eq!(parse_git_version("git version 2.22"), Some((2, 22, 0)));
    }

    #[test]
    fn parse_git_version_old() {
        assert_eq!(parse_git_version("git version 2.17.1"), Some((2, 17, 1)));
        assert!(parse_git_version("git version 2.17.1").unwrap() < MIN_GIT_VERSION);
    }

    #[test]
    fn parse_git_version_at_minimum() {
        assert_eq!(parse_git_version("git version 2.22.0"), Some((2, 22, 0)));
        assert!(parse_git_version("git version 2.22.0").unwrap() >= MIN_GIT_VERSION);
    }

    #[test]
    fn parse_git_version_invalid() {
        assert_eq!(parse_git_version("not a git version"), None);
        assert_eq!(parse_git_version(""), None);
    }
}
