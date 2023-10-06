use std::io;

use anyhow::{Context, Error, Ok};
use base64::engine::general_purpose;
use base64::Engine;
use flume::{unbounded, Receiver, Sender};
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

use crate::config::config_callback;

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
    receiver: Receiver<VeilidUpdate>,
    our_route: Option<Vec<u8>>,
    their_route: Option<Vec<u8>>,
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
        let mut app = Self {
            api,
            routing_context,
            receiver,
            our_route: None,
            their_route: None,
        };

        app.start_host().await?;

        Ok(app)
    }

    pub async fn new_client(service_key: CryptoTyped<CryptoKey>) -> Result<Self, Error> {
        let (api, routing_context, receiver) = Self::initialize().await?;
        let mut app = Self {
            api,
            routing_context,
            receiver,
            our_route: None,
            their_route: None,
        };

        app.start_client(service_key).await?;

        Ok(app)
    }

    async fn start_host(&mut self) -> Result<(), Error> {
        info!("Creating a private route");
        let (_route_id, blob) = self
            .api
            .new_custom_private_route(
                &[CRYPTO_KIND],
                veilid_core::Stability::Reliable,
                veilid_core::Sequencing::PreferOrdered,
            )
            .await
            .context("new_custom_private_route")?;

        info!("Creating a private route, done");

        let service_key = general_purpose::STANDARD_NO_PAD
            .encode(blob.clone())
            .as_bytes()
            .to_vec();
        self.our_route = Some(blob.clone());

        let key_pair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)
            .unwrap()
            .value;

        create_service_route_pin(
            self.routing_context.clone(),
            key_pair.key,
            service_key.clone(),
        )
        .await?;

        Ok(())
    }

    async fn start_client(&mut self, service_key: CryptoTyped<CryptoKey>) -> Result<(), Error> {
        info!("Looking up route on DHT: {}", service_key);
        let dht_desc = self
            .routing_context
            .open_dht_record(service_key, None)
            .await?;

        let dht_val = self
            .routing_context
            .get_dht_value(*dht_desc.key(), 0, true)
            .await?
            .ok_or(io::Error::new(io::ErrorKind::Other, "DHT value not found"))?
            .data()
            .to_vec();
        info!("DHT value: {}", String::from_utf8(dht_val.clone()).unwrap());
        self.routing_context
            .close_dht_record(*dht_desc.key())
            .await?;

        let their_route_blob = general_purpose::STANDARD_NO_PAD
            .decode(String::from_utf8(dht_val)?)
            .unwrap();
        let their_route = self
            .api
            .import_remote_private_route(their_route_blob.clone())?;
        info!("Looking up route on DHT, done: {:?}", their_route);

        info!("Сreating a private route");
        let (_, our_route_blob) = self
            .api
            .new_custom_private_route(
                &[CRYPTO_KIND],
                veilid_core::Stability::Reliable,
                veilid_core::Sequencing::EnsureOrdered,
            )
            .await?;
        info!("Сreating a private route, done");

        self.our_route = Some(our_route_blob);
        self.their_route = Some(their_route_blob);

        Ok(())
    }

    pub async fn call_host<T: DeserializeOwned>(&self, data: T) -> Result<Vec<u8>, Error>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let app_message = AppMessage {
            data,
            our_route: self.our_route.clone().unwrap(),
            their_route: self.their_route.clone().unwrap(),
        };
        // message.swap_routes();
        let app_message_blob = serde_json::to_vec(&app_message).unwrap();

        let route = self
            .api
            .import_remote_private_route(app_message.their_route.clone())?;
        let target = veilid_core::Target::PrivateRoute(route);

        info!("Initiaing message exchange, target: {:?}", target.clone());

        self.routing_context
            .app_call(target.clone(), app_message_blob)
            .await
            .context("app_call")
    }

    pub async fn network_loop<F, T>(self, on_remote_call: F) -> Result<(), Error>
    where
        F: FnOnce(T) -> T + Send + Copy + 'static,
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        loop {
            let res = self.receiver.recv()?;
            let api = self.api.clone();
            let routing_context = self.routing_context.clone();

            match res {
                VeilidUpdate::AppCall(call) => {
                    info!("VeilidUpdate::AppCall");
                    let mut app_message = serde_json::from_slice::<AppMessage<T>>(call.message())?;
                    app_message.their_route = self.our_route.clone().unwrap();
                    app_message.swap_routes();
                    app_message.data = on_remote_call(app_message.data);

                    spawn(async move {
                        let _ = api.app_call_reply(call.id(), b"ACK".to_vec()).await;

                        let app_message_blob = serde_json::to_vec(&app_message).unwrap();

                        let route: CryptoKey = api
                            .import_remote_private_route(app_message.their_route.clone())
                            .unwrap();
                        let target = veilid_core::Target::PrivateRoute(route);

                        info!("VeilidUpdate::AppCall, targer: {:?}", target.clone());

                        let _ = routing_context
                            .app_call(target.clone(), app_message_blob)
                            .await
                            .context("app_call");
                    });
                }
                VeilidUpdate::RouteChange(change) => {
                    info!("VeilidUpdate::RouteChange: {:?}", change);
                }
                _ => (),
            };
        }
    }
}
