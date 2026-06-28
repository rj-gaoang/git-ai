use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::attribution_tracker::LineAttribution;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::working_log::{Checkpoint, CheckpointKind, WorkingLogEntry};
use git_ai::git::find_repository_in_path;
use std::collections::HashMap;

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn raw_authorship_note(repo: &TestRepo, commit_sha: &str) -> Option<String> {
    repo.git_og(&["notes", "--ref=ai", "show", commit_sha])
        .ok()
        .filter(|note| !note.trim().is_empty())
}

#[test]
fn test_post_commit_empty_repo_with_checkpoint() {
    // Create an empty repo (no commits yet)
    let repo = TestRepo::new();

    // Write file without staging
    std::fs::write(repo.path().join("test.txt"), "Hello, world!\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Append to file
    std::fs::write(repo.path().join("test.txt"), "Hello, world!\nSecond line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Stage and commit - this triggers the post-commit hook
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // The key assertion: post_commit didn't panic. We can verify by checking authorship note exists
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // If post_commit ran successfully via the git hook, an authorship note should exist
    let note_result = repo.read_authorship_note(&head_sha);

    // It should succeed (the note was created during commit)
    assert!(
        note_result.is_some(),
        "post_commit should handle empty repo without errors"
    );
}

#[test]
fn test_legacy_human_batch_with_known_human_adjustment_fills_ai_gaps() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(repo.path().join("ai_a.java"), "class A {}\n").unwrap();
    std::fs::write(repo.path().join("ai_b.java"), "class B {}\n").unwrap();
    std::fs::write(repo.path().join("mixed.java"), "class M {}\nmanual tweak\n").unwrap();

    let working_log = repo.current_working_logs();
    let ai_a_blob = working_log.persist_file_version("class A {}\n").unwrap();
    let ai_b_blob = working_log.persist_file_version("class B {}\n").unwrap();
    let mixed_blob = working_log
        .persist_file_version("class M {}\nmanual tweak\n")
        .unwrap();
    let legacy_entries = vec![
        WorkingLogEntry::new("ai_a.java".to_string(), ai_a_blob, vec![], vec![]),
        WorkingLogEntry::new("ai_b.java".to_string(), ai_b_blob, vec![], vec![]),
        WorkingLogEntry::new("mixed.java".to_string(), mixed_blob.clone(), vec![], vec![]),
    ];
    let legacy_checkpoint = Checkpoint::new(
        CheckpointKind::Human,
        String::new(),
        "v-xujianfeng1".to_string(),
        legacy_entries,
    );
    let known_human_checkpoint = Checkpoint::new(
        CheckpointKind::KnownHuman,
        String::new(),
        "v-xujianfeng1".to_string(),
        vec![WorkingLogEntry::new(
            "mixed.java".to_string(),
            mixed_blob,
            vec![],
            vec![LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id:
                    git_ai::authorship::authorship_log_serialization::generate_human_short_hash(
                        "v-xujianfeng1",
                    ),
                overrode: None,
            }],
        )],
    );
    working_log
        .write_all_checkpoints(&[legacy_checkpoint, known_human_checkpoint])
        .unwrap();

    repo.git(&["add", "ai_a.java", "ai_b.java", "mixed.java"])
        .unwrap();
    repo.stage_all_and_commit("Legacy Copilot batch plus manual tweak")
        .unwrap();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 4);
    assert_eq!(stats.ai_additions, 3);
    assert_eq!(stats.human_additions, 1);
    assert_eq!(stats.unknown_additions, 0);

    let mut mixed = repo.filename("mixed.java");
    mixed.assert_lines_and_blame(crate::lines!["class M {}".ai(), "manual tweak".human()]);
}

