// SPDX-License-Identifier: Apache-2.0
//! `routed-mockgateway` binary for kind e2e.

use std::sync::Arc;

use clap::Parser;

/// Mock gateway.
#[derive(Parser, Debug)]
#[command(name = "routed-mockgateway", about)]
struct Cli {
    /// Listen address.
    #[arg(long, env = "MOCK_LISTEN_ADDR", default_value = "0.0.0.0:4000")]
    listen_addr: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    routed_telemetry::init_tracing();
    let cli = Cli::parse();
    let state = Arc::new(routed_mockgateway::MockState::default());
    let listener = tokio::net::TcpListener::bind(&cli.listen_addr).await?;
    tracing::info!(addr = %cli.listen_addr, "mock gateway listening");
    axum::serve(listener, routed_mockgateway::app(state)).await?;
    Ok(())
}
