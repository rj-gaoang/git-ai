use crate::authorship::attribution_tracker::{Attribution, LineAttribution};
use crate::authorship::working_log::{Checkpoint, CheckpointKind, WorkingLogEntry};
use crate::git::repo_storage::PersistedWorkingLog;
use crate::git::repository::Repository;
use std::collections::{HashMap, HashSet};

pub(crate) const ARCHIVED_AI_STATE_LOOKBACK_SECS: u64 = 24 * 60 * 60;

#[derive(Clone)]
pub(crate) struct ArchivedAiFileState {
    pub file: String,
    pub blob_sha: String,
    pub attributions: Vec<Attribution>,
    pub line_attributions: Vec<LineAttribution>,
    pub kind: CheckpointKind,
    pub timestamp: u64,
    pub source_working_log: PersistedWorkingLog,
    pub checkpoint: Checkpoint,
}

pub(crate) fn is_ai_author_id(author_id: &str) -> bool {
    author_id != CheckpointKind::Human.to_str()
        && author_id != CheckpointKind::KnownHuman.to_str()
        && !author_id.starts_with("h_")
}

pub(crate) fn working_log_entry_has_non_human_attribution(entry: &WorkingLogEntry) -> bool {
    entry
        .line_attributions
        .iter()
        .any(|attr| is_ai_author_id(&attr.author_id))
        || entry
            .attributions
            .iter()
            .any(|attr| is_ai_author_id(&attr.author_id))
}

pub(crate) fn previous_file_state_attributions(
    entry: &WorkingLogEntry,
    working_log: &PersistedWorkingLog,
    ts: u128,
) -> Vec<Attribution> {
    if !entry.attributions.is_empty() {
        return entry.attributions.clone();
    }

    if entry.line_attributions.is_empty() {
        return Vec::new();
    }

    let content = working_log
        .get_file_version(&entry.blob_sha)
        .unwrap_or_default();
    crate::authorship::attribution_tracker::line_attributions_to_attributions(
        &entry.line_attributions,
        &content,
        ts,
    )
}

pub(crate) fn checkpoint_is_non_bash_ai_edit(checkpoint: &Checkpoint) -> bool {
    checkpoint.kind.is_ai()
        && !checkpoint
            .agent_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("edit_kind"))
            .is_some_and(|edit_kind| edit_kind.eq_ignore_ascii_case("bash"))
}

pub(crate) fn collect_recent_archived_ai_states(
    repo: &Repository,
    files: &[String],
    ts: u128,
) -> HashMap<String, ArchivedAiFileState> {
    let target_files: HashSet<&str> = files.iter().map(String::as_str).collect();
    let now_secs = (ts / 1000) as u64;
    let mut latest_by_file: HashMap<String, ArchivedAiFileState> = HashMap::new();

    for archived_log in repo.storage.archived_working_logs() {
        let Ok(checkpoints) = archived_log.read_all_checkpoints() else {
            continue;
        };

        for checkpoint in checkpoints {
            if !checkpoint_is_non_bash_ai_edit(&checkpoint)
                || now_secs.saturating_sub(checkpoint.timestamp) > ARCHIVED_AI_STATE_LOOKBACK_SECS
            {
                continue;
            }

            for entry in &checkpoint.entries {
                if !target_files.contains(entry.file.as_str()) {
                    continue;
                }

                let state = ArchivedAiFileState {
                    file: entry.file.clone(),
                    blob_sha: entry.blob_sha.clone(),
                    attributions: previous_file_state_attributions(
                        entry,
                        &archived_log,
                        checkpoint.timestamp as u128,
                    ),
                    line_attributions: entry.line_attributions.clone(),
                    kind: checkpoint.kind,
                    timestamp: checkpoint.timestamp,
                    source_working_log: archived_log.clone(),
                    checkpoint: checkpoint.clone(),
                };

                let should_replace = latest_by_file
                    .get(&entry.file)
                    .map(|existing| existing.timestamp < state.timestamp)
                    .unwrap_or(true);
                if should_replace {
                    latest_by_file.insert(entry.file.clone(), state);
                }
            }
        }
    }

    latest_by_file
}
