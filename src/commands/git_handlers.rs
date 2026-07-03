use crate::commands::git_hook_handlers::ENV_SKIP_MANAGED_HOOKS;
use crate::config;
use crate::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use crate::git::command_classification::is_definitely_read_only_invocation_args;
use crate::git::find_repository;
use crate::git::repository::Repository;
#[cfg(windows)]
use crate::utils::CREATE_NO_WINDOW;
#[cfg(windows)]
use crate::utils::is_interactive_terminal;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(unix)]
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

fn should_run_post_commit_followups(parsed: &ParsedGitInvocation, command_succeeded: bool) -> bool {
    command_succeeded && parsed.command.as_deref() == Some("commit")
}

fn run_post_commit_followups(
    parsed: &ParsedGitInvocation,
    repository: Option<&Repository>,
    show_async_stats: bool,
    fallback_upload_source: &str,
) {
    if show_async_stats && let Some(repo) = repository {
        maybe_show_async_post_commit_stats(parsed, repo);
    }

    // Start the current commit's dashboard upload before scheduling a background
    // self-update. The update worker may stop/restart git-ai services during
    // install, so the upload must get the first chance to acquire its activity
    // lock and persist this commit's status.
    maybe_spawn_post_commit_fallback_upload(repository, fallback_upload_source);
    crate::commands::upgrade::maybe_schedule_background_update_check_after_commit();
}

const FALLBACK_UPLOAD_WAIT_FOR_AUTHORSHIP_NOTE_MS: &str = "15000";
const FALLBACK_UPLOAD_GUARD_ENV: &str = "GIT_AI_POST_COMMIT_FALLBACK_UPLOAD_SPAWNED";

fn post_commit_fallback_upload_args(commit_sha: &str, source: &str) -> Vec<String> {
    let mut args = vec![
        "upload-stats".to_string(),
        commit_sha.to_string(),
        "--source".to_string(),
        source.to_string(),
        "--wait-for-authorship-note-ms".to_string(),
        FALLBACK_UPLOAD_WAIT_FOR_AUTHORSHIP_NOTE_MS.to_string(),
        "--skip-if-already-uploaded".to_string(),
        "--acquire-activity-lock-before-stats".to_string(),
    ];
    if source == "wrapper_post_commit" {
        args.push("--skip-if-authorship-note-missing-after-wait".to_string());
    }
    args
}

#[cfg(unix)]
extern "C" fn forward_signal_handler(sig: libc::c_int) {
    let pgid = CHILD_PGID.load(Ordering::Relaxed);
    if pgid > 0 {
        unsafe {
            // Send to the whole child process group
            let _ = libc::kill(-pgid, sig);
        }
    }
}

#[cfg(unix)]
fn install_forwarding_handlers() {
    unsafe {
        let handler = forward_signal_handler as *const () as usize;
        let _ = libc::signal(libc::SIGTERM, handler);
        let _ = libc::signal(libc::SIGINT, handler);
        let _ = libc::signal(libc::SIGHUP, handler);
        let _ = libc::signal(libc::SIGQUIT, handler);
    }
}

#[cfg(unix)]
fn uninstall_forwarding_handlers() {
    unsafe {
        let _ = libc::signal(libc::SIGTERM, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGINT, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGHUP, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGQUIT, libc::SIG_DFL);
    }
}

