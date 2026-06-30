use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint_agent::presets::{
    KnownHumanEdit, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    StreamSource, UntrackedEdit,
};
use crate::config;
use crate::daemon::checkpoint::PreparedPathRole;
use crate::error::GitAiError;
use crate::git::repo_state::{
    git_dir_for_worktree, read_head_state_for_worktree, worktree_root_for_path,
};
use crate::git::repository::discover_repository_in_path_no_git_exec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BaseCommit {
    Sha(String),
    Initial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: PathBuf,
    pub content: Option<String>,
    pub repo_work_dir: PathBuf,
    pub base_commit: BaseCommit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub stream_source: Option<StreamSource>,
    pub metadata: HashMap<String, String>,
}

#[derive(Serialize)]
struct CheckpointDebugLogEntry<'a> {
    timestamp: String,
    preset_name: &'a str,
    hook_input: &'a str,
    trace_id: &'a str,
    event_count: usize,
    requests: &'a [CheckpointRequest],
}

struct RepoContext {
    repo_work_dir: PathBuf,
    base_commit: BaseCommit,
    unmerged_paths: std::collections::HashSet<PathBuf>,
}

const MAX_CHECKPOINT_FILES: usize = 1000;
const DEBUG_PATH_SAMPLE_LIMIT: usize = 20;
const MAX_BASH_CHECKPOINT_FILES: usize = 200;
const MAX_BASH_PRE_BASELINE_FILES: usize = 200;
const MAX_BASH_CHECKPOINT_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn should_skip_checkpoint_path(path: &Path) -> Option<&'static str> {
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        let name = name.as_ref();
        if name == ".git" {
            return Some("git_metadata");
        }
        if name == ".idea" || name == ".vscode" {
            return Some("ide_metadata");
        }
    }
    None
}

fn should_skip_bash_checkpoint_path(path: &Path) -> Option<&'static str> {
    if let Some(reason) = should_skip_checkpoint_path(path) {
        return Some(reason);
    }
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        let name = name.as_ref();
        if matches!(
            name,
            ".git-ai"
                | "checkpoint-debug-logs"
                | ".code-review-graph"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".next"
        ) {
            return Some("generated_or_runtime_artifact");
        }
        if name == "logs" {
            return Some("runtime_log");
        }
    }
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str())
        && matches!(ext.to_ascii_lowercase().as_str(), "log" | "jsonl")
    {
        return Some("runtime_log");
    }
    None
}