#[test]
fn test_legacy_human_checkpoint_fills_manual_gaps() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(
        repo.path().join("ReceiptAddressInfoController.java"),
        "class ReceiptAddressInfoController {}\nmanual branch\n",
    )
    .unwrap();

    let working_log = repo.current_working_logs();
    let blob = working_log
        .persist_file_version("class ReceiptAddressInfoController {}\nmanual branch\n")
        .unwrap();
    let legacy_checkpoint = Checkpoint::new(
        CheckpointKind::Human,
        String::new(),
        "v-zhangbiao6 <v-zhangbiao6@ruijie.com.cn>".to_string(),
        vec![WorkingLogEntry::new(
            "ReceiptAddressInfoController.java".to_string(),
            blob,
            vec![],
            vec![],
        )],
    );
    working_log
        .write_all_checkpoints(&[legacy_checkpoint])
        .unwrap();

    repo.git(&["add", "ReceiptAddressInfoController.java"])
        .unwrap();
    repo.stage_all_and_commit("Manual legacy human edit")
        .unwrap();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 2);
    assert_eq!(stats.ai_additions, 0);
    assert_eq!(stats.human_additions, 2);
    assert_eq!(stats.unknown_additions, 0);

    let mut file = repo.filename("ReceiptAddressInfoController.java");
    file.assert_lines_and_blame(crate::lines![
        "class ReceiptAddressInfoController {}".human(),
        "manual branch".human()
    ]);
}

#[test]
fn replay_human_checkpoint_does_not_fill_manual_gaps() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(repo.path().join("doc_a.md"), "generated a1\ngenerated a2\n").unwrap();
    std::fs::write(repo.path().join("doc_b.md"), "generated b1\n").unwrap();

    repo.checkpoint_legacy_human_via_daemon_with_metadata(
        "huangjinzhao <huangjinzhao@ruijie.com.cn>",
        HashMap::from([("git_ai_replay_checkpoint".to_string(), "true".to_string())]),
    );

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(
        checkpoints[0]
            .agent_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("git_ai_replay_checkpoint"))
            .map(String::as_str),
        Some("true")
    );

    repo.git(&["add", "doc_a.md", "doc_b.md"]).unwrap();
    repo.stage_all_and_commit("Replay human snapshot").unwrap();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 3);
    assert_eq!(stats.ai_additions, 0);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 3);
}

#[test]
#[serial_test::serial]
fn observed_commit_backfill_materializes_ai_checkpoint_after_raw_commit() {
    let _upload_guard = EnvVarGuard::set("GIT_AI_AUTO_UPLOAD_AI_STATS", "false");
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(
        repo.path().join("generated.ts"),
        "export const a = 1;\nexport const b = 2;\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "generated.ts"])
        .unwrap();

    repo.git_og(&["add", "generated.ts"]).unwrap();
    repo.git_og(&["commit", "-m", "Raw AI commit"]).unwrap();
    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert!(
        raw_authorship_note(&repo, &commit_sha).is_none(),
        "raw commit should bypass post-commit authorship note creation"
    );

    let git_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let materialized =
        git_ai::git::sync_authorship::backfill_missing_authorship_for_observed_commit(
            &git_repo,
            &commit_sha,
            "test_observed_commit",
        )
        .unwrap();

    assert!(materialized, "observed commit should materialize the note");
    assert!(
        raw_authorship_note(&repo, &commit_sha).is_some(),
        "backfill should write an authorship note"
    );

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 2);
    assert_eq!(stats.ai_additions, 2);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);
}

