use std::io;

use anyhow::{Context, Error};
use base64::engine::general_purpose;
use base64::Engine;
use clap::Parser;
use flume::{unbounded, Receiver, Sender};
use futures_util::select;
use tokio::io::AsyncWriteExt;
use tracing::info;
use tracing_subscriber::EnvFilter;
use veilid_core::{api_startup, CryptoTyped, KeyPair, RoutingContext, Target, VeilidUpdate};
use veilid_core::{tools::*, CryptoKey, Sequencing};

use bevy_veilid::{
    config::config_callback,
    veilid::{
        connect_to_route, create_api_and_connect, network_loop, update_service_route_pin,
        ServiceKeys, CRYPTO_KIND,
    },
};

/// Like netcat but with Veilid
#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    server: bool,
    #[arg(long)]
    new_keys: bool,
    #[arg(long)]
    client: Option<String>,
}

pub async fn client(
    veilid_storage_dir: std::path::PathBuf,
    key_pair: CryptoTyped<KeyPair>,
    service_dht_key: CryptoTyped<CryptoKey>,
) -> Result<(), Error> {
    let mut stdout = tokio::io::stdout();

    // flume -- channels (queues/pipes) , veilid sender and recievers
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
            CryptoTyped::new(CRYPTO_KIND, key_pair.value),
            key,
        )
    });

    let api = create_api_and_connect(update_callback, config_callback).await?;

    // Set up routing with privacy and encryption
    info!("creating a private route");
    let routing_context = api
        .routing_context()
        .with_privacy()?
        .with_sequencing(Sequencing::EnsureOrdered);

    let (target, our_route) =
        connect_to_route(api.clone(), routing_context.clone(), service_dht_key).await?;

    // network_loop(
    //     api.clone(),
    //     receiver,
    //     routing_context.clone(),
    //     on_app_message,
    //     on_app_call,
    //     Some(target),
    // )
    // .await?;

    let response = routing_context
        .app_call(target.clone(), our_route.clone())
        .await
        .context("app_call")?;

    info!(
        "Received response: {}",
        String::from_utf8(response).unwrap()
    );

    let rx = async {
        loop {
            select! {
                res = receiver.recv_async() => {
                    if let Ok(change) = res {
                        match change {
                        VeilidUpdate::AppMessage(msg) => {
                                let r = stdout.write(&msg.message()).await.unwrap();
                                if r == 0 {
                                    break;
                                }
                            },
                            _ => ()
                        }
                    } else {
                        break;
                    }
                }
            };
        }
        Ok::<_, Error>(())
    };

    //let (tx_done, rx_done) = tokio::join!(tx, rx);

    let _ = rx.await;

    api.shutdown().await;
    return Ok(());
}

async fn on_app_call(
    target: Target,
    receiver: Receiver<VeilidUpdate>,
    rc: RoutingContext,
) -> Result<(), Error> {
    Ok(())
}

async fn on_app_message(
    target: Target,
    receiver: Receiver<VeilidUpdate>,
    rc: RoutingContext,
) -> Result<(), Error> {
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

    let veilid_run_dir = tempfile::tempdir()?;

    if let Some(service_dht_str) = args.client {
        let service_dht_key = CryptoTyped::<CryptoKey>::from_str(&service_dht_str)?;
        client(
            veilid_run_dir.into_path(),
            veilid_core::Crypto::generate_keypair(CRYPTO_KIND)?,
            service_dht_key,
        )
        .await?;
    }

    // let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)?;
    // let service_keys = ServiceKeys {
    //     public_key: key_pair.value.key,
    //     secret_key: key_pair.value.secret,
    //     ..Default::default()
    // };

    // client(veilid_run_dir.into_path(), service_keys, true).await?;

    Ok(())
}
