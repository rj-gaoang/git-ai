/// Returns true if the given git subcommand is guaranteed to never mutate
/// repository state (refs, objects, config, worktree). Used to skip expensive
/// trace2 ingestion work and suppress trace2 emission for read-only commands.
pub fn is_definitely_read_only_command(command: &str) -> bool {
    matches!(
        command,
        "blame"
            | "cat-file"
            | "check-attr"
            | "check-ignore"
            | "check-mailmap"
            | "count-objects"
            | "describe"
            | "diff"
            | "diff-files"
            | "diff-index"
            | "diff-tree"
            | "for-each-ref"
            | "grep"
            | "help"
            | "log"
            | "ls-remote"
            | "ls-files"
            | "ls-tree"
            | "merge-base"
            | "name-rev"
            | "rev-list"
            | "rev-parse"
            | "shortlog"
            | "show"
            | "status"
            | "var"
            | "verify-commit"
            | "verify-tag"
            | "version"
    )
}

/// Returns true if the git invocation identified by `command` and optional
/// `subcommand` is guaranteed to never mutate repository state.
///
/// Extends `is_definitely_read_only_command` to handle commands like `stash`
/// and `worktree` whose read-only status depends on the subcommand:
/// - `git stash list` / `git stash show` are read-only
/// - `git stash pop` / `git stash apply` are not
/// - `git worktree list` is read-only
/// - `git worktree add` / `git worktree remove` are not
///
/// IDEs like Zed issue thousands of `stash list` and `worktree list` calls
/// per minute for their git panel UI. These must be identified as read-only
/// so the trace2 pipeline can drop them without processing.
pub fn is_definitely_read_only_invocation(command: &str, subcommand: Option<&str>) -> bool {
    if is_definitely_read_only_command(command) {
        return true;
    }
    match command {
        "branch" => matches!(subcommand, Some("--show-current" | "--list" | "-l")),
        "config" => matches!(
            subcommand,
            Some(
                "--get"
                    | "--get-all"
                    | "--get-regexp"
                    | "--get-urlmatch"
                    | "--list"
                    | "-l"
                    | "--null"
                    | "-z"
                    | "--name-only"
                    | "--show-origin"
                    | "--show-scope"
            )
        ),
        "fetch" => matches!(subcommand, Some("--dry-run")),
        "remote" => matches!(subcommand, Some("-v" | "--verbose" | "get-url" | "show")),
        "stash" => matches!(subcommand, Some("list" | "show")),
        "worktree" => matches!(subcommand, Some("list")),
        _ => false,
    }
}

pub fn is_definitely_read_only_invocation_args(command: &str, command_args: &[String]) -> bool {
    if is_definitely_read_only_command(command) {
        return true;
    }

    match command {
        "branch" => branch_invocation_is_read_only(command_args),
        "config" => config_invocation_is_read_only(command_args),
        "fetch" => command_args.iter().any(|arg| arg == "--dry-run"),
        "remote" => remote_invocation_is_read_only(command_args),
        "stash" => matches!(first_positional(command_args), Some("list" | "show")),
        "worktree" => matches!(first_positional(command_args), Some("list")),
        _ => false,
    }
}

fn first_positional(args: &[String]) -> Option<&str> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--" {
            return None;
        }
        if arg.starts_with('-') {
            if flag_takes_value(arg) && !arg.contains('=') {
                skip_next = true;
            }
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

fn positional_count(args: &[String]) -> usize {
    let mut count = 0usize;
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--" {
            break;
        }
        if arg.starts_with('-') {
            if flag_takes_value(arg) && !arg.contains('=') {
                skip_next = true;
            }
            continue;
        }
        count += 1;
    }
    count
}

fn flag_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-c" | "-C"
            | "-f"
            | "--file"
            | "--blob"
            | "--type"
            | "--fixed-value"
            | "--get-urlmatch"
            | "--format"
            | "--sort"
            | "--points-at"
            | "--merged"
            | "--no-merged"
            | "--contains"
            | "--no-contains"
            | "--server-option"
            | "-o"
    )
}

