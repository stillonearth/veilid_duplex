use anyhow::{Context, Error};
use base64::{engine::general_purpose, Engine as _};
use clap::Parser;
use flume::{unbounded, Receiver, Sender};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

use veilid_core::tools::*;
use veilid_core::*;

use bevy_veilid::{
    config::config_callback,
    veilid::{
        connect_to_route, create_api_and_connect, create_service_route_pin, network_loop,
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

async fn on_app_message(
    api: VeilidAPI,
    rc: RoutingContext,
    msg: Box<VeilidAppMessage>,
) -> Result<(), Error> {
    info!(
        "Received message: {}",
        String::from_utf8(msg.message().to_vec()).unwrap().as_str()
    );
    Ok(())
}

async fn on_app_call(
    api: VeilidAPI,
    rc: RoutingContext,
    call: Box<VeilidAppCall>,
) -> Result<(), Error> {
    let route_id = api
        .import_remote_private_route(call.message().to_vec())
        .unwrap();

    info!("Remote route imported: {:?}", route_id);

    let target = veilid_core::Target::PrivateRoute(route_id);

    let _ = api.app_call_reply(call.id(), b"SERVER ACK".to_vec()).await;

    const K32: usize = 1024 * 32;
    let mut buf = [0u8; K32]; // 32 kb
    let tx = async {
        info!("Sending 32k random bytes");

        loop {
            let rand_string: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(128)
                .map(char::from)
                .collect();

            let str_bytes = rand_string.as_bytes();
            buf[0..str_bytes.len()].copy_from_slice(str_bytes);
            debug!("SENDING FRAME");
            let result = rc
                .app_message(target.clone(), buf.to_vec())
                .await
                .context("app_message");

            if result.is_err() {
                return result.err();
            }

            tokio::time::sleep(core::time::Duration::from_millis(500)).await;
        }
    };

    let err = tx.await;
    if err.is_some() {
        return Err(err.unwrap());
    }

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
        .with_sequencing(Sequencing::EnsureOrdered);

    if args.server {
        info!("Creating a private route");
        let (_route_id, blob) = api
            .new_custom_private_route(
                &[CRYPTO_KIND],
                veilid_core::Stability::Reliable,
                veilid_core::Sequencing::EnsureOrdered,
            )
            .await
            .context("new_custom_private_route")?;

        info!("Creating a private route, done");

        let route_blob: String = general_purpose::STANDARD_NO_PAD.encode(blob);
        info!("Publishing route on DHT: {}", route_blob);
        let route_blob = route_blob.as_bytes().to_vec();
        create_service_route_pin(rc.clone(), key_pair.key, route_blob).await?;
        info!("Publishing route on DHT, done");
    } else if let Some(service_dht_str) = args.client {
        let service_dht_key = CryptoTyped::<CryptoKey>::from_str(&service_dht_str)?;

        info!("Calling service");
        let (target, our_route) =
            connect_to_route(api.clone(), rc.clone(), service_dht_key).await?;

        let response = rc
            .app_call(target.clone(), our_route.clone())
            .await
            .context("app_call")?;

        info!(
            "Received response: {}",
            String::from_utf8(response).unwrap()
        );
    } else {
    }

    network_loop(api, receiver, rc.clone(), on_app_call, on_app_message).await?;

    Ok(())
}
