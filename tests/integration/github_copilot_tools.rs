use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use serde_json::json;

// Helper to create a realistic Copilot transcript path matching actual VS Code format
fn fake_copilot_transcript_path(repo: &TestRepo) -> String {
    repo.path()
        .join("Library/Application Support/Code/User/workspaceStorage/3a1e37d25f1dc63984c2bcc9a52a6bdd/GitHub.copilot-chat/transcripts/session-test-uuid.jsonl")
        .to_str()
        .unwrap()
        .to_string()
}

/// Test replace_string_in_file with realistic hook data
/// This is a normal file edit tool, not a bash tool
#[test]
fn test_replace_string_in_file_basic() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O (not helpers that trigger checkpoints)
    let file_path = repo.path().join("foo.py");
    std::fs::write(&file_path, "# Human comment\n").unwrap();

    // Commit with direct git commands
    repo.git(&["add", "foo.py"]).unwrap();
    repo.git(&["commit", "-m", "Initial commit"]).unwrap();

    let session_id = "0ae773c0-f1c2-4904-bd18-fb1046ff61cd";

    // PreToolUse hook
    let pre_hook_input = json!({
        "timestamp": "2026-04-07T18:10:41.626Z",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "replace_string_in_file",
        "tool_input": {
            "filePath": file_path.to_str().unwrap(),
            "oldString": "# Human comment",
            "newString": "# Human comment\nimport argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Hello World CLI\")\n    parser.parse_args()\n    print(\"Hello, World!\")\n\nif __name__ == \"__main__\":\n    main()"
        },
        "tool_use_id": "toolu_bdrk_013o2nzaLHN3dzQimNj9PaNg__vscode-1775585312869",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // AI makes the edit with raw I/O
    std::fs::write(&file_path, "# Human comment\nimport argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Hello World CLI\")\n    parser.parse_args()\n    print(\"Hello, World!\")\n\nif __name__ == \"__main__\":\n    main()\n").unwrap();

    // PostToolUse hook
    let post_hook_input = json!({
        "timestamp": "2026-04-07T18:10:41.816Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "replace_string_in_file",
        "tool_input": {
            "filePath": file_path.to_str().unwrap(),
            "oldString": "# Human comment",
            "newString": "# Human comment\nimport argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Hello World CLI\")\n    parser.parse_args()\n    print(\"Hello, World!\")\n\nif __name__ == \"__main__\":\n    main()"
        },
        "tool_response": "",
        "tool_use_id": "toolu_bdrk_013o2nzaLHN3dzQimNj9PaNg__vscode-1775585312869",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // Sync daemon before assertions
    repo.sync_daemon();

    // Commit with direct git commands
    repo.git(&["add", "foo.py"]).unwrap();
    repo.git(&["commit", "-m", "Add CLI functionality"])
        .unwrap();

    // Sync daemon again after commit to ensure notes are written
    repo.sync_daemon();

    // AI-added lines should be attributed to AI
    let mut file = repo.filename("foo.py");
    file.assert_lines_and_blame(crate::lines![
        "# Human comment".human(),
        "import argparse".ai(),
        "".ai(),
        "def main():".ai(),
        "    parser = argparse.ArgumentParser(description=\"Hello World CLI\")".ai(),
        "    parser.parse_args()".ai(),
        "    print(\"Hello, World!\")".ai(),
        "".ai(),
        "if __name__ == \"__main__\":".ai(),
        "    main()".ai()
    ]);
}

/// Test Copilot CLI `edit` tool (str_replace-style: path + old_str + new_str)
/// This is the primary file-editing tool in Copilot CLI and was previously
/// missing from the CLI tool routing table, causing it to be silently dropped.
#[test]
fn test_copilot_cli_edit_tool_attribution() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O
    let file_path = repo.path().join("jokes.csv");
    std::fs::write(
        &file_path,
        "id,setup,punchline\n1,Why do programmers prefer dark mode?,Because light attracts bugs.\n2,Why did the developer go broke?,Because he used up all his cache.\n",
    )
    .unwrap();
    repo.git(&["add", "jokes.csv"]).unwrap();
    repo.git(&["commit", "-m", "Initial jokes"]).unwrap();

    let session_id = "ec663931-ecc5-45ce-bb5a-b4058a74b344";

    // PreToolUse hook for `edit` tool (exact format from Copilot CLI logs)
    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "timestamp": "2026-05-11T23:47:05.010Z",
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "edit",
        "tool_input": {
            "path": file_path.to_str().unwrap(),
            "old_str": "2,Why did the developer go broke?,Because he used up all his cache.\n",
            "new_str": "2,Why did the developer go broke?,Because he used up all his cache.\n3,Why did the computer go to art school?,Because it wanted to learn how to draw its graphics!\n"
        }
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // AI makes the edit (Copilot CLI writes to disk before PostToolUse)
    std::fs::write(
        &file_path,
        "id,setup,punchline\n1,Why do programmers prefer dark mode?,Because light attracts bugs.\n2,Why did the developer go broke?,Because he used up all his cache.\n3,Why did the computer go to art school?,Because it wanted to learn how to draw its graphics!\n",
    )
    .unwrap();

    // PostToolUse hook for `edit` tool
    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "timestamp": "2026-05-11T23:47:10.655Z",
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "edit",
        "tool_input": {
            "path": file_path.to_str().unwrap(),
            "old_str": "2,Why did the developer go broke?,Because he used up all his cache.\n",
            "new_str": "2,Why did the developer go broke?,Because he used up all his cache.\n3,Why did the computer go to art school?,Because it wanted to learn how to draw its graphics!\n"
        },
        "tool_result": {
            "result_type": "success",
            "text_result_for_llm": format!("File {} updated with changes.", file_path.display())
        }
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // Sync daemon before assertions
    repo.sync_daemon();

    repo.git(&["add", "jokes.csv"]).unwrap();
    repo.git(&["commit", "-m", "Add joke via copilot CLI edit"])
        .unwrap();

    repo.sync_daemon();

    // AI-added line should be attributed to AI
    let mut file = repo.filename("jokes.csv");
    file.assert_lines_and_blame(crate::lines![
        "id,setup,punchline".human(),
        "1,Why do programmers prefer dark mode?,Because light attracts bugs.".human(),
        "2,Why did the developer go broke?,Because he used up all his cache.".human(),
        "3,Why did the computer go to art school?,Because it wanted to learn how to draw its graphics!".ai(),
    ]);
}

/// Test Copilot CLI `create` tool (no transcript_path) for new file attribution
#[test]
fn test_copilot_cli_create_tool_attribution() {
    let repo = TestRepo::new();

    // Create an initial commit so HEAD exists
    let existing = repo.path().join("readme.md");
    std::fs::write(&existing, "# Hello\n").unwrap();
    repo.git(&["add", "readme.md"]).unwrap();
    repo.git(&["commit", "-m", "Initial"]).unwrap();

    let session_id = "5d46633c-00b7-47dd-9e2c-9e2c5cac44ce";
    let new_file = repo.path().join("new_file.py");

    // PreToolUse for create (CLI format: no transcript_path)
    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "create",
        "tool_input": {
            "path": new_file.to_str().unwrap(),
            "file_text": "print('hello world')\n"
        }
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // Copilot CLI writes the file
    std::fs::write(&new_file, "print('hello world')\n").unwrap();

    // PostToolUse for create
    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "create",
        "tool_input": {
            "path": new_file.to_str().unwrap(),
            "file_text": "print('hello world')\n"
        },
        "tool_result": {
            "result_type": "success",
            "text_result_for_llm": "Created file"
        }
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    repo.sync_daemon();

    repo.git(&["add", "new_file.py"]).unwrap();
    repo.git(&["commit", "-m", "Add new file via copilot CLI"])
        .unwrap();

    repo.sync_daemon();

    let mut file = repo.filename("new_file.py");
    file.assert_lines_and_blame(crate::lines!["print('hello world')".ai()]);
}

/// Test Copilot CLI `view` tool is properly skipped (read-only, no checkpoint needed)
#[test]
fn test_copilot_cli_view_tool_skipped() {
    let repo = TestRepo::new();

    let file_path = repo.path().join("test.txt");
    std::fs::write(&file_path, "hello\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git(&["commit", "-m", "Initial"]).unwrap();

    let session_id = "ec663931-ecc5-45ce-bb5a-b4058a74b344";

    // view tool should be skipped (it's read-only)
    let hook_input = json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "timestamp": "2026-05-11T23:47:02.453Z",
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "view",
        "tool_input": {
            "path": file_path.to_str().unwrap()
        }
    });

    // Should exit 0 but print a skip/error message (non-edit tool)
    let output = repo
        .git_ai(&[
            "checkpoint",
            "github-copilot",
            "--hook-input",
            &hook_input.to_string(),
        ])
        .unwrap();

    assert!(
        output.contains("Skipping") || output.contains("preset error"),
        "Expected skip message for view tool, got: {}",
        output
    );
}

/// Test run_in_terminal with realistic hook data
/// This tool should use bash checkpoint flow (snapshot diff)
#[test]
fn test_run_in_terminal_bash_checkpoint() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O — do NOT use set_contents/filename helpers
    // as they fire real checkpoints that corrupt the bash snapshot state.
    std::fs::write(
        repo.path().join("example.py"),
        "import argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Test CLI\")\n    parser.add_argument(\"--name\", default=\"World\")\n    args = parser.parse_args()\n    print(f\"Hello, {args.name}!\")\n\nif __name__ == \"__main__\":\n    main()\n",
    )
    .unwrap();
    repo.git(&["add", "example.py"]).unwrap();
    repo.git(&["commit", "-m", "Initial script"]).unwrap();

    // Wait for the daemon's watermark grace window (2s) to expire so the
    // pre-snapshot is not filtered to empty.
    std::thread::sleep(std::time::Duration::from_secs(3));

    let session_id = "b4a517c6-b9f0-4787-af3a-7c002539b448";

    // PreToolUse hook for run_in_terminal
    let pre_hook_input = json!({
        "timestamp": "2026-04-09T04:50:44.227Z",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 example.py",
            "explanation": "Run the CLI script to validate behavior.",
            "goal": "Validate behavior",
            "isBackground": false
        },
        "tool_use_id": "call_k6q1U6W9xW4fWjmJwsSI1IJP__vscode-1775710200829",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // Simulate the bash command writing a file directly to disk — raw I/O only,
    // no set_contents/filename helpers between Pre and PostToolUse.
    std::fs::write(repo.path().join("output.txt"), "Hello, World!").unwrap();

    // PostToolUse hook
    let post_hook_input = json!({
        "timestamp": "2026-04-09T04:50:44.542Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 example.py",
            "explanation": "Run the CLI script to validate behavior.",
            "goal": "Validate behavior",
            "isBackground": false
        },
        "tool_response": "Hello, World!\n",
        "tool_use_id": "call_k6q1U6W9xW4fWjmJwsSI1IJP__vscode-1775710200829",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // Sync daemon before assertions
    repo.sync_daemon();

    repo.git(&["add", "output.txt"]).unwrap();
    repo.git(&["commit", "-m", "Add output file from command"])
        .unwrap();

    repo.sync_daemon();

    // File created by bash command should be attributed to AI
    let mut output = repo.filename("output.txt");
    output.assert_lines_and_blame(crate::lines!["Hello, World!".ai()]);
}

/// Missing bash pre-snapshot should fall back to git status instead of losing
/// the AI attribution silently.
#[test]
fn test_run_in_terminal_missing_pre_snapshot_uses_git_status_fallback() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.git(&["commit", "-m", "Initial commit"]).unwrap();

    let session_id = "fallback-bash-session";
    let tool_use_id = "call_missingPreSnapshot__vscode-1775710200999";

    std::fs::write(
        repo.path().join("terminal_output.txt"),
        "Generated by terminal\n",
    )
    .unwrap();

    let post_hook_input = json!({
        "timestamp": "2026-04-09T04:50:44.542Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python generate.py",
            "explanation": "Generate a file.",
            "goal": "Generate file",
            "isBackground": false
        },
        "tool_response": "",
        "tool_use_id": tool_use_id,
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    repo.sync_daemon();

    repo.git(&["add", "terminal_output.txt"]).unwrap();
    repo.git(&["commit", "-m", "Add terminal output"]).unwrap();
    repo.sync_daemon();

    let mut output = repo.filename("terminal_output.txt");
    output.assert_lines_and_blame(crate::lines!["Generated by terminal".ai()]);
}

/// Regression: VS Code Copilot may include a broader current editor snapshot
/// in top-level `dirtyFiles` than the bash stat-diff can observe on disk at
/// PostToolUse time. Those dirty snapshot paths must be merged into the AI
/// checkpoint instead of attributing only the narrow stat-diff result.
#[test]
fn test_run_in_terminal_merges_dirty_files_when_stat_diff_is_narrow() {
    let repo = TestRepo::new();

    let disk_detected = repo.path().join("disk_detected.ts");
    let dirty_only = repo.path().join("dirty_only.ts");
    std::fs::write(&disk_detected, "export const disk = 'old';\n").unwrap();
    std::fs::write(&dirty_only, "export const dirty = 'old';\n").unwrap();
    repo.git(&["add", "disk_detected.ts", "dirty_only.ts"])
        .unwrap();
    repo.git(&["commit", "-m", "Initial files"]).unwrap();

    // Ensure the post-bash write is outside the mtime grace window used by
    // the snapshot watermark filter.
    std::thread::sleep(std::time::Duration::from_secs(3));

    let session_id = "dirty-files-merge-bash-session";
    let tool_use_id = "call_dirtyFilesMerge__vscode-1781697553325";

    let pre_hook_input = json!({
        "timestamp": "2026-06-17T20:33:27.000+08:00",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "node scripts/generate-contract-ui.js",
            "explanation": "Generate UI files.",
            "goal": "Generate files",
            "isBackground": false
        },
        "tool_use_id": tool_use_id,
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    let disk_detected_content = "export const disk = 'old';\nexport const generatedDisk = true;\n";
    let dirty_only_content = "export const dirty = 'old';\nexport const generatedDirty = true;\n";

    // Only this file reaches disk before PostToolUse, so the bash stat-diff
    // sees a single changed path.
    std::fs::write(&disk_detected, disk_detected_content).unwrap();

    let post_hook_input = json!({
        "timestamp": "2026-06-17T20:34:10.000+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "node scripts/generate-contract-ui.js",
            "explanation": "Generate UI files.",
            "goal": "Generate files",
            "isBackground": false
        },
        "tool_response": "",
        "tool_use_id": tool_use_id,
        "dirtyFiles": {
            disk_detected.to_str().unwrap(): disk_detected_content,
            dirty_only.to_str().unwrap(): dirty_only_content
        },
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // VS Code flushes the second dirty buffer after the hook has already
    // fired. The AI checkpoint should still have captured it from dirtyFiles.
    std::fs::write(&dirty_only, dirty_only_content).unwrap();

    repo.sync_daemon();
    repo.git(&["add", "disk_detected.ts", "dirty_only.ts"])
        .unwrap();
    repo.git(&["commit", "-m", "Add generated files from Copilot terminal"])
        .unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 2);
    assert_eq!(stats.ai_additions, 2);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);

    let mut disk = repo.filename("disk_detected.ts");
    disk.assert_lines_and_blame(crate::lines![
        "export const disk = 'old';".human(),
        "export const generatedDisk = true;".ai(),
    ]);

    let mut dirty = repo.filename("dirty_only.ts");
    dirty.assert_lines_and_blame(crate::lines![
        "export const dirty = 'old';".human(),
        "export const generatedDirty = true;".ai(),
    ]);
}

/// Guardrail: Copilot sessions can generate more files than a native hook
/// reports, but post-commit gap filling must not turn files with no path
/// evidence into AI. Missing checkpoint paths are fixed at hook capture time
/// (for example via dirtyFiles), not by guessing from session totals.
#[test]
fn test_copilot_ai_session_does_not_fill_uncheckpointed_committed_file_gaps() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.git(&["commit", "-m", "Initial"]).unwrap();

    let tracked_file = repo.path().join("tracked_by_hook.ts");
    let missed_file = repo.path().join("missed_by_hook.ts");
    let session_id = "gap-fill-copilot-session";

    let post_hook_input = json!({
        "timestamp": "2026-06-16T14:43:34.000+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "create_file",
        "tool_input": {
            "filePath": tracked_file.to_str().unwrap(),
            "content": "export const tracked = true;\nexport const trackedTwo = true;\nexport const notCommitted = true;\n"
        },
        "tool_response": "",
        "tool_use_id": "toolu_gap_fill_tracked__vscode-1781592214000",
        "cwd": repo.path().to_str().unwrap()
    });

    std::fs::write(
        &tracked_file,
        "export const tracked = true;\nexport const trackedTwo = true;\nexport const notCommitted = true;\n",
    )
    .unwrap();
    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // The session generated three lines in total, but only two of the tracked
    // file's lines landed in this commit. This mirrors real logs where
    // totalAiAdditions can cover AI-generated lines missed by hook pathspecs.
    std::fs::write(
        &tracked_file,
        "export const tracked = true;\nexport const trackedTwo = true;\n",
    )
    .unwrap();

    // This file was produced by the same AI session, but no checkpoint path
    // was emitted for it. Without path evidence, post-commit must leave it
    // unknown instead of guessing that every unattributed line is AI.
    std::fs::write(&missed_file, "export const missed = true;\n").unwrap();

    repo.sync_daemon();
    repo.git(&["add", "tracked_by_hook.ts", "missed_by_hook.ts"])
        .unwrap();
    repo.git(&["commit", "-m", "Add generated Copilot files"])
        .unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 3);
    assert_eq!(stats.ai_additions, 2);
    assert_eq!(stats.unknown_additions, 1);

    let mut missed = repo.filename("missed_by_hook.ts");
    missed.assert_lines_and_blame(crate::lines![
        "export const missed = true;".unattributed_human()
    ]);
}

