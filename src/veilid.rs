use std::collections::hash_map::Entry::Vacant;
use std::future::Future;
use std::io;
use std::path::Path;
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
use uuid::Uuid;

use veilid_core::tools::*;
use veilid_core::*;

use crate::config::config_callback;
use crate::utils::*;

const SEND_ATTEMPTS: u16 = 1024;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct AppMessage<T: DeserializeOwned> {
    pub data: T,
    pub dht_record: CryptoTyped<CryptoKey>,
}

#[derive(Clone)]
pub struct P2PAppRoutes {
    routes: HashMap<CryptoKey, (Target, CryptoKey)>,
}

impl P2PAppRoutes {
    pub async fn get_route(
        &mut self,
        remote_dht_record: CryptoTyped<CryptoKey>,
        api: VeilidAPI,
        routing_context: RoutingContext,
    ) -> Result<Target, Error> {
        if let Vacant(e) = self.routes.entry(remote_dht_record.value) {
            let (target, route) = get_service_route_from_dht(
                api.clone(),
                routing_context.clone(),
                remote_dht_record,
                true,
            )
            .await?;

            e.insert((target, route));
        }

        Ok(self.routes.get(&remote_dht_record.value).unwrap().clone().0)
    }

    fn remove_route_if_exists(&mut self, dead_route: CryptoKey) {
        let key_to_remove: Option<CryptoKey> = self
            .routes
            .iter()
            .filter(|(_, (_, route))| *route == dead_route)
            .map(|(key, _)| *key)
            .next();

        if key_to_remove.is_none() {
            return;
        }

        self.routes.remove(&key_to_remove.unwrap());
    }
}

#[derive(Clone)]
pub struct P2PApp {
    pub api: VeilidAPI,
    pub routing_context: RoutingContext,
    pub receiver: Receiver<VeilidUpdate>,
    pub our_route: CryptoKey,
    pub our_dht_key: CryptoTyped<CryptoKey>,
    pub key_pair: KeyPair,
    pub routes: Arc<Mutex<P2PAppRoutes>>,
    // I've experienced multiple deliveries of the same message when the route is reported brocken
    // So far the easy fix is to log hashes of al lrecived messages, and drop ones that were already recived
    // TODO: find a better solution
    pub recived_message_hashes: Arc<Mutex<Vec<u64>>>,
}

impl<T: DeserializeOwned + Serialize> AppMessage<T> {
    pub async fn send(
        &self,
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
                let change = e.into_inner();
                info!("error sending veilid update callback: {:?}", change);
            }
        });

        let id = Uuid::new_v4();
        let veilid_storage_dir = tempfile::tempdir()?
            .path()
            .join(Path::new(&id.to_string()))
            .to_path_buf();
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

        // idk if there's a bug in veilid or what, but we need to update the route twice
        update_service_route_pin(
            routing_context.clone(),
            our_route_blob.clone(),
            our_dht_key,
            key_pair.clone(),
        )
        .await?;

        let routes = Arc::new(Mutex::new(P2PAppRoutes {
            routes: HashMap::new(),
        }));

        let recived_message_hashes = Arc::new(Mutex::new(Vec::<u64>::new()));

        Ok(Self {
            api,
            routing_context,
            receiver,
            key_pair,
            our_route,
            routes,
            our_dht_key,
            recived_message_hashes,
        })
    }

    pub async fn send_message<T: DeserializeOwned>(
        &self,
        app_message: AppMessage<T>,
        remote_dht_record: CryptoTyped<CryptoKey>,
    ) -> Result<(), Error>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let routes = self.routes.clone();
        for attempt_n in 0..SEND_ATTEMPTS {
            let mut routes = routes.lock().await;
            let target = routes
                .get_route(
                    remote_dht_record,
                    self.api.clone(),
                    self.routing_context.clone(),
                )
                .await?;

            let result = app_message.send(&self.routing_context, target).await;
            if result.is_ok() {
                break;
            } else if result.is_err() {
                info!("Unable to send message, sleeping 500ms");
                sleep(Duration::from_millis(500)).await;
                continue;
            }

            if attempt_n == (SEND_ATTEMPTS - 1) {
                return Err(io::Error::new(io::ErrorKind::Other, "Couldn't send reply").into());
            }
        }
        Ok(())
    }

    pub async fn network_loop<F, T, Fut>(&mut self, on_app_message: F) -> Result<(), Error>
    where
        F: FnOnce(AppMessage<T>) -> Fut + Send + Clone + 'static,
        T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let reciever = self.receiver.clone();
        let api = self.api.clone();

        loop {
            let res = reciever.recv()?;
            let api = api.clone();
            let routes = self.routes.clone();
            let on_app_message = on_app_message.clone();
            let recived_message_hashes = self.recived_message_hashes.clone();

            match res {
                VeilidUpdate::AppCall(call) => {
                    info!("VeilidUpdate::AppMessage");

                    spawn(async move {
                        let raw_message = call.message();
                        let message_hash = calculate_hash(raw_message);

                        let mut recived_message_hashes = recived_message_hashes.lock().await;

                        let reply = api.app_call_reply(call.id(), b"ACK".to_vec()).await;
                        if reply.is_err() {
                            info!("Unable to send ACK");
                            return;
                        }

                        if recived_message_hashes.contains(&message_hash) {
                            return;
                        }

                        let app_message =
                            serde_json::from_slice::<AppMessage<T>>(raw_message).unwrap();

                        on_app_message(app_message).await;
                        recived_message_hashes.push(message_hash);
                    });
                }
                VeilidUpdate::RouteChange(change) => {
                    info!("VeilidUpdate::RouteChange, {:?}", change);

                    let our_route_is_dead = change
                        .dead_routes
                        .iter()
                        .filter(|r| **r == self.our_route)
                        .count()
                        > 0;
                    if our_route_is_dead {
                        self.update_local_route().await?;
                    }

                    let mut routes = routes.lock().await;
                    for dead_route in change.dead_remote_routes {
                        routes.remove_route_if_exists(dead_route);
                    }
                }
                _ => (),
            };
        }
    }

    pub(crate) async fn update_local_route(&mut self) -> Result<(), Error> {
        let (our_route, our_route_blob) = create_private_route(self.api.clone()).await?;
        self.our_route = our_route;
        update_service_route_pin(
            self.routing_context.clone(),
            our_route_blob,
            self.our_dht_key,
            self.key_pair,
        )
        .await?;

        info!("DHT value for route {:} changed", self.our_route);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dht_test_update() -> Result<(), Error> {
        eprintln!("test_dht_test_update");
        let mut app = P2PApp::new().await?;

        let mut old_route = app.our_route.clone();
        for i in 0..3 {
            eprintln!("Updating DHT record, try n:{}", i);
            app.update_local_route().await?;
            let new_route = app.our_route.clone();

            assert!(old_route != new_route);
            old_route = new_route
        }

        Ok(())
    }
}