pub fn handle_git(args: &[String]) {
    // If we're being invoked from a shell completion context, bypass git-ai logic
    // and delegate directly to the real git so existing completion scripts work.
    if in_shell_completion_context() {
        let orig_args: Vec<String> = std::env::args().skip(1).collect();
        crate::diagnostics::append_debug_event(
            "git_proxy_completion_passthrough",
            serde_json::json!({
                "argsPreview": crate::diagnostics::sanitized_command_args(&orig_args),
                "argCount": orig_args.len(),
                "currentDir": crate::diagnostics::current_dir_for_debug(),
                "currentExe": crate::diagnostics::current_exe_for_debug(),
            }),
        );
        proxy_to_git(&orig_args, true, None, None);
        return;
    }

    let parsed = parse_git_cli_args(args);

    // Read-only invocations don't need wrapper state (the daemon fast-paths
    // their trace events and never processes them through the normalizer).
    // Skip the invocation_id so we can also suppress trace2 for them,
    // avoiding unnecessary daemon work and wrapper_states memory leaks.
    //
    // Use is_definitely_read_only_invocation (not is_definitely_read_only_command)
    // so that subcommand-gated read-only calls like `git stash list` and
    // `git worktree list` are also suppressed — these account for thousands
    // of Zed IDE invocations per session.
    let is_read_only = is_read_only_invocation(&parsed);

    if is_read_only {
        let exit_status = proxy_to_git_quiet(args);
        exit_with_status(exit_status);
    }

    crate::diagnostics::append_debug_event(
        "git_proxy_entered",
        serde_json::json!({
            "command": parsed.command.as_deref(),
            "argsPreview": crate::diagnostics::sanitized_command_args(args),
            "argCount": args.len(),
            "currentDir": crate::diagnostics::current_dir_for_debug(),
            "currentExe": crate::diagnostics::current_exe_for_debug(),
        }),
    );

    // Repo-creating commands (clone, init) have no meaningful pre/post
    // repo state — the target repo doesn't exist yet. The wrapper would
    // either capture nothing (clone from outside a repo) or the wrong
    // repo (clone from inside a different repo). Skip the invocation_id
    // so the daemon doesn't wait for wrapper state that never arrives or
    // is misleading. This path also suppresses inherited/global trace2 in
    // proxy_to_git, so stale daemon targets cannot affect clone/init.
    let is_repo_creating = parsed
        .command
        .as_deref()
        .is_some_and(|cmd| matches!(cmd, "clone" | "init"));

    if is_repo_creating {
        crate::diagnostics::append_debug_event(
            "git_proxy_route",
            serde_json::json!({
                "route": "repo_creating_passthrough",
                "command": parsed.command.as_deref(),
            }),
        );
        let exit_status = proxy_to_git(args, false, None, None);
        exit_with_status(exit_status);
    }

    let repository = find_repository(&parsed.global_args).ok();
    if let Some(repo) = repository.as_ref() {
        crate::known_repos::record_known_repo_best_effort(repo, "git-wrapper");
    }

    // Initialize the daemon telemetry handle so we can send wrapper state.
    // If the daemon isn't available, fall back to a plain passthrough proxy
    // (no invocation_id, no wrapper state, no extra GIT_* env vars).
    let trace2_event_target = match crate::daemon::telemetry_handle::init_daemon_telemetry_handle()
    {
        crate::daemon::telemetry_handle::DaemonTelemetryInitResult::Connected {
            trace2_event_target,
        } => Some(trace2_event_target),
        _ => None,
    };

    if trace2_event_target.is_none() {
        crate::diagnostics::append_debug_event(
            "git_proxy_route",
            serde_json::json!({
                "route": "daemon_unavailable_passthrough",
                "command": parsed.command.as_deref(),
                "repositoryFound": repository.is_some(),
            }),
        );
        let exit_status = proxy_to_git(args, false, None, None);
        if should_run_post_commit_followups(&parsed, exit_status.success()) {
            run_post_commit_followups(&parsed, repository.as_ref(), false, "wrapper_no_daemon");
        }
        exit_with_status(exit_status);
    }

    let worktree = repository.as_ref().and_then(|r| r.workdir().ok());

    let pre_state = worktree
        .as_deref()
        .and_then(crate::git::repo_state::read_head_state_for_worktree);
    let invocation_id = crate::uuid::generate_v4();
    crate::diagnostics::append_debug_event(
        "git_proxy_route",
        serde_json::json!({
            "route": "daemon_wrapper",
            "command": parsed.command.as_deref(),
            "repositoryFound": repository.is_some(),
            "worktree": worktree.as_ref().map(|path| path.to_string_lossy().to_string()),
            "wrapperInvocationId": invocation_id,
            "trace2EventTarget": trace2_event_target.as_deref(),
        }),
    );

    // Send pre-state BEFORE running git so it's available when the daemon
    // processes the atexit trace event and starts the wrapper state timeout.
    send_wrapper_pre_state_to_daemon(&invocation_id, worktree.as_deref(), &pre_state);

    let exit_status = proxy_to_git(
        args,
        false,
        Some(&invocation_id),
        trace2_event_target.as_deref(),
    );

    let post_state = worktree
        .as_deref()
        .and_then(crate::git::repo_state::read_head_state_for_worktree);

    send_wrapper_post_state_to_daemon(&invocation_id, worktree.as_deref(), &post_state);

    // After a successful commit, wait briefly for the daemon to produce an
    // authorship note so we can show stats inline (same UX as plain wrapper mode).
    if should_run_post_commit_followups(&parsed, exit_status.success()) {
        run_post_commit_followups(&parsed, repository.as_ref(), true, "wrapper_post_commit");
    }

    exit_with_status(exit_status);
}

