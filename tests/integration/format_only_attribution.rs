use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Human reformats AI code (4-space → 2-space indent): attribution should stay AI.
#[test]
fn test_human_reindent_preserves_ai_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("indent.rs");

    // AI writes a function with 4-space indentation
    file.set_contents(crate::lines![
        "fn main() {".ai(),
        "    let x = 1;".ai(),
        "    let y = 2;".ai(),
        "    println!(\"{}\", x + y);".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI writes function").unwrap();

    // Human reformats to 2-space indentation (only whitespace changes)
    file.set_contents(crate::lines![
        "fn main() {".ai(),
        "  let x = 1;".ai(),
        "  let y = 2;".ai(),
        "  println!(\"{}\", x + y);".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("Human reformats indentation")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn main() {".ai(),
        "  let x = 1;".ai(),
        "  let y = 2;".ai(),
        "  println!(\"{}\", x + y);".ai(),
        "}".ai(),
    ]);
}

/// Human reformats AI code (tabs → spaces): attribution should stay AI.
#[test]
fn test_human_tab_to_space_preserves_ai_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("tabs.rs");

    // AI writes with tabs
    file.set_contents(crate::lines![
        "fn calc() {".ai(),
        "\tlet a = 10;".ai(),
        "\tlet b = 20;".ai(),
        "\ta + b".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI writes with tabs").unwrap();

    // Human converts tabs to spaces
    file.set_contents(crate::lines![
        "fn calc() {".ai(),
        "    let a = 10;".ai(),
        "    let b = 20;".ai(),
        "    a + b".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("Human converts tabs to spaces")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn calc() {".ai(),
        "    let a = 10;".ai(),
        "    let b = 20;".ai(),
        "    a + b".ai(),
        "}".ai(),
    ]);
}

/// Real edit mixed with formatting: only the edited part changes attribution.
#[test]
fn test_real_edit_inside_formatted_block() {
    let repo = TestRepo::new();
    let mut file = repo.filename("mixed_edit.rs");

    // AI writes a function
    file.set_contents(crate::lines![
        "fn process() {".ai(),
        "    let data = fetch();".ai(),
        "    transform(data);".ai(),
        "    save(data);".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI writes function").unwrap();

    // Human reformats AND changes one line
    file.set_contents(crate::lines![
        "fn process() {".ai(),
        "  let data = fetch();".ai(),
        "  transform_v2(data);", // actual semantic change → human
        "  save(data);".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("Human reformats and edits one line")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn process() {".ai(),
        "  let data = fetch();".ai(),
        "  transform_v2(data);".human(),
        "  save(data);".ai(),
        "}".ai(),
    ]);
}

/// AI checkpoint that reformats should still attribute to AI (unchanged behavior).
#[test]
fn test_ai_reformat_still_attributes_to_ai() {
    let repo = TestRepo::new();
    let mut file = repo.filename("ai_reformat.rs");

    file.set_contents(crate::lines!["fn run() {", "    execute();", "}"]);
    repo.stage_all_and_commit("Human writes function").unwrap();

    // AI reformats (only whitespace change on line 2; lines 1 and 3 unchanged)
    file.set_contents(crate::lines![
        "fn run() {", // unchanged → stays human
        "  execute();".ai(),
        "}", // unchanged → stays human
    ]);
    repo.stage_all_and_commit("AI reformats").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn run() {".human(),
        "  execute();".ai(),
        "}".human(),
    ]);
}

/// Trailing whitespace removal by human should preserve AI attribution.
#[test]
fn test_trailing_whitespace_trim_preserves_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("trailing.rs");

    // AI writes code with trailing spaces
    file.set_contents(crate::lines![
        "fn foo() {".ai(),
        "    bar();   ".ai(),
        "    baz();  ".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI writes with trailing ws")
        .unwrap();

    // Human trims trailing whitespace
    file.set_contents(crate::lines![
        "fn foo() {".ai(),
        "    bar();".ai(),
        "    baz();".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("Human trims trailing whitespace")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn foo() {".ai(),
        "    bar();".ai(),
        "    baz();".ai(),
        "}".ai(),
    ]);
}
