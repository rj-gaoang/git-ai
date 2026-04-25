use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const MCP_FILENAME: &str = "mcp.json";
const USER_ID_ENV_VAR: &str = "GIT_AI_REPORT_REMOTE_USER_ID";
const VSCODE_MCP_PATH_ENV_VAR: &str = "GIT_AI_VSCODE_MCP_CONFIG_PATH";
const IDEA_MCP_PATH_ENV_VAR: &str = "GIT_AI_IDEA_MCP_CONFIG_PATH";

pub fn resolve_x_user_id(repo_workdir: Option<&Path>) -> Option<String> {
    env_var_non_empty(USER_ID_ENV_VAR).or_else(|| {
        candidate_paths(repo_workdir)
            .into_iter()
            .find_map(|path| read_x_user_id_from_file(&path))
    })
}

fn candidate_paths(repo_workdir: Option<&Path>) -> Vec<PathBuf> {
    build_candidate_paths(
        repo_workdir,
        std::env::var_os(VSCODE_MCP_PATH_ENV_VAR).as_deref(),
        std::env::var_os(IDEA_MCP_PATH_ENV_VAR).as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("LOCALAPPDATA").as_deref(),
        dirs::home_dir().as_deref(),
    )
}

fn build_candidate_paths(
    repo_workdir: Option<&Path>,
    vscode_override: Option<&OsStr>,
    idea_override: Option<&OsStr>,
    appdata: Option<&OsStr>,
    localappdata: Option<&OsStr>,
    _home_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();

    if let Some(path) = vscode_override.filter(|path| !path.is_empty()) {
        push_unique(&mut paths, &mut seen, PathBuf::from(path));
    }
    if let Some(path) = idea_override.filter(|path| !path.is_empty()) {
        push_unique(&mut paths, &mut seen, PathBuf::from(path));
    }
    if let Some(workdir) = repo_workdir {
        push_unique(
            &mut paths,
            &mut seen,
            workdir.join(".vscode").join(MCP_FILENAME),
        );
    }

    #[cfg(windows)]
    {
        if let Some(appdata) = appdata {
            let appdata = Path::new(appdata);
            push_unique(
                &mut paths,
                &mut seen,
                appdata.join("Code").join("User").join(MCP_FILENAME),
            );
            push_unique(
                &mut paths,
                &mut seen,
                appdata
                    .join("Code - Insiders")
                    .join("User")
                    .join(MCP_FILENAME),
            );
            push_unique(
                &mut paths,
                &mut seen,
                appdata
                    .join("github-copilot")
                    .join("intellij")
                    .join(MCP_FILENAME),
            );
        }

        if let Some(localappdata) = localappdata {
            push_unique(
                &mut paths,
                &mut seen,
                Path::new(localappdata)
                    .join("github-copilot")
                    .join("intellij")
                    .join(MCP_FILENAME),
            );
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(home_dir) = _home_dir {
            push_unique(
                &mut paths,
                &mut seen,
                home_dir
                    .join(".config")
                    .join("Code")
                    .join("User")
                    .join(MCP_FILENAME),
            );
            push_unique(
                &mut paths,
                &mut seen,
                home_dir
                    .join(".config")
                    .join("Code - Insiders")
                    .join("User")
                    .join(MCP_FILENAME),
            );
            push_unique(
                &mut paths,
                &mut seen,
                home_dir
                    .join(".config")
                    .join("github-copilot")
                    .join("intellij")
                    .join(MCP_FILENAME),
            );
        }
    }

    paths
}

fn push_unique(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn read_x_user_id_from_file(path: &Path) -> Option<String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::debug!(
                "[ide_mcp] Failed to read MCP config '{}': {}",
                path.display(),
                err
            );
            return None;
        }
    };

    parse_x_user_id_from_str(&raw).or_else(|| {
        tracing::debug!(
            "[ide_mcp] Failed to parse X-USER-ID from MCP config '{}'",
            path.display()
        );
        None
    })
}

fn parse_x_user_id_from_str(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw.trim_start_matches('\u{feff}')).ok()?;
    parse_x_user_id_from_json(&value)
}

