use crate::api::client::ApiContext;
use crate::config::{self, UpdateChannel};
use crate::observability::log_message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(windows)]
type WindowsHandle = *mut std::ffi::c_void;
#[cfg(windows)]
const TH32CS_SNAPPROCESS: u32 = 0x00000002;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: WindowsHandle = (-1isize) as WindowsHandle;
#[cfg(windows)]
const WINDOWS_MAX_PATH: usize = 260;

#[cfg(windows)]
#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; WINDOWS_MAX_PATH],
}

#[cfg(windows)]
unsafe extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> WindowsHandle;
    fn Process32FirstW(snapshot: WindowsHandle, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: WindowsHandle, entry: *mut ProcessEntry32W) -> i32;
    fn CloseHandle(handle: WindowsHandle) -> i32;
}

const UPDATE_CHECK_INTERVAL_HOURS: u64 = 24;
const GIT_AI_RELEASE_ENV: &str = "GIT_AI_RELEASE_TAG";
const GIT_AI_GITHUB_REPO_ENV: &str = "GIT_AI_GITHUB_REPO";
const GIT_AI_INSTALLER_URL_ENV: &str = "GIT_AI_INSTALLER_URL";
const DEFAULT_GITHUB_RELEASE_REPO: &str = "rj-gaoang/git-ai";
const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const RAW_GITHUB_CONTENT_BASE_URL: &str = "https://raw.githubusercontent.com";
#[cfg(windows)]
const GIT_AI_RESTART_DAEMON_AFTER_INSTALL_ENV: &str = "GIT_AI_RESTART_DAEMON_AFTER_INSTALL";
const BACKGROUND_SPAWN_THROTTLE_SECS: u64 = 60;
const ENV_BACKGROUND_UPGRADE_WORKER: &str = "GIT_AI_BACKGROUND_UPGRADE_WORKER";
const ENV_BACKGROUND_UPGRADE_DELAY_SECS: &str = "GIT_AI_BACKGROUND_UPGRADE_DELAY_SECS";
const AFTER_COMMIT_BACKGROUND_UPGRADE_DELAY_SECS: &str = "30";
const UPLOAD_ACTIVITY_WAIT_SECS: u64 = 300;

#[cfg(windows)]
const INSTALL_SCRIPT_NAME: &str = "install.ps1";
#[cfg(not(windows))]
const INSTALL_SCRIPT_NAME: &str = "install.sh";

static UPDATE_NOTICE_EMITTED: AtomicBool = AtomicBool::new(false);
static LAST_BACKGROUND_SPAWN: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateCheckPolicy {
    Default,
    AfterCommit,
}

#[derive(Debug, PartialEq)]
enum UpgradeAction {
    UpgradeAvailable,
    AlreadyLatest,
    RunningNewerVersion,
    ForceReinstall,
}