#[cfg(feature = "test-support")]
pub fn resolve_alias_invocation(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<ParsedGitInvocation> {
    use std::collections::HashSet;

    let mut current = parsed_args.clone();
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        let command = match current.command.as_deref() {
            Some(command) => command,
            None => return Some(current),
        };

        if !seen.insert(command.to_string()) {
            return None;
        }

        let key = format!("alias.{}", command);
        let alias_value = match repository.config_get_str(&key) {
            Ok(Some(value)) => value,
            _ => return Some(current),
        };

        let alias_tokens = parse_alias_tokens(&alias_value)?;

        let mut expanded_args = Vec::new();
        expanded_args.extend(current.global_args.iter().cloned());
        expanded_args.extend(alias_tokens);
        expanded_args.extend(current.command_args.iter().cloned());

        current = parse_git_cli_args(&expanded_args);
    }
}

#[cfg(feature = "test-support")]
fn parse_alias_tokens(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim_start();

    if trimmed.starts_with('!') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        if in_double {
            match ch {
                '"' => in_double = false,
                '\\' => escaped = true,
                _ => current.push(ch),
            }
            continue;
        }

        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '\\' => escaped = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }

    if in_single || in_double {
        return None;
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    Some(tokens)
}

/// In async (wrapper-to-daemon) mode, after a successful `git commit`, poll for
/// the daemon-produced authorship note and display stats inline when available.
/// Mirrors the same skip/display rules as plain wrapper mode in post_commit.rs.
fn maybe_show_async_post_commit_stats(parsed: &ParsedGitInvocation, repo: &Repository) {
    use crate::authorship::ignore::effective_ignore_patterns;
    use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
    use crate::git::cli_parser::is_dry_run;
    use crate::git::notes_api::read_note as show_authorship_note;
    use std::io::IsTerminal;

    // Respect the same suppression flags as the synchronous wrapper path.
    if is_dry_run(&parsed.command_args) {
        return;
    }
    let suppress_output = parsed.has_command_flag("--porcelain")
        || parsed.has_command_flag("--quiet")
        || parsed.has_command_flag("-q")
        || parsed.has_command_flag("--no-status");
    if suppress_output || config::Config::get().is_quiet() {
        return;
    }

    let is_interactive =
        std::io::stdout().is_terminal() || std::env::var_os("GIT_AI_TEST_FORCE_TTY").is_some();
    if !is_interactive {
        return;
    }

    // Determine the new commit SHA.
    let commit_sha = match repo.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => return,
    };

    // Use a longer timeout under test to avoid flakiness on saturated CI machines.
    // GIT_AI_POST_COMMIT_TIMEOUT_MS allows tests to override the timeout.
    let timeout = if let Some(ms) = std::env::var("GIT_AI_POST_COMMIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::time::Duration::from_millis(ms)
    } else if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some() {
        std::time::Duration::from_secs(20)
    } else {
        std::time::Duration::from_millis(500)
    };

    // Poll for the authorship note the daemon should be producing.
    let poll_interval = std::time::Duration::from_millis(25);
    let start = std::time::Instant::now();
    let note_found = loop {
        if show_authorship_note(repo, &commit_sha).is_some() {
            break true;
        }
        if start.elapsed() >= timeout {
            break false;
        }
        std::thread::sleep(poll_interval);
    };

    if !note_found {
        eprintln!(
            "[git-ai] still processing commit {}... run `git ai stats` to see stats.",
            &commit_sha[..std::cmp::min(8, commit_sha.len())]
        );
        return;
    }

    // Check if this is a merge commit — skip expensive stats just like the sync path.
    let is_merge = repo
        .find_commit(commit_sha.clone())
        .map(|c| c.parent_count().unwrap_or(0) > 1)
        .unwrap_or(false);
    if is_merge {
        eprintln!(
            "[git-ai] Skipped git-ai stats for merge commit {}.",
            commit_sha
        );
        return;
    }

    // Run the same cost estimation the sync path uses.
    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    if let Ok(estimate) = crate::authorship::post_commit::estimate_stats_cost_for_head(
        repo,
        &commit_sha,
        &ignore_patterns,
    ) && estimate.should_skip()
    {
        eprintln!(
            "[git-ai] Skipped git-ai stats for large commit. Run `git ai stats {}` to compute stats on demand.",
            commit_sha
        );
        return;
    }

    // Compute and display the full stats.
    if let Ok(stats) = stats_for_commit_stats(repo, &commit_sha, &ignore_patterns) {
        write_stats_to_terminal(&stats, true);
    }
}

