// SPDX-License-Identifier: Apache-2.0
//! Library half of `routedctl`: offline validation, explanation and CRD
//! generation, shared with the golden tests.

pub mod crd;
pub mod explain;
pub mod simulate;
pub mod validate;

use std::path::{Path, PathBuf};

use anyhow::Context;
use routed_policy::CompileInput;
use routed_policy::load::{into_input, parse_documents};

/// Collect `.yaml` / `.yml` / `.json` files from files and directories (recursively, sorted).
///
/// # Errors
/// On unreadable paths.
pub fn collect_files(paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        walk(p, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let meta = std::fs::metadata(p).with_context(|| format!("reading {}", p.display()))?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(p).with_context(|| format!("listing {}", p.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            walk(&entry.path(), out)?;
        }
    } else if matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml" | "json")
    ) {
        out.push(p.to_path_buf());
    }
    Ok(())
}

/// Load and parse all resources from the given files / directories.
///
/// # Errors
/// On unreadable or malformed files.
pub fn load_input(paths: &[PathBuf]) -> anyhow::Result<CompileInput> {
    let mut resources = Vec::new();
    for f in collect_files(paths)? {
        let text =
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
        let docs = parse_documents(&text).with_context(|| format!("parsing {}", f.display()))?;
        resources.extend(docs);
    }
    Ok(into_input(resources))
}
