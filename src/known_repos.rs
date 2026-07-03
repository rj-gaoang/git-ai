use crate::error::GitAiError;
use crate::git::repository::Repository;
use crate::utils::normalize_to_posix;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const KNOWN_REPOS_FILE: &str = "known-repos.json";
const MAX_KNOWN_REPOS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnownReposRegistry {
    #[serde(default)]
    pub repos: Vec<String>,
}

pub fn known_repos_path() -> PathBuf {
    crate::mdm::utils::home_dir()
        .join(".git-ai")
        .join("internal")
        .join(KNOWN_REPOS_FILE)
}

fn normalize_repo_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let raw = raw
        .strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
        .unwrap_or(raw.as_ref());
    normalize_to_posix(raw)
}

pub fn load_known_repos() -> Result<KnownReposRegistry, GitAiError> {
    let path = known_repos_path();
    if !path.exists() {
        return Ok(KnownReposRegistry::default());
    }

    let content = fs::read_to_string(&path)?;
    let mut registry: KnownReposRegistry =
        serde_json::from_str(&content).map_err(|e| GitAiError::Generic(e.to_string()))?;
    registry.repos.retain(|repo| !repo.trim().is_empty());
    registry.repos.truncate(MAX_KNOWN_REPOS);
    Ok(registry)
}

fn save_known_repos(registry: &KnownReposRegistry) -> Result<(), GitAiError> {
    let path = known_repos_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = path.with_extension("json.tmp");
    let content =
        serde_json::to_string_pretty(registry).map_err(|e| GitAiError::Generic(e.to_string()))?;
    fs::write(&tmp, content)?;
    match fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(&tmp, &path)?;
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
    }
}

pub fn record_known_repo(repo: &Repository) -> Result<bool, GitAiError> {
    let worktree = normalize_repo_path(repo.canonical_workdir());
    let key = worktree.to_ascii_lowercase();
    let mut registry = load_known_repos()?;
    let before = registry.repos.clone();

    registry
        .repos
        .retain(|repo| repo.to_ascii_lowercase() != key);
    registry.repos.insert(0, worktree);
    registry.repos.truncate(MAX_KNOWN_REPOS);

    if registry.repos != before {
        save_known_repos(&registry)?;
        return Ok(true);
    }
    Ok(false)
}

pub fn record_known_repo_best_effort(repo: &Repository, source: &str) -> bool {
    match record_known_repo(repo) {
        Ok(changed) => changed,
        Err(error) => {
            crate::diagnostics::append_debug_event(
                "known_repo_record_failed",
                serde_json::json!({
                    "repo": repo.canonical_workdir().to_string_lossy().to_string(),
                    "source": source,
                    "error": error.to_string(),
                }),
            );
            false
        }
    }
}
