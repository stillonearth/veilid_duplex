use std::io;

use anyhow::Error;
use base64::engine::general_purpose;
use base64::Engine;
use flume::Receiver;
use tokio::{
    spawn,
    time::{sleep, Duration},
};
use tracing::info;

use veilid_core::tools::*;
use veilid_core::{
    AttachmentState::{AttachedGood, AttachedStrong, AttachedWeak, FullyAttached, OverAttached},
    *,
};

pub const CRYPTO_KIND: CryptoKind = CRYPTO_KIND_VLD0;

#[derive(Debug, Default)]
pub struct ServiceKeys {
    pub key_pair: KeyPair,
    pub dht_key: Option<CryptoTyped<CryptoKey>>,
    pub dht_owner_key: Option<PublicKey>,
    pub dht_owner_secret_key: Option<SecretKey>,
}

pub async fn wait_for_attached(api: &VeilidAPI) -> Result<(), Error> {
    info!("Awaiting attachment");
    loop {
        if let Ok(state) = api.get_state().await {
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
}

pub async fn wait_for_network_start(api: &VeilidAPI) -> Result<(), Error> {
    info!("awaiting network initialization");
    loop {
        match api.get_state().await {
            Ok(vs) => {
                if vs.network.started && !vs.network.peers.is_empty() {
                    info!(
                        "Awaiting network initialization, done ({} peer(s))",
                        vs.network.peers.len()
                    );
                    return Ok(());
                }
            }
            Err(e) => {
                return Err(e.into());
            }
        }

        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_for_public_internet_ready(api: &VeilidAPI) -> Result<(), Error> {
    info!("Awaiting 'public_internet_ready'");
    loop {
        let state = api.get_state().await;
        if let Ok(state) = state {
            if state.attachment.public_internet_ready {
                break;
            }
        }

        sleep(Duration::from_secs(5)).await;
    }

    info!("Awaiting 'public_internet_ready', done");
    Ok(())
}

pub async fn create_service_route_pin(
    rc: RoutingContext,
    member_key: PublicKey,
    route: Vec<u8>,
) -> Result<(), Error> {
    let owner_subkey_count = 4;

    match rc
        .create_dht_record(
            DHTSchema::SMPL(DHTSchemaSMPL {
                o_cnt: owner_subkey_count,
                members: vec![DHTSchemaSMPLMember {
                    m_key: member_key,
                    m_cnt: owner_subkey_count,
                }],
            }),
            Some(CRYPTO_KIND),
        )
        .await
    {
        Ok(rec) => {
            info!("DHT Key: {}", rec.key().clone());
            match rc.set_dht_value(*rec.key(), 0, route).await {
                Ok(_) => (),
                Err(e) => {
                    return Err(e.into());
                }
            }
            rc.close_dht_record(*rec.key()).await?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn update_service_route_pin(
    rc: RoutingContext,
    encoded_keys: Vec<u8>,
    dht_key: CryptoTyped<CryptoKey>,
    key_pair: KeyPair,
) -> Result<(), Error> {
    info!("DHT Key: {}", dht_key);

    match rc.open_dht_record(dht_key, Some(key_pair)).await {
        Ok(rec) => {
            match rc.set_dht_value(*rec.key(), 0, encoded_keys).await {
                Ok(_) => (),
                Err(e) => {
                    return Err(e.into());
                }
            }
            rc.close_dht_record(*rec.key()).await.unwrap();
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn create_api_and_connect(
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

pub async fn network_loop<F1, Fut1, F2, Fut2>(
    api: VeilidAPI,
    receiver: Receiver<VeilidUpdate>,
    routing_context: RoutingContext,
    on_app_call: F1,
    on_app_message: F2,
) -> Result<(), Error>
where
    F1: FnOnce(VeilidAPI, RoutingContext, Box<VeilidAppCall>) -> Fut1 + Send + Copy + 'static,
    Fut1: Future<Output = Result<(), Error>> + Send,
    F2: FnOnce(VeilidAPI, RoutingContext, Box<VeilidAppMessage>) -> Fut2 + Send + Copy + 'static,
    Fut2: Future<Output = Result<(), Error>> + Send,
{
    loop {
        let first_msg = receiver.recv()?;
        match first_msg {
            VeilidUpdate::AppMessage(msg) => {
                let routing_context = routing_context.clone();
                let api = api.clone();
                spawn(async move {
                    let _ = on_app_message(api, routing_context, msg.clone()).await;
                });
            }
            VeilidUpdate::AppCall(call) => {
                let routing_context = routing_context.clone();
                let api = api.clone();
                spawn(async move {
                    let _ = on_app_call(api, routing_context, call.clone()).await;
                });
            }
            _ => (),
        };
    }
}

pub async fn connect_to_route(
    api: VeilidAPI,
    routing_context: RoutingContext,
    service_key: TypedKey,
) -> Result<(Target, Vec<u8>), Error> {
    info!("Looking up route on DHT: {}", service_key);
    let dht_desc = routing_context.open_dht_record(service_key, None).await?;
    let dht_val = routing_context
        .get_dht_value(*dht_desc.key(), 0, true)
        .await?
        .ok_or(io::Error::new(io::ErrorKind::Other, "DHT value not found"))?
        .data()
        .to_vec();
    info!("DHT value: {}", String::from_utf8(dht_val.clone()).unwrap());
    routing_context.close_dht_record(*dht_desc.key()).await?;

    let their_route = general_purpose::STANDARD_NO_PAD
        .decode(String::from_utf8(dht_val)?)
        .unwrap();
    let their_route_id = api.import_remote_private_route(their_route)?;
    info!("Looking up route on DHT, done: {:?}", their_route_id);

    info!("Сreating a private route");
    let (_route_id, our_route) = api
        .new_custom_private_route(
            &[CRYPTO_KIND],
            veilid_core::Stability::Reliable,
            veilid_core::Sequencing::EnsureOrdered,
        )
        .await?;
    info!("Сreating a private route, done");

    let target = veilid_core::Target::PrivateRoute(their_route_id);
    Ok((target, our_route))
}