fn read_checkpoint_file_content(path: &Path) -> Option<String> {
    if path.exists() {
        let bytes = fs::read(path).ok()?;
        if bytes.iter().any(|byte| *byte == 0) {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Some(String::new())
    }
}

fn has_active_merge_state(git_dir: &Path) -> bool {
    git_dir.join("MERGE_HEAD").exists()
        || git_dir.join("CHERRY_PICK_HEAD").exists()
        || git_dir.join("rebase-merge").exists()
        || git_dir.join("rebase-apply").exists()
}

fn get_unmerged_paths_via_git(repo_work_dir: &Path) -> std::collections::HashSet<PathBuf> {
    use crate::git::repository::exec_git_allow_nonzero;
    let args = vec![
        "-C".to_string(),
        repo_work_dir.to_string_lossy().to_string(),
        "ls-files".to_string(),
        "-u".to_string(),
    ];
    let output = match exec_git_allow_nonzero(&args) {
        Ok(o) => o,
        Err(_) => return std::collections::HashSet::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split('\t').nth(1))
        .map(|path| repo_work_dir.join(path))
        .collect()
}

fn build_checkpoint_files(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFile>, GitAiError> {
    build_checkpoint_files_with_policy(file_paths, false)
}

fn build_bash_checkpoint_files(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFile>, GitAiError> {
    build_checkpoint_files_with_policy(file_paths, true)
}

fn build_checkpoint_files_with_policy(
    file_paths: &[PathBuf],
    bash_derived: bool,
) -> Result<Vec<CheckpointFile>, GitAiError> {
    let perf = std::env::var("GIT_AI_DEBUG_PERFORMANCE").is_ok_and(|v| !v.is_empty() && v != "0");

    if file_paths.len() > MAX_CHECKPOINT_FILES {
        tracing::warn!(
            "build_checkpoint_files called with {} paths (max {}); truncating",
            file_paths.len(),
            MAX_CHECKPOINT_FILES,
        );
    }
    let capped_paths = &file_paths[..file_paths.len().min(MAX_CHECKPOINT_FILES)];

    let mut repo_cache: HashMap<PathBuf, RepoContext> = HashMap::new();
    let mut files = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for path in capped_paths {
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }
        let skip_reason = if bash_derived {
            should_skip_bash_checkpoint_path(path)
        } else {
            should_skip_checkpoint_path(path)
        };
        if let Some(reason) = skip_reason {
            crate::diagnostics::append_debug_event(
                "checkpoint_file_path_skipped",
                serde_json::json!({
                    "path": path.to_string_lossy().replace('\\', "/"),
                    "reason": reason,
                }),
            );
            continue;
        }
        if !seen_paths.insert(checkpoint_path_lookup_key(path)) {
            continue;
        }

        let ctx = {
            let t_discover = std::time::Instant::now();
            let Some(repo_work_dir) = worktree_root_for_path(path) else {
                crate::diagnostics::append_debug_event(
                    "checkpoint_file_path_skipped",
                    serde_json::json!({
                        "path": path.to_string_lossy().replace('\\', "/"),
                        "reason": "no_git_repository",
                    }),
                );
                continue;
            };
            if !repo_cache.contains_key(&repo_work_dir) {
                let t_head = std::time::Instant::now();
                let base_commit = match read_head_state_for_worktree(&repo_work_dir) {
                    Some(state) => match state.head {
                        Some(sha) => BaseCommit::Sha(sha),
                        None => BaseCommit::Initial,
                    },
                    None => BaseCommit::Initial,
                };
                let head_ms = t_head.elapsed().as_secs_f64() * 1000.0;

                let t_unmerged = std::time::Instant::now();
                let unmerged_paths = if let Some(git_dir) = git_dir_for_worktree(&repo_work_dir)
                    && has_active_merge_state(&git_dir)
                {
                    get_unmerged_paths_via_git(&repo_work_dir)
                } else {
                    std::collections::HashSet::new()
                };
                let unmerged_ms = t_unmerged.elapsed().as_secs_f64() * 1000.0;

                if perf {
                    eprintln!(
                        "[perf] build_checkpoint_files: discover={:.1}ms head={:.1}ms unmerged={:.1}ms (repo={})",
                        t_discover.elapsed().as_secs_f64() * 1000.0,
                        head_ms,
                        unmerged_ms,
                        repo_work_dir.display(),
                    );
                }

                let key = repo_work_dir.clone();
                repo_cache.insert(
                    key,
                    RepoContext {
                        repo_work_dir: repo_work_dir.clone(),
                        base_commit,
                        unmerged_paths,
                    },
                );
            }
            repo_cache.get(&repo_work_dir).unwrap()
        };

        if ctx.unmerged_paths.contains(path) {
            continue;
        }
        if let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
            && bash_derived
            && metadata.len() > MAX_BASH_CHECKPOINT_FILE_BYTES
        {
            crate::diagnostics::append_debug_event(
                "checkpoint_file_path_skipped",
                serde_json::json!({
                    "path": path.to_string_lossy().replace('\\', "/"),
                    "reason": "file_too_large",
                    "bytes": metadata.len(),
                    "byteLimit": MAX_BASH_CHECKPOINT_FILE_BYTES,
                }),
            );
            continue;
        }

        let t_read = std::time::Instant::now();
        let content = read_checkpoint_file_content(path);
        if perf {
            eprintln!(
                "[perf] build_checkpoint_files: read_file={:.1}ms (path={}, size={})",
                t_read.elapsed().as_secs_f64() * 1000.0,
                path.display(),
                content.as_ref().map(|c| c.len()).unwrap_or(0),
            );
        }

        files.push(CheckpointFile {
            path: path.clone(),
            content,
            repo_work_dir: ctx.repo_work_dir.clone(),
            base_commit: ctx.base_commit.clone(),
        });
    }

    Ok(files)
}

fn apply_dirty_file_overrides(
    files: &mut [CheckpointFile],
    dirty_files: &HashMap<PathBuf, String>,
) {
    let normalized_dirty_files: HashMap<String, String> = dirty_files
        .iter()
        .map(|(path, content)| (checkpoint_path_lookup_key(path), content.clone()))
        .collect();

    for f in files {
        if let Some(override_content) = dirty_files.get(&f.path).cloned().or_else(|| {
            normalized_dirty_files
                .get(&checkpoint_path_lookup_key(&f.path))
                .cloned()
        }) {
            f.content = Some(override_content);
        }
    }
}

fn checkpoint_path_lookup_key(path: &Path) -> String {
    let normalized_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = normalized_path.to_string_lossy().replace('\\', "/");

    #[cfg(windows)]
    {
        normalized.to_ascii_lowercase()
    }

    #[cfg(not(windows))]
    {
        normalized
    }
}

fn normalize_bash_relative_paths(repo_work_dir: &Path, paths: &[String]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| {
            let joined = repo_work_dir.join(p);
            fs::canonicalize(&joined).unwrap_or(joined)
        })
        .collect()
}

fn sorted_display_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut values: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    values.sort();
    values
}

