use std::collections::hash_map::Entry::Vacant;
use std::io;
use std::sync::Arc;

use anyhow::{Context, Error, Ok};

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
use veilid_core::*;

use crate::config::config_callback;
use crate::utils::*;

const SEND_ATTEMPTS: u8 = 64;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct AppMessage<T: DeserializeOwned> {
    pub data: T,
    pub dht_record: CryptoTyped<CryptoKey>,
}

#[derive(Clone)]
pub struct P2PApp {
    pub api: VeilidAPI,
    pub routing_context: RoutingContext,
    pub receiver: Receiver<VeilidUpdate>,
    pub our_route: CryptoKey,
    pub our_dht_key: CryptoTyped<CryptoKey>,
    pub key_pair: KeyPair,
}

impl<T: DeserializeOwned + Serialize> AppMessage<T> {
    async fn send(
        &self,
        _api: &VeilidAPI,
        routing_context: &RoutingContext,
        target: Target,
    ) -> Result<Vec<u8>, Error> {
        let app_message_blob = serde_json::to_vec(self).unwrap();

        // Check if blob size > 32kb and fire an error
        if app_message_blob.len() > 32 * 1024 {
            return Err(io::Error::new(io::ErrorKind::Other, "Message size exceeds 32kb").into());
        }

        info!(
            "Sending message, origin_dht: {:?}, target: {:?}",
            self.dht_record,
            target.clone()
        );

        routing_context
            .app_call(target.clone(), app_message_blob)
            .await
            .context("app_call")
    }
}

impl P2PApp {
    async fn initialize(
    ) -> Result<(VeilidAPI, RoutingContext, Receiver<VeilidUpdate>, KeyPair), Error> {
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
        let key_pair = veilid_core::Crypto::generate_keypair(best_crypto_kind())
            .unwrap()
            .value;

        let config_callback = Arc::new(move |key| {
            config_callback(
                veilid_storage_dir.clone(),
                CryptoTyped::new(best_crypto_kind(), key_pair),
                key,
            )
        });

        let api = create_api_and_connect(update_callback, config_callback).await?;

        // Set up routing with privacy and encryption
        let rc = api
            .routing_context()
            .with_privacy()?
            .with_sequencing(Sequencing::PreferOrdered);

        Ok((api, rc, receiver, key_pair))
    }

    pub async fn new() -> Result<Self, Error> {
        let (api, routing_context, receiver, key_pair) = Self::initialize().await?;

        let (our_route, our_route_blob) = create_private_route(api.clone()).await?;
        info!("our route: {}", our_route);
        let our_dht_key = create_service_route_pin(
            routing_context.clone(),
            key_pair.key,
            our_route_blob.clone(),
        )
        .await?;

        Ok(Self {
            api,
            routing_context,
            receiver,
            key_pair,
            our_route,
            our_dht_key,
        })
    }

    pub async fn send_app_message<T: DeserializeOwned>(
        &self,
        data: T,
        target: Target,
    ) -> Result<Vec<u8>, Error>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let app_message = AppMessage {
            data,
            dht_record: self.our_dht_key,
        };

        info!("app_call_host, dht_record: {:}", app_message.dht_record);

        app_message
            .send(&self.api, &self.routing_context, target)
            .await
    }

    pub async fn network_loop<F1, F2, T>(
        mut self,
        on_app_message: F1,
        on_communication_halt: F2,
    ) -> Result<(), Error>
    where
        F1: FnOnce(T) -> T + Send + Copy + 'static,
        F2: FnOnce() + Send + Copy + 'static,
        T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
    {
        let reciever = self.receiver;
        let api = self.api;
        let routing_context = self.routing_context;
        let routes = Arc::new(Mutex::new(HashMap::<CryptoKey, Target>::new()));

        loop {
            let res = reciever.recv()?;
            let api = api.clone();
            let routing_context = routing_context.clone();
            let routes = routes.clone();

            match res {
                VeilidUpdate::AppCall(call) => {
                    info!("VeilidUpdate::AppMessage");
                    let mut app_message = serde_json::from_slice::<AppMessage<T>>(call.message())?;

                    app_message.data = on_app_message(app_message.data);
                    let remote_dht_record = app_message.dht_record;
                    app_message.dht_record = self.our_dht_key;
                    let on_halt = on_communication_halt;

                    spawn(async move {
                        let result = api.app_call_reply(call.id(), b"ACK".to_vec()).await;
                        if result.is_err() {
                            info!("Unable to send ACK");
                            return;
                        }

                        for attempt_n in 0..SEND_ATTEMPTS {
                            let mut routes = routes.lock().await;
                            if let Vacant(e) = routes.entry(app_message.dht_record.value) {
                                let result = get_service_route_from_dht(
                                    api.clone(),
                                    routing_context.clone(),
                                    remote_dht_record,
                                    true,
                                )
                                .await;
                                if result.is_err() {
                                    info!("Unable to get route from dht, waiting 500ms");
                                    info!("error: {:?}", result.err());
                                    sleep(Duration::from_millis(500)).await;
                                    continue;
                                }
                                let (target, _, _) = result.unwrap();
                                e.insert(target);
                            }
                            let target = routes.get(&app_message.dht_record.value).unwrap().clone();

                            let result = app_message.send(&api, &routing_context, target).await;
                            if result.is_ok() {
                                break;
                            } else if result.is_err() {
                                info!("Unable to send message, sleeping 500ms");
                                sleep(Duration::from_millis(500)).await;
                                continue;
                            }

                            if attempt_n == (SEND_ATTEMPTS - 1) {
                                on_halt();
                                // on_communication_halt();
                            }
                        }

                        // info!("Unable to send message after 32 tries");
                    });
                }
                VeilidUpdate::RouteChange(change) => {
                    info!("VeilidUpdate::RouteChange, {:?}", change);

                    let our_route = self.our_route;
                    for route in change.dead_routes.iter().filter(|r| **r == our_route) {
                        let (our_route, our_route_blob) = create_private_route(api.clone()).await?;
                        self.our_route = our_route;
                        update_service_route_pin(
                            routing_context.clone(),
                            our_route_blob,
                            self.our_dht_key,
                            self.key_pair,
                        )
                        .await?;

                        info!("DHT value for route {:} changed", route);
                    }

                    let mut routes = routes.lock().await;
                    for route in change.dead_remote_routes {
                        if routes.contains_key(&route) {
                            routes.remove(&route);
                            info!("DHT value for route {:} removed", route);
                        }
                    }
                }
                _ => (),
            };
        }
    }
}
