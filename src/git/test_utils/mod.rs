use crate::authorship::authorship_log_serialization::{AuthorshipLog, generate_trace_id};
use crate::authorship::post_commit::post_commit;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint_agent::orchestrator::{
    BaseCommit, CheckpointFile, CheckpointRequest,
};
use crate::daemon::checkpoint::{PreparedPathRole, ResolvedCheckpointExecution};
use crate::error::GitAiError;
use crate::git::repository::Repository;
use crate::utils::normalize_to_posix;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

fn create_unique_tmp_dir(prefix: &str) -> Result<PathBuf, GitAiError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();

    for _ in 0..100 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("{prefix}-{now}-{pid}-{seq}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(GitAiError::IoError(err)),
        }
    }

    Err(GitAiError::Generic(
        "failed to create unique temporary directory".to_string(),
    ))
}

pub fn init_test_git_config() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = std::env::temp_dir().join("git-ai-test-global-gitconfig");
        let _ = fs::write(
            &path,
            "[user]\n\tname = Test User\n\temail = test@example.com\n",
        );

        #[cfg(not(windows))]
        let canonical = path.canonicalize().unwrap_or(path);
        #[cfg(windows)]
        let canonical = path;

        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &canonical);
            #[cfg(not(windows))]
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
}

#[derive(Debug)]
pub struct TmpFile {
    repo_path: PathBuf,
    filename: String,
    contents: String,
}

impl TmpFile {
    pub fn update(&mut self, new_contents: &str) -> Result<(), GitAiError> {
        self.contents = new_contents.to_string();
        self.write_to_disk()
    }

    pub fn append(&mut self, content: &str) -> Result<(), GitAiError> {
        if let Ok(disk_contents) = fs::read_to_string(self.repo_path.join(&self.filename)) {
            self.contents = disk_contents;
        }
        if !self.contents.is_empty() && !self.contents.ends_with('\n') {
            self.contents.push('\n');
        }
        self.contents.push_str(content);
        self.write_to_disk()
    }

