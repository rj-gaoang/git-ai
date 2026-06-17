use crate::repos::test_repo::TestRepo;
use git_ai::authorship::attribution_tracker::LineAttribution;
use git_ai::authorship::authorship_log::{HumanRecord, PromptRecord};
use git_ai::authorship::working_log::AgentId;
use git_ai::daemon::{RecentWorkingLogSnapshot, restore_recent_working_log_snapshot};
use git_ai::git::find_repository_in_path;
use std::collections::{BTreeMap, HashMap};
use std::fs;

fn test_prompt_record(tool: &str) -> PromptRecord {
    PromptRecord {
        agent_id: AgentId {
            tool: tool.to_string(),
            id: "test-session".to_string(),
            model: "test-model".to_string(),
        },
        human_author: None,
        messages: Vec::new(),
        messages_url: None,
        total_additions: 0,
        total_deletions: 0,
        accepted_lines: 0,
        overriden_lines: 0,
        custom_attributes: None,
    }
}

#[test]
fn recent_working_log_snapshot_preserves_humans_on_restore() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.git_og(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial commit"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let h_hash = "h_abc123";
    let human_record = HumanRecord {
        author: "Test User <test@example.com>".to_string(),
    };

    let file_path = "test.txt";
    let line_attributions = vec![LineAttribution {
        start_line: 1,
        end_line: 1,
        author_id: h_hash.to_string(),
        overrode: None,
    }];

    let mut humans = BTreeMap::new();
    humans.insert(h_hash.to_string(), human_record.clone());

    let snapshot = RecentWorkingLogSnapshot {
        files: HashMap::from([(file_path.to_string(), line_attributions.clone())]),
        prompts: HashMap::new(),
        file_contents: HashMap::from([(file_path.to_string(), "test line\n".to_string())]),
        humans: humans.clone(),
        sessions: BTreeMap::new(),
    };

    let base_commit = "HEAD";
    let restored =
        restore_recent_working_log_snapshot(&gitai_repo, base_commit, &snapshot).unwrap();
    assert!(restored, "Snapshot should be restored");

    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(base_commit)
        .unwrap();
    let initial = working_log.read_initial_attributions();

    assert_eq!(
        initial.humans.len(),
        1,
        "Should have one human record after restore"
    );
    assert_eq!(
        initial.humans.get(h_hash),
        Some(&human_record),
        "Human record should match"
    );
}

#[test]
fn recent_working_log_snapshot_merges_missing_files_with_existing_initial() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.git_og(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial commit"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = "HEAD";
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(base_commit)
        .unwrap();

    let existing_attr = LineAttribution {
        start_line: 1,
        end_line: 1,
        author_id: "ai-existing".to_string(),
        overrode: None,
    };
    working_log
        .write_initial_attributions_with_contents(
            HashMap::from([("kept.txt".to_string(), vec![existing_attr.clone()])]),
            HashMap::from([(
                "ai-existing".to_string(),
                test_prompt_record("existing-tool"),
            )]),
            BTreeMap::new(),
            HashMap::from([("kept.txt".to_string(), "existing content\n".to_string())]),
            BTreeMap::new(),
        )
        .unwrap();

    let missing_attr = LineAttribution {
        start_line: 1,
        end_line: 2,
        author_id: "ai-missing".to_string(),
        overrode: None,
    };
    let snapshot = RecentWorkingLogSnapshot {
        files: HashMap::from([
            (
                "kept.txt".to_string(),
                vec![LineAttribution {
                    start_line: 1,
                    end_line: 1,
                    author_id: "ai-snapshot-should-not-win".to_string(),
                    overrode: None,
                }],
            ),
            ("missing.txt".to_string(), vec![missing_attr.clone()]),
        ]),
        prompts: HashMap::from([
            (
                "ai-existing".to_string(),
                test_prompt_record("snapshot-tool-should-not-win"),
            ),
            ("ai-missing".to_string(), test_prompt_record("missing-tool")),
        ]),
        file_contents: HashMap::from([
            (
                "kept.txt".to_string(),
                "snapshot should not overwrite\n".to_string(),
            ),
            (
                "missing.txt".to_string(),
                "missing one\nmissing two\n".to_string(),
            ),
        ]),
        humans: BTreeMap::new(),
        sessions: BTreeMap::new(),
    };

    let restored =
        restore_recent_working_log_snapshot(&gitai_repo, base_commit, &snapshot).unwrap();
    assert!(restored, "Snapshot should be restored");

    let initial = working_log.read_initial_attributions();
    assert_eq!(
        initial.files.get("kept.txt"),
        Some(&vec![existing_attr]),
        "existing INITIAL attribution should win"
    );
    assert_eq!(
        initial.files.get("missing.txt"),
        Some(&vec![missing_attr]),
        "missing snapshot file should be merged into INITIAL"
    );
    assert_eq!(
        initial
            .prompts
            .get("ai-existing")
            .map(|record| record.agent_id.tool.as_str()),
        Some("existing-tool"),
        "existing prompt metadata should not be overwritten"
    );
    assert_eq!(
        initial
            .prompts
            .get("ai-missing")
            .map(|record| record.agent_id.tool.as_str()),
        Some("missing-tool"),
        "missing prompt metadata should be added"
    );
    assert_eq!(
        working_log.stored_initial_file_content_from(&initial, "kept.txt"),
        Some("existing content\n".to_string()),
        "existing content snapshot should be preserved"
    );
    assert_eq!(
        working_log.stored_initial_file_content_from(&initial, "missing.txt"),
        Some("missing one\nmissing two\n".to_string()),
        "merged file content snapshot should be persisted"
    );
}