#[test]
#[serial_test::serial]
fn checkpoint_base_backfills_missing_note_after_raw_commit() {
    let _upload_guard = EnvVarGuard::set("GIT_AI_AUTO_UPLOAD_AI_STATS", "false");
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(repo.path().join("generated.rs"), "fn generated() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "generated.rs"])
        .unwrap();

    repo.git_og(&["add", "generated.rs"]).unwrap();
    repo.git_og(&["commit", "-m", "Raw AI commit"]).unwrap();
    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert!(
        raw_authorship_note(&repo, &commit_sha).is_none(),
        "raw commit should bypass post-commit authorship note creation"
    );

    std::fs::write(repo.path().join("next.rs"), "fn next() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "next.rs"]).unwrap();

    assert!(
        raw_authorship_note(&repo, &commit_sha).is_some(),
        "a later checkpoint whose baseCommit is the raw commit should backfill the missing note"
    );

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);
}

#[test]
#[serial_test::serial]
fn observed_head_backfill_skips_raw_commit_without_local_evidence() {
    let _upload_guard = EnvVarGuard::set("GIT_AI_AUTO_UPLOAD_AI_STATS", "false");
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(repo.path().join("manual.txt"), "no checkpoint\n").unwrap();
    repo.git_og(&["add", "manual.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "Raw commit without evidence"])
        .unwrap();
    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert!(
        raw_authorship_note(&repo, &commit_sha).is_none(),
        "raw commit should start without a note"
    );

    let git_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let materialized =
        git_ai::git::sync_authorship::backfill_missing_authorship_from_observed_head(
            &git_repo,
            "test_observed_head",
        )
        .unwrap();

    assert_eq!(materialized, 0);
    assert!(
        raw_authorship_note(&repo, &commit_sha).is_none(),
        "backfill must not invent attribution when no checkpoint or INITIAL evidence exists"
    );
}

#[test]
#[serial_test::serial]
fn observed_git_command_backfills_missing_note_after_raw_commit() {
    let _upload_guard = EnvVarGuard::set("GIT_AI_AUTO_UPLOAD_AI_STATS", "false");
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    std::fs::write(repo.path().join("generated.rs"), "fn generated() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "generated.rs"])
        .unwrap();

    repo.git_og(&["add", "generated.rs"]).unwrap();
    repo.git_og(&["commit", "-m", "Raw AI commit"]).unwrap();
    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    assert!(
        raw_authorship_note(&repo, &commit_sha).is_none(),
        "raw commit should bypass post-commit authorship note creation"
    );

    repo.git(&["tag", "-l"]).unwrap();
    repo.sync_daemon();

    assert!(
        raw_authorship_note(&repo, &commit_sha).is_some(),
        "daemon-observed git command should backfill the missing note"
    );

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);
}

#[test]
fn test_post_commit_empty_repo_no_checkpoint() {
    // Create an empty repo (no commits yet)
    let repo = TestRepo::new();

    // Create a file without checkpointing
    std::fs::write(repo.path().join("test.txt"), "Hello, world!\n").unwrap();

    // Stage and commit without prior checkpoint - this triggers the post-commit hook
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Should not panic or error even with no working log
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // With no checkpoints, authorship log should have empty attestations
    let note = repo.read_authorship_note(&head_sha);
    assert!(note.is_some(), "Should have authorship note");

    // No checkpoints = no AI attribution, so note should have empty attestations
    let log = AuthorshipLog::deserialize_from_string(&note.unwrap()).unwrap();
    assert!(
        log.attestations.is_empty(),
        "Should have empty attestations when no checkpoints exist"
    );
}

#[test]
fn test_post_commit_utf8_filename_with_ai_attribution() {
    // Create a repo with an initial commit
    let repo = TestRepo::new();

    // Create initial file and commit
    std::fs::write(repo.path().join("README.md"), "# Test\n").unwrap();
    repo.git(&["add", "README.md"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create a file with Chinese characters in the filename
    let chinese_filename = "中文文件.txt";
    std::fs::write(repo.path().join(chinese_filename), "Hello, 世界!\n").unwrap();
    repo.git(&["add", chinese_filename]).unwrap();

    // Trigger AI checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", chinese_filename])
        .unwrap();

    // Commit - this triggers the post-commit hook
    repo.stage_all_and_commit("Add Chinese file").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let note = repo
        .read_authorship_note(&head_sha)
        .expect("should have authorship note");

    // The note should reference the Chinese filename
    // Deserialize and check attestations contain the file
    let log = AuthorshipLog::deserialize_from_string(&note).unwrap();

    // Debug output
    println!("Authorship log attestations: {:?}", log.attestations);

    // The attestation should include the Chinese filename
    assert_eq!(
        log.attestations.len(),
        1,
        "Should have 1 attestation for the Chinese-named file"
    );
    assert_eq!(
        log.attestations[0].file_path, chinese_filename,
        "File path should be the UTF-8 filename"
    );
}
