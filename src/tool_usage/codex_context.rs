//! Recover workdir omitted by Codex's Bash hook normalization.
//! Reads only recent invocation evidence in the current transcript. No JS is run.
use serde_json::Value;
use std::io::{BufRead, BufReader, Seek, SeekFrom};

const TAIL_BYTES: u64 = 512 * 1024;
const MAX_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    String(String),
    Symbol(char),
}

fn tokens(source: &str) -> Option<Vec<Token>> {
    let mut chars = source.chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            if !closed {
                return None;
            }
            continue;
        }
        // Dynamic templates/regex are deliberately unsupported, rather than guessed.
        if matches!(c, '`' | '/') {
            return None;
        }
        if c == '\'' || c == '"' {
            let mut value = String::new();
            let mut closed = false;
            while let Some(next) = chars.next() {
                if next == c {
                    closed = true;
                    break;
                }
                if matches!(next, '\n' | '\r') {
                    return None;
                }
                if next != '\\' {
                    value.push(next);
                    continue;
                }
                value.push(match chars.next()? {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    '/' => '/',
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if hex.len() != 4 {
                            return None;
                        }
                        char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
                    }
                    _ => return None,
                });
            }
            if !closed {
                return None;
            }
            out.push(Token::String(value));
        } else if c.is_ascii_alphanumeric() || matches!(c, '_' | '$') {
            let mut word = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
            {
                word.push(chars.next()?);
            }
            out.push(Token::Word(word));
        } else {
            out.push(Token::Symbol(c));
        }
    }
    Some(out)
}

