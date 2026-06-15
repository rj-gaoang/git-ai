use crate::git::repository::{exec_git, parse_git_var_identity};
use crate::http;
use crate::integration::ide_mcp::resolve_x_user_id_from_mcp_config;
use crate::utils::LockFile;
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_UPLOAD_URL: &str =
    "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats";
const UPLOAD_TIMEOUT_SECS: u64 = 20;
const BEIJING_OFFSET_SECONDS: i32 = 8 * 60 * 60;
const INSTALL_TEST_SOURCE: &str = "git-ai-install";
const INSTALL_TEST_PRODUCER: &str = "git-ai-rust";
const INSTALL_TEST_INSTALLER_SCRIPT: &str = "git-ai-rust-install-test";
const INSTALL_TEST_MARKERS_FILE: &str = "install_test_uploads.json";
const INSTALL_TEST_MARKER_LOCK_FILE: &str = "install_test_uploads.lock";
const INSTALL_TEST_MARKER_SCHEMA_VERSION: u32 = 1;
const INSTALL_TEST_MARKER_LOCK_WAIT_SECS: u64 = 30;
const INSTALL_TEST_MARKER_LOCK_RETRY_MILLIS: u64 = 200;
const MAX_INSTALL_TEST_MARKERS: usize = 100;

pub fn maybe_upload_install_success() {
    if std::env::var("GIT_AI_SKIP_INSTALL_TEST_UPLOAD").as_deref() == Ok("1")
        || std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
    {
        append_install_debug_event(
            "install_test_upload_skipped",
            json!({
                "reason": "disabled_or_test_environment",
            }),
        );
        return;
    }

    let repo_workdir = current_repo_workdir();
    let identity = resolve_install_user_identity(repo_workdir.as_deref());

    let version = git_ai_cli_version();
    let url = resolve_upload_url();
    let marker_key = install_test_marker_key(&version, &identity, &url);

    let marker_paths = install_test_marker_paths();
    let marker_lock = match marker_paths.as_ref() {
        Some((_, lock_path)) => match acquire_install_test_marker_lock(lock_path) {
            Some(lock) => Some(lock),
            None => {
                append_install_debug_event(
                    "install_test_upload_skipped",
                    json!({
                        "reason": "marker_lock_timeout",
                        "gitAiVersion": version,
                        "markerKey": marker_key,
                    }),
                );
                return;
            }
        },
        None => None,
    };

    if let Some((marker_path, _)) = marker_paths.as_ref()
        && install_test_marker_exists(marker_path, &marker_key)
    {
        append_install_debug_event(
            "install_test_upload_skipped",
            json!({
                "reason": "already_uploaded_for_version_identity_and_endpoint",
                "gitAiVersion": version,
                "userIdentitySource": identity.source.as_str(),
                "markerKey": marker_key,
            }),
        );
        drop(marker_lock);
        return;
    }

    let payload = build_install_test_payload(&version, &identity);
    append_install_debug_event(
        "install_test_upload_started",
        json!({
            "url": url,
            "gitAiVersion": version,
            "gitVersion": git_version_string(),
            "osName": std::env::consts::OS,
            "osVersion": os_version_string(),
            "hasUserIdentity": true,
            "userIdentitySource": identity.source.as_str(),
        }),
    );

    match send_install_test_payload(&url, &payload, &version, &identity) {
        Ok(status_code) => {
            if let Some((marker_path, _)) = marker_paths.as_ref() {
                match record_install_test_marker(
                    marker_path,
                    &marker_key,
                    &version,
                    &identity,
                    &url,
                ) {
                    Ok(()) => {}
                    Err(error) => append_install_debug_event(
                        "install_test_upload_marker_failed",
                        json!({
                            "error": error,
                            "gitAiVersion": version,
                            "markerKey": marker_key,
                        }),
                    ),
                }
            }
            append_install_debug_event(
                "install_test_upload_succeeded",
                json!({
                    "url": url,
                    "statusCode": status_code,
                    "gitAiVersion": version,
                    "markerKey": marker_key,
                }),
            );
        }
        Err(error) => append_install_debug_event(
            "install_test_upload_failed",
            json!({
                "url": url,
                "error": error,
                "gitAiVersion": version,
                "markerKey": marker_key,
            }),
        ),
    }

    drop(marker_lock);
}

