// SPDX-License-Identifier: Apache-2.0
//! Build metadata shared by every routeD binary.
//!
//! `COMMIT` is injected at build time by `build.rs` from the `ROUTED_COMMIT`
//! environment variable (set by the Makefile and image builds) or from git.

/// Crate version from `Cargo.toml`.
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Short git commit the binary was built from, or `unknown`.
pub const COMMIT: &str = env!("ROUTED_COMMIT");
/// One-line version string used by `--version` on every binary.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ROUTED_COMMIT"), ")");

/// Multi-line version report.
#[must_use]
pub fn long(binary: &str) -> String {
    format!(
        "{binary} {PKG_VERSION}\ncommit: {COMMIT}\narch: {}",
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_populated() {
        assert!(!COMMIT.is_empty());
        assert!(VERSION.starts_with(PKG_VERSION));
        assert!(long("routed").starts_with("routed "));
    }
}