fn literal_invocations(source: &str, command: &str) -> Option<Vec<Option<String>>> {
    if source.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let t = tokens(source)?;
    let mut dirs = Vec::new();
    let mut i = 0;
    while i + 5 < t.len() {
        if t[i..i + 5]
            != [
                Token::Word("tools".into()),
                Token::Symbol('.'),
                Token::Word("exec_command".into()),
                Token::Symbol('('),
                Token::Symbol('{'),
            ]
        {
            i += 1;
            continue;
        }
        i += 5;
        let mut cmd = None;
        let mut workdir = None;
        let mut valid = true;
        while i < t.len() && t[i] != Token::Symbol('}') {
            let key = match &t[i] {
                Token::Word(key) | Token::String(key) => key.clone(),
                _ => return None, // spread/computed keys can override workdir
            };
            i += 1;
            if t.get(i) != Some(&Token::Symbol(':')) {
                return None;
            }
            i += 1;
            let start = i;
            let mut depth = 0usize;
            while i < t.len() {
                match t[i] {
                    Token::Symbol(',' | '}') if depth == 0 => break,
                    Token::Symbol('(' | '[' | '{') => depth += 1,
                    Token::Symbol(')' | ']' | '}') => depth = depth.checked_sub(1)?,
                    _ => (),
                }
                i += 1;
            }
            if matches!(
                key.as_str(),
                "cmd" | "command" | "workdir" | "cwd" | "working_directory"
            ) {
                let value = if i == start + 1 {
                    if let Token::String(value) = &t[start] {
                        Some(value.clone())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if value.is_none() {
                    valid = false;
                }
                if matches!(key.as_str(), "cmd" | "command") {
                    if cmd.is_some() {
                        valid = false;
                    }
                    cmd = value;
                } else {
                    if workdir.is_some() {
                        valid = false;
                    }
                    workdir = value;
                }
            }
            if t.get(i) == Some(&Token::Symbol(',')) {
                i += 1;
            }
        }
        if t.get(i) != Some(&Token::Symbol('}')) || t.get(i + 1) != Some(&Token::Symbol(')')) {
            return None;
        }
        if cmd.as_deref() == Some(command) {
            dirs.push(if valid {
                workdir.filter(|s| !s.is_empty())
            } else {
                None
            });
        }
        i += 2;
    }
    Some(dirs)
}

pub(super) fn invocation_workdir(data: &Value, session: &str) -> Option<String> {
    let input = data.get("tool_input").or_else(|| data.get("toolInput"))?;
    let command = input
        .get("command")
        .or_else(|| input.get("cmd"))?
        .as_str()?;
    let path = data
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            crate::streams::agents::CodexAgent::find_rollout_path_for_session_in_home(
                session,
                &crate::mdm::utils::codex_home_dir(),
            )
            .ok()
            .flatten()
        })?;
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(TAIL_BYTES);
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start)).ok()?;
    let mut line = String::new();
    if start > 0 {
        reader.read_line(&mut line).ok()?;
    }
    let id = data
        .get("tool_use_id")
        .or_else(|| data.get("toolUseId"))
        .and_then(Value::as_str);
    let mut dirs = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let Ok(raw) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if raw["type"] == "turn_context" {
            dirs.clear();
            continue;
        }
        if raw["type"] != "response_item" {
            continue;
        }
        let Some(timestamp) = raw
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        else {
            continue;
        };
        let age = chrono::Utc::now()
            .signed_duration_since(timestamp)
            .num_seconds();
        if !(0..=300).contains(&age) {
            continue;
        }
        let payload = &raw["payload"];
        if payload["type"] == "function_call"
            && payload["call_id"].as_str() == id
            && matches!(
                payload["name"].as_str(),
                Some("exec_command" | "functions.exec_command")
            )
        {
            let args: Value = serde_json::from_str(payload["arguments"].as_str()?).ok()?;
            if args
                .get("cmd")
                .or_else(|| args.get("command"))
                .and_then(Value::as_str)
                == Some(command)
            {
                return args["workdir"].as_str().map(str::to_owned);
            }
        }
        // The orchestration bridge generates exec-* IDs unrelated to rollout call_id.
        // Require an exact command and a unique literal cwd within the current turn.
        if payload["type"] == "custom_tool_call" && payload["name"] == "exec" {
            // Only the newest orchestration invocation can describe the current
            // hook. Never borrow a cwd from an older call with identical text.
            dirs = payload["input"]
                .as_str()
                .and_then(|source| literal_invocations(source, command))
                .unwrap_or_default();
        }
    }
    dirs.sort();
    dirs.dedup();
    if dirs.len() == 1 {
        dirs.remove(0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_usage::tests::TelemetryFlag;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn static_workdir_parser_never_executes_or_guesses() {
        let source = r#"text(await tools.exec_command({cmd:'cat .agents/skills/review/SKILL.md', workdir:"C:\\project", max_output_tokens:2000}));"#;
        assert_eq!(
            literal_invocations(source, "cat .agents/skills/review/SKILL.md").unwrap(),
            vec![Some("C:\\project".into())]
        );
        for source in [
            r#"tools.exec_command({cmd:'cat a',workdir:project})"#,
            r#"tools.exec_command({cmd:'cat a',workdir:'wrong' + '/repo'})"#,
            r#"tools.exec_command({cmd:'cat a',workdir:'wrong',...override})"#,
            r#"tools.exec_command({cmd:'cat a',workdir:`${project}`})"#,
            r#"const x = "tools.exec_command({cmd:'cat a',workdir:'wrong'})";"#,
            r#"/* tools.exec_command({cmd:'cat a',workdir:'wrong'}) */"#,
        ] {
            assert!(
                literal_invocations(source, "cat a").is_none_or(|v| v.iter().all(Option::is_none))
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn normalized_codex_hook_reads_actual_workdir_and_rejects_ambiguity() {
        let _flag = TelemetryFlag::set("true");
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let path = project.join(".github/agents/review.agent.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "完整正文\n结尾").unwrap();
        let mut transcript = tempfile::NamedTempFile::new().unwrap();
        let command = "Get-Content .github/agents/review.agent.md";
        let mut hook = json!({"hook_event_name":"PostToolUse", "tool_name":"Bash", "tool_use_id":"exec-generated", "cwd":root.path(), "tool_input":{"command":command}, "transcript_path":transcript.path()});
        for dir in [&project, root.path()] {
            let source = format!(
                "text(await tools.exec_command({{cmd:{},workdir:{}}}));",
                serde_json::to_string(command).unwrap(),
                serde_json::to_string(dir).unwrap()
            );
            writeln!(transcript, "{}", json!({"type":"response_item","timestamp":chrono::Utc::now().to_rfc3339(),"payload":{"type":"custom_tool_call","name":"exec","call_id":"outer-call","input":source}})).unwrap();
            if dir == &project {
                let crate::commands::checkpoint_agent::presets::ParsedHookEvent::ToolInvocation {
                    event,
                    ..
                } = super::super::native_invocation(&hook, "codex", "s", "t").unwrap()
                else {
                    panic!()
                };
                assert_eq!(event.definition_content.as_deref(), Some("完整正文\n结尾"));
                assert_eq!(event.definition_path.as_deref(), path.to_str());
            } else {
                assert_eq!(
                    invocation_workdir(&hook, "s"),
                    Some(root.path().to_string_lossy().into_owned())
                );
            }
        }
        let source = format!(
            "tools.exec_command({{cmd:{command},workdir:{project}}}); tools.exec_command({{cmd:{command},workdir:{root}}});",
            command = serde_json::to_string(command).unwrap(),
            project = serde_json::to_string(&project).unwrap(),
            root = serde_json::to_string(root.path()).unwrap()
        );
        writeln!(transcript,"{}",json!({"type":"response_item","timestamp":chrono::Utc::now().to_rfc3339(),"payload":{"type":"custom_tool_call","name":"exec","input":source}})).unwrap();
        assert!(invocation_workdir(&hook, "s").is_none());
        // Missing workdir in the latest call must not borrow an older directory.
        let source = format!(
            "tools.exec_command({{cmd:{}}})",
            serde_json::to_string(command).unwrap()
        );
        writeln!(transcript,"{}",json!({"type":"response_item","timestamp":chrono::Utc::now().to_rfc3339(),"payload":{"type":"custom_tool_call","name":"exec","input":source}})).unwrap();
        assert!(invocation_workdir(&hook, "s").is_none());
        hook["tool_input"]["command"] = json!("Get-Content other.agent.md");
        assert!(invocation_workdir(&hook, "s").is_none());
    }
}