fn current_repo_workdir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    repo_workdir_for_path(&cwd)
}

fn repo_workdir_for_path(path: &Path) -> Option<PathBuf> {
    crate::git::repository::discover_repository_in_path_no_git_exec(path)
        .ok()
        .map(|repo| repo.canonical_workdir().to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallUserIdentity {
    value: String,
    source: InstallUserIdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallUserIdentitySource {
    EnvUserId,
    McpUserId,
    GitEmail,
    IpAddress,
}

impl InstallUserIdentitySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::EnvUserId => "GIT_AI_REPORT_REMOTE_USER_ID",
            Self::McpUserId => "ide_mcp_config",
            Self::GitEmail => "git_email",
            Self::IpAddress => "ip_address",
        }
    }
}

fn resolve_install_user_identity(repo_workdir: Option<&Path>) -> InstallUserIdentity {
    resolve_install_user_identity_with_ip(repo_workdir, local_ip_address)
}

fn resolve_install_user_identity_with_ip<F>(
    repo_workdir: Option<&Path>,
    ip_resolver: F,
) -> InstallUserIdentity
where
    F: FnOnce() -> Option<String>,
{
    if let Some(value) = install_identity_candidate(
        env_non_empty("GIT_AI_REPORT_REMOTE_USER_ID"),
        InstallUserIdentitySource::EnvUserId,
    ) {
        return InstallUserIdentity {
            value,
            source: InstallUserIdentitySource::EnvUserId,
        };
    }

    if let Some(value) = install_identity_candidate(
        resolve_x_user_id_from_mcp_config(repo_workdir),
        InstallUserIdentitySource::McpUserId,
    ) {
        return InstallUserIdentity {
            value,
            source: InstallUserIdentitySource::McpUserId,
        };
    }

    if let Some(value) = git_user_email(repo_workdir) {
        return InstallUserIdentity {
            value,
            source: InstallUserIdentitySource::GitEmail,
        };
    }

    InstallUserIdentity {
        value: ip_resolver().unwrap_or_else(|| "127.0.0.1".to_string()),
        source: InstallUserIdentitySource::IpAddress,
    }
}

fn install_identity_candidate(
    value: Option<String>,
    source: InstallUserIdentitySource,
) -> Option<String> {
    let value = value?;
    if is_flag_shaped_install_identity(&value) {
        append_install_debug_event(
            "install_test_identity_rejected",
            json!({
                "reason": "looks_like_powershell_parameter",
                "source": source.as_str(),
                "value": value,
            }),
        );
        None
    } else {
        Some(value)
    }
}

fn is_flag_shaped_install_identity(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('-')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|ch| ch.is_ascii_alphabetic())
}

fn git_user_email(repo_workdir: Option<&Path>) -> Option<String> {
    if let Some(repo_workdir) = repo_workdir
        && let Ok(repo) =
            crate::git::repository::discover_repository_in_path_no_git_exec(repo_workdir)
        && let Some(email) = repo
            .git_author_identity()
            .email
            .as_deref()
            .and_then(trim_email)
    {
        return Some(email.to_string());
    }

    env_first_non_empty(&["GIT_COMMITTER_EMAIL", "GIT_AUTHOR_EMAIL"])
        .and_then(|value| trim_email(&value).map(str::to_string))
        .or_else(|| git_var_email("GIT_COMMITTER_IDENT"))
        .or_else(|| git_config_email())
}

fn git_var_email(name: &str) -> Option<String> {
    let output = exec_git(&["var".to_string(), name.to_string()]).ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_git_var_identity(&stdout)
        .email
        .as_deref()
        .and_then(trim_email)
        .map(str::to_string)
}

fn git_config_email() -> Option<String> {
    let output = exec_git(&[
        "config".to_string(),
        "--get".to_string(),
        "user.email".to_string(),
    ])
    .ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    trim_email(&stdout).map(str::to_string)
}

fn trim_email(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.contains('@') {
        Some(trimmed)
    } else {
        None
    }
}

fn local_ip_address() -> Option<String> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        None
    } else {
        Some(ip.to_string())
    }
}

