use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DEBUG_LOG_FILE: &str = "debug.jsonl";

pub(crate) fn debug_enabled() -> bool {
    std::env::var("GIT_AI_DEBUG").is_ok()
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

    let mut record = Map::new();
    record.insert("schemaVersion".to_string(), json!(1));
    record.insert("event".to_string(), Value::String(event.to_string()));
    record.insert("timestampMs".to_string(), json!(timestamp_ms()));
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

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
