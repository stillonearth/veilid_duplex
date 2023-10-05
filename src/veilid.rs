use std::io;

use anyhow::{Error, Ok};
use base64::engine::general_purpose;
use base64::Engine;
use flume::Receiver;
use futures_util::select;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct AppMessage<T: DeserializeOwned> {
    pub data: T,
    pub their_route: Vec<u8>,
    pub our_route: Vec<u8>,
}

impl<T: DeserializeOwned> AppMessage<T> {
    pub fn swap_routes(&mut self) {
        std::mem::swap(&mut self.their_route, &mut self.our_route);
    }
}

pub async fn wait_for_attached(api: &VeilidAPI) -> Result<(), Error> {
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

pub async fn wait_for_network_start(api: &VeilidAPI) -> Result<(), Error> {
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

pub async fn wait_for_public_internet_ready(api: &VeilidAPI) -> Result<(), Error> {
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

pub async fn create_service_route_pin(
    rc: RoutingContext,
    member_key: PublicKey,
    route: Vec<u8>,
) -> Result<(), Error> {
    let owner_subkey_count = 1;

    let rec = rc
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
        .await?;

    info!("DHT Key: {}", rec.key().clone());
    rc.set_dht_value(*rec.key(), 0, route).await?;
    rc.close_dht_record(*rec.key()).await?;

    Ok(())
}

pub async fn update_service_route_pin(
    rc: RoutingContext,
    route: Vec<u8>,
    dht_key: CryptoTyped<CryptoKey>,
    key_pair: KeyPair,
) -> Result<(), Error> {
    info!("DHT Key: {}", dht_key);
    let rec = rc.open_dht_record(dht_key, Some(key_pair)).await?;
    rc.set_dht_value(*rec.key(), 0, route).await?;
    rc.close_dht_record(*rec.key()).await?;

    Ok(())
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

pub struct P2PApp {
    api: VeilidAPI,
    routing_context: RoutingContext,
    our_route: Vec<u8>,
    receiver: Receiver<VeilidUpdate>,
}

impl P2PApp {
    pub fn new(
        api: VeilidAPI,
        routing_context: RoutingContext,
        our_route: Vec<u8>,
        receiver: Receiver<VeilidUpdate>,
    ) -> Self {
        Self {
            api,
            routing_context,
            our_route,
            receiver,
        }
    }

    pub async fn network_loop<F1, Fut1, F2, Fut2, T>(
        self,
        on_app_call: F1,
        on_app_message: F2,
        our_route: Vec<u8>,
    ) -> Result<(), Error>
    where
        F1: FnOnce(VeilidAPI, RoutingContext, AppMessage<T>) -> Fut1 + Send + Copy + 'static,
        Fut1: Future<Output = Result<(), Error>> + Send,
        F2: FnOnce(VeilidAPI, RoutingContext, AppMessage<T>) -> Fut2 + Send + Copy + 'static,
        Fut2: Future<Output = Result<(), Error>> + Send,
        F2: FnOnce(VeilidAPI, RoutingContext, AppMessage<T>) -> Fut2 + Send + Copy + 'static,
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        loop {
            let res = self.receiver.recv()?;

            let routing_context = self.routing_context.clone();
            let api = self.api.clone();
            let our_route = our_route.clone();

            match res {
                VeilidUpdate::AppMessage(msg) => {
                    info!("VeilidUpdate::AppMessage");
                    let mut app_message = serde_json::from_slice::<AppMessage<T>>(msg.message())?;
                    app_message.our_route = our_route;

                    spawn(async move {
                        let _ = on_app_message(api, routing_context, app_message).await;
                    });
                }
                VeilidUpdate::AppCall(call) => {
                    info!("VeilidUpdate::AppCall");
                    let mut app_message = serde_json::from_slice::<AppMessage<T>>(call.message())?;
                    app_message.our_route = our_route;

                    spawn(async move {
                        let _ = api.app_call_reply(call.id(), b"ACK".to_vec()).await;
                        let _ = on_app_call(api, routing_context, app_message).await;
                    });
                }
                VeilidUpdate::RouteChange(change) => {
                    info!("change: {:?}", change);
                }
                _ => (),
            };
        }
    }
}

pub async fn connect_to_service(
    api: VeilidAPI,
    routing_context: RoutingContext,
    service_key: TypedKey,
) -> Result<(Target, Vec<u8>, Vec<u8>), Error> {
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

    let route_blob = general_purpose::STANDARD_NO_PAD
        .decode(String::from_utf8(dht_val)?)
        .unwrap();
    let route = api.import_remote_private_route(route_blob.clone())?;
    info!("Looking up route on DHT, done: {:?}", route);

    info!("Сreating a private route");
    let (_route_id, our_route) = api
        .new_custom_private_route(
            &[CRYPTO_KIND],
            veilid_core::Stability::Reliable,
            veilid_core::Sequencing::EnsureOrdered,
        )
        .await?;
    info!("Сreating a private route, done");

    let target = veilid_core::Target::PrivateRoute(route);
    Ok((target, our_route, route_blob.clone()))
}
