use crate::utils::LockFile;
use chrono::{FixedOffset, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEBUG_LOG_FILE: &str = "debug.jsonl";
const DEBUG_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEBUG_LOG_RETAIN_BYTES: u64 = 512 * 1024 * 1024;
const DEBUG_LOG_BEIJING_OFFSET_SECONDS: i32 = 8 * 60 * 60;
const DEBUG_LOG_LOCK_FILE: &str = "debug.lock";
const DEBUG_LOG_LOCK_WAIT_MILLIS: u64 = 2_000;
const DEBUG_LOG_LOCK_RETRY_MILLIS: u64 = 20;

pub(crate) fn debug_enabled() -> bool {
    match std::env::var("GIT_AI_DEBUG") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

pub(crate) fn debug_stderr_enabled() -> bool {
    cfg!(debug_assertions)
        || std::env::var("GIT_AI_DEBUG_STDERR")
            .ok()
            .is_some_and(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "" | "0" | "false" | "off" | "no"
                )
            })
}

pub fn append_process_started_event(binary_name: &str, args: &[String]) {
    append_debug_event(
        "process_started",
        json!({
            "binaryName": binary_name,
            "argv0": std::env::args_os().next().map(os_string_to_lossy),
            "argsPreview": sanitized_command_args(args),
            "argCount": args.len(),
            "currentDir": current_dir_for_debug(),
            "currentExe": current_exe_for_debug(),
            "gitAiVersion": env!("CARGO_PKG_VERSION"),
            "isGitProxy": binary_name != "git-ai" && binary_name != "git-ai.exe",
            "pathHead": path_head_for_debug(8),
        }),
    );
}

pub(crate) fn append_debug_event(event: &str, fields: Value) {
    if !debug_enabled() {
        return;
    }

    let Some(log_path) = debug_log_path() else {
        return;
    };
    let Some(log_dir) = log_path.parent() else {
        return;
    };
    if fs::create_dir_all(log_dir).is_err() {
        return;
    }

    let Some(_log_lock) = acquire_debug_log_lock(log_dir) else {
        return;
    };

    enforce_debug_log_size_limit(&log_path);

    let now = Utc::now();
    let mut record = Map::new();
    record.insert("schemaVersion".to_string(), json!(1));
    record.insert("event".to_string(), Value::String(event.to_string()));
    record.insert("timestampMs".to_string(), json!(now.timestamp_millis()));
    record.insert("timestamp".to_string(), Value::String(debug_timestamp(now)));
    record.insert("processId".to_string(), json!(std::process::id()));

    match fields {
        Value::Object(fields) => {
            for (key, value) in fields {
                record.insert(key, value);
            }
        }
        other => {
            record.insert("details".to_string(), other);
        }
    }

    let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    else {
        return;
    };
    let _ = writeln!(file, "{}", Value::Object(record));
}

pub(crate) fn debug_log_path() -> Option<PathBuf> {
    crate::config::git_ai_dir_path().map(|dir| dir.join("logs").join(DEBUG_LOG_FILE))
}

pub(crate) fn sanitized_command_args(args: &[String]) -> Vec<String> {
    let mut sanitized = Vec::with_capacity(args.len());
    let mut redact_next = false;

    for arg in args {
        if redact_next {
            sanitized.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }

        if matches!(arg.as_str(), "-m" | "--message" | "-F" | "--file") {
            sanitized.push(arg.clone());
            redact_next = true;
            continue;
        }

        if let Some((flag, _)) = arg.split_once('=')
            && matches!(flag, "--message" | "--file")
        {
            sanitized.push(format!("{flag}=<redacted>"));
            continue;
        }

        if arg.starts_with("-m") && arg.len() > 2 {
            sanitized.push("-m<redacted>".to_string());
            continue;
        }

        sanitized.push(redact_url_credentials(arg));
    }

    sanitized
}

pub(crate) fn current_dir_for_debug() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

pub(crate) fn current_exe_for_debug() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

fn path_head_for_debug(limit: usize) -> Option<Vec<String>> {
    let path = std::env::var_os("PATH")?;
    Some(
        std::env::split_paths(&path)
            .take(limit)
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
    )
}

fn os_string_to_lossy(value: OsString) -> String {
    value.to_string_lossy().to_string()
}

fn redact_url_credentials(value: &str) -> String {
    for scheme in ["https://", "http://", "ssh://"] {
        let Some(rest) = value.strip_prefix(scheme) else {
            continue;
        };
        let Some(at_index) = rest.find('@') else {
            return value.to_string();
        };
        let Some(first_slash) = rest.find('/') else {
            return value.to_string();
        };
        if at_index < first_slash {
            return format!("{scheme}<redacted>@{}", &rest[at_index + 1..]);
        }
    }

    value.to_string()
}

