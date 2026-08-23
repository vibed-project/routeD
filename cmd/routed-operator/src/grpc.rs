// SPDX-License-Identifier: Apache-2.0
//! `SnapshotService` server (ADR-0014): streams the current compiled
//! snapshot's canonical JSON immediately on connect, then again every time
//! it changes. Every replica serves this independently from its own
//! in-memory copy; there is no leader gating on the read path.

use std::pin::Pin;

use routed_proto::snapshot_service_server::SnapshotService;
use routed_proto::{SnapshotChunk, WatchRequest};
use tokio::sync::watch;
use tokio_stream::{Stream, StreamExt, wrappers::WatchStream};
use tonic::{Request, Response, Status};

/// Broadcasts the latest compiled snapshot's JSON to every gRPC watcher.
pub struct Service {
    rx: watch::Receiver<Option<String>>,
}

impl Service {
    #[must_use]
    pub fn new(rx: watch::Receiver<Option<String>>) -> Self {
        Self { rx }
    }
}

#[tonic::async_trait]
impl SnapshotService for Service {
    type WatchStream = Pin<Box<dyn Stream<Item = Result<SnapshotChunk, Status>> + Send>>;

    async fn watch(
        &self,
        request: Request<WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let client = request.into_inner().client;
        tracing::info!(client, "snapshot watcher subscribed");
        let stream = WatchStream::new(self.rx.clone()).filter_map(|json: Option<String>| {
            json.map(|snapshot_json| Ok::<_, Status>(SnapshotChunk { snapshot_json }))
        });
        Ok(Response::new(Box::pin(stream)))
    }
}