/// Regression: an AI-heavy commit can also contain a tiny manual version bump
/// in files outside the AI checkpoint pathspec. The AI gap-fill budget may be
/// large enough to cover those lines, but they must not be attributed to AI
/// unless the file itself had current AI path evidence.
#[test]
fn test_copilot_gap_fill_does_not_claim_manual_files_outside_ai_pathspec() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("Cargo.toml"), "version = \"2.2.40\"\n").unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "version = \"2.2.40\"\n").unwrap();
    std::fs::write(repo.path().join("src.rs"), "pub fn old() {}\n").unwrap();
    repo.git(&["add", "Cargo.toml", "Cargo.lock", "src.rs"])
        .unwrap();
    repo.git(&["commit", "-m", "Initial"]).unwrap();

    let ai_file = repo.path().join("src.rs");
    let checkpointed_content =
        "pub fn old() {}\npub fn generated() {}\npub fn generated_not_committed() {}\n";
    let final_ai_content = "pub fn old() {}\npub fn generated() {}\npub fn generated_tail() {}\n";

    std::fs::write(&ai_file, checkpointed_content).unwrap();
    let post_hook_input = json!({
        "timestamp": "2026-06-17T23:46:04.000+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": "manual-version-bump-guard",
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "edit_file",
        "tool_input": {
            "filePath": ai_file.to_str().unwrap(),
            "content": checkpointed_content
        },
        "tool_response": "",
        "tool_use_id": "toolu_manual_version_bump_guard__vscode-1781711164000",
        "cwd": repo.path().to_str().unwrap()
    });
    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // AI tail line has no direct line-range attestation but the same file is
    // in the AI pathspec, so gap-fill may safely cover it.
    std::fs::write(&ai_file, final_ai_content).unwrap();

    // These version bumps are manual and outside the AI pathspec. They used
    // to be swallowed by the broad post-commit AI gap fill.
    std::fs::write(repo.path().join("Cargo.toml"), "version = \"2.2.41\"\n").unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "version = \"2.2.41\"\n").unwrap();

    repo.sync_daemon();
    repo.git(&["add", "Cargo.toml", "Cargo.lock", "src.rs"])
        .unwrap();
    repo.git(&["commit", "-m", "AI edit plus manual version bump"])
        .unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    // stats ignores lockfiles by default, so this counts the two source lines
    // plus Cargo.toml; Cargo.lock is checked below via blame/note behavior.
    assert_eq!(stats.git_diff_added_lines, 3);
    assert_eq!(stats.ai_additions, 2);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 1);

    let mut ai_file = repo.filename("src.rs");
    ai_file.assert_lines_and_blame(crate::lines![
        "pub fn old() {}".human(),
        "pub fn generated() {}".ai(),
        "pub fn generated_tail() {}".ai()
    ]);

    let mut manifest = repo.filename("Cargo.toml");
    manifest.assert_lines_and_blame(crate::lines!["version = \"2.2.41\"".unattributed_human()]);

    let mut lockfile = repo.filename("Cargo.lock");
    lockfile.assert_lines_and_blame(crate::lines!["version = \"2.2.41\"".unattributed_human()]);
}