fn should_spawn_post_commit_fallback_upload() -> bool {
    if std::env::var(FALLBACK_UPLOAD_GUARD_ENV).as_deref() == Ok("1") {
        return false;
    }

    config::Config::fresh().feature_flags().auto_upload_ai_stats
}

fn maybe_spawn_post_commit_fallback_upload(repo: Option<&Repository>, source: &str) {
    if !should_spawn_post_commit_fallback_upload() {
        return;
    }

    let Some(repo) = repo else {
        return;
    };
    let Some(commit_sha) = repo.head().ok().and_then(|head| head.target().ok()) else {
        return;
    };
    let workdir = repo.canonical_workdir().to_path_buf();
    let commit_sha = commit_sha.to_string();
    let commit_short = if commit_sha.len() > 7 {
        commit_sha[..7].to_string()
    } else {
        commit_sha.clone()
    };

    let Ok(exe) = crate::utils::current_git_ai_exe() else {
        crate::diagnostics::append_debug_event(
            "post_commit_fallback_upload_spawn_skipped",
            serde_json::json!({
                "reason": "git_ai_exe_unavailable",
                "source": source,
                "repo": workdir.to_string_lossy().to_string(),
                "commitSha": commit_sha,
                "commitShort": commit_short,
            }),
        );
        return;
    };

    let mut cmd = Command::new(exe);
    cmd.current_dir(&workdir)
        .args(post_commit_fallback_upload_args(&commit_sha, source))
        .env(crate::commands::git_hook_handlers::ENV_SKIP_ALL_HOOKS, "1")
        .env(FALLBACK_UPLOAD_GUARD_ENV, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.spawn() {
        Ok(child) => {
            crate::diagnostics::append_debug_event(
                "post_commit_fallback_upload_spawned",
                serde_json::json!({
                    "source": source,
                    "repo": workdir.to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "commitShort": commit_short,
                    "waitForAuthorshipNoteMs": FALLBACK_UPLOAD_WAIT_FOR_AUTHORSHIP_NOTE_MS,
                    "processId": child.id(),
                }),
            );
        }
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "post_commit_fallback_upload_spawn_failed",
                serde_json::json!({
                    "source": source,
                    "repo": workdir.to_string_lossy().to_string(),
                    "commitSha": commit_sha,
                    "commitShort": commit_short,
                    "error": error.to_string(),
                }),
            );
        }
    }
}