fn sorted_display_path_sample(paths: &[PathBuf]) -> Vec<String> {
    let mut values = sorted_display_paths(paths);
    values.truncate(DEBUG_PATH_SAMPLE_LIMIT);
    values
}

fn sorted_string_path_sample(paths: &[String]) -> Vec<String> {
    let mut values = paths.to_vec();
    values.sort();
    values.truncate(DEBUG_PATH_SAMPLE_LIMIT);
    values
}

fn omitted_path_count(total: usize, sample_len: usize) -> usize {
    total.saturating_sub(sample_len)
}

fn resolve_bash_detected_paths_for_checkpoint(
    repo_work_dir: &Path,
    paths: &[String],
    skip_if_too_many: bool,
    fallback_reason: &mut Option<String>,
    error_message: &mut Option<String>,
) -> (usize, Vec<String>, Vec<PathBuf>) {
    let detected_path_count = paths.len();
    let detected_path_sample = sorted_string_path_sample(paths);
    let resolved_paths: Vec<PathBuf> = normalize_bash_relative_paths(repo_work_dir, paths)
        .into_iter()
        .filter(|path| should_skip_bash_checkpoint_path(path).is_none())
        .collect();
    if skip_if_too_many && resolved_paths.len() > MAX_BASH_CHECKPOINT_FILES {
        *fallback_reason = Some(format!(
            "{}_too_many_paths",
            fallback_reason.as_deref().unwrap_or("git_status_fallback")
        ));
        *error_message = Some(format!(
            "bash post checkpoint detected {} candidate paths ({} after filtering), exceeding MAX_BASH_CHECKPOINT_FILES={}; skipping noisy checkpoint",
            paths.len(),
            resolved_paths.len(),
            MAX_BASH_CHECKPOINT_FILES
        ));
        return (detected_path_count, detected_path_sample, vec![]);
    }

    (detected_path_count, detected_path_sample, resolved_paths)
}

fn merge_paths_from_dirty_files(
    file_paths: &mut Vec<PathBuf>,
    dirty_files: &HashMap<PathBuf, String>,
) {
    let mut seen: std::collections::HashSet<String> = file_paths
        .iter()
        .map(|path| checkpoint_path_lookup_key(path))
        .collect();

    let mut dirty_paths: Vec<PathBuf> = dirty_files.keys().cloned().collect();
    dirty_paths.sort();
    for path in dirty_paths {
        let key = checkpoint_path_lookup_key(&path);
        if seen.insert(key) {
            file_paths.push(path);
        }
    }

    file_paths.sort();
}

pub fn execute_preset_checkpoint(
    preset_name: &str,
    hook_input: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let perf = std::env::var("GIT_AI_DEBUG_PERFORMANCE").is_ok_and(|v| !v.is_empty() && v != "0");
    let t0 = std::time::Instant::now();

    let trace_id = generate_trace_id();
    let preset = super::presets::resolve_preset(preset_name)?;
    let events = preset.parse(hook_input, &trace_id)?;
    let events_len = events.len();

    if perf {
        eprintln!(
            "[perf] orchestrator: parse={:.1}ms (events={})",
            t0.elapsed().as_secs_f64() * 1000.0,
            events_len,
        );
    }

    let mut requests = Vec::new();
    for event in events {
        let t_event = std::time::Instant::now();
        let event_name = format!("{:?}", std::mem::discriminant(&event));
        let new_requests = execute_event(event, preset_name)?;
        if perf {
            eprintln!(
                "[perf] orchestrator: execute_event({})={:.1}ms (requests={})",
                event_name,
                t_event.elapsed().as_secs_f64() * 1000.0,
                new_requests.len(),
            );
        }
        requests.extend(new_requests);
    }

    if config::Config::get()
        .get_feature_flags()
        .checkpoint_debug_log
    {
        write_checkpoint_debug_log(preset_name, hook_input, &trace_id, events_len, &requests);
    }

    Ok(requests)
}

