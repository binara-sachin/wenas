use assert_cmd::Command;

fn wenas() -> Command {
    Command::cargo_bin("wenas").unwrap()
}

#[test]
fn identical_files_exit_zero() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "x\n").unwrap();
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n");
}

#[test]
fn differing_files_without_a_terminal_exit_two_with_hint() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("git diff --no-index"));
}

#[test]
fn missing_file_and_mixed_types_exit_two() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    std::fs::write(&a, "x\n").unwrap();
    wenas().arg(&a).arg(d.path().join("nope")).assert().code(2);
    wenas().arg(&a).arg(d.path()).assert().code(2);
}

#[test]
fn git_config_prints_snippet() {
    wenas()
        .arg("--git-config")
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "cmd = wenas \"$LOCAL\" \"$REMOTE\"",
        ))
        .stdout(predicates::str::contains("dd = difftool --dir-diff"));
}

#[test]
fn whitespace_only_difference_is_identical_under_w() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, "a b\n").unwrap();
    std::fs::write(&b, "a  b\n").unwrap();
    wenas().arg("-b").arg(&a).arg(&b).assert().success();
}

#[test]
fn git_config_sets_trust_exit_code_where_git_reads_it() {
    // git ignores trustExitCode under [difftool "<tool>"]; it must be in [difftool].
    let out = wenas().arg("--git-config").output().unwrap().stdout;
    let text = String::from_utf8(out).unwrap();
    let difftool = text.split("[difftool \"wenas\"]").next().unwrap();
    assert!(
        difftool.contains("[difftool]") && difftool.contains("trustExitCode = true"),
        "{text}"
    );
    assert!(
        !text
            .split("[difftool \"wenas\"]")
            .nth(1)
            .unwrap()
            .contains("trustExitCode")
    );
}

#[test]
fn directories_need_a_terminal_even_when_identical() {
    // The directory list always opens (spec 6.3), so with no terminal to show it
    // on there is nothing sensible to do but exit 2 at once (8.3).
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    for root in [&a, &b] {
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/f.txt"), "same\n").unwrap();
    }
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("git diff --no-index"));
}

#[test]
fn differing_directories_without_a_terminal_exit_two() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("f"), "1").unwrap();
    std::fs::write(b.join("f"), "2").unwrap();
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("git diff --no-index"));
}
