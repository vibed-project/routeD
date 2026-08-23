// SPDX-License-Identifier: Apache-2.0
//! `routed-operator`: reconciles routed.io CRDs into routing snapshots and
//! distributes them to `routed` (ADR-0014).

mod configmap;
mod grpc;
mod health;
mod leader;
mod reconcile;
mod status;
mod webhook;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use prometheus_client::registry::Registry;
use tokio_stream::StreamExt as _;

/// routeD Kubernetes operator.
#[derive(Parser, Debug)]
#[command(name = "routed-operator", version = routed_version::VERSION, about)]
pub struct Cli {
    /// Address for the Prometheus metrics endpoint.
    #[arg(long, env = "ROUTED_METRICS_ADDR", default_value = "0.0.0.0:8080")]
    pub metrics_addr: String,
    /// Address for liveness/readiness probes.
    #[arg(long, env = "ROUTED_HEALTH_ADDR", default_value = "0.0.0.0:8081")]
    pub health_addr: String,
    /// Address for the snapshot distribution gRPC service.
    #[arg(
        long,
        env = "ROUTED_SNAPSHOT_GRPC_ADDR",
        default_value = "0.0.0.0:9090"
    )]
    pub snapshot_grpc_addr: String,
    /// Enable leader election (required when running more than one replica).
    #[arg(long, env = "ROUTED_LEADER_ELECT", default_value_t = false)]
    pub leader_elect: bool,
    /// Restrict watches to a single namespace (default: all namespaces).
    #[arg(long, env = "ROUTED_WATCH_NAMESPACE")]
    pub watch_namespace: Option<String>,
    /// Namespace the leader lease and fallback `ConfigMap` are created in.
    #[arg(long, env = "ROUTED_OPERATOR_NAMESPACE", default_value = "default")]
    pub namespace: String,
    /// Name of the leader election `Lease`.
    #[arg(
        long,
        env = "ROUTED_LEASE_NAME",
        default_value = "routed-operator-leader"
    )]
    pub lease_name: String,
    /// Name of the fallback snapshot `ConfigMap` (ADR-0014).
    #[arg(
        long,
        env = "ROUTED_SNAPSHOT_CONFIGMAP",
        default_value = "routed-snapshot"
    )]
    pub configmap_name: String,
    /// This replica's identity for leader election (defaults to the pod
    /// name via the downward API, then `$HOSTNAME`).
    #[arg(long, env = "POD_NAME")]
    pub pod_name: Option<String>,
    /// Address for the validating admission webhook (served only when
    /// `--webhook-certs-dir` is set).
    #[arg(long, env = "ROUTED_WEBHOOK_ADDR", default_value = "0.0.0.0:9443")]
    pub webhook_addr: String,
    /// Directory holding the webhook TLS material (`tls.crt`, `tls.key`);
    /// setting it enables the webhook server (ADR-0015).
    #[arg(long, env = "ROUTED_WEBHOOK_CERTS_DIR")]
    pub webhook_certs_dir: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // kube never installs a rustls CryptoProvider itself, and this binary's
    // dependency graph enables more than one; without an explicit choice
    // rustls panics on the first TLS connection to the API server.
    let _ = rustls::crypto::ring::default_provider().install_default();
    routed_telemetry::init_tracing();
    let cli = Cli::parse();
    tracing::info!(
        metrics = %cli.metrics_addr,
        health = %cli.health_addr,
        grpc = %cli.snapshot_grpc_addr,
        leader_elect = cli.leader_elect,
        "routed-operator starting"
    );

    let client = kube::Client::try_default().await?;

    let identity = cli.pod_name.clone().unwrap_or_else(|| {
        std::env::var("HOSTNAME").unwrap_or_else(|_| "routed-operator".to_owned())
    });
    let leadership = if cli.leader_elect {
        leader::spawn(
            client.clone(),
            cli.namespace.clone(),
            cli.lease_name.clone(),
            identity,
        )
    } else {
        leader::Leadership::always()
    };

    let (snapshot_tx, snapshot_rx) = tokio::sync::watch::channel::<Option<String>>(None);
    let ready = Arc::new(AtomicBool::new(false));
    let mut registry = Registry::default();
    let metrics = health::Metrics::register(&mut registry);
    let app_state = health::AppState {
        ready: Arc::clone(&ready),
        registry: Arc::new(registry),
    };

    let grpc_addr: std::net::SocketAddr = cli.snapshot_grpc_addr.parse()?;
    let grpc_service = grpc::Service::new(snapshot_rx);
    let grpc_server = tonic::transport::Server::builder()
        .add_service(
            routed_proto::snapshot_service_server::SnapshotServiceServer::new(grpc_service),
        )
        .serve(grpc_addr);

    let health_addr: std::net::SocketAddr = cli.health_addr.parse()?;
    let health_listener = tokio::net::TcpListener::bind(health_addr).await?;
    let health_server = axum::serve(health_listener, health::health_router(app_state.clone()));

    let metrics_addr: std::net::SocketAddr = cli.metrics_addr.parse()?;
    let metrics_listener = tokio::net::TcpListener::bind(metrics_addr).await?;
    let metrics_server = axum::serve(metrics_listener, health::metrics_router(app_state));

    let webhook_server = {
        let webhook_addr: std::net::SocketAddr = cli.webhook_addr.parse()?;
        let state = Arc::new(webhook::WebhookState {
            client: client.clone(),
            watch_namespace: cli.watch_namespace.clone(),
        });
        let certs_dir = cli.webhook_certs_dir.clone();
        async move {
            match certs_dir {
                Some(dir) => webhook::serve(webhook_addr, dir, state).await,
                None => std::future::pending().await,
            }
        }
    };

    let reconcile_loop = run_reconcile_loop(
        client,
        cli.watch_namespace,
        cli.namespace,
        cli.configmap_name,
        leadership,
        snapshot_tx,
        ready,
        metrics,
    );

    tokio::select! {
        r = grpc_server => r.map_err(anyhow::Error::from),
        r = health_server => r.map_err(anyhow::Error::from),
        r = metrics_server => r.map_err(anyhow::Error::from),
        r = webhook_server => r,
        () = reconcile_loop => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_reconcile_loop(
    client: kube::Client,
    watch_namespace: Option<String>,
    write_namespace: String,
    configmap_name: String,
    leadership: leader::Leadership,
    snapshot_tx: tokio::sync::watch::Sender<Option<String>>,
    ready: Arc<AtomicBool>,
    metrics: health::Metrics,
) {
    let mut triggers = Box::pin(reconcile::trigger_stream(
        &client,
        watch_namespace.as_deref(),
    ));
    // Hashes of the last snapshot sent to gRPC watchers and the last one
    // published to the ConfigMap, tracked separately: a replica can become
    // leader after the snapshot changed and must still publish. Status writes
    // fire our own watcher, so reconciles with an unchanged snapshot are
    // common and must not re-send or re-publish.
    let mut sent_hash: Option<String> = None;
    let mut cm_hash: Option<String> = None;
    loop {
        match reconcile::compile_once(&client, watch_namespace.as_deref()).await {
            Ok(compiled) => {
                metrics.reconciles_total.inc();
                metrics
                    .compile_errors
                    .set(i64::try_from(compiled.report.errors().count()).unwrap_or(i64::MAX));
                metrics.is_leader.set(i64::from(leadership.is_leader()));

                if let Some(snapshot) = compiled
                    .snapshot
                    .as_ref()
                    .filter(|s| sent_hash.as_deref() != Some(s.hash.as_str()))
                {
                    match serde_json::to_string(snapshot) {
                        Ok(json) => {
                            let _ = snapshot_tx.send(Some(json));
                            ready.store(true, Ordering::Relaxed);
                            sent_hash = Some(snapshot.hash.clone());
                        }
                        Err(e) => tracing::error!(error = %e, "failed to serialise snapshot"),
                    }
                }

                if leadership.is_leader() {
                    status::apply(&client, &compiled).await;
                    if let Some(snapshot) = compiled
                        .snapshot
                        .as_ref()
                        .filter(|s| cm_hash.as_deref() != Some(s.hash.as_str()))
                    {
                        match configmap::publish(
                            &client,
                            &write_namespace,
                            &configmap_name,
                            snapshot,
                        )
                        .await
                        {
                            Ok(()) => cm_hash = Some(snapshot.hash.clone()),
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to publish fallback ConfigMap");
                            }
                        }
                    }
                }

                tracing::info!(
                    errors = compiled.report.errors().count(),
                    warnings = compiled.report.warnings().count(),
                    hash = compiled
                        .snapshot
                        .as_ref()
                        .map_or("<none>", |s| s.hash.as_str()),
                    leader = leadership.is_leader(),
                    "reconciled"
                );
            }
            Err(e) => {
                // A transient list failure must not park the loop on the
                // trigger stream (no CRD change, no retry): back off and retry.
                tracing::error!(error = %e, "failed to list CRDs; retrying in 5s");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        }

        if triggers.next().await.is_none() {
            break;
        }
        // Debounce: coalesce a burst of events (an applied manifest with many
        // objects, or our own status writes echoing back) into one recompile.
        while tokio::time::timeout(std::time::Duration::from_millis(300), triggers.next())
            .await
            .is_ok_and(|ev| ev.is_some())
        {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn manager_flags_exist() {
        let ids: Vec<_> = Cli::command()
            .get_arguments()
            .map(|a| a.get_id().as_str().to_owned())
            .collect();
        for want in [
            "metrics_addr",
            "health_addr",
            "leader_elect",
            "snapshot_grpc_addr",
        ] {
            assert!(ids.contains(&want.to_owned()), "missing flag {want}");
        }
    }
}