/// Regression: a Copilot session can land some AI-attributed lines in a file,
/// then append more generated lines to that same file through a terminal/batch
/// step whose post hook reports no changed paths. The committed file is in the
/// AI pathspec, but the tail hunk has no line-range attestation and used to
/// remain unknown (e.g. 298 added lines = 159 AI + 139 unknown).
#[test]
fn test_copilot_ai_session_fills_unattributed_tail_in_checkpointed_file() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.git(&["commit", "-m", "Initial"]).unwrap();

    let service_file = repo.path().join("src/main/java/com/acme/FlowService.java");
    std::fs::create_dir_all(service_file.parent().unwrap()).unwrap();
    let session_id = "same-file-tail-gap-session";

    let mut checkpointed_content = String::new();
    for idx in 1..=159 {
        checkpointed_content.push_str(&format!("// generated checkpointed line {}\n", idx));
    }

    let mut final_content = checkpointed_content.clone();
    for idx in 160..=298 {
        final_content.push_str(&format!("// generated tail line {}\n", idx));
    }

    std::fs::write(&service_file, &checkpointed_content).unwrap();
    let post_hook_input = json!({
        "timestamp": "2026-06-17T21:50:22.000+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "create_file",
        "tool_input": {
            "filePath": service_file.to_str().unwrap(),
            "content": checkpointed_content
        },
        "tool_response": "",
        "tool_use_id": "toolu_same_file_tail__vscode-1781704222000",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    let budget_file = repo.path().join("generated-budget.txt");
    let mut uncommitted_budget_content = String::new();
    for idx in 1..=139 {
        uncommitted_budget_content.push_str(&format!("// generated budget line {}\n", idx));
    }
    std::fs::write(&budget_file, &uncommitted_budget_content).unwrap();
    let budget_hook_input = json!({
        "timestamp": "2026-06-17T21:51:59.000+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "create_file",
        "tool_input": {
            "filePath": budget_file.to_str().unwrap(),
            "content": uncommitted_budget_content
        },
        "tool_response": "",
        "tool_use_id": "toolu_same_file_tail_budget__vscode-1781704319000",
        "cwd": repo.path().to_str().unwrap()
    });
    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &budget_hook_input.to_string(),
    ])
    .unwrap();

    // Later generated content reaches disk, but no successful post snapshot
    // covers these tail lines.
    std::fs::write(&service_file, final_content).unwrap();

    repo.sync_daemon();
    repo.git(&["add", "src/main/java/com/acme/FlowService.java"])
        .unwrap();
    repo.git(&["commit", "-m", "Add generated flow service"])
        .unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 298);
    assert_eq!(stats.ai_additions, 298);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);

    let mut file = repo.filename("src/main/java/com/acme/FlowService.java");
    let expected_lines = (1..=298)
        .map(|idx| {
            if idx <= 159 {
                format!("// generated checkpointed line {}", idx).ai()
            } else {
                format!("// generated tail line {}", idx).ai()
            }
        })
        .collect();
    file.assert_lines_and_blame(expected_lines);
}