fn branch_invocation_is_read_only(args: &[String]) -> bool {
    let mut has_explicit_read_flag = false;
    for arg in args {
        match arg.as_str() {
            "--show-current" | "--list" | "-l" | "--all" | "-a" | "--remotes" | "-r"
            | "--verbose" | "-v" | "-vv" | "--contains" | "--no-contains" | "--merged"
            | "--no-merged" | "--points-at" | "--format" | "--sort" | "--color" | "--no-color" => {
                has_explicit_read_flag = true;
            }
            "-d"
            | "-D"
            | "--delete"
            | "-m"
            | "-M"
            | "--move"
            | "-c"
            | "-C"
            | "--copy"
            | "--set-upstream-to"
            | "-u"
            | "--unset-upstream"
            | "--edit-description"
            | "--track"
            | "--no-track"
            | "-f"
            | "--force"
            | "--create-reflog"
            | "--recurse-submodules" => return false,
            _ => {}
        }
    }

    has_explicit_read_flag || positional_count(args) == 0
}

fn config_invocation_is_read_only(args: &[String]) -> bool {
    let mut has_read_mode = false;
    let mut non_flag_count = 0usize;
    let mut skip_next = false;

    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "--get" | "--get-all" | "--get-regexp" | "--get-urlmatch" | "--get-color"
            | "--get-colorbool" | "--list" | "-l" | "--name-only" => {
                has_read_mode = true;
            }
            "--null" | "-z" | "-lz" | "--show-origin" | "--show-scope" | "--includes"
            | "--global" | "--system" | "--local" | "--worktree" | "--fixed-value" => {}
            "--file" | "-f" | "--blob" | "--type" => {
                skip_next = !arg.contains('=');
            }
            "--add" | "--replace-all" | "--unset" | "--unset-all" | "--rename-section"
            | "--remove-section" | "--edit" | "-e" | "--set" => return false,
            _ if arg.starts_with("--") && arg.contains('=') => {}
            _ if arg.starts_with('-') => {}
            _ => non_flag_count += 1,
        }
    }

    has_read_mode || non_flag_count <= 1
}

