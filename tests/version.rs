//! The release contract: the binary's `--version` must report the version
//! this crate is built from, and it must not need a database, a socket or a
//! network to do so.
//!
//! This is what a third party checks first ("which version am I running?"),
//! and it is what a tag has to agree with. A tag that says `v0.1.0` while
//! the binary prints `0.0.1` is a broken release.

use std::process::Command;

#[test]
fn version_flag_prints_the_crate_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .arg("--version")
        .output()
        .expect("reeflog binary runs");

    assert!(out.status.success(), "--version must exit 0");

    let stdout = String::from_utf8(out.stdout).expect("version output is utf-8");
    let version = env!("CARGO_PKG_VERSION");

    assert_eq!(
        stdout.trim(),
        format!("reeflog {version}"),
        "the binary must report exactly the crate version"
    );
}

#[test]
fn version_flag_needs_no_arguments_and_no_database() {
    // Run in an empty directory: if `--version` ever touched the journal it
    // would create a database file there.
    let dir = std::env::temp_dir().join(format!("reeflog-version-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .arg("--version")
        .current_dir(&dir)
        .output()
        .expect("reeflog binary runs");

    assert!(out.status.success());
    assert!(
        !dir.join("reeflog.db").exists(),
        "--version must not create a journal"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