fn write_checkpoint_debug_log(
    preset_name: &str,
    hook_input: &str,
    trace_id: &str,
    event_count: usize,
    requests: &[CheckpointRequest],
) {
    let Some(internal_dir) = config::internal_dir_path() else {
        return;
    };

    let log_dir = internal_dir.join("checkpoint-debug-logs");
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let log_path = log_dir.join(format!("{}.log", date));

    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("[checkpoint_debug_log] failed to create dir: {}", e);
        return;
    }

    cleanup_old_debug_logs(&log_dir);

    let entry = CheckpointDebugLogEntry {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        preset_name,
        hook_input,
        trace_id,
        event_count,
        requests,
    };

    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };

    let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    else {
        return;
    };

    let _ = file
        .write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.flush());
}

fn cleanup_old_debug_logs(log_dir: &Path) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };

    let cutoff = chrono::Utc::now() - chrono::Duration::days(14);

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(file_date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d")
            && file_date < cutoff.date_naive()
        {
            let _ = fs::remove_file(&path);
        }
    }
}

fn execute_event(
    event: ParsedHookEvent,
    preset_name: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    match event {
        ParsedHookEvent::PreFileEdit(e) => execute_pre_file_edit(e),
        ParsedHookEvent::PostFileEdit(e) => execute_post_file_edit(e, preset_name),
        ParsedHookEvent::PreBashCall(e) => execute_pre_bash_call(e),
        ParsedHookEvent::PostBashCall(e) => execute_post_bash_call(e),
        ParsedHookEvent::KnownHumanEdit(e) => execute_known_human_edit(e),
        ParsedHookEvent::UntrackedEdit(e) => execute_untracked_edit(e),
    }
}

fn split_files_into_requests(
    all_files: Vec<CheckpointFile>,
    trace_id: String,
    checkpoint_kind: CheckpointKind,
    agent_id: Option<AgentId>,
    path_role: PreparedPathRole,
    stream_source: Option<StreamSource>,
    metadata: HashMap<String, String>,
) -> Vec<CheckpointRequest> {
    let mut by_repo: HashMap<PathBuf, Vec<CheckpointFile>> = HashMap::new();
    for f in all_files {
        by_repo.entry(f.repo_work_dir.clone()).or_default().push(f);
    }

    by_repo
        .into_values()
        .map(|files| CheckpointRequest {
            trace_id: trace_id.clone(),
            checkpoint_kind,
            agent_id: agent_id.clone(),
            files,
            path_role,
            stream_source: stream_source.clone(),
            metadata: metadata.clone(),
        })
        .collect()
}

fn is_human_tool_name(tool: &str) -> bool {
    let tool = tool.trim();
    tool.eq_ignore_ascii_case("human") || tool.eq_ignore_ascii_case("known_human")
}

fn is_ai_pre_edit_context(agent_id: &AgentId) -> bool {
    !agent_id.tool.trim().is_empty() && !is_human_tool_name(&agent_id.tool)
}

fn execute_pre_file_edit(e: PreFileEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    let is_ai_pre_edit = is_ai_pre_edit_context(&e.context.agent_id);
    let mut metadata = e.context.metadata;
    if let Some(tuid) = e.tool_use_id {
        metadata.entry("tool_use_id".to_string()).or_insert(tuid);
    }
    if is_ai_pre_edit {
        metadata
            .entry("edit_kind".to_string())
            .or_insert_with(|| "file_edit".to_string());
        metadata
            .entry("ai_pre_edit".to_string())
            .or_insert_with(|| "true".to_string());
        metadata
            .entry("agent_tool".to_string())
            .or_insert_with(|| e.context.agent_id.tool.clone());
    }
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::Human,
        Some(e.context.agent_id),
        PreparedPathRole::WillEdit,
        None,
        metadata,
    ))
}

fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    let checkpoint_kind = match preset_name {
        "ai_tab" => CheckpointKind::AiTab,
        _ => CheckpointKind::AiAgent,
    };
    let mut metadata = e.context.metadata;
    if let Some(tuid) = e.tool_use_id {
        metadata.entry("tool_use_id".to_string()).or_insert(tuid);
    }
    metadata
        .entry("edit_kind".to_string())
        .or_insert_with(|| "file_edit".to_string());
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        checkpoint_kind,
        Some(e.context.agent_id),
        PreparedPathRole::Edited,
        e.stream_source,
        metadata,
    ))
}

fn execute_known_human_edit(e: KnownHumanEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    Ok(split_files_into_requests(
        files,
        e.trace_id,
        CheckpointKind::KnownHuman,
        None,
        PreparedPathRole::Edited,
        None,
        e.editor_metadata,
    ))
}

fn execute_untracked_edit(e: UntrackedEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;
    Ok(split_files_into_requests(
        files,
        e.trace_id,
        CheckpointKind::Human,
        None,
        PreparedPathRole::WillEdit,
        None,
        HashMap::new(),
    ))
}

fn execute_pre_bash_call(e: PreBashCall) -> Result<Vec<CheckpointRequest>, GitAiError> {
    use crate::commands::checkpoint_agent::bash_tool;

    let repo = discover_repository_in_path_no_git_exec(e.context.cwd.as_path())?;
    let repo_work_dir = repo.workdir()?;

    let dirty_paths = match bash_tool::handle_bash_pre_tool_use_with_context(
        &repo_work_dir,
        &e.context.external_session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
    ) {
        Ok(result) => result.dirty_paths,
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.external_session_id,
                error
            );
            return Ok(vec![]);
        }
    };

    if dirty_paths.is_empty() {
        return Ok(vec![]);
    }

    if dirty_paths.len() > MAX_BASH_PRE_BASELINE_FILES {
        crate::diagnostics::append_debug_event(
            "bash_pre_checkpoint_skipped_too_many_dirty_paths",
            serde_json::json!({
                "repo": repo_work_dir.to_string_lossy().replace('\\', "/"),
                "tool": e.context.agent_id.tool,
                "sessionId": e.context.external_session_id,
                "toolUseId": e.tool_use_id,
                "dirtyPathCount": dirty_paths.len(),
                "dirtyPathSample": sorted_display_path_sample(&dirty_paths),
                "dirtyPathOmitted": omitted_path_count(dirty_paths.len(), DEBUG_PATH_SAMPLE_LIMIT),
                "dirtyPathsTruncated": true,
                "pathLimit": MAX_BASH_PRE_BASELINE_FILES,
            }),
        );
        return Ok(vec![]);
    }

    let files = build_bash_checkpoint_files(&dirty_paths)?;
    let mut metadata = e.context.metadata;
    metadata
        .entry("tool_use_id".to_string())
        .or_insert(e.tool_use_id);
    metadata
        .entry("edit_kind".to_string())
        .or_insert_with(|| "bash_pre".to_string());
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::Human,
        None,
        PreparedPathRole::WillEdit,
        None,
        metadata,
    ))
}

