// SPDX-License-Identifier: Apache-2.0
//! `routedctl`: offline policy validation, decision explanation, simulation, model management.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use routedctl::explain::{ExplainRequest, Overrides, render_trace, use_color};

/// routedctl command-line tool.
#[derive(Parser, Debug)]
#[command(name = "routedctl", version = routed_version::VERSION, about)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Validate routed.io resources offline using the same compiler as the operator.
    Validate {
        /// Files or directories containing resources to validate.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Print diagnostics as JSON.
        #[arg(long)]
        json: bool,
        /// Write the compiled snapshot JSON to a file (the trainer's input,
        /// ADR-0018; same document the operator distributes).
        #[arg(long)]
        emit_snapshot: Option<PathBuf>,
    },
    /// Explain the decision the router would make for a request.
    Explain {
        /// Resource files (or directory) defining the policy, tiers and data classes.
        #[arg(long, required_unless_present = "dir")]
        policy: Vec<PathBuf>,
        /// OpenAI-format request JSON.
        #[arg(long, required_unless_present = "dir")]
        request: Option<PathBuf>,
        /// Example directory (request.json, resources.yaml, headers.json, findings.json, overrides.json).
        #[arg(long, conflicts_with_all = ["policy", "request"])]
        dir: Option<PathBuf>,
        /// Request path.
        #[arg(long, default_value = "/v1/chat/completions")]
        path: String,
        /// Header `Name: value` (repeatable).
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
        /// Print only the decision JSON.
        #[arg(long)]
        json: bool,
    },
    /// Replay a request log against a snapshot and report cost/quality/sovereignty summaries.
    Simulate {
        /// Resource files (or directory) defining the policy to simulate.
        #[arg(long)]
        policy: PathBuf,
        /// JSONL file of requests to replay (bare `OpenAI` requests, or
        /// `{"request": ..., "path": ..., "headers": {...}}`).
        #[arg(long)]
        requests: PathBuf,
        /// Print the summary as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage local classifier model artifacts.
    Models {
        /// Models subcommand.
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// Work with the CRD manifests.
    Crd {
        /// CRD subcommand.
        #[command(subcommand)]
        cmd: CrdCmd,
    },
    /// Print version and build metadata.
    Version,
}

/// `routedctl models` subcommands.
#[derive(Subcommand, Debug)]
pub enum ModelsCmd {
    /// Pre-warm model artifacts into a local cache.
    Pull {
        /// Artifact reference (for example `oci://ghcr.io/vibed-project/routed-models/classifier@sha256:...`).
        reference: String,
        /// Destination directory (default: the configured model cache).
        #[arg(long)]
        dest: Option<PathBuf>,
    },
}

/// `routedctl crd` subcommands.
#[derive(Subcommand, Debug)]
pub enum CrdCmd {
    /// Generate CRD manifests from the API types.
    Gen {
        /// Output directory.
        #[arg(long, default_value = "config/crd")]
        out: PathBuf,
    },
    /// Print CRD manifests to stdout.
    Print,
    /// Generate the CRD field reference (docs/crds.md).
    Docs {
        /// Output file.
        #[arg(long, default_value = "docs/crds.md")]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("{}", routed_version::long("routedctl"));
            Ok(())
        }
        Command::Validate {
            files,
            json,
            emit_snapshot,
        } => {
            let v = routedctl::validate::run(&files)?;
            if let (Some(path), Some(snapshot)) = (&emit_snapshot, &v.snapshot) {
                std::fs::write(path, serde_json::to_string_pretty(snapshot)?)?;
                eprintln!("wrote {}", path.display());
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "diagnostics": v.report.diags, "snapshotHash": v.hash })
                    )?
                );
            } else {
                print!("{}", v.report);
                if let Some(h) = &v.hash {
                    println!(
                        "ok: {} resource file(s) compile to snapshot {h}",
                        routedctl::collect_files(&files)?.len()
                    );
                } else {
                    println!("failed: {} error(s)", v.report.errors().count());
                }
            }
            if v.hash.is_none() {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Explain {
            policy,
            request,
            dir,
            path,
            headers,
            json,
        } => {
            let req = if let Some(d) = dir {
                ExplainRequest::from_dir(&d, &new_id())?
            } else {
                let request = request.ok_or_else(|| anyhow::anyhow!("--request is required"))?;
                let mut hmap = BTreeMap::new();
                for h in headers {
                    let (k, v) = h
                        .split_once(':')
                        .ok_or_else(|| anyhow::anyhow!("header must be `Name: value`: {h}"))?;
                    hmap.insert(k.trim().to_owned(), v.trim().to_owned());
                }
                ExplainRequest {
                    policy,
                    request: std::fs::read(&request)?,
                    path,
                    headers: hmap,
                    findings: None,
                    overrides: Overrides::default(),
                    id: new_id(),
                }
            };
            let e = routedctl::explain::run(&req)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&e.decision)?);
            } else {
                print!("{}", render_trace(&e, use_color()));
                println!("{}", serde_json::to_string_pretty(&e.decision)?);
            }
            Ok(())
        }
        Command::Simulate {
            policy,
            requests,
            json,
        } => {
            let summary = routedctl::simulate::run(&policy, &requests)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print!("{}", routedctl::simulate::render(&summary));
            }
            Ok(())
        }
        Command::Models {
            cmd: ModelsCmd::Pull { reference, dest },
        } => {
            let resolver = dest.map_or_else(
                routed_artifact::Resolver::from_env,
                routed_artifact::Resolver::new,
            );
            let path = resolver.resolve(&reference)?;
            println!("{}", path.display());
            Ok(())
        }
        Command::Crd {
            cmd: CrdCmd::Gen { out },
        } => {
            for f in routedctl::crd::write(&out)? {
                println!("wrote {}", out.join(f).display());
            }
            Ok(())
        }
        Command::Crd {
            cmd: CrdCmd::Docs { out },
        } => {
            std::fs::write(&out, routedctl::crd::render_docs()?)?;
            println!("wrote {}", out.display());
            Ok(())
        }
        Command::Crd { cmd: CrdCmd::Print } => {
            for (_, yaml) in routedctl::crd::render()? {
                println!("---\n{yaml}");
            }
            Ok(())
        }
    }
}

fn new_id() -> String {
    // ULID-like: time-ordered and unique enough for a CLI; the router uses real ULIDs.
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{t:026x}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn subcommand_set() {
        let cmd = Cli::command();
        let names: Vec<_> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "validate", "explain", "simulate", "models", "crd", "version"
            ]
        );
        let models = cmd.find_subcommand("models").unwrap();
        let subs: Vec<_> = models
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        assert_eq!(subs, ["pull"]);
    }
}