    pub fn path(&self) -> PathBuf {
        self.repo_path.join(&self.filename)
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    fn write_to_disk(&self) -> Result<(), GitAiError> {
        let file_path = self.path();
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(file_path, &self.contents)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct TmpRepo {
    path: PathBuf,
    repo_gitai: Repository,
}

impl TmpRepo {
    pub fn new() -> Result<Self, GitAiError> {
        if std::env::var("GIT_AI_TEST_DB_PATH").is_err() {
            let test_db_path = std::env::temp_dir().join("git-ai-unit-test-db");
            unsafe {
                std::env::set_var("GIT_AI_TEST_DB_PATH", &test_db_path);
            }
        }

        init_test_git_config();

        let path = create_unique_tmp_dir("git-ai-tmp")?;
        run_git_in(&path, &["init", "-q"])?;
        run_git_in(&path, &["config", "user.name", "Test User"])?;
        run_git_in(&path, &["config", "user.email", "test@example.com"])?;
        run_git_in(&path, &["config", "core.autocrlf", "false"])?;

        let repo_gitai = crate::git::repository::find_repository_in_path(
            path.to_str()
                .ok_or_else(|| GitAiError::Generic("invalid temp path".to_string()))?,
        )?;

        Ok(Self { path, repo_gitai })
    }

    pub fn write_file(
        &self,
        filename: &str,
        contents: &str,
        add_to_git: bool,
    ) -> Result<TmpFile, GitAiError> {
        let file_path = self.path.join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&file_path, contents)?;

        if add_to_git {
            self.run_git(&["add", filename])?;
        }

        Ok(TmpFile {
            repo_path: self.path.clone(),
            filename: filename.to_string(),
            contents: contents.to_string(),
        })
    }

    pub fn trigger_checkpoint_with_author(
        &self,
        author: &str,
    ) -> Result<(usize, usize, usize), GitAiError> {
        self.trigger_checkpoint(author, CheckpointKind::KnownHuman, None)
    }

    pub fn trigger_checkpoint_with_ai(
        &self,
        agent_name: &str,
        model: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(usize, usize, usize), GitAiError> {
        let session_id = match agent_name {
            "Claude" | "GPT-4" | "GPT-4o" => "test_session_fixed".to_string(),
            other => other.to_string(),
        };
        let agent_id = AgentId {
            tool: tool.unwrap_or("test_tool").to_string(),
            id: session_id,
            model: model.unwrap_or("test_model").to_string(),
        };

        self.trigger_checkpoint(agent_name, CheckpointKind::AiAgent, Some(agent_id))
    }

    pub fn commit_with_message(&self, message: &str) -> Result<AuthorshipLog, GitAiError> {
        self.run_git(&["add", "-A"])?;
        let parent_sha = self.head_sha_optional()?;
        self.run_git_with_env(
            &["commit", "-m", message, "--no-verify"],
            &[
                ("GIT_AUTHOR_DATE", "2023-01-01T12:00:00Z"),
                ("GIT_COMMITTER_DATE", "2023-01-01T12:00:00Z"),
                ("GIT_EDITOR", "true"),
            ],
        )?;
        let commit_sha = self.get_head_commit_sha()?;
        let result = post_commit(
            &self.repo_gitai,
            parent_sha,
            commit_sha,
            "Test User".to_string(),
            false,
        )?;
        Ok(result.1)
    }

    pub fn create_branch(&self, branch_name: &str) -> Result<(), GitAiError> {
        self.run_git(&["checkout", "-q", "-b", branch_name])?;
        Ok(())
    }

    pub fn switch_branch(&self, branch_name: &str) -> Result<(), GitAiError> {
        self.run_git(&["checkout", "-q", branch_name])?;
        Ok(())
    }

    pub fn merge_branch(&self, branch_name: &str, message: &str) -> Result<(), GitAiError> {
        self.run_git_with_env(
            &["merge", branch_name, "-m", message, "-X", "theirs"],
            &[("GIT_EDITOR", "true")],
        )?;
        let parent_sha = self.git_stdout(&["rev-parse", "HEAD^1"]).ok();
        let commit_sha = self.get_head_commit_sha()?;
        post_commit(
            &self.repo_gitai,
            parent_sha,
            commit_sha,
            "Test User".to_string(),
            false,
        )?;
        Ok(())
    }

    pub fn current_branch(&self) -> Result<String, GitAiError> {
        self.git_stdout(&["branch", "--show-current"])
    }

    pub fn head_commit_sha(&self) -> Result<String, GitAiError> {
        self.git_stdout(&["rev-parse", "HEAD"])
    }

    pub fn get_head_commit_sha(&self) -> Result<String, GitAiError> {
        self.head_commit_sha()
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn gitai_repo(&self) -> &Repository {
        &self.repo_gitai
    }

    fn trigger_checkpoint(
        &self,
        author: &str,
        kind: CheckpointKind,
        agent_id: Option<AgentId>,
    ) -> Result<(usize, usize, usize), GitAiError> {
        let paths = self.current_checkpoint_scope_paths()?;
        if paths.is_empty() {
            return Ok((0, 0, 0));
        }

        let base_commit = self.head_sha_optional()?;
        let base = base_commit
            .as_ref()
            .map(|sha| BaseCommit::Sha(sha.clone()))
            .unwrap_or(BaseCommit::Initial);
        let base_string = base_commit.unwrap_or_else(|| "initial".to_string());
        let trace_id = generate_trace_id();
        let mut dirty_files = HashMap::new();
        let mut checkpoint_files = Vec::new();

        for path in paths {
            let absolute_path = self.path.join(&path);
            let content = if absolute_path.exists() {
                Some(fs::read_to_string(&absolute_path).unwrap_or_default())
            } else {
                Some(String::new())
            };

            if let Some(ref content) = content
                && !content.chars().any(|ch| ch == '\0')
            {
                dirty_files.insert(path.clone(), content.clone());
            }

            checkpoint_files.push(CheckpointFile {
                path: absolute_path,
                content,
                repo_work_dir: self.path.clone(),
                base_commit: base.clone(),
            });
        }

        let path_role = if kind == CheckpointKind::Human {
            PreparedPathRole::WillEdit
        } else {
            PreparedPathRole::Edited
        };
        let request = CheckpointRequest {
            trace_id: trace_id.clone(),
            checkpoint_kind: kind,
            agent_id,
            files: checkpoint_files,
            path_role,
            transcript_source: None,
            metadata: HashMap::new(),
        };
        let resolved = ResolvedCheckpointExecution {
            base_commit: base_string,
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            files: dirty_files.keys().cloned().collect(),
            dirty_files,
        };

        crate::daemon::checkpoint::execute_resolved_checkpoint_from_daemon(
            &self.repo_gitai,
            author,
            kind,
            request,
            resolved,
        )?;

        Ok((0, 0, 0))
    }

    fn current_checkpoint_scope_paths(&self) -> Result<Vec<String>, GitAiError> {
        let mut paths = HashSet::new();
        for args in [
            ["diff", "--name-only"].as_slice(),
            ["diff", "--cached", "--name-only"].as_slice(),
            ["ls-files", "--others", "--exclude-standard"].as_slice(),
        ] {
            let output = self.git_stdout(args)?;
            for line in output.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    paths.insert(normalize_to_posix(trimmed));
                }
            }
        }
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    fn head_sha_optional(&self) -> Result<Option<String>, GitAiError> {
        let output = self.git_output(&["rev-parse", "--verify", "HEAD"])?;
        if output.status.success() {
            Ok(Some(stdout_to_string(&output)))
        } else {
            Ok(None)
        }
    }

    fn git_stdout(&self, args: &[&str]) -> Result<String, GitAiError> {
        let output = self.git_output(args)?;
        if output.status.success() {
            Ok(stdout_to_string(&output))
        } else {
            Err(git_command_error(args, &output))
        }
    }

    fn run_git(&self, args: &[&str]) -> Result<(), GitAiError> {
        self.run_git_with_env(args, &[])
    }

    fn run_git_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<(), GitAiError> {
        let mut command = git_command(&self.path);
        command.args(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command.output().map_err(GitAiError::IoError)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(git_command_error(args, &output))
        }
    }

    fn git_output(&self, args: &[&str]) -> Result<Output, GitAiError> {
        let mut command = git_command(&self.path);
        command.args(args);
        command.output().map_err(GitAiError::IoError)
    }
}

impl Drop for TmpRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_git_in(path: &Path, args: &[&str]) -> Result<(), GitAiError> {
    let mut command = git_command(path);
    command.args(args);
    let output = command.output().map_err(GitAiError::IoError)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_command_error(args, &output))
    }
}

fn git_command(path: &Path) -> Command {
    let mut command = Command::new(crate::config::Config::get().git_cmd());
    command.current_dir(path);
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GCM_INTERACTIVE", "0");
    command.env("GCM_GUI_PROMPT", "0");
    command.env("GIT_PAGER", "cat");
    command
}

fn stdout_to_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_command_error(args: &[&str], output: &Output) -> GitAiError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    GitAiError::Generic(format!(
        "git {} failed: {}{}",
        args.join(" "),
        stderr.trim(),
        if stdout.trim().is_empty() {
            String::new()
        } else {
            format!("\n{}", stdout.trim())
        }
    ))
}