fn head_state_to_repo_context(
    s: crate::git::repo_state::HeadState,
) -> crate::daemon::domain::RepoContext {
    crate::daemon::domain::RepoContext {
        head: s.head,
        branch: s.branch,
        detached: s.detached,
    }
}

fn send_wrapper_pre_state_to_daemon(
    invocation_id: &str,
    worktree: Option<&std::path::Path>,
    pre_state: &Option<crate::git::repo_state::HeadState>,
) {
    let Some(wt) = worktree else { return };
    let Some(pre) = pre_state.clone() else { return };
    let wt_str = wt.to_string_lossy().to_string();
    if let Err(e) = crate::daemon::telemetry_handle::send_wrapper_pre_state(
        invocation_id,
        &wt_str,
        head_state_to_repo_context(pre),
    ) {
        tracing::debug!(
            "wrapper: failed to send pre-state for {}: {}",
            invocation_id,
            e
        );
    }
}

fn send_wrapper_post_state_to_daemon(
    invocation_id: &str,
    worktree: Option<&std::path::Path>,
    post_state: &Option<crate::git::repo_state::HeadState>,
) {
    let Some(wt) = worktree else { return };
    let Some(post) = post_state.clone() else {
        return;
    };
    let wt_str = wt.to_string_lossy().to_string();
    if let Err(e) = crate::daemon::telemetry_handle::send_wrapper_post_state(
        invocation_id,
        &wt_str,
        head_state_to_repo_context(post),
    ) {
        tracing::debug!(
            "wrapper: failed to send post-state for {}: {}",
            invocation_id,
            e
        );
    }
}

fn apply_git_trace2_event_target_override(cmd: &mut Command, trace2_event_target: Option<&str>) {
    let Some(trace2_event_target) = trace2_event_target else {
        return;
    };

    // trace2 is initialized early in Git startup. Command-line config
    // injection (`-c`/GIT_CONFIG_COUNT) can be too late on some Git builds, so
    // use the dedicated environment variable for the single child process that
    // needs daemon observation.
    cmd.env("GIT_TRACE2_EVENT", trace2_event_target);
    cmd.env("GIT_TRACE2_EVENT_NESTING", "10");
}

