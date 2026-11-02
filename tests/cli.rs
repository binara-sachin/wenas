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
fn identical_directories_exit_zero_without_the_ui() {
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
        .success()
        .stdout("identical\n");
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

#[test]
fn large_identical_directories_still_report_identical_without_a_terminal() {
    // More files than a 150 ms look-ahead could cover on a slow machine: the
    // non-terminal path must wait for the whole scan rather than give up.
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    for root in [&a, &b] {
        for dir in 0..40 {
            let p = root.join(format!("d{dir}"));
            std::fs::create_dir_all(&p).unwrap();
            for f in 0..100 {
                std::fs::write(p.join(format!("f{f}")), format!("{dir}-{f}\n")).unwrap();
            }
        }
    }
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n");
}
