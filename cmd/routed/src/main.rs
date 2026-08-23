// SPDX-License-Identifier: Apache-2.0
//! `routed`: the semantic router service.

mod cli;
mod snapshot_source;

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use cli::{Cli, Command, Mode};
use routed_ingress_inline::{AppState, Config};
use routed_snapshot::SnapshotHolder;
use routed_telemetry::{Telemetry, TelemetryConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            routed_telemetry::init_tracing();
            println!("{}", routed_version::long("routed"));
            Ok(())
        }
        Command::Serve(args) => serve(*args).await,
    }
}

async fn serve(args: cli::ServeArgs) -> anyhow::Result<()> {
    if args.log_prompt_hashes && args.prompt_hash_salt.trim().is_empty() {
        anyhow::bail!(
            "--log-prompt-hashes requires a non-empty --prompt-hash-salt (ROUTED_PROMPT_HASH_SALT)"
        );
    }
    let telemetry = Arc::new(Telemetry::init(TelemetryConfig {
        otlp_endpoint: args.otlp_endpoint.clone(),
        service_name: "routed".into(),
        prompt_hashes: args.log_prompt_hashes,
        prompt_hash_salt: args.prompt_hash_salt.clone(),
    })?);
    tracing::info!(mode = args.mode.as_str(), http = %args.http_addr, extproc = %args.extproc_addr, version = routed_version::VERSION, "routed serve");

    let holder = Arc::new(SnapshotHolder::new());
    if args.resources.is_empty() && args.snapshot_addr.is_none() && args.snapshot_path.is_none() {
        anyhow::bail!(
            "no snapshot source: pass one of --snapshot-addr, --snapshot-path or --resources"
        );
    }
    let mut classifier_timeout = Duration::from_millis(25);
    if let Some(addr) = args.snapshot_addr.clone() {
        tracing::info!(addr = %addr, "snapshot source: operator gRPC");
        let source = snapshot_source::GrpcSource::new(addr);
        tokio::spawn(source.watch(Arc::clone(&holder)));
        // Wait briefly for the first snapshot so the classifier is built from
        // the distributed RouterProfile instead of defaults. /readyz stays 503
        // until a snapshot arrives either way; this only closes the window in
        // which a late snapshot would leave defaults in place until restart.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while holder.load().is_none() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(s) = holder.load() {
            if let Some(p) = s
                .core
                .profiles
                .get("default")
                .or_else(|| s.core.profiles.values().next())
            {
                classifier_timeout = Duration::from_millis(p.classifier_timeout_ms);
            }
        } else {
            tracing::warn!(
                "no snapshot from the operator after 30s; classifier uses defaults until restart"
            );
        }
    } else if let Some(path) = args.snapshot_path.clone() {
        tracing::info!(path = %path.display(), "snapshot source: compiled file");
        let source = snapshot_source::CompiledFileSource::new(path);
        // The file may not exist yet: the operator publishes the fallback
        // ConfigMap only after its first successful compile, and the chart
        // mounts it as an optional volume. With polling enabled the watcher
        // picks it up when it appears; /readyz stays 503 until then.
        match source.load() {
            Ok(snapshot) => {
                if let Some(p) = snapshot
                    .core
                    .profiles
                    .get("default")
                    .or_else(|| snapshot.core.profiles.values().next())
                {
                    classifier_timeout = Duration::from_millis(p.classifier_timeout_ms);
                }
                tracing::info!(hash = %snapshot.hash, tiers = snapshot.core.tiers.len(), policies = snapshot.core.policies.len(), "snapshot loaded from compiled file");
                holder.store(snapshot);
            }
            Err(e) if args.snapshot_path_poll_secs > 0 => {
                tracing::warn!(error = %e, "compiled snapshot not readable yet; polling until it appears");
            }
            Err(e) => return Err(e),
        }
        if args.snapshot_path_poll_secs > 0 {
            let h = Arc::clone(&holder);
            tokio::spawn(source.watch(h, Duration::from_secs(args.snapshot_path_poll_secs)));
        }
    } else {
        tracing::info!(resources = ?args.resources, "snapshot source: local resource files");
        let source = snapshot_source::FileSource::new(args.resources.clone());
        let snapshot = source.load()?;
        if let Some(p) = snapshot
            .core
            .profiles
            .get("default")
            .or_else(|| snapshot.core.profiles.values().next())
        {
            classifier_timeout = Duration::from_millis(p.classifier_timeout_ms);
        }
        tracing::info!(hash = %snapshot.hash, tiers = snapshot.core.tiers.len(), policies = snapshot.core.policies.len(), "snapshot loaded from files");
        holder.store(snapshot);
        if args.resources_poll_secs > 0 {
            let h = Arc::clone(&holder);
            tokio::spawn(source.watch(h, Duration::from_secs(args.resources_poll_secs)));
        }
    }

    let profile = holder.load().and_then(|s| {
        s.core
            .profiles
            .get("default")
            .or_else(|| s.core.profiles.values().next())
            .cloned()
    });
    let classifier: Arc<dyn routed_classify::Classifier> =
        Arc::from(routed_classify::from_profile(profile.as_ref())?);
    tracing::info!(
        classifier = classifier.name(),
        timeout_ms = u64::try_from(classifier_timeout.as_millis()).unwrap_or(u64::MAX),
        "classifier ready"
    );
    // Fail safe: a configured learned router that cannot load stops startup
    // rather than silently routing on priors (ADR-0018).
    let predictor = routed_classify::predictor_from_profile(profile.as_ref())?;
    if predictor.is_some() {
        tracing::info!("learned router predictor ready");
    }
    let feedback_sink: Option<Arc<dyn routed_feedback::FeedbackSink>> = match &args.feedback_dir {
        Some(dir) => {
            let sink = routed_feedback::JsonlSink::spawn(dir)?;
            tracing::info!(dir = %dir.display(), "feedback persistence enabled");
            Some(Arc::new(sink))
        }
        None => None,
    };
    let finish_state = move |mut state: AppState| {
        if let Some(p) = predictor {
            state = state.with_predictor(p);
        }
        if let Some(s) = feedback_sink {
            state = state.with_feedback(s);
        }
        state
    };

    match args.mode {
        Mode::Inline => {
            let upstream = args
                .upstream
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--upstream is required in inline mode"))?;
            let config = Config {
                upstream,
                max_body_bytes: args.max_body_bytes,
                classify_timeout: classifier_timeout,
                classify_concurrency: args.classify_concurrency,
                stream_idle_timeout: Duration::from_secs(args.stream_idle_secs),
                passthrough_all: args.passthrough_all,
                ..Config::default()
            };
            let state = Arc::new(finish_state(AppState::new(
                Arc::clone(&holder),
                classifier,
                Arc::clone(&telemetry),
                config,
            )?));
            let addr: std::net::SocketAddr = args.http_addr.parse()?;
            routed_ingress_inline::serve(addr, state, shutdown_signal()).await?;
        }
        Mode::Extproc => {
            // The upstream client exists but is never contacted: ext_proc
            // mutates in place and Envoy forwards. The HTTP port serves the
            // decision / feedback / health / metrics APIs only.
            let config = Config {
                upstream: args
                    .upstream
                    .clone()
                    .unwrap_or_else(|| "http://127.0.0.1:9".to_owned()),
                max_body_bytes: args.max_body_bytes,
                classify_timeout: classifier_timeout,
                classify_concurrency: args.classify_concurrency,
                stream_idle_timeout: Duration::from_secs(args.stream_idle_secs),
                passthrough_all: args.passthrough_all,
                ..Config::default()
            };
            let state = Arc::new(finish_state(AppState::new(
                Arc::clone(&holder),
                classifier,
                Arc::clone(&telemetry),
                config,
            )?));
            let http_addr: std::net::SocketAddr = args.http_addr.parse()?;
            let grpc_addr: std::net::SocketAddr = args.extproc_addr.parse()?;
            let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
            tracing::info!(%http_addr, "api listening");
            let api = axum::serve(
                http_listener,
                routed_ingress_inline::api_router(Arc::clone(&state)),
            )
            .with_graceful_shutdown(shutdown_signal());
            let extproc = routed_ingress_extproc::serve(grpc_addr, state, shutdown_signal());
            tokio::select! {
                r = api => r?,
                r = extproc => r?,
            }
        }
    }
    telemetry.shutdown();
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
