// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

use std::process::Command;

fn main() {
    buildinfo::generate_version_info();

    // Tell cargo when to rebuild - watch entire bpf directory so make can handle detailed dependencies
    println!("cargo:rerun-if-changed=bpf");

    // Shell out to make for actual compilation
    let status = Command::new("make")
        .arg("-C")
        .arg("../bpf")
        .status()
        .expect("Failed to execute make");

    if !status.success() {
        panic!("Failed to build dispatcher bytecode - required for hermetic builds");
    }
}
