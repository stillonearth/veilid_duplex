use std::hash::Hasher;
use std::io;

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use anyhow::{Context, Error, Ok};
use base64::engine::general_purpose;
use base64::Engine;
use fnv::FnvHasher;
use tracing::info;

#[cfg(not(target_arch = "wasm32"))]
use uuid::Uuid;

use veilid_core::tools::*;
use veilid_core::{
    AttachmentState::{AttachedGood, AttachedStrong, AttachedWeak, FullyAttached, OverAttached},
    *,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::config::config_callback;

pub const CRYPTO_KIND: CryptoKind = CRYPTO_KIND_VLD0;

pub async fn get_service_route_from_dht(
    api: VeilidAPI,
    routing_context: RoutingContext,
    service_key: CryptoTyped<CryptoKey>,
    force_refresh: bool,
) -> Result<(Target, CryptoKey), Error> {
    info!("Looking up route on DHT: {}", service_key);
    let service_key = service_key;
    let dht_desc = routing_context.open_dht_record(service_key, None).await?;

    let dht_val = routing_context
        .get_dht_value(*dht_desc.key(), 1, force_refresh)
        .await?
        .ok_or(io::Error::new(io::ErrorKind::Other, "DHT value not found"))?
        .data()
        .to_vec();

    routing_context.close_dht_record(*dht_desc.key()).await?;
    let their_route_blob = general_purpose::STANDARD_NO_PAD
        .decode(String::from_utf8(dht_val)?)
        .unwrap();
    let their_route = api.import_remote_private_route(their_route_blob.clone())?;
    info!("Looking up route on DHT, done: {:?}", their_route);

    let target = veilid_core::Target::PrivateRoute(their_route);

    Ok((target, their_route))
}

pub(crate) async fn create_private_route(api: VeilidAPI) -> Result<(CryptoKey, Vec<u8>), Error> {
    let (route_id, blob) = api
        .new_custom_private_route(
            &[CRYPTO_KIND],
            veilid_core::Stability::Reliable,
            veilid_core::Sequencing::PreferOrdered,
        )
        .await
        .context("new_custom_private_route")?;

    let blob = general_purpose::STANDARD_NO_PAD
        .encode(blob)
        .as_bytes()
        .to_vec();

    Ok((route_id, blob))
}

pub(crate) async fn wait_for_attached(api: &VeilidAPI) -> Result<(), Error> {
    info!("Awaiting attachment");
    loop {
        let state = api.get_state().await?;
        match state.attachment.state {
            AttachedWeak | AttachedGood | AttachedStrong | FullyAttached | OverAttached => {
                info!("Awaiting attachment, done");
                return Ok(());
            }
            _ => (),
        }
        sleep(1000).await;
    }
}

pub(crate) async fn wait_for_network_start(api: &VeilidAPI) -> Result<(), Error> {
    info!("awaiting network initialization");
    loop {
        let vs = api.get_state().await?;
        if vs.network.started && !vs.network.peers.is_empty() {
            info!(
                "Awaiting network initialization, done ({} peer(s))",
                vs.network.peers.len()
            );
            return Ok(());
        }
        sleep(100).await;
    }
}

pub(crate) async fn wait_for_public_internet_ready(api: &VeilidAPI) -> Result<(), Error> {
    info!("Awaiting 'public_internet_ready'");
    loop {
        let state = api.get_state().await?;
        if state.attachment.public_internet_ready {
            break;
        }
        sleep(5000).await;
    }

    info!("Awaiting 'public_internet_ready', done");
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn create_api_and_connect_with_keypair(
    update_callback: UpdateCallback,
    key_pair: KeyPair,
) -> Result<VeilidAPI, Error> {
    let id = Uuid::new_v4();
    let veilid_storage_dir = tempfile::tempdir()?
        .path()
        .join(Path::new(&id.to_string()))
        .to_path_buf();

    let config_callback = Arc::new(move |key| {
        config_callback(
            veilid_storage_dir.clone(),
            CryptoTyped::new(CRYPTO_KIND, key_pair),
            key,
        )
    });

    let api = api_startup(update_callback, config_callback).await?;

    // Network
    api.attach().await?;
    wait_for_network_start(&api).await?;
    wait_for_attached(&api).await?;
    wait_for_public_internet_ready(&api).await?;

    Ok(api)
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn create_api_and_connect(
    update_callback: UpdateCallback,
) -> Result<VeilidAPI, Error> {
    let config = r#"
    {
        "program_name":"veilid_duplex",
        "namespace":"",
        "capabilities":{
           "disable":[
              
           ]
        },
        "protected_store":{
           "allow_insecure_fallback":true,
           "always_use_insecure_storage":true,
           "directory":"",
           "delete":false,
           "device_encryption_key_password":"none"
        },
        "table_store":{
           "directory":"",
           "delete":false
        },
        "block_store":{
           "directory":"",
           "delete":false
        },
        "network":{
           "connection_initial_timeout_ms":2000,
           "connection_inactivity_timeout_ms":60000,
           "max_connections_per_ip4":32,
           "max_connections_per_ip6_prefix":32,
           "max_connections_per_ip6_prefix_size":56,
           "max_connection_frequency_per_min":128,
           "client_whitelist_timeout_ms":300000,
           "reverse_connection_receipt_time_ms":5000,
           "hole_punch_receipt_time_ms":5000,
           "network_key_password":"",
           "disable_capabilites":[
              
           ],
           "routing_table":{
              "node_id":[
                 
              ],
              "node_id_secret":[
                 
              ],
              "bootstrap":[
                 "ws://bootstrap.veilid.net:5150/ws"
              ],
              "limit_over_attached":64,
              "limit_fully_attached":32,
              "limit_attached_strong":16,
              "limit_attached_good":8,
              "limit_attached_weak":4
           },
           "rpc":{
              "concurrency":0,
              "queue_size":1024,
              "max_timestamp_behind_ms":10000,
              "max_timestamp_ahead_ms":10000,
              "timeout_ms":5000,
              "max_route_hop_count":4,
              "default_route_hop_count":1
           },
           "dht":{
              "max_find_node_count":20,
              "resolve_node_timeout_ms":10000,
              "resolve_node_count":1,
              "resolve_node_fanout":4,
              "get_value_timeout_ms":10000,
              "get_value_count":3,
              "get_value_fanout":4,
              "set_value_timeout_ms":10000,
              "set_value_count":5,
              "set_value_fanout":4,
              "min_peer_count":20,
              "min_peer_refresh_time_ms":60000,
              "validate_dial_info_receipt_time_ms":2000,
              "local_subkey_cache_size":128,
              "local_max_subkey_cache_memory_mb":256,
              "remote_subkey_cache_size":1024,
              "remote_max_records":65536,
              "remote_max_subkey_cache_memory_mb":256,
              "remote_max_storage_space_mb":0
           },
           "upnp":true,
           "detect_address_changes":true,
           "restricted_nat_retries":0,
           "tls":{
              "certificate_path":"",
              "private_key_path":"",
              "connection_initial_timeout_ms":2000
           },
           "application":{
              "https":{
                 "enabled":false,
                 "listen_address":":5150",
                 "path":"app"
              },
              "http":{
                 "enabled":false,
                 "listen_address":":5150",
                 "path":"app"
              }
           },
           "protocol":{
              "udp":{
                 "enabled":false,
                 "socket_pool_size":0,
                 "listen_address":""
              },
              "tcp":{
                 "connect":false,
                 "listen":false,
                 "max_connections":32,
                 "listen_address":""
              },
              "ws":{
                 "connect":true,
                 "listen":true,
                 "max_connections":16,
                 "listen_address":":5150",
                 "path":"ws"
              },
              "wss":{
                 "connect":true,
                 "listen":false,
                 "max_connections":16,
                 "listen_address":"",
                 "path":"ws"
              }
           }
        }
     }
    "#
    .to_string();

    let api = api_startup_json(update_callback, config.clone()).await?;

    // Network
    api.attach().await?;
    wait_for_network_start(&api).await?;
    wait_for_attached(&api).await?;
    wait_for_public_internet_ready(&api).await?;

    Ok(api)
}

pub(crate) async fn create_service_route_pin(
    rc: RoutingContext,
    member_key: PublicKey,
    route: Vec<u8>,
) -> Result<CryptoTyped<CryptoKey>, Error> {
    // let subkey_count = 1;

    let rec = rc
        .create_dht_record(
            DHTSchema::SMPL(DHTSchemaSMPL {
                o_cnt: 1,
                members: vec![DHTSchemaSMPLMember {
                    m_key: member_key,
                    m_cnt: 1,
                }],
            }),
            Some(CRYPTO_KIND),
        )
        .await?;

    let dht_key = *rec.key();

    info!("Setting DHT Key: {}", dht_key);
    rc.set_dht_value(*rec.key(), 0, route).await?;
    rc.close_dht_record(*rec.key()).await?;

    Ok(dht_key)
}

pub(crate) async fn update_service_route_pin(
    rc: RoutingContext,
    route: Vec<u8>,
    dht_key: CryptoTyped<CryptoKey>,
    key_pair: KeyPair,
) -> Result<(), Error> {
    info!("Updating DHT Key: {} ", dht_key);
    let rec = rc.open_dht_record(dht_key, Some(key_pair)).await?;
    rc.set_dht_value(*rec.key(), 1, route).await?;
    rc.close_dht_record(*rec.key()).await?;

    Ok(())
}

pub(crate) fn calculate_hash(data: &[u8]) -> u64 {
    let mut hasher = FnvHasher::default();
    hasher.write(data);
    hasher.finish()
}

pub fn crypto_key_from_str(dht_key: String) -> Result<CryptoTyped<CryptoKey>, VeilidAPIError> {
    CryptoTyped::<CryptoKey>::from_str(&dht_key)
}
