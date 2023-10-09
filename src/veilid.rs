use std::io;
use std::sync::Arc;

use anyhow::{Context, Error, Ok};
use base64::engine::general_purpose;
use base64::Engine;
use flume::{unbounded, Receiver, Sender};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::{
    spawn,
    sync::Mutex,
    time::{sleep, Duration},
};
use tracing::info;

use veilid_core::tools::*;
use veilid_core::{
    AttachmentState::{AttachedGood, AttachedStrong, AttachedWeak, FullyAttached, OverAttached},
    *,
};

use crate::config::config_callback;

pub const CRYPTO_KIND: CryptoKind = CRYPTO_KIND_VLD0;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct AppMessage<T: DeserializeOwned> {
    pub data: T,
    pub their_route: Vec<u8>,
    pub our_route: Vec<u8>,
}

impl<T: DeserializeOwned + Serialize> AppMessage<T> {
    pub fn swap_routes(&mut self) {
        std::mem::swap(&mut self.their_route, &mut self.our_route);
    }

    async fn send(
        &self,
        api: &VeilidAPI,
        routing_context: &RoutingContext,
    ) -> Result<Vec<u8>, Error> {
        let app_message_blob = serde_json::to_vec(self).unwrap();

        // Check if blob size > 32kb and fire an error
        if app_message_blob.len() > 32 * 1024 {
            return Err(io::Error::new(io::ErrorKind::Other, "Message size exceeds 32kb").into());
        }

        let route = api.import_remote_private_route(self.their_route.clone())?;
        let target = veilid_core::Target::PrivateRoute(route);

        info!("Sending message, target: {:?}", target.clone());

        routing_context
            .app_call(target.clone(), app_message_blob)
            .await
            .context("app_call")
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
    receiver: Receiver<VeilidUpdate>,
    our_route: Vec<u8>,
    their_route: Option<Vec<u8>>,
    service_key: CryptoTyped<CryptoKey>,
}

pub struct P2PAppRoutes {
    pub our_route: Vec<u8>,
    pub their_route: Vec<u8>,
}

impl P2PApp {
    async fn initialize() -> Result<(VeilidAPI, RoutingContext, Receiver<VeilidUpdate>), Error> {
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

        let veilid_storage_dir = tempfile::tempdir()?.path().to_path_buf();
        let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)
            .unwrap()
            .value;

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

        Ok((api, rc, receiver))
    }

    pub async fn new_host() -> Result<Self, Error> {
        let (api, routing_context, receiver) = Self::initialize().await?;

        info!("Creating a private route");
        let (_route_id, blob) = api
            .new_custom_private_route(
                &[CRYPTO_KIND],
                veilid_core::Stability::Reliable,
                veilid_core::Sequencing::PreferOrdered,
            )
            .await
            .context("new_custom_private_route")?;

        let service_key_blob = general_purpose::STANDARD_NO_PAD
            .encode(blob.clone())
            .as_bytes()
            .to_vec();

        let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)
            .unwrap()
            .value;

        create_service_route_pin(
            routing_context.clone(),
            key_pair.key,
            service_key_blob.clone(),
        )
        .await?;

        let service_key = api.import_remote_private_route(blob.clone())?;
        let service_key = CryptoTyped::<CryptoKey>::new(best_crypto_kind(), service_key);

        info!("Our own service key: {:?}", service_key);

