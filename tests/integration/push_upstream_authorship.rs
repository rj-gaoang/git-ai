use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

#[test]
fn push_with_set_upstream_flag_pushes_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("upstream_feature.rs");
    file.set_contents(vec!["fn upstream_feature() {}".ai()]);
    let commit = local
        .stage_all_and_commit("add upstream feature")
        .expect("commit should succeed");

    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push with -u should succeed");

    let note = local.read_authorship_note_in_git_dir(upstream.path(), &commit.commit_sha);
    assert!(
        note.is_some(),
        "expected authorship notes to be pushed to the remote when using -u"
    );
}

#[test]
fn push_after_branch_set_upstream_pushes_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("upstream_branch.rs");
    file.set_contents(vec!["fn initial() {}".ai()]);
    local
        .stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    local
        .git(&["push", "origin", "HEAD"])
        .expect("initial push should succeed");

    let branch = local.current_branch();
    local
        .git(&["branch", "-u", &format!("origin/{}", branch)])
        .expect("branch -u should succeed");

    file.set_contents(vec!["fn initial() {}".ai(), "fn follow_up() {}".ai()]);
    let follow_up = local
        .stage_all_and_commit("follow-up commit")
        .expect("follow-up commit should succeed");

    local
        .git(&["push"])
        .expect("push with configured upstream should succeed");

    let note = local.read_authorship_note_in_git_dir(upstream.path(), &follow_up.commit_sha);
    assert!(
        note.is_some(),
        "expected authorship notes to be pushed after setting upstream with git branch -u"
    );
}

#[test]
fn push_backfills_missing_authorship_note_for_uncaptured_commit() {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("uncaptured_push_backfill.rs");
    file.set_contents(vec!["fn generated_by_ai() {}".ai()]);

    local
        .git_og(&["add", "-A"])
        .expect("raw git add should succeed");
    local
        .git_og(&["commit", "-m", "uncaptured commit"])
        .expect("raw git commit should succeed");
    let commit_sha = local
        .git_og(&["rev-parse", "HEAD"])
        .expect("rev-parse HEAD should succeed")
        .trim()
        .to_string();

    assert!(
        local.read_authorship_note(&commit_sha).is_none(),
        "test setup should create a commit without a git-ai authorship note"
    );

    local
        .git(&["push", "origin", "HEAD"])
        .expect("push should succeed");

    let local_note = local.read_authorship_note(&commit_sha);
    assert!(
        local_note.is_some(),
        "push should backfill a missing local authorship note before syncing notes"
    );

    let remote_note = local.read_authorship_note_in_git_dir(upstream.path(), &commit_sha);
    assert!(
        remote_note.is_some(),
        "push should sync the backfilled authorship note to the remote"
    );
}

crate::reuse_tests_in_worktree!(
    push_with_set_upstream_flag_pushes_authorship_notes,
    push_after_branch_set_upstream_pushes_authorship_notes,
    push_backfills_missing_authorship_note_for_uncaptured_commit,
);
