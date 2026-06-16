use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
use std::collections::{HashMap, HashSet};

/// Attribute committed hunk lines that are not already covered by any
/// attestation entry. Existing human and AI attestations always win.
pub fn fill_unattributed_hunks(
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
    attestation_hash: &str,
) -> usize {
    if committed_hunks.is_empty() {
        return 0;
    }

    let mut attributed_lines: HashMap<String, HashSet<u32>> = HashMap::new();
    for file_attestation in &authorship_log.attestations {
        let lines = attributed_lines
            .entry(file_attestation.file_path.clone())
            .or_default();
        for entry in &file_attestation.entries {
            for range in &entry.line_ranges {
                for line in range.expand() {
                    lines.insert(line);
                }
            }
        }
    }

    let mut filled = 0usize;
    for (file_path, line_ranges) in committed_hunks {
        let existing = attributed_lines.get(file_path.as_str());
        let mut unattributed: Vec<u32> = Vec::new();
        for range in line_ranges {
            for line in range.expand() {
                if existing.is_none_or(|set| !set.contains(&line)) {
                    unattributed.push(line);
                }
            }
        }

        if unattributed.is_empty() {
            continue;
        }

        unattributed.sort_unstable();
        unattributed.dedup();
        filled += unattributed.len();

        let file_attestation = authorship_log.get_or_create_file(file_path);
        file_attestation.add_entry(AttestationEntry::new(
            attestation_hash.to_string(),
            LineRange::compress_lines(&unattributed),
        ));
    }

    filled
}