        Ok(Self {
            api,
            routing_context,
            receiver,
            service_key,
            our_route: blob.clone(),
            their_route: None,
        })
    }

    pub async fn new_client(service_key: CryptoTyped<CryptoKey>) -> Result<Self, Error> {
        let (api, routing_context, receiver) = Self::initialize().await?;

        let (our_route_blob, their_route_blob) =
            P2PApp::get_service_route(api.clone(), routing_context.clone(), service_key).await?;

        Ok(Self {
            api,
            routing_context,
            receiver,
            service_key: service_key,
            our_route: our_route_blob,
            their_route: Some(their_route_blob),
        })
    }

    async fn get_service_route(
        api: VeilidAPI,
        routing_context: RoutingContext,
        service_key: CryptoTyped<CryptoKey>,
    ) -> Result<(Vec<u8>, Vec<u8>), Error> {
        info!("Looking up route on DHT: {}", service_key);
        let service_key = CryptoTyped::<CryptoKey>::from(service_key);
        let dht_desc = routing_context.open_dht_record(service_key, None).await?;

        let dht_val = routing_context
            .get_dht_value(*dht_desc.key(), 0, true)
            .await?
            .ok_or(io::Error::new(io::ErrorKind::Other, "DHT value not found"))?
            .data()
            .to_vec();
        info!("DHT value: {}", String::from_utf8(dht_val.clone()).unwrap());
        routing_context.close_dht_record(*dht_desc.key()).await?;

        let their_route_blob = general_purpose::STANDARD_NO_PAD
            .decode(String::from_utf8(dht_val)?)
            .unwrap();
        let their_route = api.import_remote_private_route(their_route_blob.clone())?;
        info!("Looking up route on DHT, done: {:?}", their_route);

        info!("Сreating a private route");
        let (_, our_route_blob) = api
            .new_custom_private_route(
                &[CRYPTO_KIND],
                veilid_core::Stability::Reliable,
                veilid_core::Sequencing::EnsureOrdered,
            )
            .await?;
        info!("Сreating a private route, done");

        Ok((our_route_blob, their_route_blob))
    }

    pub async fn app_call_host<T: DeserializeOwned>(&self, data: T) -> Result<Vec<u8>, Error>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let app_message = AppMessage {
            data,
            our_route: self.our_route.clone(),
            their_route: self.their_route.clone().unwrap(),
        };

        app_message.send(&self.api, &self.routing_context).await
    }

    async fn app_call_host_reply<T: DeserializeOwned + Serialize>(
        api: VeilidAPI,
        routing_context: RoutingContext,
        id: AlignedU64,
        app_message: AppMessage<T>,
    ) -> Result<(), Error> {
        let reply_result = api.app_call_reply(id, b"ACK".to_vec()).await;
        if reply_result.is_err() {
            return Err(reply_result.err().unwrap().into());
        }

        let app_message_result = app_message.send(&api, &routing_context).await;
        if app_message_result.is_err() {
            return Err(app_message_result.err().unwrap().into());
        }
        return Ok(());
    }

    pub async fn network_loop<F, T>(self, on_remote_call: F) -> Result<(), Error>
    where
        F: FnOnce(T) -> T + Send + Copy + 'static,
        T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
    {
        let reciever = self.receiver.clone();
        let api = self.api.clone();
        let routing_context = self.routing_context.clone();
        let app = Arc::new(Mutex::new(self));

        loop {
            let res = reciever.recv()?;
            let app = app.clone();
            let api = api.clone();
            let routing_context = routing_context.clone();

            match res {
                VeilidUpdate::AppCall(call) => {
                    info!("VeilidUpdate::AppCall");
                    let mut app_message = serde_json::from_slice::<AppMessage<T>>(call.message())?;

                    {
                        let mut app: AsyncMutexGuard<'_, P2PApp> = app.lock().await;
                        app.their_route = Some(app_message.our_route.clone());
                    }

                    app_message.data = on_remote_call(app_message.data);

                    // call.i

                    spawn(async move {
                        loop {
                            let app = app.lock().await;
                            let mut app_message = app_message.clone();
                            app_message.our_route = app.our_route.clone();
                            app_message.their_route = app.their_route.clone().unwrap();

                            let result = P2PApp::app_call_host_reply(
                                api.clone(),
                                routing_context.clone(),
                                call.id(),
                                app_message,
                            )
                            .await;

                            if result.is_ok() {
                                break;
                            }
                        }
                    });
                }
                VeilidUpdate::RouteChange(change) => {
                    let mut app = app.lock().await;

                    info!("VeilidUpdate::RouteChange, {:?}", change);
                    if app.their_route.is_none() {
                        continue;
                    }
                    let their_route: CryptoKey = app
                        .api
                        .import_remote_private_route(app.their_route.clone().unwrap())?;

                    for route in change.dead_remote_routes {
                        // update remote route if it's dead
                        if their_route == route {
                            let (our_route, their_route) = P2PApp::get_service_route(
                                app.api.clone(),
                                app.routing_context.clone(),
                                app.service_key,
                            )
                            .await?;

                            app.our_route = our_route;
                            app.their_route = Some(their_route);
                        }
                    }
                }
                _ => (),
            };
        }
    }
}