/// Guardrail: a non-landing AI session should not make unrelated committed
/// human lines look AI-authored just because prompt/session metadata exists.
#[test]
fn test_copilot_gap_fill_requires_landed_ai_attestation() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    repo.git(&["add", "seed.txt"]).unwrap();
    repo.git(&["commit", "-m", "Initial"]).unwrap();

    let uncommitted_ai_file = repo.path().join("uncommitted_ai.ts");
    let human_file = repo.path().join("human_commit.ts");
    let session_id = "gap-fill-non-landing-session";

    let post_hook_input = json!({
        "timestamp": "2026-06-16T14:43:34.000+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "create_file",
        "tool_input": {
            "filePath": uncommitted_ai_file.to_str().unwrap(),
            "content": "export const aiOnly = true;\n"
        },
        "tool_response": "",
        "tool_use_id": "toolu_gap_fill_non_landing__vscode-1781592214001",
        "cwd": repo.path().to_str().unwrap()
    });

    std::fs::write(&uncommitted_ai_file, "export const aiOnly = true;\n").unwrap();
    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    std::fs::write(&human_file, "export const humanOnly = true;\n").unwrap();

    repo.sync_daemon();
    repo.git(&["add", "human_commit.ts"]).unwrap();
    repo.git(&["commit", "-m", "Add human-only file"]).unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 0);
    assert_eq!(stats.unknown_additions, 1);
}