impl UpgradeAction {
    fn to_string(&self) -> &str {
        match self {
            UpgradeAction::UpgradeAvailable => "upgrade_available",
            UpgradeAction::AlreadyLatest => "already_latest",
            UpgradeAction::RunningNewerVersion => "running_newer_version",
            UpgradeAction::ForceReinstall => "force_reinstall",
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelRelease {
    tag: String,
    semver: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCache {
    last_checked_at: u64,
    available_tag: Option<String>,
    available_semver: Option<String>,
    channel: String,
}

impl UpdateCache {
    fn new(channel: UpdateChannel) -> Self {
        Self {
            last_checked_at: 0,
            available_tag: None,
            available_semver: None,
            channel: channel.as_str().to_string(),
        }
    }

    fn update_available(&self) -> bool {
        self.available_semver.is_some()
    }

    fn matches_channel(&self, channel: UpdateChannel) -> bool {
        self.channel == channel.as_str()
    }
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct ChannelInfo {
    version: String,
    checksum: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct ReleasesResponse {
    channels: HashMap<String, ChannelInfo>,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseResponse {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

fn get_update_check_cache_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Ok(test_cache_dir) = std::env::var("GIT_AI_TEST_CACHE_DIR") {
            return Some(PathBuf::from(test_cache_dir).join("update_check"));
        }
    }

    crate::config::update_check_path()
}

fn read_update_cache() -> Option<UpdateCache> {
    let path = get_update_check_cache_path()?;
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_update_cache(cache: &UpdateCache) {
    if let Some(path) = get_update_check_cache_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec(cache) {
            let _ = fs::write(path, json);
        }
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

#[cfg(windows)]
fn exit_if_invoked_via_git_extension() {
    if should_block_git_extension_upgrade(
        parent_process_name().as_deref(),
        std::env::var(ENV_BACKGROUND_UPGRADE_WORKER).as_deref() == Ok("1"),
    ) {
        eprintln!(
            "error: `git ai upgrade` is not supported on Windows. Run `git-ai upgrade` instead."
        );
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn should_block_git_extension_upgrade(
    parent_process_name: Option<&str>,
    is_background_worker: bool,
) -> bool {
    !is_background_worker && parent_process_name.is_some_and(is_git_process_name)
}

#[cfg(windows)]
fn is_git_process_name(name: &str) -> bool {
    std::path::Path::new(name)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| {
            file_name.eq_ignore_ascii_case("git") || file_name.eq_ignore_ascii_case("git.exe")
        })
}

#[cfg(windows)]
fn parent_process_name() -> Option<String> {
    struct SnapshotGuard(WindowsHandle);

    impl Drop for SnapshotGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    let _snapshot_guard = SnapshotGuard(snapshot);

    let current_pid = std::process::id();
    let parent_pid = find_parent_pid(snapshot, current_pid)?;
    process_name_for_pid(snapshot, parent_pid)
}

#[cfg(windows)]
fn find_parent_pid(snapshot: WindowsHandle, current_pid: u32) -> Option<u32> {
    let mut entry = windows_process_entry_template();
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        return None;
    }

    loop {
        if entry.th32_process_id == current_pid {
            return Some(entry.th32_parent_process_id);
        }
        if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
            return None;
        }
    }
}

#[cfg(windows)]
fn process_name_for_pid(snapshot: WindowsHandle, pid: u32) -> Option<String> {
    let mut entry = windows_process_entry_template();
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        return None;
    }

    loop {
        if entry.th32_process_id == pid {
            let len = entry
                .sz_exe_file
                .iter()
                .position(|&ch| ch == 0)
                .unwrap_or(entry.sz_exe_file.len());
            return Some(String::from_utf16_lossy(&entry.sz_exe_file[..len]));
        }
        if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
            return None;
        }
    }
}

#[cfg(windows)]
fn windows_process_entry_template() -> ProcessEntry32W {
    ProcessEntry32W {
        dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
        cnt_usage: 0,
        th32_process_id: 0,
        th32_default_heap_id: 0,
        th32_module_id: 0,
        cnt_threads: 0,
        th32_parent_process_id: 0,
        pc_pri_class_base: 0,
        dw_flags: 0,
        sz_exe_file: [0; WINDOWS_MAX_PATH],
    }
}

fn should_check_for_updates(
    channel: UpdateChannel,
    cache: Option<&UpdateCache>,
    policy: UpdateCheckPolicy,
    auto_updates_disabled: bool,
) -> bool {
    if policy == UpdateCheckPolicy::AfterCommit && !auto_updates_disabled {
        return true;
    }

    let now = current_timestamp();
    match cache {
        Some(cache) if cache.last_checked_at > 0 => {
            // If cache doesn't match the channel, we should check for updates
            if !cache.matches_channel(channel) {
                return true;
            }
            let elapsed = now.saturating_sub(cache.last_checked_at);
            elapsed > UPDATE_CHECK_INTERVAL_HOURS * 3600
        }
        _ => true,
    }
}

fn semver_from_tag(tag: &str) -> String {
    let trimmed = tag
        .trim()
        .trim_start_matches("enterprise-")
        .trim_start_matches('v');
    trimmed.split(['-', '+']).next().unwrap_or("").to_string()
}

fn determine_action(force: bool, release: &ChannelRelease, current_version: &str) -> UpgradeAction {
    if force {
        return UpgradeAction::ForceReinstall;
    }

    if release.semver == current_version {
        UpgradeAction::AlreadyLatest
    } else if is_newer_version(&release.semver, current_version) {
        UpgradeAction::UpgradeAvailable
    } else {
        UpgradeAction::RunningNewerVersion
    }
}

fn persist_update_state(channel: UpdateChannel, release: Option<&ChannelRelease>) {
    let mut cache = UpdateCache::new(channel);
    cache.last_checked_at = current_timestamp();
    if let Some(release) = release {
        cache.available_tag = Some(release.tag.clone());
        cache.available_semver = Some(release.semver.clone());
    }
    write_update_cache(&cache);
}

pub(crate) fn clear_cached_update_state() {
    let channel = config::Config::fresh().update_channel();
    persist_update_state(channel, None);
}

#[allow(dead_code)]
fn releases_endpoint() -> &'static str {
    "/worker/releases"
}

fn configured_github_repo() -> String {
    std::env::var(GIT_AI_GITHUB_REPO_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_GITHUB_RELEASE_REPO.to_string())
}

fn release_installer_url(repo: &str, release_tag: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/{}/{}",
        repo, release_tag, INSTALL_SCRIPT_NAME
    )
}

fn raw_main_installer_url(repo: &str) -> String {
    format!(
        "{}/{}/main/{}",
        RAW_GITHUB_CONTENT_BASE_URL, repo, INSTALL_SCRIPT_NAME
    )
}

fn configured_installer_url(repo: &str, release_tag: &str) -> String {
    std::env::var(GIT_AI_INSTALLER_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| release_installer_url(repo, release_tag))
}

fn release_source_url(api_base_url: &str, repo: &str) -> String {
    if api_base_url.starts_with("mock://") {
        api_base_url.to_string()
    } else {
        format!("{}/repos/{}/releases", GITHUB_API_BASE_URL, repo)
    }
}

fn fetch_text(url: &str, label: &str) -> Result<String, String> {
    let (_agent, request) = ApiContext::http_get(url, Some(30));
    let response = crate::http::send(request)
        .map_err(|e| format!("Failed to fetch {}: {}", label, e))?;

    if response.status_code != 200 {
        return Err(format!(
            "Failed to fetch {}: HTTP {}",
            label, response.status_code
        ));
    }

    response
        .as_str()
        .map(|body| body.to_string())
        .map_err(|e| format!("{} is not valid UTF-8: {}", label, e))
}

fn release_from_github_release(release: &GitHubReleaseResponse) -> Result<ChannelRelease, String> {
    if release.draft {
        return Err("GitHub release is a draft".to_string());
    }

    let tag = release.tag_name.trim().to_string();
    if tag.is_empty() {
        return Err("Release tag not found in GitHub response".to_string());
    }

    let semver = semver_from_tag(&tag);
    if semver.is_empty() {
        return Err(format!("Unable to parse semver from tag '{}'", tag));
    }

    Ok(ChannelRelease {
        tag,
        semver,
    })
}

fn fetch_github_release_list(repo: &str) -> Result<Vec<GitHubReleaseResponse>, String> {
    let url = format!("{}/repos/{}/releases?per_page=100", GITHUB_API_BASE_URL, repo);
    let body = fetch_text(&url, "GitHub release list")?;
    serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse GitHub release list response: {}", e))
}

fn select_newest_github_release(
    releases: &[GitHubReleaseResponse],
    include_prerelease: bool,
) -> Result<ChannelRelease, String> {
    let mut newest_release: Option<ChannelRelease> = None;
    let mut parse_error: Option<String> = None;

    for release in releases {
        if release.draft || release.prerelease != include_prerelease {
            continue;
        }

        match release_from_github_release(release) {
            Ok(candidate) => {
                let should_replace = match newest_release.as_ref() {
                    Some(current) => is_newer_version(&candidate.semver, &current.semver),
                    None => true,
                };

                if should_replace {
                    newest_release = Some(candidate);
                }
            }
            Err(err) => {
                if parse_error.is_none() {
                    parse_error = Some(err);
                }
            }
        }
    }

    newest_release.ok_or_else(|| {
        parse_error.unwrap_or_else(|| {
            if include_prerelease {
                "No GitHub prerelease found".to_string()
            } else {
                "No GitHub stable release found".to_string()
            }
        })
    })
}

fn fetch_latest_github_release(repo: &str) -> Result<ChannelRelease, String> {
    // Prefer the releases list over GitHub's /latest pointer because the pointer can lag
    // behind a freshly published stable release and block commit-triggered auto-updates.
    let releases = fetch_github_release_list(repo)?;
    select_newest_github_release(&releases, false)
}

fn fetch_next_github_release(repo: &str) -> Result<ChannelRelease, String> {
    let releases = fetch_github_release_list(repo)?;
    select_newest_github_release(&releases, true)
        .or_else(|_| select_newest_github_release(&releases, false))
}

fn fetch_install_script(repo: &str, release_tag: &str) -> Result<String, String> {
    let url = configured_installer_url(repo, release_tag);
    match fetch_text(&url, INSTALL_SCRIPT_NAME) {
        Ok(script) => Ok(script),
        Err(primary_error) => {
            let has_override = std::env::var(GIT_AI_INSTALLER_URL_ENV)
                .ok()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);

            if has_override {
                Err(primary_error)
            } else {
                let fallback_url = raw_main_installer_url(repo);
                fetch_text(&fallback_url, INSTALL_SCRIPT_NAME).map_err(|fallback_error| {
                    format!(
                        "Failed to fetch installer from release asset {}: {}; fallback {} also failed: {}",
                        url, primary_error, fallback_url, fallback_error
                    )
                })
            }
        }
    }
}

fn maybe_wait_before_background_upgrade() {
    let delay_secs = std::env::var(ENV_BACKGROUND_UPGRADE_DELAY_SECS)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0);

    if let Some(delay_secs) = delay_secs {
        std::thread::sleep(Duration::from_secs(delay_secs));
    }

    let _ = crate::integration::upload_stats::wait_for_upload_activity_to_finish(
        Duration::from_secs(UPLOAD_ACTIVITY_WAIT_SECS),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InstallInvocationMode {
    skip_install: bool,
    silent_install: bool,
}

fn install_invocation_mode(background: bool, auto_updates_disabled: bool) -> InstallInvocationMode {
    InstallInvocationMode {
        skip_install: background && auto_updates_disabled,
        silent_install: background,
    }
}

#[cfg(windows)]
fn should_request_daemon_restart_after_update(
    action: &UpgradeAction,
    silent_install: bool,
) -> bool {
    silent_install && *action == UpgradeAction::UpgradeAvailable
}

#[cfg(windows)]
fn try_request_daemon_restart_after_update() -> Result<(), String> {
    let daemon_config = crate::daemon::DaemonConfig::from_env_or_default_paths()
        .map_err(|e| e.to_string())?;
    crate::commands::daemon::request_restart_after_update(&daemon_config)
}

#[allow(dead_code)]
fn verify_sha256(content: &[u8], expected_hash: &str) -> Result<(), String> {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let actual_hash = format!("{:x}", hasher.finalize());

    if actual_hash.eq_ignore_ascii_case(expected_hash) {
        Ok(())
    } else {
        Err(format!(
            "Checksum mismatch: expected {}, got {}",
            expected_hash, actual_hash
        ))
    }
}

/// Parse SHA256SUMS file content into a map of filename → hash.
/// Format: `<hash>  <filename>` (two spaces between hash and filename)
#[allow(dead_code)]
fn parse_checksums(content: &str) -> HashMap<String, String> {
    let mut checksums = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: "<hash>  <filename>" (two spaces)
        if let Some((hash, filename)) = line.split_once("  ") {
            checksums.insert(filename.to_string(), hash.to_string());
        }
    }

    checksums
}

/// Fetch SHA256SUMS from the releases API and verify against expected checksum.
#[allow(dead_code)]
fn fetch_and_verify_checksums(
    api_base_url: &str,
    channel: &str,
    expected_checksum: &str,
) -> Result<HashMap<String, String>, String> {
    let endpoint = format!("/worker/releases/{}/download/SHA256SUMS", channel);

    let (_agent, request) =
        ApiContext::http_get(&format!("{}{}", api_base_url, endpoint), Some(30));
    let response =
        crate::http::send(request).map_err(|e| format!("Failed to fetch SHA256SUMS: {}", e))?;

    if response.status_code != 200 {
        return Err(format!(
            "Failed to fetch SHA256SUMS: HTTP {}",
            response.status_code
        ));
    }

    let content = response.as_bytes();

    verify_sha256(content, expected_checksum)
        .map_err(|e| format!("SHA256SUMS verification failed: {}", e))?;

    let content_str = std::str::from_utf8(content)
        .map_err(|e| format!("SHA256SUMS is not valid UTF-8: {}", e))?;

    Ok(parse_checksums(content_str))
}

/// Fetch install script from the releases API and verify against checksums.
#[allow(dead_code)]
fn fetch_and_verify_install_script(
    api_base_url: &str,
    channel: &str,
    checksums: &HashMap<String, String>,
) -> Result<String, String> {
    #[cfg(windows)]
    let script_name = "install.ps1";
    #[cfg(not(windows))]
    let script_name = "install.sh";

    let expected_checksum = checksums
        .get(script_name)
        .ok_or_else(|| format!("Checksum for {} not found in SHA256SUMS", script_name))?;

    let endpoint = format!("/worker/releases/{}/download/{}", channel, script_name);

    let (_agent, request) =
        ApiContext::http_get(&format!("{}{}", api_base_url, endpoint), Some(30));
    let response = crate::http::send(request)
        .map_err(|e| format!("Failed to fetch {}: {}", script_name, e))?;

    if response.status_code != 200 {
        return Err(format!(
            "Failed to fetch {}: HTTP {}",
            script_name, response.status_code
        ));
    }

    let content = response.as_bytes();

    verify_sha256(content, expected_checksum)
        .map_err(|e| format!("{} verification failed: {}", script_name, e))?;

    let script = std::str::from_utf8(content)
        .map_err(|e| format!("{} is not valid UTF-8: {}", script_name, e))?;

    Ok(script.to_string())
}

fn fetch_release_for_channel(
    _api_base_url: &str,
    channel: UpdateChannel,
) -> Result<ChannelRelease, String> {
    #[cfg(test)]
    if let Some(result) = try_mock_releases(_api_base_url, channel) {
        return result;
    }

    let repo = configured_github_repo();
    match channel {
        UpdateChannel::Latest | UpdateChannel::EnterpriseLatest => fetch_latest_github_release(&repo),
        UpdateChannel::Next | UpdateChannel::EnterpriseNext => fetch_next_github_release(&repo),
    }
}

#[cfg(test)]
fn release_from_response(
    releases: ReleasesResponse,
    channel: UpdateChannel,
) -> Result<ChannelRelease, String> {
    let channel_name = channel.as_str();

    let channel_info = releases
        .channels
        .get(channel_name)
        .ok_or_else(|| format!("Channel '{}' not found in releases", channel_name))?;

    let tag = channel_info.version.trim().to_string();
    if tag.is_empty() {
        return Err("Release tag not found in response".to_string());
    }

    let semver = semver_from_tag(&tag);
    if semver.is_empty() {
        return Err(format!("Unable to parse semver from tag '{}'", tag));
    }

    let checksum = channel_info.checksum.trim().to_string();
    if checksum.is_empty() {
        return Err("Checksum not found in response".to_string());
    }

    Ok(ChannelRelease {
        tag,
        semver,
    })
}

#[cfg(test)]
fn try_mock_releases(base: &str, channel: UpdateChannel) -> Option<Result<ChannelRelease, String>> {
    let json = base.strip_prefix("mock://")?;
    Some(
        serde_json::from_str::<ReleasesResponse>(json)
            .map_err(|e| format!("Invalid mock releases payload: {}", e))
            .and_then(|releases| release_from_response(releases, channel)),
    )
}

fn run_install_script(script_content: &str, repo: &str, tag: &str, silent: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        if !silent
            && let Ok(daemon_config) = crate::daemon::DaemonConfig::from_env_or_default_paths()
        {
            // Best effort: stop the daemon before we hand off to the detached installer.
            // The install script also has a fallback kill path so old released binaries
            // can still recover, but stopping here makes upgrades complete sooner.
            let _ = crate::commands::daemon::stop_daemon(&daemon_config, Duration::from_secs(10));
        }

        // On Windows, we need to run the installer detached because the current git-ai
        // binary and shims are in use and need to be replaced. The installer will wait
        // for the files to be released before proceeding.
        let pid = std::process::id();
        let log_dir = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?
            .join(".git-ai")
            .join("upgrade-logs");

        // Ensure the log directory exists
        fs::create_dir_all(&log_dir)
            .map_err(|e| format!("Failed to create log directory: {}", e))?;

        let log_file = log_dir.join(format!("upgrade-{}.log", pid));
        let log_path_str = log_file.to_string_lossy().to_string();

        // Write the install script to a temp file
        let script_path = log_dir.join(format!("install-{}.ps1", pid));
        fs::write(&script_path, script_content)
            .map_err(|e| format!("Failed to write install script: {}", e))?;
        let script_path_str = script_path.to_string_lossy().to_string();

        // Create log file with initial message
        fs::write(&log_file, format!("Starting upgrade at PID {}\n", pid))
            .map_err(|e| format!("Failed to create log file: {}", e))?;

        // PowerShell wrapper that executes the script file with logging
        let ps_wrapper = format!(
            "$logFile = '{}'; \
             Start-Transcript -Path $logFile -Append -Force | Out-Null; \
             Write-Host 'Running verified install script...'; \
             try {{ \
                  $ErrorActionPreference = 'Continue'; \
                  & '{}'; \
                  Write-Host 'Install script completed'; \
              }} catch {{ \
                  Write-Host \"Error: $_\"; \
                  Write-Host \"Stack trace: $($_.ScriptStackTrace)\"; \
              }} finally {{ \
                  if ($env:{} -eq '1') {{ \
                      $daemonExe = Join-Path $HOME '.git-ai\\bin\\git-ai.exe'; \
                      if (Test-Path $daemonExe) {{ try {{ & $daemonExe bg start *> $null }} catch {{ }} }} \
                  }}; \
                  Stop-Transcript | Out-Null; \
                  Remove-Item -Path '{}' -Force -ErrorAction SilentlyContinue; \
              }}",
            log_path_str, script_path_str, GIT_AI_RESTART_DAEMON_AFTER_INSTALL_ENV, script_path_str
        );

        let spawn_powershell = |exe: &str| -> std::io::Result<std::process::Child> {
            let mut cmd = Command::new(exe);
            cmd.arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-Command")
                .arg(&ps_wrapper)
                .env(GIT_AI_GITHUB_REPO_ENV, repo)
                .env(GIT_AI_RELEASE_ENV, tag);

            // Hide the spawned console to prevent any host/UI bleed-through
            cmd.creation_flags(CREATE_NO_WINDOW);

            if silent {
                cmd.env(GIT_AI_RESTART_DAEMON_AFTER_INSTALL_ENV, "1");
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }

            cmd.spawn()
        };

        let spawn_result = spawn_powershell("pwsh").or_else(|_| spawn_powershell("powershell"));

        match spawn_result {
            Ok(_) => {
                if !silent {
                    println!(
                        "\x1b[1;33mNote: The installation is running in the background on Windows.\x1b[0m"
                    );
                    println!(
                        "This allows the current git-ai process to exit and release file locks."
                    );
                    println!("Check the log file for progress: {}", log_path_str);
                    println!(
                        "The installer will stop lingering git-ai background processes if needed, but active git commands can still delay completion."
                    );
                }
                Ok(())
            }
            Err(e) => Err(format!("Failed to run installation script: {}", e)),
        }
    }

    #[cfg(not(windows))]
    {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        // Write script to ~/.git-ai/tmp/ to avoid /tmp noexec or permission issues.
        // Fall back to the system temp dir if the home-based path is unavailable.
        let temp_dir = crate::config::git_ai_dir_path()
            .map(|p| p.join("tmp"))
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp directory: {}", e))?;
        let script_path = temp_dir.join(format!("git-ai-install-{}.sh", std::process::id()));

        // Write and make executable
        let mut file = fs::File::create(&script_path)
            .map_err(|e| format!("Failed to create temp script file: {}", e))?;
        file.write_all(script_content.as_bytes())
            .map_err(|e| format!("Failed to write install script: {}", e))?;
        drop(file);

        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to make script executable: {}", e))?;

        let script_path_str = script_path.to_string_lossy().to_string();

        let mut cmd = Command::new("bash");
        cmd.arg(&script_path_str)
            .env(GIT_AI_GITHUB_REPO_ENV, repo)
            .env(GIT_AI_RELEASE_ENV, tag);

        if silent {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }

        let result = match cmd.status() {
            Ok(status) => {
                if status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "Installation script failed with exit code: {:?}",
                        status.code()
                    ))
                }
            }
            Err(e) => Err(format!("Failed to run installation script: {}", e)),
        };

        // Clean up temp script
        let _ = fs::remove_file(&script_path);

        result
    }
}

