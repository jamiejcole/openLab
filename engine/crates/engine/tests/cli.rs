// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::Command;

const ENGINE: &str = env!("CARGO_BIN_EXE_openlab-engine");

#[test]
fn version_prints_to_stdout() {
    let out = Command::new(ENGINE).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("openlab-engine {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn version_into_closed_pipe_does_not_panic() {
    // Close the read end before the engine starts, so its write always hits a broken pipe.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let status = Command::new(ENGINE)
        .arg("--version")
        .stdout(writer)
        .status()
        .unwrap();
    // 101 is Rust's panic exit code.
    assert_ne!(status.code(), Some(101), "engine panicked: {status}");
}
