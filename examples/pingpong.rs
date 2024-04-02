#![feature(async_closure)]

use anyhow::Error;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::info;

use veilid_core::tools::*;
use veilid_core::*;

use veilid_duplex::veilid::{AppMessage, VeilidDuplex};

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

    let mut app = VeilidDuplex::new().await?;

    if let Some(service_dht_str) = args.client {
        let service_dht_key = CryptoTyped::<CryptoKey>::from_str(&service_dht_str)?;

        let app_message: AppMessage<ChatMessage> = AppMessage {
            data: ChatMessage { count: 0 },
            dht_record: app.our_dht_key,
            uuid: "".to_string(),
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
            info!("Failed to send message, sleeping 1 second");
            sleep(1000).await;
        }
    };

    app.network_loop(on_message).await?;
    app.api.shutdown().await;
    Ok(())
}