fn build_install_test_payload(version: &str, identity: &InstallUserIdentity) -> Value {
    let now_ms = now_epoch_ms();
    let now_text = format_install_test_timestamp(Utc::now());
    let git_version = git_version_string();
    let os_name = std::env::consts::OS.to_string();
    let os_arch = std::env::consts::ARCH.to_string();
    let os_version = os_version_string();
    let commit_sha = synthetic_commit_sha(version, &identity.value, now_ms);
    let identity_source = identity.source.as_str();

    json!({
        "repoUrl": "git-ai-install-test",
        "projectName": "git-ai-install",
        "branch": "install-success",
        "source": INSTALL_TEST_SOURCE,
        "reviewDocumentId": null,
        "authorshipSchemaVersion": "authorship/3.0.0",
        "clientContext": {
            "gitAiCliVersion": version,
            "installTestProducer": INSTALL_TEST_PRODUCER,
            "installerScript": INSTALL_TEST_INSTALLER_SCRIPT,
            "gitAiPluginVersion": plugin_version(),
            "ideName": ide_name(),
            "ideVersion": ide_version(),
            "gitVersion": git_version,
            "osName": os_name,
            "osVersion": os_version,
            "osArch": os_arch,
            "installUserIdentity": identity.value,
            "installUserIdentitySource": identity_source,
        },
        "commits": [{
            "commitSha": commit_sha,
            "commitMessage": format!("git-ai install success test ({version})"),
            "author": identity.value,
            "timestamp": now_text,
            "hasAuthorshipNote": false,
            "stats": {
                "humanAdditions": 0,
                "unknownAdditions": 0,
                "mixedAdditions": 0,
                "aiAdditions": 0,
                "aiAccepted": 0,
                "totalAiAdditions": 0,
                "totalAiDeletions": 0,
                "gitDiffAddedLines": 0,
                "gitDiffDeletedLines": 0,
                "timeWaitingForAi": 0,
                "files": [],
                "toolModelBreakdown": [{
                    "tool": "git-ai-installer",
                    "model": version,
                    "aiAdditions": 0,
                    "aiAccepted": 0,
                    "mixedAdditions": 0,
                    "totalAiAdditions": 0,
                    "totalAiDeletions": 0,
                    "timeWaitingForAi": 0,
                }],
            },
            "prompts": [{
                "promptHash": commit_sha,
                "tool": "git-ai-installer",
                "model": version,
                "humanAuthor": identity.value,
                "promptText": format!("git-ai install success test. gitAiVersion={version}"),
                "messages": [],
                "messagesUrl": null,
                "totalAdditions": 0,
                "totalDeletions": 0,
                "acceptedLines": 0,
                "overridenLines": 0,
                "customAttributes": {
                    "gitAiVersion": version,
                    "gitVersion": git_version,
                    "osName": os_name,
                    "osVersion": os_version,
                    "osArch": os_arch,
                    "installTest": "true",
                    "source": INSTALL_TEST_SOURCE,
                    "installTestProducer": INSTALL_TEST_PRODUCER,
                    "installerScript": INSTALL_TEST_INSTALLER_SCRIPT,
                    "installUserIdentity": identity.value,
                    "installUserIdentitySource": identity_source,
                },
            }],
        }],
    })
}

fn send_install_test_payload(
    url: &str,
    payload: &Value,
    version: &str,
    identity: &InstallUserIdentity,
) -> Result<u16, String> {
    let agent = http::build_agent(Some(UPLOAD_TIMEOUT_SECS));
    let mut request = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("User-Agent", &format!("git-ai-install-test/{version}"))
        .set("X-GIT-AI-INSTALL-PRODUCER", INSTALL_TEST_PRODUCER)
        .set("X-GIT-AI-INSTALLER-SCRIPT", INSTALL_TEST_INSTALLER_SCRIPT)
        .set("X-Distinct-ID", &crate::config::get_or_create_distinct_id())
        .set("X-USER-ID", &identity.value)
        .set("X-GIT-AI-INSTALL-IDENTITY-SOURCE", identity.source.as_str());

    match identity.source {
        InstallUserIdentitySource::GitEmail => {
            request = request.set("X-USER-EMAIL", &identity.value);
        }
        InstallUserIdentitySource::IpAddress => {
            request = request.set("X-USER-IP", &identity.value);
        }
        InstallUserIdentitySource::EnvUserId | InstallUserIdentitySource::McpUserId => {}
    }

    if let Some(api_key) = env_non_empty("GIT_AI_REPORT_REMOTE_API_KEY") {
        request = request.set("Authorization", &format!("Bearer {api_key}"));
    }

    let body = serde_json::to_string(payload).map_err(|error| error.to_string())?;
    let response = http::send_with_body(request, &body).map_err(|error| error.to_string())?;
    if (200..300).contains(&response.status_code) {
        inspect_backend_response_body(response.as_bytes())?;
        Ok(response.status_code)
    } else {
        Err(format!(
            "HTTP {}: {}",
            response.status_code,
            response_body_excerpt(response.as_bytes())
        ))
    }
}