pub fn run_with_args(args: &[String]) {
    #[cfg(windows)]
    exit_if_invoked_via_git_extension();

    let mut force = false;
    let mut background = false;

    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            "--background" => background = true, // Undocumented flag for internal use when spawning background process
            _ => {
                eprintln!("Unknown argument: {}", arg);
                eprintln!("Usage: git-ai upgrade [--force]");
                std::process::exit(1);
            }
        }
    }

    run_impl(force, background);
}

fn run_impl(force: bool, background: bool) {
    if background {
        maybe_wait_before_background_upgrade();
    }

    let config = config::Config::fresh();
    let channel = config.update_channel();
    let install_mode = install_invocation_mode(background, config.auto_updates_disabled());
    let _ = run_impl_with_url(
        force,
        config.api_base_url(),
        channel,
        install_mode.skip_install,
        install_mode.silent_install,
    );
}

fn run_impl_with_url(
    force: bool,
    api_base_url: &str,
    channel: UpdateChannel,
    skip_install: bool,
    silent_install: bool,
) -> UpgradeAction {
    let current_version = env!("CARGO_PKG_VERSION");
    let repo = configured_github_repo();
    let release_source = release_source_url(api_base_url, &repo);

    println!(
        "Checking for updates (channel: {}, repo: {})...",
        channel.as_str(),
        repo
    );

    let release = match fetch_release_for_channel(api_base_url, channel) {
        Ok(release) => release,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    let installer_url = configured_installer_url(&repo, &release.tag);

    println!("Current version: v{}", current_version);
    println!(
        "Available {} version: v{} (tag {})",
        channel.as_str(),
        release.semver,
        release.tag
    );
    println!();

    let action = determine_action(force, &release, current_version);
    let cache_release = matches!(action, UpgradeAction::UpgradeAvailable);
    persist_update_state(channel, cache_release.then_some(&release));

    log_message(
        "checked_for_update",
        "info",
        Some(serde_json::json!({
            "current_version": current_version,
            "release_source": release_source,
            "release_repo": repo,
            "channel": channel.as_str(),
            "result": action.to_string()
        })),
    );

    match action {
        UpgradeAction::AlreadyLatest => {
            println!("You are already on the latest version!");
            println!();
            println!("To reinstall anyway, run:");
            println!("  \x1b[1;36mgit-ai upgrade --force\x1b[0m");
            return action;
        }
        UpgradeAction::RunningNewerVersion => {
            println!("You are running a newer version than the selected release channel.");
            println!("(This usually means you're running a development build)");
            println!();
            println!("To reinstall the selected release anyway, run:");
            println!("  \x1b[1;36mgit-ai upgrade --force\x1b[0m");
            return action;
        }
        UpgradeAction::ForceReinstall => {
            println!(
                "\x1b[1;33mForce mode enabled - reinstalling {}\x1b[0m",
                release.tag
            );
        }
        UpgradeAction::UpgradeAvailable => {
            println!("\x1b[1;33mA new version is available!\x1b[0m");
        }
    }
    println!();

    if skip_install {
        return action;
    }

    #[cfg(windows)]
    if should_request_daemon_restart_after_update(&action, silent_install) {
        match try_request_daemon_restart_after_update() {
            Ok(()) => {
                log_message(
                    "daemon_restart_after_update_requested",
                    "info",
                    Some(serde_json::json!({
                        "release_tag": release.tag,
                        "current_version": current_version,
                        "release_source": release_source,
                        "release_repo": repo,
                        "installer_url": installer_url,
                        "channel": channel.as_str()
                    })),
                );
                return action;
            }
            Err(error) => {
                log_message(
                    "daemon_restart_after_update_request_failed",
                    "warn",
                    Some(serde_json::json!({
                        "error": error,
                        "release_tag": release.tag,
                        "current_version": current_version,
                        "release_source": release_source,
                        "release_repo": repo,
                        "installer_url": installer_url,
                        "channel": channel.as_str()
                    })),
                );
            }
        }
    }

    println!("Fetching installer script...");

    let script_content = match fetch_install_script(&repo, &release.tag) {
        Ok(content) => {
            #[cfg(windows)]
            println!("\x1b[1;32m✓\x1b[0m install.ps1 fetched from {}", installer_url);
            #[cfg(not(windows))]
            println!("\x1b[1;32m✓\x1b[0m install.sh fetched from {}", installer_url);
            content
        }
        Err(err) => {
            eprintln!("Failed to fetch install script: {}", err);
            std::process::exit(1);
        }
    };

    println!();
    println!("Running installation script...");
    println!();

    match run_install_script(&script_content, &repo, &release.tag, silent_install) {
        Ok(()) => {
            // On Windows, we spawn the installer in the background and can't verify success
            #[cfg(not(windows))]
            {
                println!("\x1b[1;32m✓\x1b[0m Successfully installed {}!", release.tag);
            }

            log_message(
                "upgraded",
                "info",
                Some(serde_json::json!({
                    "release_tag": release.tag,
                    "current_version": current_version,
                    "release_source": release_source,
                    "release_repo": repo,
                    "installer_url": installer_url,
                    "channel": channel.as_str()
                })),
            );
        }
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    }

    action
}

fn print_cached_notice(cache: &UpdateCache) {
    if cache.available_semver.is_none() || cache.available_tag.is_none() {
        return;
    }

    if !std::io::stdout().is_terminal() {
        // Don't print the version check notice if stdout is not a terminal/interactive shell
        return;
    }

    if UPDATE_NOTICE_EMITTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let current_version = env!("CARGO_PKG_VERSION");
    let available_version = cache.available_semver.as_deref().unwrap_or("");

    eprintln!();
    eprintln!(
        "\x1b[1;33mA new version of git-ai is available: \x1b[1;32mv{}\x1b[0m → \x1b[1;32mv{}\x1b[0m",
        current_version, available_version
    );
    eprintln!(
        "\x1b[1;33mRun \x1b[1;36mgit-ai upgrade\x1b[0m \x1b[1;33mto upgrade to the latest version.\x1b[0m"
    );
    eprintln!();
}

pub fn maybe_schedule_background_update_check() {
    maybe_schedule_background_update_check_with_policy(UpdateCheckPolicy::Default);
}

pub fn maybe_schedule_background_update_check_after_commit() {
    maybe_schedule_background_update_check_with_policy(UpdateCheckPolicy::AfterCommit);
}

fn maybe_schedule_background_update_check_with_policy(policy: UpdateCheckPolicy) {
    let config = config::Config::get();
    if config.version_checks_disabled() {
        return;
    }

    let channel = config.update_channel();
    let cache = read_update_cache();

    if config.auto_updates_disabled()
        && let Some(cache) = cache.as_ref()
        && cache.matches_channel(channel)
        && cache.update_available()
    {
        print_cached_notice(cache);
    }

    if !should_check_for_updates(
        channel,
        cache.as_ref(),
        policy,
        config.auto_updates_disabled(),
    ) {
        return;
    }

    let now = current_timestamp();
    let last_spawn = LAST_BACKGROUND_SPAWN.load(Ordering::SeqCst);
    if now.saturating_sub(last_spawn) < BACKGROUND_SPAWN_THROTTLE_SECS {
        return;
    }

    let spawned = match policy {
        UpdateCheckPolicy::Default => spawn_background_upgrade_process(),
        UpdateCheckPolicy::AfterCommit => spawn_background_upgrade_process_after_commit(),
    };

    if spawned {
        LAST_BACKGROUND_SPAWN.store(now, Ordering::SeqCst);
    }
}

fn spawn_background_upgrade_process() -> bool {
    crate::utils::spawn_internal_git_ai_subcommand(
        "upgrade",
        &["--background"],
        ENV_BACKGROUND_UPGRADE_WORKER,
        &[],
    )
}

fn spawn_background_upgrade_process_after_commit() -> bool {
    crate::utils::spawn_internal_git_ai_subcommand(
        "upgrade",
        &["--background"],
        ENV_BACKGROUND_UPGRADE_WORKER,
        &[(
            ENV_BACKGROUND_UPGRADE_DELAY_SECS,
            AFTER_COMMIT_BACKGROUND_UPGRADE_DELAY_SECS,
        )],
    )
}

/// Result of checking whether a daemon-initiated update is available.
#[derive(Debug, PartialEq)]
pub enum DaemonUpdateCheckResult {
    /// No update is needed (already latest, checks disabled, or not yet time to check).
    NoUpdate,
    /// An update is available and auto-updates are enabled.
    UpdateReady,
}

/// Install a previously-detected update.
///
/// Designed for use by the daemon process **after** a clean shutdown.  Reads
/// the on-disk update cache (written earlier by `check_for_update_available`)
/// to decide whether an update is pending, bypassing the 24-hour time guard.
/// Uses `Config::fresh()` (not the `OnceLock` singleton) so the daemon
/// respects runtime config changes (e.g. disabling auto-updates).
///
/// Returns `Ok(UpdateReady)` if the install script ran, `Ok(NoUpdate)` if
/// no pending update was found or updates are disabled.
pub fn check_and_install_update_if_available() -> Result<DaemonUpdateCheckResult, String> {
    let config = config::Config::fresh();
    if config.version_checks_disabled() || config.auto_updates_disabled() {
        return Ok(DaemonUpdateCheckResult::NoUpdate);
    }

    let _ = crate::integration::upload_stats::wait_for_upload_activity_to_finish(
        Duration::from_secs(UPLOAD_ACTIVITY_WAIT_SECS),
    );

    let channel = config.update_channel();
    let api_base_url = config.api_base_url();
    let repo = configured_github_repo();
    let release_source = release_source_url(api_base_url, &repo);

    // Read the cache that check_for_update_available() populated earlier.
    // We intentionally skip should_check_for_updates() here because the
    // hourly check loop already confirmed an update is available and
    // persisted that fact — re-checking the 24h guard would always say
    // "too soon" and the install would never run.
    let cache = read_update_cache();
    let has_pending_update = cache
        .as_ref()
        .is_some_and(|c| c.matches_channel(channel) && c.update_available());

    if !has_pending_update {
        return Ok(DaemonUpdateCheckResult::NoUpdate);
    }

    // Re-fetch the release to get the tag needed for the installer.
    let release = fetch_release_for_channel(api_base_url, channel)?;
    let current_version = env!("CARGO_PKG_VERSION");
    let action = determine_action(false, &release, current_version);
    let installer_url = configured_installer_url(&repo, &release.tag);

    if action != UpgradeAction::UpgradeAvailable {
        // Cache was stale or version changed between check and install.
        persist_update_state(channel, None);
        return Ok(DaemonUpdateCheckResult::NoUpdate);
    }

    log_message(
        "daemon_installing_update",
        "info",
        Some(serde_json::json!({
            "current_version": current_version,
            "release_tag": release.tag,
            "release_source": release_source,
            "release_repo": repo,
            "installer_url": installer_url,
            "channel": channel.as_str()
        })),
    );

    let script_content = fetch_install_script(&repo, &release.tag)?;
    run_install_script(&script_content, &repo, &release.tag, true)?;

    // Clear the cached update now that we've installed it.
    persist_update_state(channel, None);

    log_message(
        "daemon_upgraded",
        "info",
        Some(serde_json::json!({
            "release_tag": release.tag,
            "current_version": current_version,
            "release_source": release_source,
            "release_repo": repo,
            "installer_url": installer_url,
            "channel": channel.as_str()
        })),
    );

    Ok(DaemonUpdateCheckResult::UpdateReady)
}

/// Check whether a newer version is available without installing it.
///
/// Like `check_and_install_update_if_available` but only queries the releases API
/// and updates the local cache. Returns `DaemonUpdateCheckResult::UpdateReady` when
/// the channel has a newer version than the running binary.
pub fn check_for_update_available() -> Result<DaemonUpdateCheckResult, String> {
    let config = config::Config::fresh();
    if config.version_checks_disabled() {
        return Ok(DaemonUpdateCheckResult::NoUpdate);
    }

    let channel = config.update_channel();
    let api_base_url = config.api_base_url();
    let repo = configured_github_repo();
    let release_source = release_source_url(api_base_url, &repo);
    let cache = read_update_cache();

    if !should_check_for_updates(
        channel,
        cache.as_ref(),
        UpdateCheckPolicy::Default,
        config.auto_updates_disabled(),
    ) {
        // Even if it's not time to re-check, an earlier check may have found an update.
        if let Some(ref c) = cache
            && c.matches_channel(channel)
            && c.update_available()
            && !config.auto_updates_disabled()
        {
            return Ok(DaemonUpdateCheckResult::UpdateReady);
        }
        return Ok(DaemonUpdateCheckResult::NoUpdate);
    }

    let release = fetch_release_for_channel(api_base_url, channel)?;
    let current_version = env!("CARGO_PKG_VERSION");
    let action = determine_action(false, &release, current_version);
    let cache_release = matches!(action, UpgradeAction::UpgradeAvailable);
    persist_update_state(channel, cache_release.then_some(&release));

    log_message(
        "checked_for_update",
        "info",
        Some(serde_json::json!({
            "current_version": current_version,
            "release_source": release_source,
            "release_repo": repo,
            "channel": channel.as_str(),
            "result": action.to_string()
        })),
    );

    if action == UpgradeAction::UpgradeAvailable && !config.auto_updates_disabled() {
        Ok(DaemonUpdateCheckResult::UpdateReady)
    } else {
        Ok(DaemonUpdateCheckResult::NoUpdate)
    }
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse_version =
        |v: &str| -> Vec<u32> { v.split('.').filter_map(|s| s.parse::<u32>().ok()).collect() };

    let latest_parts = parse_version(latest);
    let current_parts = parse_version(current);

    for i in 0..latest_parts.len().max(current_parts.len()) {
        let latest_part = latest_parts.get(i).copied().unwrap_or(0);
        let current_part = current_parts.get(i).copied().unwrap_or(0);

        if latest_part > current_part {
            return true;
        } else if latest_part < current_part {
            return false;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn set_test_cache_dir(dir: &tempfile::TempDir) {
        unsafe {
            std::env::set_var("GIT_AI_TEST_CACHE_DIR", dir.path());
        }
    }

    fn clear_test_cache_dir() {
        unsafe {
            std::env::remove_var("GIT_AI_TEST_CACHE_DIR");
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_is_git_process_name() {
        assert!(is_git_process_name("git"));
        assert!(is_git_process_name("git.exe"));
        assert!(is_git_process_name(r"C:\Program Files\Git\cmd\git.exe"));
        assert!(!is_git_process_name("git-ai.exe"));
        assert!(!is_git_process_name("powershell.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn test_should_block_git_extension_upgrade() {
        assert!(should_block_git_extension_upgrade(Some("git.exe"), false));
        assert!(should_block_git_extension_upgrade(
            Some(r"C:\Program Files\Git\cmd\git.exe"),
            false
        ));
        assert!(!should_block_git_extension_upgrade(Some("git.exe"), true));
        assert!(!should_block_git_extension_upgrade(
            Some("powershell.exe"),
            false
        ));
        assert!(!should_block_git_extension_upgrade(None, false));
    }

    #[test]
    fn test_is_newer_version() {
        assert!(!is_newer_version("1.0.0", "1.0.0"));
        assert!(!is_newer_version("1.0.10", "1.0.10"));

        assert!(is_newer_version("1.0.1", "1.0.0"));
        assert!(is_newer_version("1.0.11", "1.0.10"));
        assert!(!is_newer_version("1.0.0", "1.0.1"));
        assert!(!is_newer_version("1.0.10", "1.0.11"));

        assert!(is_newer_version("1.1.0", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.1.0"));

        assert!(is_newer_version("2.0.0", "1.0.0"));
        assert!(is_newer_version("2.0.0", "1.9.9"));
        assert!(!is_newer_version("1.9.9", "2.0.0"));

        assert!(is_newer_version("1.0.0.1", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.0.0.1"));

        assert!(is_newer_version("1.10.0", "1.9.0"));
        assert!(is_newer_version("1.0.100", "1.0.99"));
        assert!(is_newer_version("100.200.300", "100.200.299"));
    }

    #[test]
    fn test_semver_from_tag_strips_prefix_and_suffix() {
        assert_eq!(semver_from_tag("v1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("v1.2.3-next-abc"), "1.2.3");
        assert_eq!(semver_from_tag("enterprise-v1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("enterprise-v1.2.3-next-abc"), "1.2.3");
    }

    fn github_release(tag_name: &str, prerelease: bool, draft: bool) -> GitHubReleaseResponse {
        GitHubReleaseResponse {
            tag_name: tag_name.to_string(),
            prerelease,
            draft,
        }
    }

    #[test]
    fn test_select_newest_github_release_uses_highest_stable_semver() {
        let releases = vec![
            github_release("v2.1.9", false, false),
            github_release("v2.1.11", false, false),
            github_release("v2.1.12-next-deadbeef", true, false),
            github_release("v2.1.12", false, true),
        ];

        let release = select_newest_github_release(&releases, false).unwrap();

        assert_eq!(release.tag, "v2.1.11");
        assert_eq!(release.semver, "2.1.11");
    }

    #[test]
    fn test_select_newest_github_release_uses_highest_prerelease_semver() {
        let releases = vec![
            github_release("v2.1.10-next-aaaa1111", true, false),
            github_release("v2.1.11-next-bbbb2222", true, false),
            github_release("v2.1.11", false, false),
        ];

        let release = select_newest_github_release(&releases, true).unwrap();

        assert_eq!(release.tag, "v2.1.11-next-bbbb2222");
        assert_eq!(release.semver, "2.1.11");
    }

    #[test]
    fn test_select_newest_github_release_errors_when_channel_missing() {
        let releases = vec![github_release("v2.1.11-next-bbbb2222", true, false)];

        let result = select_newest_github_release(&releases, false);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "No GitHub stable release found");
    }

    #[test]
    #[serial]
    fn test_should_check_for_updates_respects_cache_by_default() {
        let channel = UpdateChannel::Latest;
        let mut cache = UpdateCache::new(channel);
        cache.last_checked_at = current_timestamp();

        assert!(!should_check_for_updates(
            channel,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    #[test]
    #[serial]
    fn test_should_check_for_updates_bypasses_no_update_cache_after_commit() {
        let channel = UpdateChannel::Latest;
        let mut cache = UpdateCache::new(channel);
        cache.last_checked_at = current_timestamp();

        assert!(should_check_for_updates(
            channel,
            Some(&cache),
            UpdateCheckPolicy::AfterCommit,
            false,
        ));
    }

    #[test]
    #[serial]
    fn test_should_check_for_updates_keeps_cache_when_auto_updates_disabled() {
        let channel = UpdateChannel::Latest;
        let mut cache = UpdateCache::new(channel);
        cache.last_checked_at = current_timestamp();

        assert!(!should_check_for_updates(
            channel,
            Some(&cache),
            UpdateCheckPolicy::AfterCommit,
            true,
        ));
    }

    #[test]
    #[serial]
    fn test_configured_github_repo_defaults_to_rj_gaoang_repo() {
        unsafe {
            std::env::remove_var(GIT_AI_GITHUB_REPO_ENV);
        }

        assert_eq!(configured_github_repo(), DEFAULT_GITHUB_RELEASE_REPO);
    }

    #[test]
    #[serial]
    fn test_configured_installer_url_uses_override_then_repo_default() {
        unsafe {
            std::env::remove_var(GIT_AI_INSTALLER_URL_ENV);
        }

        assert_eq!(
            configured_installer_url(DEFAULT_GITHUB_RELEASE_REPO, "v2.1.13"),
            format!(
                "https://github.com/{}/releases/download/{}/{}",
                DEFAULT_GITHUB_RELEASE_REPO,
                "v2.1.13",
                INSTALL_SCRIPT_NAME
            )
        );

        unsafe {
            std::env::set_var(GIT_AI_INSTALLER_URL_ENV, "https://example.com/install-script");
        }

        assert_eq!(
            configured_installer_url(DEFAULT_GITHUB_RELEASE_REPO, "v2.1.13"),
            "https://example.com/install-script"
        );

        unsafe {
            std::env::remove_var(GIT_AI_INSTALLER_URL_ENV);
        }
    }

    #[test]
    fn test_raw_main_installer_url_points_to_main_branch_script() {
        assert_eq!(
            raw_main_installer_url(DEFAULT_GITHUB_RELEASE_REPO),
            format!(
                "{}/{}/main/{}",
                RAW_GITHUB_CONTENT_BASE_URL,
                DEFAULT_GITHUB_RELEASE_REPO,
                INSTALL_SCRIPT_NAME
            )
        );
    }

    #[test]
    #[serial]
    fn test_run_impl_with_url() {
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let mock_url = |body: &str| format!("mock://{}", body);
        let current = env!("CARGO_PKG_VERSION");
        let test_checksum = "a".repeat(64); // Valid SHA256 length

        // Newer version available - should upgrade
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"latest":{{"version":"v999.0.0","checksum":"{}"}},"next":{{"version":"v999.0.0-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::Latest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::UpgradeAvailable);

        // Same version without --force - already latest
        let same_version_payload = format!(
            "{{\"channels\":{{\"latest\":{{\"version\":\"v{}\",\"checksum\":\"{}\"}},\"next\":{{\"version\":\"v{}-next-deadbeef\",\"checksum\":\"{}\"}}}}}}",
            current, test_checksum, current, test_checksum
        );
        let action = run_impl_with_url(
            false,
            &mock_url(&same_version_payload),
            UpdateChannel::Latest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::AlreadyLatest);

        // Same version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&same_version_payload),
            UpdateChannel::Latest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        // Older version without --force - running newer version
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"latest":{{"version":"v1.0.9","checksum":"{}"}},"next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::Latest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::RunningNewerVersion);

        // Older version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&format!(
                r#"{{"channels":{{"latest":{{"version":"v1.0.9","checksum":"{}"}},"next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::Latest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        clear_test_cache_dir();
    }

    #[test]
    #[serial]
    fn test_run_impl_with_url_enterprise_channels() {
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let mock_url = |body: &str| format!("mock://{}", body);
        let current = env!("CARGO_PKG_VERSION");
        let test_checksum = "a".repeat(64); // Valid SHA256 length

        // Newer version available - should upgrade
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"enterprise-latest":{{"version":"v999.0.0","checksum":"{}"}},"enterprise-next":{{"version":"v999.0.0-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::EnterpriseLatest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::UpgradeAvailable);

        // Same version without --force - already latest
        let same_version_payload = format!(
            "{{\"channels\":{{\"enterprise-latest\":{{\"version\":\"v{}\",\"checksum\":\"{}\"}},\"enterprise-next\":{{\"version\":\"v{}-next-deadbeef\",\"checksum\":\"{}\"}}}}}}",
            current, test_checksum, current, test_checksum
        );
        let action = run_impl_with_url(
            false,
            &mock_url(&same_version_payload),
            UpdateChannel::EnterpriseLatest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::AlreadyLatest);

        // Same version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&same_version_payload),
            UpdateChannel::EnterpriseLatest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        // Older version without --force - running newer version
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"enterprise-latest":{{"version":"v1.0.9","checksum":"{}"}},"enterprise-next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::EnterpriseLatest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::RunningNewerVersion);

        // Older version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&format!(
                r#"{{"channels":{{"enterprise-latest":{{"version":"v1.0.9","checksum":"{}"}},"enterprise-next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::EnterpriseLatest,
            true,
            false,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        clear_test_cache_dir();
    }

    #[test]
    fn test_install_invocation_mode_background_runs_silently() {
        assert_eq!(
            install_invocation_mode(false, false),
            InstallInvocationMode {
                skip_install: false,
                silent_install: false,
            }
        );
        assert_eq!(
            install_invocation_mode(true, false),
            InstallInvocationMode {
                skip_install: false,
                silent_install: true,
            }
        );
        assert_eq!(
            install_invocation_mode(true, true),
            InstallInvocationMode {
                skip_install: true,
                silent_install: true,
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_should_request_daemon_restart_after_update_only_for_background_upgrade() {
        assert!(!should_request_daemon_restart_after_update(
            &UpgradeAction::AlreadyLatest,
            true,
        ));
        assert!(!should_request_daemon_restart_after_update(
            &UpgradeAction::UpgradeAvailable,
            false,
        ));
        assert!(should_request_daemon_restart_after_update(
            &UpgradeAction::UpgradeAvailable,
            true,
        ));
    }

    #[test]
    fn test_should_check_for_updates_respects_interval() {
        let now = current_timestamp();
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = now;
        assert!(!should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));

        let stale_offset = (UPDATE_CHECK_INTERVAL_HOURS * 3600) + 10;
        cache.last_checked_at = now.saturating_sub(stale_offset);
        assert!(should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));

        assert!(should_check_for_updates(
            UpdateChannel::Latest,
            None,
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    #[test]
    fn test_should_check_for_updates_verifies_channel() {
        let now = current_timestamp();
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = now;

        // Cache matches channel - should respect interval
        assert!(!should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));

        // Cache doesn't match channel - should check for updates
        assert!(should_check_for_updates(
            UpdateChannel::Next,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    #[test]
    fn test_verify_sha256_success() {
        let content = b"hello world";
        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_sha256(content, expected).is_ok());
    }

    #[test]
    fn test_verify_sha256_case_insensitive() {
        let content = b"hello world";
        let expected_upper = "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9";
        assert!(verify_sha256(content, expected_upper).is_ok());
    }

    #[test]
    fn test_verify_sha256_mismatch() {
        let content = b"hello world";
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        let result = verify_sha256(content, wrong_hash);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Checksum mismatch"));
    }

    #[test]
    fn test_verify_sha256_empty_content() {
        let content = b"";
        // SHA256 of empty string
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_sha256(content, expected).is_ok());
    }

    #[test]
    fn test_parse_checksums_valid_format() {
        let content = "594de6cf107e8ffb6efd9029bf727b465ab55a9b4c4c3995eb3e628c857dc423  git-ai-linux-arm64\n\
                       88db3c0c7fc62a815579ec0ca42535c2b83ab18d9e3af8efe345dee96677b1d8  git-ai-linux-x64\n\
                       75d1692d347c3e08a208dc6373df4cee2b5ffd0e2aee62ccb1bb47aae866b2c8  install.sh";

        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 3);
        assert_eq!(
            checksums.get("git-ai-linux-arm64"),
            Some(&"594de6cf107e8ffb6efd9029bf727b465ab55a9b4c4c3995eb3e628c857dc423".to_string())
        );
        assert_eq!(
            checksums.get("git-ai-linux-x64"),
            Some(&"88db3c0c7fc62a815579ec0ca42535c2b83ab18d9e3af8efe345dee96677b1d8".to_string())
        );
        assert_eq!(
            checksums.get("install.sh"),
            Some(&"75d1692d347c3e08a208dc6373df4cee2b5ffd0e2aee62ccb1bb47aae866b2c8".to_string())
        );
    }

    #[test]
    fn test_parse_checksums_with_extensions() {
        let content = "23c693a25f4f2e99463c911e67d534ae17cbd9b98513aa65f0ae9da861775d54  git-ai-windows-x64.exe\n\
                       f895af791eb30f6b074b2ab9f0f803e91230b084f5864befcb51ee9ced752adf  install.ps1";

        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 2);
        assert!(checksums.contains_key("git-ai-windows-x64.exe"));
        assert!(checksums.contains_key("install.ps1"));
    }

    #[test]
    fn test_parse_checksums_empty_input() {
        let checksums = parse_checksums("");
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_whitespace_lines() {
        let content = "  \n\nhash  file\n  \n";
        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 1);
        assert_eq!(checksums.get("file"), Some(&"hash".to_string()));
    }

    #[test]
    fn test_parse_checksums_ignores_invalid_lines() {
        // Lines with single space or no space should be ignored
        let content = "valid  file1\ninvalid file2\nalsovalid  file3";
        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 2);
        assert!(checksums.contains_key("file1"));
        assert!(checksums.contains_key("file3"));
        assert!(!checksums.contains_key("file2"));
    }

    // --- Additional comprehensive tests ---

    #[test]
    fn test_update_cache_new() {
        let cache = UpdateCache::new(UpdateChannel::Latest);
        assert_eq!(cache.last_checked_at, 0);
        assert!(cache.available_tag.is_none());
        assert!(cache.available_semver.is_none());
        assert_eq!(cache.channel, "latest");
        assert!(!cache.update_available());
        assert!(cache.matches_channel(UpdateChannel::Latest));
        assert!(!cache.matches_channel(UpdateChannel::Next));
    }

    #[test]
    fn test_update_cache_update_available() {
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.available_semver = Some("2.0.0".to_string());
        assert!(cache.update_available());
    }

    #[test]
    fn test_update_cache_matches_channel_enterprise() {
        let cache_latest = UpdateCache::new(UpdateChannel::EnterpriseLatest);
        assert!(cache_latest.matches_channel(UpdateChannel::EnterpriseLatest));
        assert!(!cache_latest.matches_channel(UpdateChannel::EnterpriseNext));
        assert!(!cache_latest.matches_channel(UpdateChannel::Latest));
    }

    #[test]
    fn test_determine_action_force() {
        let release = ChannelRelease {
            tag: "v1.0.0".to_string(),
            semver: "1.0.0".to_string(),
        };
        let action = determine_action(true, &release, "1.0.0");
        assert_eq!(action, UpgradeAction::ForceReinstall);
    }

    #[test]
    fn test_determine_action_already_latest() {
        let release = ChannelRelease {
            tag: "v1.0.0".to_string(),
            semver: "1.0.0".to_string(),
        };
        let action = determine_action(false, &release, "1.0.0");
        assert_eq!(action, UpgradeAction::AlreadyLatest);
    }

    #[test]
    fn test_determine_action_upgrade_available() {
        let release = ChannelRelease {
            tag: "v2.0.0".to_string(),
            semver: "2.0.0".to_string(),
        };
        let action = determine_action(false, &release, "1.0.0");
        assert_eq!(action, UpgradeAction::UpgradeAvailable);
    }

    #[test]
    fn test_determine_action_running_newer() {
        let release = ChannelRelease {
            tag: "v1.0.0".to_string(),
            semver: "1.0.0".to_string(),
        };
        let action = determine_action(false, &release, "2.0.0");
        assert_eq!(action, UpgradeAction::RunningNewerVersion);
    }

    #[test]
    fn test_upgrade_action_to_string() {
        assert_eq!(
            UpgradeAction::UpgradeAvailable.to_string(),
            "upgrade_available"
        );
        assert_eq!(UpgradeAction::AlreadyLatest.to_string(), "already_latest");
        assert_eq!(
            UpgradeAction::RunningNewerVersion.to_string(),
            "running_newer_version"
        );
        assert_eq!(UpgradeAction::ForceReinstall.to_string(), "force_reinstall");
    }

    #[test]
    fn test_semver_from_tag_enterprise_prefix() {
        assert_eq!(semver_from_tag("enterprise-v1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("enterprise-1.2.3"), "1.2.3");
    }

    #[test]
    fn test_semver_from_tag_with_build_metadata() {
        assert_eq!(semver_from_tag("v1.2.3+build123"), "1.2.3");
        assert_eq!(semver_from_tag("1.2.3+build123"), "1.2.3");
    }

    #[test]
    fn test_semver_from_tag_empty() {
        assert_eq!(semver_from_tag(""), "");
        assert_eq!(semver_from_tag("v"), "");
        assert_eq!(semver_from_tag("enterprise-v"), "");
    }

    #[test]
    fn test_is_newer_version_major() {
        assert!(is_newer_version("2.0.0", "1.9.9"));
        assert!(!is_newer_version("1.9.9", "2.0.0"));
    }

    #[test]
    fn test_is_newer_version_minor() {
        assert!(is_newer_version("1.2.0", "1.1.9"));
        assert!(!is_newer_version("1.1.9", "1.2.0"));
    }

    #[test]
    fn test_is_newer_version_patch() {
        assert!(is_newer_version("1.0.1", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.0.1"));
    }

    #[test]
    fn test_is_newer_version_empty_parts() {
        assert!(is_newer_version("1", "0.9.9"));
        assert!(!is_newer_version("0.9.9", "1"));
    }

    #[test]
    fn test_is_newer_version_equal() {
        assert!(!is_newer_version("1.0.0", "1.0.0"));
        assert!(!is_newer_version("2.5.10", "2.5.10"));
    }

    #[test]
    fn test_parse_checksums_multiple_spaces() {
        // Format requires exactly two spaces between hash and filename
        // More spaces should still work because split_once("  ") matches the first occurrence
        let content = "abc123  file_with_spaces.txt";
        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 1);
        assert_eq!(
            checksums.get("file_with_spaces.txt"),
            Some(&"abc123".to_string())
        );
    }

    #[test]
    fn test_verify_sha256_with_binary_content() {
        let content = b"\x00\x01\x02\x03\xff\xfe";
        let mut hasher = sha2::Sha256::new();
        hasher.update(content);
        let expected = format!("{:x}", hasher.finalize());
        assert!(verify_sha256(content, &expected).is_ok());
    }

    #[test]
    fn test_release_from_response_missing_channel() {
        let releases = ReleasesResponse {
            channels: HashMap::new(),
        };
        let result = release_from_response(releases, UpdateChannel::Latest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_release_from_response_empty_tag() {
        let mut channels = HashMap::new();
        channels.insert(
            "latest".to_string(),
            ChannelInfo {
                version: "".to_string(),
                checksum: "abc123".to_string(),
            },
        );
        let releases = ReleasesResponse { channels };
        let result = release_from_response(releases, UpdateChannel::Latest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_release_from_response_empty_checksum() {
        let mut channels = HashMap::new();
        channels.insert(
            "latest".to_string(),
            ChannelInfo {
                version: "v1.0.0".to_string(),
                checksum: "".to_string(),
            },
        );
        let releases = ReleasesResponse { channels };
        let result = release_from_response(releases, UpdateChannel::Latest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Checksum"));
    }

    #[test]
    fn test_release_from_response_invalid_semver() {
        let mut channels = HashMap::new();
        channels.insert(
            "latest".to_string(),
            ChannelInfo {
                version: "v-invalid-version".to_string(),
                checksum: "abc123".to_string(),
            },
        );
        let releases = ReleasesResponse { channels };
        let result = release_from_response(releases, UpdateChannel::Latest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("semver"));
    }

    #[test]
    fn test_release_from_response_success() {
        let mut channels = HashMap::new();
        channels.insert(
            "latest".to_string(),
            ChannelInfo {
                version: "v1.2.3".to_string(),
                checksum: "abc123def456".to_string(),
            },
        );
        let releases = ReleasesResponse { channels };
        let result = release_from_response(releases, UpdateChannel::Latest);
        assert!(result.is_ok());
        let release = result.unwrap();
        assert_eq!(release.tag, "v1.2.3");
        assert_eq!(release.semver, "1.2.3");
    }

    #[test]
    fn test_should_check_for_updates_no_cache() {
        assert!(should_check_for_updates(
            UpdateChannel::Latest,
            None,
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    #[test]
    fn test_should_check_for_updates_zero_last_checked() {
        let cache = UpdateCache {
            last_checked_at: 0,
            available_tag: None,
            available_semver: None,
            channel: "latest".to_string(),
        };
        assert!(should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    #[test]
    fn test_should_check_for_updates_channel_mismatch() {
        let now = current_timestamp();
        let cache = UpdateCache {
            last_checked_at: now,
            available_tag: None,
            available_semver: None,
            channel: "latest".to_string(),
        };
        assert!(should_check_for_updates(
            UpdateChannel::Next,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    #[test]
    fn test_update_cache_serialization() {
        // Test serialization/deserialization without file I/O
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = 1234567890;
        cache.available_tag = Some("v1.0.0".to_string());
        cache.available_semver = Some("1.0.0".to_string());

        let json = serde_json::to_vec(&cache).unwrap();
        let deserialized: UpdateCache = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.last_checked_at, 1234567890);
        assert_eq!(deserialized.available_tag, Some("v1.0.0".to_string()));
        assert_eq!(deserialized.available_semver, Some("1.0.0".to_string()));
        assert_eq!(deserialized.channel, "latest");
    }

    #[test]
    fn test_persist_update_state_creates_cache_object() {
        // Test that persist_update_state creates correct UpdateCache structure
        // without relying on file I/O
        let release = ChannelRelease {
            tag: "v1.5.0".to_string(),
            semver: "1.5.0".to_string(),
        };

        // Manually construct what persist_update_state would create
        let mut cache = UpdateCache::new(UpdateChannel::Next);
        cache.last_checked_at = current_timestamp();
        cache.available_tag = Some(release.tag.clone());
        cache.available_semver = Some(release.semver.clone());

        assert_eq!(cache.available_tag, Some("v1.5.0".to_string()));
        assert_eq!(cache.available_semver, Some("1.5.0".to_string()));
        assert_eq!(cache.channel, "next");
        assert!(cache.last_checked_at > 0);
    }

    #[test]
    fn test_persist_update_state_no_release_structure() {
        // Test that persist_update_state without release creates correct structure
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = current_timestamp();
        // No available_tag or available_semver set

        assert!(cache.available_tag.is_none());
        assert!(cache.available_semver.is_none());
        assert_eq!(cache.channel, "latest");
        assert!(cache.last_checked_at > 0);
    }

    #[test]
    fn test_daemon_update_check_result_debug() {
        // Verify that DaemonUpdateCheckResult derives Debug and PartialEq correctly.
        assert_eq!(
            DaemonUpdateCheckResult::NoUpdate,
            DaemonUpdateCheckResult::NoUpdate
        );
        assert_eq!(
            DaemonUpdateCheckResult::UpdateReady,
            DaemonUpdateCheckResult::UpdateReady
        );
        assert_ne!(
            DaemonUpdateCheckResult::NoUpdate,
            DaemonUpdateCheckResult::UpdateReady
        );
    }

    #[test]
    #[serial]
    fn test_check_for_update_available_no_cache_newer_version() {
        // When the cache is empty and a newer version is available, the function should
        // report UpdateReady (assuming version checks and auto-updates are enabled,
        // which is the default in debug/test builds).
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let test_checksum = "a".repeat(64);
        let mock_payload = format!(
            r#"{{"channels":{{"latest":{{"version":"v999.0.0","checksum":"{}"}}}}}}"#,
            test_checksum
        );
        // check_for_update_available uses Config::fresh() which reads the real config,
        // but fetch_release_for_channel respects mock:// URLs only in tests.
        // We can't easily inject a mock URL into Config::fresh(), so we test the
        // underlying building blocks instead:
        let release =
            fetch_release_for_channel(&format!("mock://{}", mock_payload), UpdateChannel::Latest)
                .unwrap();
        let action = determine_action(false, &release, env!("CARGO_PKG_VERSION"));
        assert_eq!(action, UpgradeAction::UpgradeAvailable);

        // Persist and verify the cache reflects the available update.
        persist_update_state(UpdateChannel::Latest, Some(&release));
        let cache = read_update_cache().unwrap();
        assert!(cache.update_available());
        assert_eq!(cache.available_semver.as_deref(), Some("999.0.0"));

        clear_test_cache_dir();
    }

    #[test]
    fn test_check_for_update_available_same_version() {
        let current = env!("CARGO_PKG_VERSION");
        let test_checksum = "a".repeat(64);
        let mock_payload = format!(
            r#"{{"channels":{{"latest":{{"version":"v{}","checksum":"{}"}}}}}}"#,
            current, test_checksum
        );
        let release =
            fetch_release_for_channel(&format!("mock://{}", mock_payload), UpdateChannel::Latest)
                .unwrap();
        let action = determine_action(false, &release, current);
        assert_eq!(action, UpgradeAction::AlreadyLatest);

        // When the action is AlreadyLatest, persist_update_state is called with None.
        // Verify that such a cache does NOT mark an update as available.
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = current_timestamp();
        // No available_tag/semver set — mirrors what persist_update_state(channel, None) does.
        assert!(!cache.update_available());
    }

    #[test]
    fn test_should_check_for_updates_skips_when_recently_checked() {
        // When the cache was recently written, should_check_for_updates returns false.
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = current_timestamp();
        assert!(!should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache),
            UpdateCheckPolicy::Default,
            false,
        ));
    }

    fn with_update_check_env(
        cache_has_update: bool,
        auto_updates_disabled: bool,
        f: impl FnOnce(),
    ) {
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = current_timestamp();
        if cache_has_update {
            cache.available_tag = Some("v99.99.99".to_string());
            cache.available_semver = Some("99.99.99".to_string());
        }
        write_update_cache(&cache);

        let patch = serde_json::json!({
            "disable_version_checks": false,
            "disable_auto_updates": auto_updates_disabled
        })
        .to_string();
        unsafe { std::env::set_var("GIT_AI_TEST_CONFIG_PATCH", &patch) };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

        unsafe { std::env::remove_var("GIT_AI_TEST_CONFIG_PATCH") };
        clear_test_cache_dir();

        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    #[serial]
    fn check_for_update_available_returns_update_ready_when_cache_has_pending_update() {
        with_update_check_env(true, false, || {
            let result = check_for_update_available().unwrap();
            assert_eq!(result, DaemonUpdateCheckResult::UpdateReady);
        });
    }

    #[test]
    #[serial]
    fn check_for_update_available_returns_no_update_when_auto_updates_disabled() {
        with_update_check_env(true, true, || {
            let result = check_for_update_available().unwrap();
            assert_eq!(result, DaemonUpdateCheckResult::NoUpdate);
        });
    }

    #[test]
    #[serial]
    fn check_for_update_available_returns_no_update_when_cache_has_no_pending_update() {
        with_update_check_env(false, false, || {
            let result = check_for_update_available().unwrap();
            assert_eq!(result, DaemonUpdateCheckResult::NoUpdate);
        });
    }
}
