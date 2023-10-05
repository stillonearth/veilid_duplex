use anyhow::{Context, Error};
use base64::{engine::general_purpose, Engine as _};
use clap::Parser;
use flume::{unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use tracing::info;
use tracing_subscriber::EnvFilter;

use veilid_core::tools::*;
use veilid_core::*;

use bevy_veilid::{
    config::config_callback,
    veilid::{
        connect_to_service, create_api_and_connect, create_service_route_pin, AppMessage, P2PApp,
        CRYPTO_KIND,
    },
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    server: bool,
    #[arg(long)]
    client: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ChatMessage {
    count: u64,
}

async fn on_app_message(
    api: VeilidAPI,
    rc: RoutingContext,
    mut app_message: AppMessage<ChatMessage>,
) -> Result<(), Error> {
    info!("on_app_message::Received message: {:?}\t", app_message.data);

    let route = api.import_remote_private_route(app_message.their_route.clone())?;
    let target = veilid_core::Target::PrivateRoute(route.clone());
    app_message.swap_routes();
    app_message.data.count += 1;

    info!(
        "on_app_message::Sending message: {:?}\ttarget: {:?}",
        app_message.data, target
    );

    // serialize message

    let app_message = serde_json::to_vec(&app_message).unwrap();

    let result = rc
        .app_call(target.clone(), app_message.to_vec())
        .await
        .context("app_message");

    Ok(())
}

async fn on_app_call(
    api: VeilidAPI,
    rc: RoutingContext,
    mut app_message: AppMessage<ChatMessage>,
) -> Result<(), Error> {
    info!("on_app_call::Received message: {:?}\t", app_message.data);

    let their_route_id = api.import_remote_private_route(app_message.their_route.clone())?;
    let their_target = veilid_core::Target::PrivateRoute(their_route_id);

    app_message.swap_routes();
    app_message.data.count += 1;

    info!(
        "on_app_call::Sending message: {:?}\their_target: {:?}",
        app_message.data, their_target
    );

    // serialize message
    let app_message = serde_json::to_vec(&app_message)?;

    let result = rc
        .app_message(their_target.clone(), app_message)
        .await
        .context("app_message");
    info!("sending response, done");

    result
}

async fn start_pingpong(
    target: Target,
    rc: RoutingContext,
    our_route: Vec<u8>,
    their_route: Vec<u8>,
) -> Result<(), Error> {
    let mut message = AppMessage {
        data: ChatMessage { count: 0 },
        our_route: our_route.clone(),
        their_route: their_route.clone(),
    };
    message.swap_routes();
    let message = serde_json::to_vec(&message).unwrap();

    info!("Initiaing message exchange, target: {:?}", target.clone());

    let response = rc
        .app_call(target.clone(), message)
        .await
        .context("app_call")?;

    info!(
        "Received response: {}",
        String::from_utf8(response).unwrap()
    );

    Ok(())
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

    let veilid_storage_dir = tempfile::tempdir()?.path().to_path_buf();
    let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)
        .unwrap()
        .value;

    let (sender, receiver): (
        Sender<veilid_core::VeilidUpdate>,
        Receiver<veilid_core::VeilidUpdate>,
    ) = unbounded();

    // Create VeilidCore setup
    let update_callback = Arc::new(move |change: veilid_core::VeilidUpdate| {
        if let Err(e) = sender.send(change) {
            // Don't log here, as that loops the update callback in some cases and will deadlock
            let change = e.into_inner();
            info!("error sending veilid update callback: {:?}", change);
        }
    });

    let config_callback = Arc::new(move |key| {
        config_callback(
            veilid_storage_dir.clone(),
            CryptoTyped::new(CRYPTO_KIND, key_pair),
            key,
        )
    });

    let api = create_api_and_connect(update_callback, config_callback).await?;

    // Set up routing with privacy and encryption
    let rc = api
        .routing_context()
        .with_privacy()?
        .with_sequencing(Sequencing::PreferOrdered);

    let their_route: Vec<u8>;
    let our_route: Vec<u8>;
    let target: Target;

    if args.server {
        info!("Creating a private route");
        let (_route_id, blob) = api
            .new_custom_private_route(
                &[CRYPTO_KIND],
                veilid_core::Stability::Reliable,
                veilid_core::Sequencing::PreferOrdered,
            )
            .await
            .context("new_custom_private_route")?;

        info!("Creating a private route, done");

        let service_key = general_purpose::STANDARD_NO_PAD
            .encode(blob.clone())
            .as_bytes()
            .to_vec();
        our_route = blob.clone();

        create_service_route_pin(rc.clone(), key_pair.key, service_key.clone()).await?;
    } else if let Some(service_dht_str) = args.client {
        let service_dht_key = CryptoTyped::<CryptoKey>::from_str(&service_dht_str)?;

        (target, our_route, their_route) =
            connect_to_service(api.clone(), rc.clone(), service_dht_key).await?;

        start_pingpong(target, rc.clone(), our_route.clone(), their_route).await?;
    } else {
        return Ok(());
    }

    info!("starting network loop");

    let app = P2PApp::new(api.clone(), rc.clone(), our_route.clone(), receiver);
    app.network_loop(on_app_call, on_app_message, our_route.clone())
        .await?;

    api.shutdown().await;
    return Ok(());
}
