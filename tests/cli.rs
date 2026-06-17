#![allow(clippy::unwrap_used)]
use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("sync"))
        .stdout(contains("render"))
        .stdout(contains("status"));
}

#[test]
fn sync_rejects_bad_slug() {
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args(["sync", "noslash"])
        .assert()
        .failure();
}
