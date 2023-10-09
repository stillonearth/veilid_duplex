use anyhow::Error;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::info;
use tracing_subscriber::EnvFilter;

use veilid_core::tools::*;
use veilid_core::*;

use bevy_veilid::veilid::P2PApp;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    server: bool,
    #[arg(long)]
    client: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ChatMessage {
    count: u64,
}

fn on_remote_call(chat_message: ChatMessage) -> ChatMessage {
    info!("on_remote_call::Received message: {:?}\t", chat_message);

    let mut chat_message = chat_message;
    chat_message.count += 1;

    chat_message
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();

    // Logging
    let default_env_filter = EnvFilter::try_from_default_env();
    let fallback_filter = EnvFilter::new("veilid_core=warn,info");
    let env_filter = default_env_filter.unwrap_or(fallback_filter);

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .init();

    let app: P2PApp;
    if args.server {
        app = P2PApp::new_host().await?;
    } else if let Some(service_dht_str) = args.client {
        let service_dht_key = CryptoTyped::<CryptoKey>::from_str(&service_dht_str)?;
        app = P2PApp::new_client(service_dht_key).await?;

        app.app_call_host(ChatMessage { count: 0 }).await?;
    } else {
        return Ok(());
    }

    info!("Starting network loop");
    app.network_loop(on_remote_call).await?;
    Ok(())
}
