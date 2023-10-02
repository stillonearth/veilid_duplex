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

pub async fn pin_new_service_key(
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

pub async fn update_service_key(
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