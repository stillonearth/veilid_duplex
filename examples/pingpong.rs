#![feature(async_closure)]

use anyhow::Error;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::info;
use tracing_subscriber::EnvFilter;

use veilid_core::tools::*;
use veilid_core::*;

use veilid_duplex::veilid::{AppMessage, P2PApp};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    server: bool,
    #[arg(long)]
    client: Option<String>,
    #[arg(long, default_value_t = false)]
    verbose: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ChatMessage {
    count: u64,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();

    // Logging
    let default_env_filter = EnvFilter::try_from_default_env();
    // let fallback_filter = EnvFilter::new("veilid_core=warn,info");
    let fallback_filter = match args.verbose {
        true => EnvFilter::new("veilid_core=warn,info"),
        false => EnvFilter::new("veilid_core=error"),
    };
    let env_filter = default_env_filter.unwrap_or(fallback_filter);

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .init();

    let mut app = P2PApp::new().await?;

    if let Some(service_dht_str) = args.client {
        let service_dht_key = CryptoTyped::<CryptoKey>::from_str(&service_dht_str)?;

        let app_message: AppMessage<ChatMessage> = AppMessage {
            data: ChatMessage { count: 0 },
            dht_record: app.our_dht_key,
        };

        app.send_message(app_message, service_dht_key).await?;
    }
    info!("Starting network loop");
    let our_dht_key = app.our_dht_key;

    println!("Our DHT key: {}", our_dht_key);

    let api = app.api.clone();
    let routing_context = app.routing_context.clone();
    let routes = app.routes.clone();

    let on_message = async move |message: AppMessage<ChatMessage>| {
        println!("on_remote_call\treceived: {:?}\t", message.data);
        let mut message = message.clone();

        message.data.count += 1;

        let remote_dht_record = message.dht_record;
        message.dht_record = our_dht_key;

        loop {
            let mut routes = routes.lock().await;
            let target = routes
                .get_route(remote_dht_record, api.clone(), routing_context.clone())
                .await
                .unwrap();

            let result = message.send(&routing_context, target).await;
            if result.is_ok() {
                return;
            }
        }
    };

    app.network_loop(on_message).await?;
    Ok(())
}