fn parse_x_user_id_from_json(root: &Value) -> Option<String> {
    let servers = root.get("servers")?.as_object()?;
    let mut candidates = servers
        .iter()
        .map(|(name, server)| ServerCandidate {
            name,
            server,
            score: server_score(name, server),
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.name.cmp(right.name))
    });

    candidates
        .into_iter()
        .find_map(|candidate| extract_x_user_id_from_server(candidate.server))
}

fn extract_x_user_id_from_server(server: &Value) -> Option<String> {
    value_to_non_empty_string(server.pointer("/requestInit/headers/X-USER-ID"))
        .or_else(|| value_to_non_empty_string(server.pointer("/headers/X-USER-ID")))
}

fn value_to_non_empty_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn server_score(name: &str, server: &Value) -> i32 {
    let mut score = 0;

    if matches!(name, "codereview-mcp" | "codereview-mcp-server") {
        score += 100;
    }

    if let Some(url) = server.get("url").and_then(Value::as_str) {
        if url.contains("mcppage.ruijie.com.cn:9810/mcp") {
            score += 50;
        }
        if url.contains("localhost:9810/mcp") {
            score += 25;
        }
    }

    if extract_x_user_id_from_server(server).is_some() {
        score += 10;
    }

    score
}

fn env_var_non_empty(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

struct ServerCandidate<'a> {
    name: &'a str,
    server: &'a Value,
    score: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_idea_style_request_init_headers() {
        let raw = r#"
        {
            "servers": {
                "codereview-mcp": {
                    "url": "http://mcppage.ruijie.com.cn:9810/mcp",
                    "requestInit": {
                        "headers": {
                            "X-USER-ID": "105"
                        }
                    }
                }
            }
        }
        "#;

        assert_eq!(parse_x_user_id_from_str(raw), Some("105".to_string()));
    }

    #[test]
    fn parses_vscode_style_headers_and_numeric_values() {
        let raw = r#"
        {
            "servers": {
                "codereview-cc22": {
                    "url": "http://localhost:9810/mcp",
                    "headers": {
                        "X-USER-ID": 205
                    }
                }
            }
        }
        "#;

        assert_eq!(parse_x_user_id_from_str(raw), Some("205".to_string()));
    }

    #[test]
    fn prefers_higher_ranked_server_candidates() {
        let raw = r#"
        {
            "servers": {
                "generic-server": {
                    "url": "http://example.com/mcp",
                    "headers": {
                        "X-USER-ID": "999"
                    }
                },
                "codereview-mcp-server": {
                    "url": "http://localhost:9810/mcp",
                    "headers": {
                        "X-USER-ID": "106"
                    }
                }
            }
        }
        "#;

        assert_eq!(parse_x_user_id_from_str(raw), Some("106".to_string()));
    }

    #[test]
    fn returns_none_for_invalid_json() {
        assert_eq!(parse_x_user_id_from_str("{not-json}"), None);
    }

    #[test]
    fn reads_x_user_id_from_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join(MCP_FILENAME);
        fs::write(
            &file_path,
            r#"{"servers":{"codereview-mcp":{"headers":{"X-USER-ID":"301"}}}}"#,
        )
        .unwrap();

        assert_eq!(
            read_x_user_id_from_file(&file_path),
            Some("301".to_string())
        );
    }

    #[test]
    fn builds_candidate_paths_in_priority_order() {
        let repo_root = Path::new("/repo/root");
        let paths = build_candidate_paths(
            Some(repo_root),
            Some(OsStr::new("/overrides/vscode-mcp.json")),
            Some(OsStr::new("/overrides/idea-mcp.json")),
            Some(OsStr::new("/appdata")),
            Some(OsStr::new("/localappdata")),
            Some(Path::new("/home/user")),
        );

        assert_eq!(paths[0], PathBuf::from("/overrides/vscode-mcp.json"));
        assert_eq!(paths[1], PathBuf::from("/overrides/idea-mcp.json"));
        assert_eq!(paths[2], PathBuf::from("/repo/root/.vscode/mcp.json"));
    }
}
