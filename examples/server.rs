use anyhow::{Context, Error};
use base64::{engine::general_purpose, Engine as _};
use flume::{unbounded, Receiver, Sender};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use tracing::{debug, info};

use serde_json::{self, Value};
use tracing_subscriber::EnvFilter;

use veilid_core::tools::*;
use veilid_core::*;

use bevy_veilid::{
    config::config_callback,
    veilid::{
        create_api_and_connect, create_service_route_pin, network_loop, update_service_route_pin,
        ServiceKeys, CRYPTO_KIND,
    },
};

pub async fn server(
    veilid_storage_dir: std::path::PathBuf,
    service_keys: ServiceKeys,
    new_keys: bool,
) -> Result<(), Error> {
    // Create channels for communication with Veilid
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
            CryptoTyped::new(CRYPTO_KIND, service_keys.key_pair),
            key,
        )
    });

    let api = create_api_and_connect(update_callback, config_callback).await?;

    // Set up routing with privacy and encryption
    info!("creating a private route");
    let rc = api
        .routing_context()
        .with_privacy()?
        .with_sequencing(Sequencing::EnsureOrdered);

    let (_route_id, blob) = api
        .new_custom_private_route(
            &[CRYPTO_KIND],
            veilid_core::Stability::Reliable,
            veilid_core::Sequencing::EnsureOrdered,
        )
        .await
        .context("new_custom_private_route")?;

    info!("creating a private route, done");

    let route_blob: String = general_purpose::STANDARD_NO_PAD.encode(blob);
    info!("Publishing route on DHT: {}", route_blob);
    let route_blob = route_blob.as_bytes().to_vec();

    if new_keys {
        create_service_route_pin(rc.clone(), service_keys.key_pair.key, route_blob).await?;
    } else {
        // update_service_key_pin(rc, encoded_keys, service_keys).await?;
    }
    info!("Publishing route on DHT, done");

    network_loop(api, receiver, rc.clone(), on_app_message, on_app_call, None).await?;

    Ok(())
}

async fn on_app_message(
    target: Target,
    receiver: Receiver<VeilidUpdate>,
    rc: RoutingContext,
) -> Result<(), Error> {
    const K32: usize = 1024 * 32;
    let mut buf = [0u8; K32]; // 32 kb
    let tx = async {
        info!("Sending 32k random bytes");

        loop {
            let rand_string: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(K32)
                .map(char::from)
                .collect();

            buf[0..K32].copy_from_slice(&rand_string.as_bytes()[0..K32]);
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

async fn on_app_call(
    target: Target,
    receiver: Receiver<VeilidUpdate>,
    rc: RoutingContext,
) -> Result<(), Error> {
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
            buf[0..str_bytes.len()].copy_from_slice(&str_bytes);
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
    // Logging
    let default_env_filter = EnvFilter::try_from_default_env();
    let fallback_filter = EnvFilter::new("veilid_core=warn,info");
    let env_filter = default_env_filter.unwrap_or(fallback_filter);

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .init();

    let veilid_run_dir = tempfile::tempdir()?;
    let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)
        .unwrap()
        .value;

    let service_keys = ServiceKeys {
        key_pair,
        ..Default::default()
    };

    server(veilid_run_dir.into_path(), service_keys, true).await?;

    Ok(())
}
