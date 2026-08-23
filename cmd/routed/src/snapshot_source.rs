// SPDX-License-Identifier: Apache-2.0
//! Snapshot sources (ADR-0014):
//! - [`FileSource`]: compiles resource files at startup and re-reads them
//!   when they change (standalone, no-operator use).
//! - [`CompiledFileSource`]: loads a pre-compiled snapshot JSON file (the
//!   operator's fallback `ConfigMap`, mounted as a volume); no recompilation.
//! - [`GrpcSource`]: the operator's `SnapshotService`, the primary path.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use routed_policy::compile;
use routed_snapshot::{Snapshot, SnapshotHolder};

/// Resource files / directories.
pub struct FileSource {
    paths: Vec<PathBuf>,
}

impl FileSource {
    /// New source.
    #[must_use]
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    fn fingerprint(&self) -> (u64, SystemTime) {
        let files = routedctl::collect_files(&self.paths).unwrap_or_default();
        let newest = files
            .iter()
            .filter_map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
            .max()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        (files.len() as u64, newest)
    }

    /// Compile the current files.
    ///
    /// # Errors
    /// On read / parse / compile errors.
    pub fn load(&self) -> anyhow::Result<Snapshot> {
        let input = routedctl::load_input(&self.paths)?;
        let (snapshot, report) =
            compile(&input).map_err(|e| anyhow::anyhow!("resources do not compile:\n{}", e.0))?;
        for w in report.warnings() {
            tracing::warn!(target: "routed.compile", "{w}");
        }
        Ok(snapshot)
    }

    /// Poll for changes and hot-swap the snapshot; compile errors keep the previous snapshot.
    pub async fn watch(self, holder: Arc<SnapshotHolder>, every: Duration) {
        let mut last = self.fingerprint();
        loop {
            tokio::time::sleep(every).await;
            let now = self.fingerprint();
            if now == last {
                continue;
            }
            last = now;
            match self.load() {
                Ok(snapshot) => {
                    let changed = holder.load().is_none_or(|cur| cur.hash != snapshot.hash);
                    if changed {
                        tracing::info!(hash = %snapshot.hash, "snapshot reloaded from files");
                        holder.store(snapshot);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "resource reload failed; keeping the previous snapshot");
                }
            }
        }
    }
}

/// A pre-compiled snapshot JSON file, refreshed on a poll interval. Used for
/// the operator's fallback `ConfigMap`, mounted as a volume: no
/// recompilation happens on the router side (ADR-0014).
pub struct CompiledFileSource {
    path: PathBuf,
}

impl CompiledFileSource {
    /// New source reading `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Read and parse the current file.
    ///
    /// # Errors
    /// On read or JSON errors.
    pub fn load(&self) -> anyhow::Result<Snapshot> {
        let text = std::fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn modified(&self) -> SystemTime {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    /// Poll for changes and hot-swap the snapshot; parse errors keep the
    /// previous snapshot. Also retries while no snapshot is loaded at all, so
    /// a file that appears after startup (the operator's fallback `ConfigMap`
    /// being published) is picked up even if its mtime was already observed.
    pub async fn watch(self, holder: Arc<SnapshotHolder>, every: Duration) {
        let mut last = self.modified();
        loop {
            tokio::time::sleep(every).await;
            let now = self.modified();
            if now == last && holder.load().is_some() {
                continue;
            }
            last = now;
            match self.load() {
                Ok(snapshot) => {
                    let changed = holder.load().is_none_or(|cur| cur.hash != snapshot.hash);
                    if changed {
                        tracing::info!(hash = %snapshot.hash, "snapshot reloaded from compiled file");
                        holder.store(snapshot);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "compiled snapshot reload failed; keeping the previous snapshot");
                }
            }
        }
    }
}

/// The operator's `SnapshotService` gRPC `Watch` stream: the primary
/// distribution path (ADR-0014). Reconnects with a fixed backoff on any
/// stream error or end; never gives up.
pub struct GrpcSource {
    addr: String,
    tls: Option<tonic::transport::ClientTlsConfig>,
}

impl GrpcSource {
    /// New source connecting to `addr` (e.g. `http://routed-operator:9090`).
    /// With `tls` set the connection is mutual TLS (ADR-0021); the address
    /// scheme should then be `https://`.
    #[must_use]
    pub fn new(addr: String, tls: Option<tonic::transport::ClientTlsConfig>) -> Self {
        Self { addr, tls }
    }

    /// Connect and stream updates into `holder` until the process exits.
    pub async fn watch(self, holder: Arc<SnapshotHolder>) {
        loop {
            if let Err(e) = self.watch_once(&holder).await {
                tracing::warn!(error = %e, addr = %self.addr, "snapshot gRPC stream ended; reconnecting");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn watch_once(&self, holder: &Arc<SnapshotHolder>) -> anyhow::Result<()> {
        use futures_util::StreamExt as _;

        let mut endpoint = tonic::transport::Channel::from_shared(self.addr.clone())?;
        if let Some(tls) = &self.tls {
            endpoint = endpoint.tls_config(tls.clone())?;
        }
        let mut client = routed_proto::snapshot_service_client::SnapshotServiceClient::new(
            endpoint.connect().await?,
        );
        let mut stream = client
            .watch(routed_proto::WatchRequest {
                client: "routed".to_owned(),
            })
            .await?
            .into_inner();
        tracing::info!(addr = %self.addr, "snapshot gRPC stream connected");
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            match serde_json::from_str::<Snapshot>(&chunk.snapshot_json) {
                Ok(snapshot) => {
                    let changed = holder.load().is_none_or(|cur| cur.hash != snapshot.hash);
                    if changed {
                        tracing::info!(hash = %snapshot.hash, "snapshot reloaded from gRPC");
                        holder.store(snapshot);
                    }
                }
                Err(e) => tracing::error!(error = %e, "malformed snapshot from operator; ignoring"),
            }
        }
        Ok(())
    }
}