fn remote_invocation_is_read_only(args: &[String]) -> bool {
    match first_positional(args) {
        None => true,
        Some("-v" | "--verbose" | "get-url" | "show") => true,
        Some("add" | "rename" | "remove" | "rm" | "set-head" | "set-branches" | "set-url")
        | Some("prune" | "update") => false,
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_commands_detected() {
        assert!(is_definitely_read_only_command("check-ignore"));
        assert!(is_definitely_read_only_command("rev-parse"));
        assert!(is_definitely_read_only_command("status"));
        assert!(is_definitely_read_only_command("diff"));
        assert!(is_definitely_read_only_command("log"));
        assert!(is_definitely_read_only_command("cat-file"));
        assert!(is_definitely_read_only_command("ls-files"));
        assert!(is_definitely_read_only_command("ls-remote"));
    }

    #[test]
    fn mutating_commands_not_read_only() {
        assert!(!is_definitely_read_only_command("commit"));
        assert!(!is_definitely_read_only_command("push"));
        assert!(!is_definitely_read_only_command("pull"));
        assert!(!is_definitely_read_only_command("rebase"));
        assert!(!is_definitely_read_only_command("merge"));
        assert!(!is_definitely_read_only_command("checkout"));
        assert!(!is_definitely_read_only_command("stash"));
        assert!(!is_definitely_read_only_command("reset"));
        assert!(!is_definitely_read_only_command("fetch"));
    }

    #[test]
    fn unknown_commands_not_read_only() {
        assert!(!is_definitely_read_only_command("my-custom-alias"));
        assert!(!is_definitely_read_only_command(""));
    }

    // --- is_definitely_read_only_invocation tests ---

    #[test]
    fn stash_list_is_read_only_invocation() {
        assert!(is_definitely_read_only_invocation("stash", Some("list")));
    }

    #[test]
    fn stash_show_is_read_only_invocation() {
        assert!(is_definitely_read_only_invocation("stash", Some("show")));
    }

    #[test]
    fn stash_mutating_subcommands_are_not_read_only() {
        assert!(!is_definitely_read_only_invocation("stash", Some("pop")));
        assert!(!is_definitely_read_only_invocation("stash", Some("apply")));
        assert!(!is_definitely_read_only_invocation("stash", Some("drop")));
        assert!(!is_definitely_read_only_invocation("stash", Some("branch")));
        assert!(!is_definitely_read_only_invocation("stash", Some("push")));
        assert!(!is_definitely_read_only_invocation("stash", Some("save")));
        // stash with no subcommand defaults to stash push (mutating)
        assert!(!is_definitely_read_only_invocation("stash", None));
    }

    #[test]
    fn worktree_list_is_read_only_invocation() {
        assert!(is_definitely_read_only_invocation("worktree", Some("list")));
    }

    #[test]
    fn ide_polling_invocations_are_read_only() {
        assert!(is_definitely_read_only_invocation("config", Some("--get")));
        assert!(is_definitely_read_only_invocation("config", Some("--null")));
        assert!(is_definitely_read_only_invocation(
            "branch",
            Some("--show-current")
        ));
        assert!(is_definitely_read_only_invocation("remote", Some("-v")));
        assert!(is_definitely_read_only_invocation(
            "fetch",
            Some("--dry-run")
        ));
        assert!(!is_definitely_read_only_invocation("config", Some("--set")));
        assert!(!is_definitely_read_only_invocation("fetch", None));
    }

    #[test]
    fn fetch_invocation_args_only_treat_explicit_dry_run_as_read_only() {
        assert!(is_definitely_read_only_invocation_args(
            "fetch",
            &["--dry-run".into(), "origin".into()]
        ));
        assert!(!is_definitely_read_only_invocation_args(
            "fetch",
            &["-n".into(), "origin".into()]
        ));
        assert!(!is_definitely_read_only_invocation_args(
            "fetch",
            &["origin".into()]
        ));
    }

    #[test]
    fn branch_invocation_args_keep_mutating_forms_on_full_path() {
        assert!(is_definitely_read_only_invocation_args(
            "branch",
            &["--show-current".into()]
        ));
        assert!(is_definitely_read_only_invocation_args(
            "branch",
            &["--format".into(), "%(refname:short)".into()]
        ));
        assert!(!is_definitely_read_only_invocation_args(
            "branch",
            &["-f".into(), "topic".into(), "HEAD".into()]
        ));
        assert!(!is_definitely_read_only_invocation_args(
            "branch",
            &["-m".into(), "old".into(), "new".into()]
        ));
    }

    #[test]
    fn config_invocation_args_distinguish_reads_from_writes() {
        assert!(is_definitely_read_only_invocation_args(
            "config",
            &["--null".into(), "--get".into(), "core.fsmonitor".into()]
        ));
        assert!(is_definitely_read_only_invocation_args(
            "config",
            &["user.email".into()]
        ));
        assert!(is_definitely_read_only_invocation_args(
            "config",
            &["-lz".into(), "--show-origin".into(), "--name-only".into()]
        ));
        assert!(!is_definitely_read_only_invocation_args(
            "config",
            &["--local".into(), "git-ai.test".into(), "1".into()]
        ));
        assert!(!is_definitely_read_only_invocation_args(
            "config",
            &["--unset".into(), "git-ai.test".into()]
        ));
    }

    #[test]
    fn worktree_mutating_subcommands_are_not_read_only() {
        assert!(!is_definitely_read_only_invocation("worktree", Some("add")));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("remove")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("move")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("lock")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("unlock")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("prune")
        ));
        assert!(!is_definitely_read_only_invocation("worktree", None));
    }

    #[test]
    fn standard_read_only_commands_are_read_only_invocations_regardless_of_subcommand() {
        for cmd in &[
            "status",
            "diff",
            "show",
            "log",
            "cat-file",
            "rev-parse",
            "for-each-ref",
            "blame",
            "grep",
            "ls-files",
            "ls-tree",
        ] {
            assert!(
                is_definitely_read_only_invocation(cmd, None),
                "{cmd} should be read-only with no subcommand"
            );
            assert!(
                is_definitely_read_only_invocation(cmd, Some("anything")),
                "{cmd} should be read-only regardless of subcommand"
            );
        }
    }

    #[test]
    fn mutating_commands_are_not_read_only_invocations() {
        for cmd in &[
            "commit", "push", "pull", "rebase", "merge", "checkout", "reset", "fetch",
        ] {
            assert!(
                !is_definitely_read_only_invocation(cmd, None),
                "{cmd} should not be read-only"
            );
        }
    }
}