fn debug_timestamp(now_utc: chrono::DateTime<Utc>) -> String {
    let beijing_offset = FixedOffset::east_opt(DEBUG_LOG_BEIJING_OFFSET_SECONDS)
        .expect("UTC+08:00 should always be a valid fixed offset");
    now_utc
        .with_timezone(&beijing_offset)
        .to_rfc3339_opts(SecondsFormat::Millis, false)
}

fn enforce_debug_log_size_limit(log_path: &Path) {
    let Ok(metadata) = fs::metadata(log_path) else {
        return;
    };
    if metadata.len() <= DEBUG_LOG_MAX_BYTES {
        return;
    }

    let _ = retain_debug_log_tail(log_path, DEBUG_LOG_RETAIN_BYTES);
}

fn retain_debug_log_tail(log_path: &Path, retain_bytes: u64) -> std::io::Result<()> {
    let len = fs::metadata(log_path)?.len();
    if len <= retain_bytes {
        return Ok(());
    }

    let mut input = fs::File::open(log_path)?;
    let mut start = len.saturating_sub(retain_bytes);
    if start > 0 {
        input.seek(SeekFrom::Start(start))?;
        let mut byte = [0u8; 1];
        while input.read(&mut byte)? == 1 {
            start += 1;
            if byte[0] == b'\n' {
                break;
            }
        }
    }

    input.seek(SeekFrom::Start(start))?;
    let temp_path = log_path.with_file_name(format!(
        "{}.tmp",
        log_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(DEBUG_LOG_FILE)
    ));
    let mut output = fs::File::create(&temp_path)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    drop(output);
    drop(input);

    fs::remove_file(log_path)?;
    fs::rename(temp_path, log_path)?;
    Ok(())
}

fn acquire_debug_log_lock(log_dir: &Path) -> Option<LockFile> {
    let lock_path = log_dir.join(DEBUG_LOG_LOCK_FILE);
    let started_at = Instant::now();
    let max_wait = Duration::from_millis(DEBUG_LOG_LOCK_WAIT_MILLIS);
    let retry = Duration::from_millis(DEBUG_LOG_LOCK_RETRY_MILLIS);
    loop {
        if let Some(lock) = LockFile::try_acquire(&lock_path) {
            return Some(lock);
        }
        if started_at.elapsed() >= max_wait {
            return None;
        }
        std::thread::sleep(retry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn debug_enabled_defaults_on_and_accepts_explicit_disable_values() {
        unsafe {
            std::env::remove_var("GIT_AI_DEBUG");
        }
        assert!(debug_enabled());

        for value in ["false", "0", "off", "no"] {
            unsafe {
                std::env::set_var("GIT_AI_DEBUG", value);
            }
            assert!(!debug_enabled(), "value {value} should disable debug JSONL");
        }

        unsafe {
            std::env::set_var("GIT_AI_DEBUG", "true");
        }
        assert!(debug_enabled());

        unsafe {
            std::env::remove_var("GIT_AI_DEBUG");
        }
    }

    #[test]
    fn retain_debug_log_tail_keeps_recent_complete_lines() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join(DEBUG_LOG_FILE);
        fs::write(&log_path, "old\nmiddle\nnew\n").unwrap();

        retain_debug_log_tail(&log_path, 8).unwrap();

        let retained = fs::read_to_string(&log_path).unwrap();
        assert_eq!(retained, "new\n");
    }

    #[test]
    fn debug_timestamp_uses_beijing_time() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-04T06:30:00.123Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(debug_timestamp(now), "2026-05-04T14:30:00.123+08:00");
    }

    #[test]
    fn debug_log_lock_serializes_concurrent_writes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_dir = temp_dir.path().join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join(DEBUG_LOG_FILE);
        let thread_count: usize = 8;
        let writes_per_thread: usize = 50;
        let mut handles = Vec::new();

        for thread_idx in 0..thread_count {
            let log_dir = log_dir.clone();
            let log_path = log_path.clone();
            handles.push(std::thread::spawn(move || {
                for write_idx in 0..writes_per_thread {
                    let _lock = acquire_debug_log_lock(&log_dir)
                        .expect("debug log lock should be acquired");
                    let mut file = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                        .unwrap();
                    writeln!(
                        file,
                        "{}",
                        json!({
                            "thread": thread_idx,
                            "write": write_idx,
                            "payload": "x".repeat(2048),
                        })
                    )
                    .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let content = fs::read_to_string(&log_path).unwrap();
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), thread_count * writes_per_thread);
        for line in lines {
            serde_json::from_str::<Value>(line).expect("debug log line should remain valid JSON");
        }
    }

    #[test]
    fn sanitized_command_args_redacts_commit_message_and_url_credentials() {
        let args = vec![
            "commit".to_string(),
            "-m".to_string(),
            "secret message".to_string(),
            "--message=another secret".to_string(),
            "https://token@example.com/repo.git".to_string(),
        ];

        assert_eq!(
            sanitized_command_args(&args),
            vec![
                "commit",
                "-m",
                "<redacted>",
                "--message=<redacted>",
                "https://<redacted>@example.com/repo.git",
            ]
        );
    }
}
