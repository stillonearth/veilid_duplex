use anyhow::{Context, Error};
use base64::{engine::general_purpose, Engine as _};
use flume::{unbounded, Receiver, Sender};
use tracing::{debug, error, info};

use veilid_core::tools::*;
use veilid_core::*;

use bevy_veilid::config::config_callback;
use bevy_veilid::utils::{
    wait_for_attached, wait_for_network_start, wait_for_public_internet_ready,
};
use bevy_veilid::{BevyVeilidError, ServiceKeys, CRYPTO_KIND};

async fn pin_new_service_key(
    rc: RoutingContext,
    encoded_keys: Vec<u8>,
    service_keys: ServiceKeys,
) -> Result<(), Error> {
    let owner_subkey_count = 4;

    match rc
        .create_dht_record(
            DHTSchema::SMPL(DHTSchemaSMPL {
                o_cnt: owner_subkey_count,
                members: vec![DHTSchemaSMPLMember {
                    m_key: service_keys.public_key,
                    m_cnt: owner_subkey_count,
                }],
            }),
            Some(CRYPTO_KIND),
        )
        .await
    {
        Ok(rec) => {
            info!("DHT Key: {}", rec.key().clone());
            match rc.set_dht_value(rec.key().clone(), 0, encoded_keys).await {
                Ok(_) => (),
                Err(e) => {
                    return Err(e.into());
                }
            }
            rc.close_dht_record(rec.key().clone()).await?;
            return Ok(());
        }
        Err(e) => {
            return Err(e.into());
        }
    }
}

async fn update_service_key(
    rc: RoutingContext,
    encoded_keys: Vec<u8>,
    service_keys: ServiceKeys,
) -> Result<(), Error> {
    if let Some(dht_key) = service_keys.dht_key {
        info!("DHT Key: {}", dht_key);

        if let (Some(public), Some(private)) = (
            service_keys.dht_owner_key,
            service_keys.dht_owner_secret_key,
        ) {
            let key_pair = KeyPair::new(public, private);

            match rc.open_dht_record(dht_key, Some(key_pair)).await {
                Ok(rec) => {
                    match rc.set_dht_value(rec.key().clone(), 0, encoded_keys).await {
                        Ok(_) => (),
                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                    rc.close_dht_record(rec.key().clone()).await.unwrap();
                    return Ok(());
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
    }

    return Err(BevyVeilidError::new("DHT Key not found").into());
}

pub async fn server(
    veilid_storage_dir: std::path::PathBuf,
    service_keys: crate::ServiceKeys,
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
            CryptoTyped::new(
                CRYPTO_KIND,
                KeyPair::new(service_keys.public_key, service_keys.secret_key),
            ),
            key,
        )
    });

    let api = api_startup(update_callback, config_callback)
        .await
        .expect("startup failed");

    // Network
    api.attach().await?;
    wait_for_network_start(&api).await;
    wait_for_attached(&api).await;
    wait_for_public_internet_ready(&api).await?;

    // Set up routing with privacy and encryption
    info!("creating a private route");
    let rc = api
        .routing_context()
        .with_privacy()?
        .with_sequencing(Sequencing::EnsureOrdered);

    let (route_id, blob) = api
        .new_custom_private_route(
            &[CRYPTO_KIND],
            veilid_core::Stability::Reliable,
            veilid_core::Sequencing::EnsureOrdered,
        )
        .await
        .context("new_custom_private_route")?;

    info!("creating a private route, done");

    let encoded_keys: String = general_purpose::STANDARD_NO_PAD.encode(blob);
    info!("Publishing route on DHT: {}", encoded_keys);
    let encoded_keys = encoded_keys.as_bytes().to_vec();

    if new_keys {
        pin_new_service_key(rc, encoded_keys, service_keys).await?;
    } else {
        update_service_key(rc, encoded_keys, service_keys).await?;
    }
    info!("Publishing route on DHT, done");

    info!("waiting for remote route");
    loop {
        let first_msg = receiver.recv()?;
        match first_msg {
            VeilidUpdate::AppMessage(ref msg) => {
                info!(
                    "Received message: {}",
                    String::from_utf8(msg.message().to_vec()).unwrap().as_str()
                );
            }
            VeilidUpdate::AppCall(call) => {
                info!("waiting for remote route, received");

                let route_id = api
                    .import_remote_private_route(call.message().to_vec())
                    .unwrap();

                info!("waiting for remote route, imported");

                let target = veilid_core::Target::PrivateRoute(route_id);

                let _ = api.app_call_reply(call.id(), b"SERVER ACK".to_vec()).await;

                // let (receiver_clone, rc_clone, frames_clone) =
                //     (receiver.clone(), rc.clone(), frames.clone());
                tokio::spawn(async move {
                    // send something to client
                    // handle_request(target.clone(), receiver_clone, rc_clone, frames_clone).await;
                });
            }
            _ => (),
        };
    }
}



#[tokio::main]
async fn main() -> Result<(), Error> {
    let veilid_run_dir = tempfile::tempdir()?;

    let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)?;
    let service_keys = ServiceKeys {
        public_key: key_pair.value.key,
        secret_key: key_pair.value.secret,
        ..Default::default()
    };

    server(veilid_run_dir.into_path(), service_keys, true).await?;
    
    Ok(())
}