fn is_read_only_invocation(parsed: &ParsedGitInvocation) -> bool {
    parsed
        .command
        .as_deref()
        .is_some_and(|cmd| is_definitely_read_only_invocation_args(cmd, &parsed.command_args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::cli_parser::parse_git_cli_args;

    #[test]
    fn post_commit_followups_require_successful_commit() {
        let commit =
            parse_git_cli_args(&["commit".to_string(), "-m".to_string(), "msg".to_string()]);
        let push = parse_git_cli_args(&["push".to_string()]);

        assert!(should_run_post_commit_followups(&commit, true));
        assert!(!should_run_post_commit_followups(&commit, false));
        assert!(!should_run_post_commit_followups(&push, true));
    }

    #[test]
    fn post_commit_fallback_upload_waits_for_note_but_does_not_skip_on_note_found() {
        let args = post_commit_fallback_upload_args(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "wrapper_post_commit",
        );

        assert!(args.contains(&"--wait-for-authorship-note-ms".to_string()));
        assert!(args.contains(&"--skip-if-authorship-note-missing-after-wait".to_string()));
        assert!(args.contains(&"--skip-if-already-uploaded".to_string()));
        assert!(args.contains(&"--acquire-activity-lock-before-stats".to_string()));
        assert!(
            !args.contains(&"--skip-if-authorship-note-found".to_string()),
            "fallback upload must still upload this commit after the daemon writes its note"
        );
    }

    #[test]
    fn read_only_invocations_are_quiet_passthrough_candidates() {
        let status = parse_git_cli_args(&["status".to_string(), "-z".to_string()]);
        let worktree_list = parse_git_cli_args(&["worktree".to_string(), "list".to_string()]);
        let config_read = parse_git_cli_args(&[
            "config".to_string(),
            "--null".to_string(),
            "--get".to_string(),
            "core.fsmonitor".to_string(),
        ]);
        let config_write = parse_git_cli_args(&[
            "config".to_string(),
            "--local".to_string(),
            "git-ai.test".to_string(),
            "1".to_string(),
        ]);
        let commit =
            parse_git_cli_args(&["commit".to_string(), "-m".to_string(), "msg".to_string()]);
        let pull = parse_git_cli_args(&["pull".to_string()]);

        assert!(is_read_only_invocation(&status));
        assert!(is_read_only_invocation(&worktree_list));
        assert!(is_read_only_invocation(&config_read));
        assert!(!is_read_only_invocation(&config_write));
        assert!(!is_read_only_invocation(&commit));
        assert!(!is_read_only_invocation(&pull));
    }
}

fn proxy_to_git_quiet(args: &[String]) -> std::process::ExitStatus {
    let real_git_path = config::Config::get().git_cmd().to_string();
    #[cfg(windows)]
    let interactive_terminal = is_interactive_terminal();

    let child = {
        #[cfg(unix)]
        {
            let is_interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
            let should_setpgid = !is_interactive;

            let mut cmd = Command::new(&real_git_path);
            cmd.args(args)
                .env(ENV_SKIP_MANAGED_HOOKS, "1")
                .env("GIT_TRACE2_EVENT", "0");
            unsafe {
                let setpgid_flag = should_setpgid;
                cmd.pre_exec(move || {
                    if setpgid_flag {
                        let _ = libc::setpgid(0, 0);
                    }
                    Ok(())
                });
            }
            cmd.spawn().map(|child| (child, should_setpgid))
        }
        #[cfg(not(unix))]
        {
            let mut cmd = Command::new(&real_git_path);
            cmd.args(args)
                .env(ENV_SKIP_MANAGED_HOOKS, "1")
                .env("GIT_TRACE2_EVENT", "0");

            #[cfg(windows)]
            {
                if !interactive_terminal {
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }
            }

            cmd.spawn()
        }
    };

    #[cfg(unix)]
    match child {
        Ok((mut child, setpgid)) => {
            if setpgid {
                let pgid: i32 = child.id() as i32;
                CHILD_PGID.store(pgid, Ordering::Relaxed);
                install_forwarding_handlers();
            }
            match child.wait() {
                Ok(status) => {
                    if setpgid {
                        CHILD_PGID.store(0, Ordering::Relaxed);
                        uninstall_forwarding_handlers();
                    }
                    status
                }
                Err(e) => {
                    if setpgid {
                        CHILD_PGID.store(0, Ordering::Relaxed);
                        uninstall_forwarding_handlers();
                    }
                    eprintln!("Failed to wait for git process: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to execute git command: {}", e);
            std::process::exit(1);
        }
    }

    #[cfg(not(unix))]
    match child {
        Ok(mut child) => match child.wait() {
            Ok(status) => status,
            Err(e) => {
                eprintln!("Failed to wait for git process: {}", e);
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Failed to execute git command: {}", e);
            std::process::exit(1);
        }
    }
}

fn proxy_to_git(
    args: &[String],
    exit_on_completion: bool,
    wrapper_invocation_id: Option<&str>,
    trace2_event_target: Option<&str>,
) -> std::process::ExitStatus {
    // Suppress inherited/global trace2 unless this wrapper has explicitly
    // connected to the current daemon and is about to inject that fresh target.
    // This prevents stale global trace2.eventTarget config from sending Git
    // traffic to dead sockets or old daemon runtimes.
    let suppress_trace2 = trace2_event_target.is_none();
    let real_git_path = config::Config::get().git_cmd().to_string();
    #[cfg(windows)]
    let interactive_terminal = is_interactive_terminal();
    let spawn_started_fields = {
        let fields = serde_json::json!({
        "realGitPath": real_git_path,
        "argsPreview": crate::diagnostics::sanitized_command_args(args),
        "argCount": args.len(),
        "exitOnCompletion": exit_on_completion,
        "suppressTrace2": suppress_trace2,
        "wrapperInvocationIdPresent": wrapper_invocation_id.is_some(),
        "trace2EventTargetPresent": trace2_event_target.is_some(),
        "currentDir": crate::diagnostics::current_dir_for_debug(),
        });
        #[cfg(windows)]
        {
            let mut fields = fields;
            if let Some(fields) = fields.as_object_mut() {
                fields.insert(
                    "interactiveTerminal".to_string(),
                    serde_json::json!(interactive_terminal),
                );
            }
            fields
        }
        #[cfg(not(windows))]
        {
            fields
        }
    };
    crate::diagnostics::append_debug_event("git_proxy_spawn_started", spawn_started_fields);

    // Use spawn for interactive commands
    let child = {
        #[cfg(unix)]
        {
            // Only create a new process group for non-interactive runs.
            // If stdin is a TTY, the child must remain in the foreground
            // terminal process group to avoid SIGTTIN/SIGTTOU hangs.
            let is_interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
            let should_setpgid = !is_interactive;

            let mut cmd = Command::new(&real_git_path);
            cmd.args(args);
            cmd.env(ENV_SKIP_MANAGED_HOOKS, "1");
            if suppress_trace2 {
                cmd.env("GIT_TRACE2_EVENT", "0");
            }
            if let Some(id) = wrapper_invocation_id {
                cmd.env("GIT_AI_WRAPPER_INVOCATION_ID", id);
                cmd.env("GIT_TRACE2_ENV_VARS", "GIT_AI_WRAPPER_INVOCATION_ID");
            }
            apply_git_trace2_event_target_override(&mut cmd, trace2_event_target);
            unsafe {
                let setpgid_flag = should_setpgid;
                cmd.pre_exec(move || {
                    if setpgid_flag {
                        // Make the child its own process group leader so we can signal the group
                        let _ = libc::setpgid(0, 0);
                    }
                    Ok(())
                });
            }
            // We return both the spawned child and whether we changed PGID
            match cmd.spawn() {
                Ok(child) => Ok((child, should_setpgid)),
                Err(e) => Err(e),
            }
        }
        #[cfg(not(unix))]
        {
            let mut cmd = Command::new(&real_git_path);
            cmd.args(args);
            cmd.env(ENV_SKIP_MANAGED_HOOKS, "1");
            if suppress_trace2 {
                cmd.env("GIT_TRACE2_EVENT", "0");
            }
            if let Some(id) = wrapper_invocation_id {
                cmd.env("GIT_AI_WRAPPER_INVOCATION_ID", id);
                cmd.env("GIT_TRACE2_ENV_VARS", "GIT_AI_WRAPPER_INVOCATION_ID");
            }
            apply_git_trace2_event_target_override(&mut cmd, trace2_event_target);

            #[cfg(windows)]
            {
                if !interactive_terminal {
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }
            }

            cmd.spawn()
        }
    };

    #[cfg(unix)]
    match child {
        Ok((mut child, setpgid)) => {
            crate::diagnostics::append_debug_event(
                "git_proxy_spawn_succeeded",
                serde_json::json!({
                    "realGitPath": real_git_path,
                    "childProcessId": child.id(),
                    "setProcessGroup": setpgid,
                }),
            );
            #[cfg(unix)]
            {
                if setpgid {
                    // Record the child's process group id (same as its pid after setpgid)
                    let pgid: i32 = child.id() as i32;
                    CHILD_PGID.store(pgid, Ordering::Relaxed);
                    install_forwarding_handlers();
                }
            }
            let status = child.wait();
            match status {
                Ok(status) => {
                    crate::diagnostics::append_debug_event(
                        "git_proxy_child_exited",
                        serde_json::json!({
                            "realGitPath": real_git_path,
                            "exitCode": status.code(),
                            "success": status.success(),
                        }),
                    );
                    #[cfg(unix)]
                    {
                        if setpgid {
                            CHILD_PGID.store(0, Ordering::Relaxed);
                            uninstall_forwarding_handlers();
                        }
                    }
                    if exit_on_completion {
                        exit_with_status(status);
                    }
                    status
                }
                Err(e) => {
                    crate::diagnostics::append_debug_event(
                        "git_proxy_wait_failed",
                        serde_json::json!({
                            "realGitPath": real_git_path,
                            "errorKind": format!("{:?}", e.kind()),
                            "error": e.to_string(),
                        }),
                    );
                    #[cfg(unix)]
                    {
                        if setpgid {
                            CHILD_PGID.store(0, Ordering::Relaxed);
                            uninstall_forwarding_handlers();
                        }
                    }
                    eprintln!("Failed to wait for git process: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            crate::diagnostics::append_debug_event(
                "git_proxy_spawn_failed",
                serde_json::json!({
                    "realGitPath": real_git_path,
                    "errorKind": format!("{:?}", e.kind()),
                    "error": e.to_string(),
                }),
            );
            eprintln!("Failed to execute git command: {}", e);
            std::process::exit(1);
        }
    }

    #[cfg(not(unix))]
    match child {
        Ok(mut child) => {
            crate::diagnostics::append_debug_event(
                "git_proxy_spawn_succeeded",
                serde_json::json!({
                    "realGitPath": real_git_path,
                    "childProcessId": child.id(),
                }),
            );
            let status = child.wait();
            match status {
                Ok(status) => {
                    crate::diagnostics::append_debug_event(
                        "git_proxy_child_exited",
                        serde_json::json!({
                            "realGitPath": real_git_path,
                            "exitCode": status.code(),
                            "success": status.success(),
                        }),
                    );
                    if exit_on_completion {
                        exit_with_status(status);
                    }
                    status
                }
                Err(e) => {
                    crate::diagnostics::append_debug_event(
                        "git_proxy_wait_failed",
                        serde_json::json!({
                            "realGitPath": real_git_path,
                            "errorKind": format!("{:?}", e.kind()),
                            "error": e.to_string(),
                        }),
                    );
                    eprintln!("Failed to wait for git process: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            crate::diagnostics::append_debug_event(
                "git_proxy_spawn_failed",
                serde_json::json!({
                    "realGitPath": real_git_path,
                    "errorKind": format!("{:?}", e.kind()),
                    "error": e.to_string(),
                }),
            );
            eprintln!("Failed to execute git command: {}", e);
            std::process::exit(1);
        }
    }
}

// Exit mirroring the child's termination: same signal if signaled, else exit code
fn exit_with_status(status: std::process::ExitStatus) -> ! {
    #[cfg(unix)]
    {
        if let Some(sig) = status.signal() {
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            // Should not return
            unreachable!();
        }
    }
    std::process::exit(status.code().unwrap_or(1));
}

// Detect if current process invocation is coming from shell completion machinery
// (bash, zsh via bashcompinit). If so, we should proxy directly to the real git
// without any extra behavior that could interfere with completion scripts.
fn in_shell_completion_context() -> bool {
    std::env::var("COMP_LINE").is_ok()
        || std::env::var("COMP_POINT").is_ok()
        || std::env::var("COMP_TYPE").is_ok()
}
