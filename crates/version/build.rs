// SPDX-License-Identifier: Apache-2.0
//! Emits `ROUTED_COMMIT` for `env!` in lib.rs.
//! Precedence: `ROUTED_COMMIT` env (Makefile / image build-arg) > `git rev-parse` > "unknown".
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=ROUTED_COMMIT");
    let git_dir = std::path::Path::new("../../.git");
    if git_dir.join("HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
        if let Some(r) = std::fs::read_to_string(git_dir.join("HEAD"))
            .ok()
            .as_deref()
            .and_then(|h| h.strip_prefix("ref: "))
        {
            println!("cargo:rerun-if-changed=../../.git/{}", r.trim());
        }
    }
    let commit = std::env::var("ROUTED_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=ROUTED_COMMIT={commit}");
}
