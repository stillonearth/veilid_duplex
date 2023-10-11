use std::io;

use anyhow::{Context, Error, Ok};
use base64::engine::general_purpose;
use base64::Engine;
use tokio::time::{sleep, Duration};
use tracing::info;

use veilid_core::tools::*;
use veilid_core::{
    AttachmentState::{AttachedGood, AttachedStrong, AttachedWeak, FullyAttached, OverAttached},
    *,
};

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
        .get_dht_value(*dht_desc.key(), 0, force_refresh)
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

    // info!("Сreating a private route");
    // let (our_route, _) = api
    //     .new_custom_private_route(
    //         &[best_crypto_kind()],
    //         veilid_core::Stability::Reliable,
    //         veilid_core::Sequencing::EnsureOrdered,
    //     )
    //     .await?;
    // info!("Сreating a private route, done");

    let target = veilid_core::Target::PrivateRoute(their_route);

    Ok((target, their_route))
}

pub(crate) async fn create_private_route(api: VeilidAPI) -> Result<(CryptoKey, Vec<u8>), Error> {
    let (route_id, blob) = api
        .new_custom_private_route(
            &[best_crypto_kind()],
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
        sleep(Duration::from_millis(100)).await;
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
        sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) async fn wait_for_public_internet_ready(api: &VeilidAPI) -> Result<(), Error> {
    info!("Awaiting 'public_internet_ready'");
    loop {
        let state = api.get_state().await?;
        if state.attachment.public_internet_ready {
            break;
        }
        sleep(Duration::from_secs(5)).await;
    }

    info!("Awaiting 'public_internet_ready', done");
    Ok(())
}

pub(crate) async fn create_api_and_connect(
    update_callback: UpdateCallback,
    config_callback: ConfigCallback,
) -> Result<VeilidAPI, Error> {
    let api = api_startup(update_callback, config_callback).await?;

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
    let subkey_count = 1;

    let rec = rc
        .create_dht_record(
            DHTSchema::SMPL(DHTSchemaSMPL {
                o_cnt: 1,
                members: vec![DHTSchemaSMPLMember {
                    m_key: member_key,
                    m_cnt: subkey_count,
                }],
            }),
            Some(best_crypto_kind()),
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
    rc.set_dht_value(*rec.key(), 0, route).await?;
    rc.close_dht_record(*rec.key()).await?;

    Ok(())
}