fn execute_post_bash_call(e: PostBashCall) -> Result<Vec<CheckpointRequest>, GitAiError> {
    use crate::commands::checkpoint_agent::bash_tool;

    let repo = discover_repository_in_path_no_git_exec(e.context.cwd.as_path())?;
    let repo_work_dir = repo.workdir()?;
    let session_id = e.context.external_session_id.clone();
    let tool_use_id = e.tool_use_id.clone();
    let agent_tool = e.context.agent_id.tool.clone();
    let trace_id = e.context.trace_id.clone();

    let mut detected_path_count = 0usize;
    let mut detected_path_sample: Vec<String> = Vec::new();

    let bash_result = bash_tool::handle_bash_post_tool_use(
        &repo_work_dir,
        &e.context.external_session_id,
        &e.tool_use_id,
        &trace_id,
    );

    let mut action_name = "error";
    let mut fallback_reason: Option<String> = None;
    let mut error_message: Option<String> = None;
    let mut file_paths: Vec<PathBuf> = match &bash_result {
        Ok(result) => {
            action_name = match &result.action {
                bash_tool::BashCheckpointAction::Checkpoint(_) => "checkpoint",
                bash_tool::BashCheckpointAction::NoChanges => "no_changes",
                bash_tool::BashCheckpointAction::HookTimeout => "hook_timeout",
                bash_tool::BashCheckpointAction::SnapshotFailed => "snapshot_failed",
                bash_tool::BashCheckpointAction::MissingPreSnapshot => "missing_pre_snapshot",
            };

            match &result.action {
                bash_tool::BashCheckpointAction::Checkpoint(paths) => {
                    let (count, sample, resolved_paths) =
                        resolve_bash_detected_paths_for_checkpoint(
                            &repo_work_dir,
                            paths,
                            true,
                            &mut fallback_reason,
                            &mut error_message,
                        );
                    detected_path_count = count;
                    detected_path_sample = sample;
                    resolved_paths
                }
                bash_tool::BashCheckpointAction::NoChanges => vec![],
                bash_tool::BashCheckpointAction::HookTimeout
                | bash_tool::BashCheckpointAction::SnapshotFailed
                | bash_tool::BashCheckpointAction::MissingPreSnapshot => {
                    match bash_tool::git_status_fallback(&repo_work_dir) {
                        Ok(paths) if !paths.is_empty() => {
                            fallback_reason = Some("git_status_fallback".to_string());
                            let (count, sample, resolved_paths) =
                                resolve_bash_detected_paths_for_checkpoint(
                                    &repo_work_dir,
                                    &paths,
                                    true,
                                    &mut fallback_reason,
                                    &mut error_message,
                                );
                            detected_path_count = count;
                            detected_path_sample = sample;
                            resolved_paths
                        }
                        Ok(_) => vec![],
                        Err(err) => {
                            error_message = Some(err.to_string());
                            vec![]
                        }
                    }
                }
            }
        }
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            error_message = Some(err.to_string());
            match bash_tool::git_status_fallback(&repo_work_dir) {
                Ok(paths) if !paths.is_empty() => {
                    fallback_reason = Some("git_status_fallback_after_error".to_string());
                    let (count, sample, resolved_paths) =
                        resolve_bash_detected_paths_for_checkpoint(
                            &repo_work_dir,
                            &paths,
                            true,
                            &mut fallback_reason,
                            &mut error_message,
                        );
                    detected_path_count = count;
                    detected_path_sample = sample;
                    resolved_paths
                }
                Ok(_) => vec![],
                Err(fallback_err) => {
                    error_message = Some(format!("{}; fallback: {}", err, fallback_err));
                    vec![]
                }
            }
        }
    };

    let dirty_file_paths: Vec<PathBuf> = e
        .dirty_files
        .as_ref()
        .map(|dirty| dirty.keys().cloned().collect())
        .unwrap_or_default();
    let dirty_file_path_count = dirty_file_paths.len();
    let dirty_file_path_sample = sorted_display_path_sample(&dirty_file_paths);
    let before_dirty_merge_file_count = file_paths.len();
    if let Some(ref dirty) = e.dirty_files
        && !dirty.is_empty()
    {
        merge_paths_from_dirty_files(&mut file_paths, dirty);
        if file_paths.len() > before_dirty_merge_file_count {
            fallback_reason = match fallback_reason {
                Some(reason) => Some(format!("{}+dirty_files_merge", reason)),
                None => Some("dirty_files_merge".to_string()),
            };
        }
    }
    let merged_from_dirty_files = file_paths.len() > before_dirty_merge_file_count;

    crate::diagnostics::append_debug_event(
        "bash_post_checkpoint_resolved",
        serde_json::json!({
            "repo": repo_work_dir.to_string_lossy().replace('\\', "/"),
            "traceId": trace_id,
            "tool": agent_tool,
            "sessionId": session_id,
            "toolUseId": tool_use_id,
            "action": action_name,
            "fallbackReason": fallback_reason,
            "detectedPathCount": detected_path_count,
            "detectedPathSample": detected_path_sample,
            "detectedPathOmitted": omitted_path_count(detected_path_count, detected_path_sample.len()),
            "detectedPathsTruncated": detected_path_count > detected_path_sample.len(),
            "dirtyFilePathCount": dirty_file_path_count,
            "dirtyFilePathSample": dirty_file_path_sample,
            "dirtyFilePathOmitted": omitted_path_count(dirty_file_path_count, dirty_file_path_sample.len()),
            "dirtyFilePathsTruncated": dirty_file_path_count > dirty_file_path_sample.len(),
            "mergedFromDirtyFiles": merged_from_dirty_files,
            "finalFileCount": file_paths.len(),
            "pathLimit": MAX_BASH_CHECKPOINT_FILES,
            "error": error_message,
        }),
    );

    if file_paths.len() > MAX_BASH_CHECKPOINT_FILES {
        crate::diagnostics::append_debug_event(
            "bash_post_checkpoint_skipped_too_many_paths",
            serde_json::json!({
                "repo": repo_work_dir.to_string_lossy().replace('\\', "/"),
                "traceId": trace_id,
                "tool": agent_tool,
                "sessionId": session_id,
                "toolUseId": tool_use_id,
                "finalFileCount": file_paths.len(),
                "pathLimit": MAX_BASH_CHECKPOINT_FILES,
            }),
        );
        return Ok(vec![]);
    }

    let mut files = build_bash_checkpoint_files(&file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    let mut metadata = e.context.metadata;
    metadata
        .entry("tool_use_id".to_string())
        .or_insert(e.tool_use_id);
    metadata
        .entry("edit_kind".to_string())
        .or_insert_with(|| "bash".to_string());
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::AiAgent,
        Some(e.context.agent_id),
        PreparedPathRole::Edited,
        e.stream_source,
        metadata,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_path_samples_are_bounded_and_report_omitted_count() {
        let paths = (0..50)
            .rev()
            .map(|idx| format!("src/file-{idx:02}.rs"))
            .collect::<Vec<_>>();

        let sample = sorted_string_path_sample(&paths);

        assert_eq!(sample.len(), DEBUG_PATH_SAMPLE_LIMIT);
        assert_eq!(sample[0], "src/file-00.rs");
        assert_eq!(
            omitted_path_count(paths.len(), sample.len()),
            paths.len() - DEBUG_PATH_SAMPLE_LIMIT
        );
    }

    #[test]
    fn oversized_bash_git_status_fallback_is_logged_but_skipped() {
        let paths = (0..=MAX_CHECKPOINT_FILES)
            .map(|idx| format!("src/file-{idx:04}.rs"))
            .collect::<Vec<_>>();
        let mut fallback_reason = Some("git_status_fallback".to_string());
        let mut error_message = None;

        let (count, sample, resolved_paths) = resolve_bash_detected_paths_for_checkpoint(
            Path::new("/repo"),
            &paths,
            true,
            &mut fallback_reason,
            &mut error_message,
        );

        assert_eq!(count, MAX_CHECKPOINT_FILES + 1);
        assert_eq!(sample.len(), DEBUG_PATH_SAMPLE_LIMIT);
        assert!(resolved_paths.is_empty());
        assert_eq!(
            fallback_reason.as_deref(),
            Some("git_status_fallback_too_many_paths")
        );
        assert!(
            error_message
                .as_deref()
                .is_some_and(|message| message.contains("skipping noisy checkpoint"))
        );
    }

    #[test]
    fn oversized_bash_snapshot_diff_is_logged_but_skipped() {
        let paths = (0..=MAX_CHECKPOINT_FILES)
            .map(|idx| format!("src/file-{idx:04}.rs"))
            .collect::<Vec<_>>();
        let mut fallback_reason = None;
        let mut error_message = None;

        let (count, sample, resolved_paths) = resolve_bash_detected_paths_for_checkpoint(
            Path::new("/repo"),
            &paths,
            true,
            &mut fallback_reason,
            &mut error_message,
        );

        assert_eq!(count, MAX_CHECKPOINT_FILES + 1);
        assert_eq!(sample.len(), DEBUG_PATH_SAMPLE_LIMIT);
        assert!(resolved_paths.is_empty());
        assert_eq!(
            fallback_reason.as_deref(),
            Some("git_status_fallback_too_many_paths")
        );
        assert!(
            error_message
                .as_deref()
                .is_some_and(|message| message.contains("skipping noisy checkpoint"))
        );
    }

    #[test]
    fn bash_runtime_log_filter_does_not_apply_to_explicit_file_edits() {
        let runtime_log = Path::new("/repo/logs/session.jsonl");

        assert!(should_skip_checkpoint_path(runtime_log).is_none());
        assert_eq!(
            should_skip_bash_checkpoint_path(runtime_log),
            Some("runtime_log")
        );
    }
}