/// Regression: some legacy text files contain non-UTF-8 bytes. The Copilot
/// bash fallback must still keep explicit changed paths in the checkpoint,
/// regardless of file extension.
#[test]
fn test_run_in_terminal_non_utf8_existing_text_files_are_attributed() {
    let repo = TestRepo::new();

    let java_path = repo.path().join("src/test/java/LegacyEncodingTest.java");
    let properties_path = repo.path().join("config/legacy-messages.properties");
    std::fs::create_dir_all(java_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(properties_path.parent().unwrap()).unwrap();

    let mut legacy_java = b"class LegacyEncodingTest {\n".to_vec();
    legacy_java.extend_from_slice(b"    // legacy bytes: ");
    legacy_java.extend_from_slice(&[0xE5, 0xA4, b'\n']);
    legacy_java.extend_from_slice(b"}\n");

    let mut legacy_properties = b"title=Legacy\ncomment=".to_vec();
    legacy_properties.extend_from_slice(&[0xE8, 0x83, b'\n']);

    std::fs::write(&java_path, &legacy_java).unwrap();
    std::fs::write(&properties_path, &legacy_properties).unwrap();
    repo.git(&[
        "add",
        "src/test/java/LegacyEncodingTest.java",
        "config/legacy-messages.properties",
    ])
    .unwrap();
    repo.git(&["commit", "-m", "Initial legacy files"]).unwrap();

    let mut updated_java = legacy_java;
    updated_java.splice(
        updated_java.len() - 2..updated_java.len() - 2,
        b"\n    @org.junit.jupiter.api.Test\n    public void copilotAddedSimpleTest2() {\n        org.junit.jupiter.api.Assertions.assertTrue(true);\n    }\n"
            .iter()
            .copied(),
    );
    let mut updated_properties = legacy_properties;
    updated_properties.extend_from_slice(b"generated.message=hello\n");
    std::fs::write(&java_path, updated_java).unwrap();
    std::fs::write(&properties_path, updated_properties).unwrap();

    let session_id = "non-utf8-fallback-bash-session";
    let tool_use_id = "call_nonUtf8Fallback__vscode-1775710201000";
    let post_hook_input = json!({
        "timestamp": "2026-04-09T04:50:44.542Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python generate_tests.py",
            "explanation": "Append generated tests.",
            "goal": "Generate tests",
            "isBackground": false
        },
        "tool_response": "",
        "tool_use_id": tool_use_id,
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    repo.sync_daemon();
    repo.git(&[
        "add",
        "src/test/java/LegacyEncodingTest.java",
        "config/legacy-messages.properties",
    ])
    .unwrap();
    repo.git(&["commit", "-m", "Append generated content to legacy files"])
        .unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.unknown_additions, 0);
    assert_eq!(stats.ai_additions, 6);
}

/// Test run_in_terminal with no file changes (no checkpoint created)
#[test]
fn test_run_in_terminal_no_changes() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O
    std::fs::write(repo.path().join("test.py"), "print('test')\n").unwrap();
    repo.git(&["add", "test.py"]).unwrap();
    repo.git(&["commit", "-m", "Initial commit"]).unwrap();

    let session_id = "c3f5a7b8-9d0e-1f2a-3b4c-5d6e7f8a9b0c";

    // PreToolUse hook
    let pre_hook_input = json!({
        "timestamp": "2026-04-09T05:00:00.000Z",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 test.py",
            "explanation": "Run test",
            "goal": "Validate",
            "isBackground": false
        },
        "tool_use_id": "call_testNoChanges__vscode-1775710200900",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // Command runs but doesn't modify any files

    // PostToolUse hook
    let post_hook_input = json!({
        "timestamp": "2026-04-09T05:00:00.200Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 test.py",
            "explanation": "Run test",
            "goal": "Validate",
            "isBackground": false
        },
        "tool_response": "test\n",
        "tool_use_id": "call_testNoChanges__vscode-1775710200900",
        "cwd": repo.path().to_str().unwrap()
    });

    // This should succeed but not create a checkpoint (no file changes)
    let result = repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ]);

    // Should either succeed with no checkpoint or fail with "No editable file paths" error
    match result {
        Ok(_) => {
            // No checkpoint created, which is fine
        }
        Err(msg) => {
            assert!(
                msg.contains("No editable file paths") || msg.contains("Skipping checkpoint"),
                "Unexpected error: {}",
                msg
            );
        }
    }
}