fn resolve_upload_url() -> String {
    if let Some(url) = env_non_empty("GIT_AI_REPORT_REMOTE_URL") {
        return url;
    }

    let endpoint = env_non_empty("GIT_AI_REPORT_REMOTE_ENDPOINT");
    let path = env_non_empty("GIT_AI_REPORT_REMOTE_PATH");
    if let (Some(endpoint), Some(path)) = (endpoint.as_ref(), path.as_ref()) {
        return format!(
            "{}/{}",
            endpoint.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
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

fn env_first_non_empty(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| env_non_empty(name))
}

fn git_ai_cli_version() -> String {
    if cfg!(debug_assertions) {
        format!("{} (debug)", env!("CARGO_PKG_VERSION"))
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

fn git_version_string() -> Option<String> {
    let output = exec_git(&["--version".to_string()]).ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(
            trimmed
                .strip_prefix("git version ")
                .unwrap_or(trimmed)
                .trim()
                .to_string(),
        )
    }
}

fn os_version_string() -> Option<String> {
    #[cfg(windows)]
    let output = Command::new("cmd").args(["/C", "ver"]).output().ok()?;

    #[cfg(not(windows))]
    let output = Command::new("uname").args(["-sr"]).output().ok()?;

    let text = String::from_utf8(output.stdout).ok()?;
    let version = text.trim().trim_matches('[').trim_matches(']').trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
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

fn format_install_test_timestamp(now: DateTime<Utc>) -> String {
    let beijing_offset = FixedOffset::east_opt(BEIJING_OFFSET_SECONDS)
        .expect("UTC+08:00 should always be a valid fixed offset");
    now.with_timezone(&beijing_offset)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn synthetic_commit_sha(version: &str, user_id: &str, now_ms: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"git-ai-install-test");
    hasher.update(version.as_bytes());
    hasher.update(user_id.as_bytes());
    hasher.update(now_ms.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
        .chars()
        .take(40)
        .collect()
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn inspect_backend_response_body(body: &[u8]) -> Result<(), String> {
    if body.is_empty() {
        return Ok(());
    }

    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };

    let Some(code) = value.get("code").and_then(Value::as_i64) else {
        return Ok(());
    };
    if code == 200 {
        Ok(())
    } else if let Some(message) = value.get("msg").and_then(Value::as_str) {
        Err(format!("backend returned code {code}: {message}"))
    } else {
        Err(format!("backend returned code {code}"))
    }
}

fn response_body_excerpt(body: &[u8]) -> String {
    std::str::from_utf8(body)
        .map(|text| text.chars().take(200).collect::<String>())
        .unwrap_or_else(|_| format!("<{} bytes non-utf8>", body.len()))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct InstallTestMarkers {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    uploads: Vec<InstallTestMarker>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallTestMarker {
    key: String,
    version: String,
    identity_source: String,
    identity_hash: String,
    upload_url_hash: String,
    producer: String,
    installer_script: String,
    uploaded_at_ms: u64,
}

fn install_test_marker_paths() -> Option<(PathBuf, PathBuf)> {
    let internal_dir = crate::config::internal_dir_path()?;
    Some((
        internal_dir.join(INSTALL_TEST_MARKERS_FILE),
        internal_dir.join(INSTALL_TEST_MARKER_LOCK_FILE),
    ))
}

fn acquire_install_test_marker_lock(lock_path: &Path) -> Option<LockFile> {
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let started_at = Instant::now();
    let max_wait = Duration::from_secs(INSTALL_TEST_MARKER_LOCK_WAIT_SECS);
    let retry = Duration::from_millis(INSTALL_TEST_MARKER_LOCK_RETRY_MILLIS);
    loop {
        if let Some(lock) = LockFile::try_acquire(lock_path) {
            return Some(lock);
        }
        if started_at.elapsed() >= max_wait {
            return None;
        }
        std::thread::sleep(retry);
    }
}

fn install_test_marker_exists(marker_path: &Path, marker_key: &str) -> bool {
    read_install_test_markers(marker_path)
        .uploads
        .iter()
        .any(|marker| marker.key == marker_key)
}

fn record_install_test_marker(
    marker_path: &Path,
    marker_key: &str,
    version: &str,
    identity: &InstallUserIdentity,
    url: &str,
) -> Result<(), String> {
    if let Some(parent) = marker_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let mut markers = read_install_test_markers(marker_path);
    markers.schema_version = INSTALL_TEST_MARKER_SCHEMA_VERSION;
    markers.uploads.retain(|marker| marker.key != marker_key);
    markers.uploads.push(InstallTestMarker {
        key: marker_key.to_string(),
        version: version.to_string(),
        identity_source: identity.source.as_str().to_string(),
        identity_hash: short_sha256_hex(&identity.value),
        upload_url_hash: short_sha256_hex(url),
        producer: INSTALL_TEST_PRODUCER.to_string(),
        installer_script: INSTALL_TEST_INSTALLER_SCRIPT.to_string(),
        uploaded_at_ms: now_epoch_ms(),
    });
    if markers.uploads.len() > MAX_INSTALL_TEST_MARKERS {
        let remove_count = markers.uploads.len() - MAX_INSTALL_TEST_MARKERS;
        markers.uploads.drain(0..remove_count);
    }

    let json = serde_json::to_vec_pretty(&markers).map_err(|error| error.to_string())?;
    fs::write(marker_path, json).map_err(|error| error.to_string())
}

fn read_install_test_markers(marker_path: &Path) -> InstallTestMarkers {
    let Ok(bytes) = fs::read(marker_path) else {
        return InstallTestMarkers {
            schema_version: INSTALL_TEST_MARKER_SCHEMA_VERSION,
            uploads: Vec::new(),
        };
    };
    let mut markers = serde_json::from_slice::<InstallTestMarkers>(&bytes).unwrap_or_default();
    if markers.schema_version == 0 {
        markers.schema_version = INSTALL_TEST_MARKER_SCHEMA_VERSION;
    }
    markers
}

fn install_test_marker_key(version: &str, identity: &InstallUserIdentity, url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"git-ai-install-test-marker-v1");
    hasher.update(INSTALL_TEST_PRODUCER.as_bytes());
    hasher.update(INSTALL_TEST_INSTALLER_SCRIPT.as_bytes());
    hasher.update(version.as_bytes());
    hasher.update(identity.source.as_str().as_bytes());
    hasher.update(identity.value.as_bytes());
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn short_sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
        .chars()
        .take(16)
        .collect()
}

fn append_install_debug_event(event: &str, mut value: Value) {
    if let Value::Object(map) = &mut value {
        map.insert("event".to_string(), Value::String(event.to_string()));
    }
    crate::diagnostics::append_debug_event(event, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_GUARD_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn install_test_payload_contains_required_environment_versions() {
        let identity = InstallUserIdentity {
            value: "real-user-123".to_string(),
            source: InstallUserIdentitySource::McpUserId,
        };
        let payload = build_install_test_payload("2.2.24", &identity);
        assert_eq!(payload["clientContext"]["gitAiCliVersion"], "2.2.24");
        assert_eq!(
            payload["clientContext"]["installTestProducer"],
            INSTALL_TEST_PRODUCER
        );
        assert_eq!(
            payload["clientContext"]["installerScript"],
            INSTALL_TEST_INSTALLER_SCRIPT
        );
        assert!(payload["clientContext"].get("gitVersion").is_some());
        assert_eq!(payload["clientContext"]["osName"], std::env::consts::OS);
        assert!(payload["clientContext"].get("osVersion").is_some());
        assert_eq!(
            payload["clientContext"]["installUserIdentity"],
            "real-user-123"
        );
        assert_eq!(
            payload["clientContext"]["installUserIdentitySource"],
            "ide_mcp_config"
        );
        assert_eq!(payload["commits"][0]["author"], "real-user-123");
        let commit_sha = payload["commits"][0]["commitSha"].as_str().unwrap();
        let timestamp = payload["commits"][0]["timestamp"].as_str().unwrap();
        assert_eq!(commit_sha.len(), 40);
        assert!(timestamp.contains(' '));
        assert!(!timestamp.contains('T'));
        assert_eq!(
            payload["commits"][0]["prompts"][0]["humanAuthor"],
            "real-user-123"
        );
        assert_eq!(
            payload["commits"][0]["prompts"][0]["customAttributes"]["installTest"],
            "true"
        );
        assert_eq!(payload["source"], INSTALL_TEST_SOURCE);
        assert_eq!(
            payload["commits"][0]["prompts"][0]["customAttributes"]["source"],
            INSTALL_TEST_SOURCE
        );
        assert_eq!(
            payload["commits"][0]["prompts"][0]["customAttributes"]["installTestProducer"],
            INSTALL_TEST_PRODUCER
        );
        assert_eq!(
            payload["commits"][0]["prompts"][0]["customAttributes"]["installerScript"],
            INSTALL_TEST_INSTALLER_SCRIPT
        );
    }

    #[test]
    fn install_test_timestamp_matches_backend_format() {
        let formatted = format_install_test_timestamp(
            chrono::DateTime::parse_from_rfc3339("2026-06-14T04:48:37.873568900+00:00")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert_eq!(formatted, "2026-06-14 12:48:37");
    }

    #[test]
    fn install_test_marker_key_is_stable_for_same_version_identity_and_url() {
        let identity = InstallUserIdentity {
            value: "real-user-123".to_string(),
            source: InstallUserIdentitySource::McpUserId,
        };

        let first = install_test_marker_key("2.2.32", &identity, "https://example.test/upload");
        let second = install_test_marker_key("2.2.32", &identity, "https://example.test/upload");
        let different_version =
            install_test_marker_key("2.2.33", &identity, "https://example.test/upload");

        assert_eq!(first, second);
        assert_ne!(first, different_version);
    }

    #[test]
    fn install_test_marker_records_success_once() {
        let temp_dir = tempfile::tempdir().unwrap();
        let marker_path = temp_dir.path().join(INSTALL_TEST_MARKERS_FILE);
        let identity = InstallUserIdentity {
            value: "real-user-123".to_string(),
            source: InstallUserIdentitySource::McpUserId,
        };
        let url = "https://example.test/upload";
        let key = install_test_marker_key("2.2.32", &identity, url);

        assert!(!install_test_marker_exists(&marker_path, &key));
        record_install_test_marker(&marker_path, &key, "2.2.32", &identity, url).unwrap();
        assert!(install_test_marker_exists(&marker_path, &key));

        record_install_test_marker(&marker_path, &key, "2.2.32", &identity, url).unwrap();
        let markers = read_install_test_markers(&marker_path);
        assert_eq!(markers.uploads.len(), 1);
        assert_eq!(markers.uploads[0].key, key);
        assert_eq!(markers.uploads[0].producer, INSTALL_TEST_PRODUCER);
        assert_eq!(
            markers.uploads[0].installer_script,
            INSTALL_TEST_INSTALLER_SCRIPT
        );
    }

    #[test]
    fn resolves_user_id_from_current_repo_mcp_config() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        write_minimal_git_repo(temp_dir.path(), None);
        std::fs::create_dir(temp_dir.path().join(".vscode")).unwrap();
        std::fs::write(
            temp_dir.path().join(".vscode").join("mcp.json"),
            r#"{"servers":{"codereview-mcp":{"headers":{"X-USER-ID":"repo-user-456"}}}}"#,
        )
        .unwrap();

        unsafe {
            std::env::remove_var("GIT_AI_REPORT_REMOTE_USER_ID");
            std::env::remove_var("GIT_AI_VSCODE_MCP_CONFIG_PATH");
            std::env::remove_var("GIT_AI_IDEA_MCP_CONFIG_PATH");
            std::env::set_var("APPDATA", temp_dir.path().join("appdata"));
            std::env::set_var("LOCALAPPDATA", temp_dir.path().join("localappdata"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var(
                "GIT_CONFIG_GLOBAL",
                temp_dir.path().join("empty-global-gitconfig"),
            );
            std::env::remove_var("GIT_COMMITTER_EMAIL");
            std::env::remove_var("GIT_AUTHOR_EMAIL");
        }

        let repo_workdir = repo_workdir_for_path(temp_dir.path());
        let result = resolve_install_user_identity_with_ip(repo_workdir.as_deref(), || {
            Some("192.0.2.10".to_string())
        });
        assert_eq!(
            result,
            InstallUserIdentity {
                value: "repo-user-456".to_string(),
                source: InstallUserIdentitySource::McpUserId,
            }
        );
    }

    #[test]
    fn rejects_powershell_switch_identity_from_env() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        write_minimal_git_repo(temp_dir.path(), None);

        unsafe {
            std::env::set_var("GIT_AI_DEBUG", "0");
            std::env::set_var("GIT_AI_REPORT_REMOTE_USER_ID", "-BestEffort");
            std::env::remove_var("GIT_AI_VSCODE_MCP_CONFIG_PATH");
            std::env::remove_var("GIT_AI_IDEA_MCP_CONFIG_PATH");
            std::env::set_var("APPDATA", temp_dir.path().join("appdata"));
            std::env::set_var("LOCALAPPDATA", temp_dir.path().join("localappdata"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var(
                "GIT_CONFIG_GLOBAL",
                temp_dir.path().join("empty-global-gitconfig"),
            );
            std::env::remove_var("GIT_COMMITTER_EMAIL");
            std::env::remove_var("GIT_AUTHOR_EMAIL");
        }

        let repo_workdir = repo_workdir_for_path(temp_dir.path());
        let result = resolve_install_user_identity_with_ip(repo_workdir.as_deref(), || {
            Some("192.0.2.10".to_string())
        });
        assert_eq!(
            result,
            InstallUserIdentity {
                value: "192.0.2.10".to_string(),
                source: InstallUserIdentitySource::IpAddress,
            }
        );
    }

    #[test]
    fn falls_back_to_mcp_when_env_identity_is_powershell_switch() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        write_minimal_git_repo(temp_dir.path(), None);
        std::fs::create_dir(temp_dir.path().join(".vscode")).unwrap();
        std::fs::write(
            temp_dir.path().join(".vscode").join("mcp.json"),
            r#"{"servers":{"codereview-mcp":{"headers":{"X-USER-ID":"repo-user-456"}}}}"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("GIT_AI_DEBUG", "0");
            std::env::set_var("GIT_AI_REPORT_REMOTE_USER_ID", "-BestEffort");
            std::env::remove_var("GIT_AI_VSCODE_MCP_CONFIG_PATH");
            std::env::remove_var("GIT_AI_IDEA_MCP_CONFIG_PATH");
            std::env::set_var("APPDATA", temp_dir.path().join("appdata"));
            std::env::set_var("LOCALAPPDATA", temp_dir.path().join("localappdata"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var(
                "GIT_CONFIG_GLOBAL",
                temp_dir.path().join("empty-global-gitconfig"),
            );
            std::env::remove_var("GIT_COMMITTER_EMAIL");
            std::env::remove_var("GIT_AUTHOR_EMAIL");
        }

        let repo_workdir = repo_workdir_for_path(temp_dir.path());
        let result = resolve_install_user_identity_with_ip(repo_workdir.as_deref(), || {
            Some("192.0.2.10".to_string())
        });
        assert_eq!(
            result,
            InstallUserIdentity {
                value: "repo-user-456".to_string(),
                source: InstallUserIdentitySource::McpUserId,
            }
        );
    }

    #[test]
    fn rejects_powershell_switch_identity_from_mcp_config() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        write_minimal_git_repo(temp_dir.path(), None);
        std::fs::create_dir(temp_dir.path().join(".vscode")).unwrap();
        std::fs::write(
            temp_dir.path().join(".vscode").join("mcp.json"),
            r#"{"servers":{"codereview-mcp":{"headers":{"X-USER-ID":"-GitPath"}}}}"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("GIT_AI_DEBUG", "0");
            std::env::remove_var("GIT_AI_REPORT_REMOTE_USER_ID");
            std::env::remove_var("GIT_AI_VSCODE_MCP_CONFIG_PATH");
            std::env::remove_var("GIT_AI_IDEA_MCP_CONFIG_PATH");
            std::env::set_var("APPDATA", temp_dir.path().join("appdata"));
            std::env::set_var("LOCALAPPDATA", temp_dir.path().join("localappdata"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var(
                "GIT_CONFIG_GLOBAL",
                temp_dir.path().join("empty-global-gitconfig"),
            );
            std::env::remove_var("GIT_COMMITTER_EMAIL");
            std::env::remove_var("GIT_AUTHOR_EMAIL");
        }

        let repo_workdir = repo_workdir_for_path(temp_dir.path());
        let result = resolve_install_user_identity_with_ip(repo_workdir.as_deref(), || {
            Some("192.0.2.10".to_string())
        });
        assert_eq!(
            result,
            InstallUserIdentity {
                value: "192.0.2.10".to_string(),
                source: InstallUserIdentitySource::IpAddress,
            }
        );
    }

    #[test]
    fn falls_back_to_git_email_when_user_id_is_missing() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        write_minimal_git_repo(temp_dir.path(), Some("gitlab-user@example.com"));

        unsafe {
            std::env::remove_var("GIT_AI_REPORT_REMOTE_USER_ID");
            std::env::remove_var("GIT_AI_VSCODE_MCP_CONFIG_PATH");
            std::env::remove_var("GIT_AI_IDEA_MCP_CONFIG_PATH");
            std::env::set_var("APPDATA", temp_dir.path().join("appdata"));
            std::env::set_var("LOCALAPPDATA", temp_dir.path().join("localappdata"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var(
                "GIT_CONFIG_GLOBAL",
                temp_dir.path().join("empty-global-gitconfig"),
            );
            std::env::remove_var("GIT_COMMITTER_EMAIL");
            std::env::remove_var("GIT_AUTHOR_EMAIL");
        }

        let repo_workdir = repo_workdir_for_path(temp_dir.path());
        let result = resolve_install_user_identity_with_ip(repo_workdir.as_deref(), || {
            Some("192.0.2.10".to_string())
        });
        assert_eq!(
            result,
            InstallUserIdentity {
                value: "gitlab-user@example.com".to_string(),
                source: InstallUserIdentitySource::GitEmail,
            }
        );
    }

    #[test]
    fn falls_back_to_ip_address_when_user_id_and_email_are_missing() {
        let _guard = EnvGuard::new();
        let temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            std::env::remove_var("GIT_AI_REPORT_REMOTE_USER_ID");
            std::env::remove_var("GIT_AI_VSCODE_MCP_CONFIG_PATH");
            std::env::remove_var("GIT_AI_IDEA_MCP_CONFIG_PATH");
            std::env::set_var("APPDATA", temp_dir.path().join("appdata"));
            std::env::set_var("LOCALAPPDATA", temp_dir.path().join("localappdata"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var(
                "GIT_CONFIG_GLOBAL",
                temp_dir.path().join("empty-global-gitconfig"),
            );
            std::env::remove_var("GIT_COMMITTER_EMAIL");
            std::env::remove_var("GIT_AUTHOR_EMAIL");
        }

        let result = resolve_install_user_identity_with_ip(None, || Some("192.0.2.10".to_string()));
        assert_eq!(
            result,
            InstallUserIdentity {
                value: "192.0.2.10".to_string(),
                source: InstallUserIdentitySource::IpAddress,
            }
        );
    }

    fn write_minimal_git_repo(workdir: &Path, email: Option<&str>) {
        let git_dir = workdir.join(".git");
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();
        std::fs::create_dir_all(git_dir.join("refs").join("heads")).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let email_config = email
            .map(|email| format!("\n[user]\n\tname = Test User\n\temail = {email}\n"))
            .unwrap_or_default();
        std::fs::write(
            git_dir.join("config"),
            format!("[core]\n\tbare = false\n{email_config}"),
        )
        .unwrap();
    }

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        vars: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let vars = [
                "GIT_AI_REPORT_REMOTE_USER_ID",
                "GIT_AI_DEBUG",
                "GIT_AI_VSCODE_MCP_CONFIG_PATH",
                "GIT_AI_IDEA_MCP_CONFIG_PATH",
                "APPDATA",
                "LOCALAPPDATA",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_CONFIG_GLOBAL",
                "GIT_COMMITTER_EMAIL",
                "GIT_AUTHOR_EMAIL",
            ];
            Self {
                _lock: ENV_GUARD_LOCK.lock().unwrap_or_else(|err| err.into_inner()),
                vars: vars
                    .into_iter()
                    .map(|var| (var, std::env::var_os(var)))
                    .collect(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, value) in &self.vars {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(var, value),
                        None => std::env::remove_var(var),
                    }
                }
            }
        }
    }
}
