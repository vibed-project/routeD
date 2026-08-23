// SPDX-License-Identifier: Apache-2.0
//! Command-line interface for the `routed` service.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// routeD semantic router service.
#[derive(Parser, Debug)]
#[command(name = "routed", version = routed_version::VERSION, about)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the router service.
    Serve(Box<ServeArgs>),
    /// Print version and build metadata.
    Version,
}

/// Arguments for `routed serve`.
#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Ingress mode: inline OpenAI-compatible forwarder, or Envoy `ext_proc` gRPC server.
    #[arg(long, value_enum, env = "ROUTED_MODE")]
    pub mode: Mode,
    /// HTTP listen address: inline ingress (inline mode), decision and feedback APIs,
    /// health and metrics (both modes).
    #[arg(long, env = "ROUTED_HTTP_ADDR", default_value = "0.0.0.0:8080")]
    pub http_addr: String,
    /// gRPC listen address for the Envoy external processor (extproc mode only).
    #[arg(long, env = "ROUTED_EXTPROC_ADDR", default_value = "0.0.0.0:9002")]
    pub extproc_addr: String,
    /// Upstream gateway URL (inline mode).
    #[arg(long, env = "ROUTED_UPSTREAM")]
    pub upstream: Option<String>,
    /// Resource files or directories (`ModelTier`, `DataClass`, `RoutingPolicy`, `RouterProfile`)
    /// compiled into the snapshot at startup and re-read when they change.
    #[arg(long, env = "ROUTED_RESOURCES", value_delimiter = ',')]
    pub resources: Vec<PathBuf>,
    /// Seconds between checks of the resource files for changes (0 disables).
    #[arg(long, env = "ROUTED_RESOURCES_POLL_SECS", default_value_t = 5)]
    pub resources_poll_secs: u64,
    /// Operator `SnapshotService` gRPC address (ADR-0014); falls back to
    /// `--snapshot-path` when unset.
    #[arg(long, env = "ROUTED_SNAPSHOT_ADDR")]
    pub snapshot_addr: Option<String>,
    /// Directory with `tls.crt`/`tls.key`/`ca.crt` enabling mutual TLS to
    /// the operator's `SnapshotService` (ADR-0021). Unset dials plain TCP.
    #[arg(long, env = "ROUTED_SNAPSHOT_TLS_DIR")]
    pub snapshot_tls_dir: Option<PathBuf>,
    /// Server name checked against the operator certificate when it differs
    /// from the dial address.
    #[arg(long, env = "ROUTED_SNAPSHOT_TLS_DOMAIN")]
    pub snapshot_tls_domain: Option<String>,
    /// Path to a compiled snapshot JSON file (the operator's fallback
    /// `ConfigMap`, mounted as a volume; ADR-0014). Used when
    /// `--snapshot-addr` is unset; takes precedence over `--resources`.
    #[arg(long, env = "ROUTED_SNAPSHOT_PATH")]
    pub snapshot_path: Option<PathBuf>,
    /// Seconds between checks of `--snapshot-path` for changes.
    #[arg(long, env = "ROUTED_SNAPSHOT_PATH_POLL_SECS", default_value_t = 5)]
    pub snapshot_path_poll_secs: u64,
    /// Directory for the decision journal and feedback JSONL streams
    /// (ADR-0018); unset disables persistence.
    #[arg(long, env = "ROUTED_FEEDBACK_DIR")]
    pub feedback_dir: Option<PathBuf>,
    /// Maximum request body size in bytes.
    #[arg(long, env = "ROUTED_MAX_BODY_BYTES", default_value_t = 10 * 1024 * 1024)]
    pub max_body_bytes: usize,
    /// Idle timeout for streamed upstream responses, in seconds.
    #[arg(long, env = "ROUTED_STREAM_IDLE_SECS", default_value_t = 60)]
    pub stream_idle_secs: u64,
    /// Maximum concurrent classifier executions.
    #[arg(long, env = "ROUTED_CLASSIFY_CONCURRENCY", default_value_t = 8)]
    pub classify_concurrency: usize,
    /// OTLP gRPC endpoint for trace export (also `OTEL_EXPORTER_OTLP_ENDPOINT`).
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    pub otlp_endpoint: Option<String>,
    /// Record salted prompt hashes on decisions (never prompt text).
    #[arg(long, env = "ROUTED_LOG_PROMPT_HASHES", default_value_t = false)]
    pub log_prompt_hashes: bool,
    /// Forward every non-decision path to the upstream (default: only /v1/*).
    #[arg(long, env = "ROUTED_PASSTHROUGH_ALL", default_value_t = false)]
    pub passthrough_all: bool,
    /// Salt for prompt hashes.
    #[arg(
        long,
        env = "ROUTED_PROMPT_HASH_SALT",
        default_value = "",
        hide_env_values = true
    )]
    pub prompt_hash_salt: String,
}

/// Ingress mode.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// OpenAI-compatible HTTP forwarder.
    Inline,
    /// Envoy external processor (gRPC).
    Extproc,
}

impl Mode {
    /// Stable lowercase name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Inline => "inline",
            Mode::Extproc => "extproc",
        }
    }
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
        let names: Vec<_> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        assert_eq!(names, ["serve", "version"]);
    }

    #[test]
    fn mode_rejects_unknown() {
        assert!(Cli::try_parse_from(["routed", "serve", "--mode", "sidecar"]).is_err());
    }

    #[test]
    fn mode_parses() {
        let cli = Cli::try_parse_from(["routed", "serve", "--mode", "extproc"]).unwrap();
        match cli.command {
            Command::Serve(a) => assert_eq!(a.mode, Mode::Extproc),
            Command::Version => panic!("expected serve"),
        }
    }
}