/// Regression: a VS Code workspace can contain a plain top-level folder plus
/// nested Git repositories. One non-repository workspace file in the same
/// Copilot payload must not drop AI checkpoints for nested repo files.
#[test]
fn copilot_mixed_workspace_paths_skip_non_repo_and_keep_nested_repo_ai() {
    let workspace = tempfile::tempdir().unwrap();
    let repo_path = workspace.path().join("ai-rag-doc");
    std::fs::create_dir_all(&repo_path).unwrap();
    let repo = TestRepo::new_at_path(&repo_path);

    std::fs::write(workspace.path().join("CHANGELOG.md"), "workspace notes\n").unwrap();
    std::fs::write(repo.path().join("seed.md"), "seed\n").unwrap();
    repo.git(&["add", "seed.md"]).unwrap();
    repo.git(&["commit", "-m", "Initial commit"]).unwrap();

    let doc_path = repo.path().join("generated.md");
    let workspace_file = workspace.path().join("CHANGELOG.md");
    let session_id = "mixed-workspace-session";
    let tool_use_id = "call_mixedWorkspace__vscode-1782464813820";

    let pre_hook_input = json!({
        "timestamp": "2026-06-27T23:19:05.820+08:00",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "apply_patch",
        "tool_input": {
            "patch": format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n workspace notes\n+more\n*** Add File: {}\n+generated by copilot\n*** End Patch",
                workspace_file.to_string_lossy().replace('\\', "/"),
                doc_path.to_string_lossy().replace('\\', "/")
            )
        },
        "tool_use_id": tool_use_id,
        "cwd": workspace.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    std::fs::write(&doc_path, "generated by copilot\n").unwrap();
    std::fs::write(
        workspace.path().join("CHANGELOG.md"),
        "workspace notes\nmore\n",
    )
    .unwrap();

    let post_hook_input = json!({
        "timestamp": "2026-06-27T23:19:06.520+08:00",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "apply_patch",
        "tool_input": {
            "patch": format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n workspace notes\n+more\n*** Add File: {}\n+generated by copilot\n*** End Patch",
                workspace_file.to_string_lossy().replace('\\', "/"),
                doc_path.to_string_lossy().replace('\\', "/")
            )
        },
        "tool_response": "",
        "tool_use_id": tool_use_id,
        "cwd": workspace.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    repo.sync_daemon();
    repo.git(&["add", "generated.md"]).unwrap();
    repo.git(&["commit", "-m", "Add generated nested repo doc"])
        .unwrap();
    repo.sync_daemon();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);
}
