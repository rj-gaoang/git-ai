use clap::Parser;
use git_ai::commands;

#[derive(Parser)]
#[command(name = "git-ai")]
#[command(about = "git proxy with AI authorship tracking", long_about = None)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Cli {
    /// Git command and arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn is_git_ai_cli_binary_name(binary_name: &str) -> bool {
    if binary_name.eq_ignore_ascii_case("git-ai") || binary_name.eq_ignore_ascii_case("git-ai.exe") {
        return true;
    }

    let stem = binary_name
        .strip_suffix(".exe")
        .unwrap_or(binary_name);
    stem.eq_ignore_ascii_case("git-ai-daemon") || stem.starts_with("git-ai-daemon-")
}

fn main() {
    // Get the binary name that was called
    let binary_name = std::env::args_os()
        .next()
        .and_then(|arg| arg.into_string().ok())
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or("git-ai".to_string());

    if commands::git_hook_handlers::is_git_hook_binary_name(&binary_name) {
        eprintln!(
            "git-ai: the git core hooks feature has been sunset.\n\
             To remove the deprecated git-ai hook symlinks from this repository, run:\n\
             \n\
             \x20 git-ai git-hooks remove\n"
        );
        std::process::exit(0);
    }

    let cli = Cli::parse();

    #[cfg(debug_assertions)]
    {
        if std::env::var("GIT_AI").as_deref() == Ok("git") {
            commands::git_handlers::handle_git(&cli.args);
            return;
        }
    }

    if is_git_ai_cli_binary_name(&binary_name) {
        commands::git_ai_handlers::handle_git_ai(&cli.args);
        std::process::exit(0);
    }

    commands::git_handlers::handle_git(&cli.args);
}

#[cfg(test)]
mod tests {
    use super::is_git_ai_cli_binary_name;

    #[test]
    fn recognizes_git_ai_cli_binary_names() {
        assert!(is_git_ai_cli_binary_name("git-ai"));
        assert!(is_git_ai_cli_binary_name("git-ai.exe"));
        assert!(is_git_ai_cli_binary_name("git-ai-daemon.exe"));
        assert!(is_git_ai_cli_binary_name(
            "git-ai-daemon-1778661624140634200-28892.exe"
        ));
        assert!(!is_git_ai_cli_binary_name("git"));
        assert!(!is_git_ai_cli_binary_name("git.exe"));
    }
}
