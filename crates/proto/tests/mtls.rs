// SPDX-License-Identifier: Apache-2.0
//! mTLS round-trip for `SnapshotService` (ADR-0021): a client with a
//! CA-signed identity streams snapshots; a client without one is refused.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::pin::Pin;

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use routed_proto::snapshot_service_client::SnapshotServiceClient;
use routed_proto::snapshot_service_server::{SnapshotService, SnapshotServiceServer};
use routed_proto::{SnapshotChunk, WatchRequest};
use tokio_stream::{Stream, wrappers::TcpListenerStream};
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};

struct StaticSnapshot;

#[tonic::async_trait]
impl SnapshotService for StaticSnapshot {
    type WatchStream = Pin<Box<dyn Stream<Item = Result<SnapshotChunk, Status>> + Send>>;

    async fn watch(
        &self,
        _request: Request<WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let stream = tokio_stream::once(Ok(SnapshotChunk {
            snapshot_json: r#"{"hash":"sha256:test"}"#.to_owned(),
        }));
        Ok(Response::new(Box::pin(stream)))
    }
}

/// Write a CA plus a signed identity into `dir` (`tls.crt`/`tls.key`/`ca.crt`).
fn write_identity(dir: &std::path::Path, sans: &[&str], ca: &(rcgen::Certificate, KeyPair)) {
    let key = KeyPair::generate().unwrap();
    let params =
        CertificateParams::new(sans.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap();
    let cert = params.signed_by(&key, &ca.0, &ca.1).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("tls.crt"), cert.pem()).unwrap();
    std::fs::write(dir.join("tls.key"), key.serialize_pem()).unwrap();
    std::fs::write(dir.join("ca.crt"), ca.0.pem()).unwrap();
}

fn make_ca() -> (rcgen::Certificate, KeyPair) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let cert = params.self_signed(&key).unwrap();
    (cert, key)
}

async fn spawn_mtls_server(server_dir: &std::path::Path) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tls = routed_proto::tls::server_mtls(server_dir).unwrap();
    tokio::spawn(async move {
        Server::builder()
            .tls_config(tls)
            .unwrap()
            .add_service(SnapshotServiceServer::new(StaticSnapshot))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    port
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("routed-mtls-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test]
async fn mtls_client_streams_and_anonymous_client_is_refused() {
    let ca = make_ca();
    let server_dir = tmp("server");
    let client_dir = tmp("client");
    write_identity(&server_dir, &["localhost"], &ca);
    write_identity(&client_dir, &["routed-client"], &ca);
    let port = spawn_mtls_server(&server_dir).await;

    // With a CA-signed client identity: the stream delivers the snapshot.
    let tls = routed_proto::tls::client_mtls(&client_dir, Some("localhost")).unwrap();
    let channel = Channel::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = SnapshotServiceClient::new(channel);
    let mut stream = client
        .watch(WatchRequest {
            client: "mtls-test".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let chunk = stream.message().await.unwrap().unwrap();
    assert!(chunk.snapshot_json.contains("sha256:test"));

    // Without a client identity: the handshake (or first call) is refused.
    let anon = tonic::transport::ClientTlsConfig::new()
        .ca_certificate(tonic::transport::Certificate::from_pem(
            std::fs::read(client_dir.join("ca.crt")).unwrap(),
        ))
        .domain_name("localhost");
    let attempt = Channel::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .tls_config(anon)
        .unwrap()
        .connect()
        .await;
    let refused = match attempt {
        Err(_) => true,
        Ok(channel) => SnapshotServiceClient::new(channel)
            .watch(WatchRequest {
                client: "anon".into(),
            })
            .await
            .is_err(),
    };
    assert!(refused, "server must require a client certificate");

    // A wrong CA on the client side must also fail (server not trusted).
    let other_ca = make_ca();
    let wrong_dir = tmp("wrong");
    write_identity(&wrong_dir, &["routed-client"], &other_ca);
    let tls = routed_proto::tls::client_mtls(&wrong_dir, Some("localhost")).unwrap();
    let attempt = Channel::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await;
    let refused = match attempt {
        Err(_) => true,
        Ok(channel) => SnapshotServiceClient::new(channel)
            .watch(WatchRequest {
                client: "wrong-ca".into(),
            })
            .await
            .is_err(),
    };
    assert!(refused, "client must reject an untrusted server");

    for d in [server_dir, client_dir, wrong_dir] {
        std::fs::remove_dir_all(d).ok();
    }
}

#[test]
fn missing_material_is_a_readable_error() {
    let missing = tmp("missing");
    let err = routed_proto::tls::server_mtls(&missing)
        .unwrap_err()
        .to_string();
    assert!(err.contains("tls.crt"), "{err}");
}
