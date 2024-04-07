use std::collections::hash_map::Entry::Vacant;
use std::future::Future;
use std::io;

use std::sync::Arc;

use anyhow::{Context, Error, Ok};

use async_std::sync::Mutex;
use flume::{unbounded, Receiver, Sender};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use veilid_core::tools::*;
use veilid_core::*;

use crate::utils::*;

const SEND_ATTEMPTS: u16 = 1024;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct AppMessage<T: DeserializeOwned> {
    pub data: T,
    pub uuid: String,
    pub dht_record: CryptoTyped<CryptoKey>,
}

#[derive(Clone)]
pub struct VeilidDuplexRoutes {
    routes: HashMap<CryptoKey, (Target, CryptoKey)>,
}

impl VeilidDuplexRoutes {
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

        Ok(self.routes.get(&remote_dht_record.value).unwrap().0)
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
pub struct VeilidDuplex {
    pub api: VeilidAPI,
    pub routing_context: RoutingContext,
    pub receiver: Receiver<VeilidUpdate>,
    pub our_route: CryptoKey,
    pub our_dht_key: CryptoTyped<CryptoKey>,
    pub node_keypair: KeyPair,
    pub dht_keypair: KeyPair,
    pub routes: Arc<Mutex<VeilidDuplexRoutes>>,
    // There can be multiple deliveries of the same message when the route is reported broken
    // So far the easy fix is to log hashes of all received messages, and drop ones that were already received
    pub received_message_hashes: Arc<Mutex<Vec<u64>>>,
}

impl<T: DeserializeOwned + Serialize> AppMessage<T> {
    pub async fn send(
        &mut self,
        routing_context: &RoutingContext,
        target: Target,
    ) -> Result<Vec<u8>, Error> {
        self.set_uuid();
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
            .app_call(target, app_message_blob)
            .await
            .context("app_call")
    }

    fn set_uuid(&mut self) {
        self.uuid = format!("{}", Uuid::new_v4());
    }
}

impl VeilidDuplex {
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

        let node_keypair = veilid_core::Crypto::generate_keypair(CRYPTO_KIND)
            .unwrap()
            .value;

        #[cfg(target_arch = "wasm32")]
        let api = create_api_and_connect(update_callback).await?;
        #[cfg(not(target_arch = "wasm32"))]
        let api = create_api_and_connect_with_keypair(update_callback, node_keypair).await?;

        let rc = api
            .routing_context()?
            .with_sequencing(Sequencing::PreferOrdered);

        Ok((api, rc, receiver, node_keypair))
    }

    pub async fn new() -> Result<Self, Error> {
        let (api, routing_context, receiver, node_keypair) = Self::initialize().await?;

        let (our_route, our_route_blob) = create_private_route(api.clone()).await?;
        info!("our route: {}", our_route);
        let (our_dht_key, dht_keypair) =
            create_service_route_pin(routing_context.clone(), our_route_blob.clone()).await?;

        // println!("Updating dht record");
        // // idk if there's a bug in veilid or what, but we need to update the route twice
        // update_service_route_pin(
        //     routing_context.clone(),
        //     our_route_blob.clone(),
        //     our_dht_key,
        //     dht_keypair.clone(),
        // )
        // .await?;

        let routes = Arc::new(Mutex::new(VeilidDuplexRoutes {
            routes: HashMap::new(),
        }));

        let received_message_hashes = Arc::new(Mutex::new(Vec::<u64>::new()));

        Ok(Self {
            api,
            routing_context,
            receiver,
            node_keypair,
            dht_keypair,
            our_route,
            routes,
            our_dht_key,
            received_message_hashes,
        })
    }

    pub async fn send_message<T: DeserializeOwned>(
        &self,
        mut app_message: AppMessage<T>,
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
                sleep(500).await;
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
        loop {
            self.network_loop_cycle(on_app_message.clone()).await?;
        }
    }

    pub async fn network_loop_cycle<F, T, Fut>(&mut self, on_app_message: F) -> Result<(), Error>
    where
        F: FnOnce(AppMessage<T>) -> Fut + Send + Clone + 'static,
        T: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let reciever = self.receiver.clone();
        let api = self.api.clone();

        if reciever.is_empty() {
            return Ok(());
        }

        let res = reciever.recv()?;
        let routes = self.routes.clone();
        let on_app_message = on_app_message.clone();
        let received_message_hashes = self.received_message_hashes.clone();

        match res {
            VeilidUpdate::AppCall(call) => {
                info!("VeilidUpdate::AppMessage");

                spawn(async move {
                    let raw_message = call.message();
                    let message_hash = calculate_hash(raw_message);

                    let reply = api.app_call_reply(call.id(), b"ACK".to_vec()).await;
                    if reply.is_err() {
                        info!("Unable to send ACK");
                        return;
                    }

                    let app_message = serde_json::from_slice::<AppMessage<T>>(raw_message).unwrap();

                    {
                        let mut received_message_hashes = received_message_hashes.lock().await;
                        if received_message_hashes.contains(&message_hash) {
                            info!("Message already received, skipping");
                            return;
                        }

                        received_message_hashes.push(message_hash);
                    }

                    on_app_message(app_message).await;
                })
                .await;
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

        Ok(())
    }

    async fn update_local_route(&mut self) -> Result<(), Error> {
        let (our_route, our_route_blob) = create_private_route(self.api.clone()).await?;
        self.our_route = our_route;
        update_service_route_pin(
            self.routing_context.clone(),
            our_route_blob,
            self.our_dht_key,
            self.dht_keypair,
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
        let mut app = VeilidDuplex::new().await?;

        let mut old_route = app.our_route;
        for i in 0..3 {
            eprintln!("Updating DHT record, try n:{}", i);
            app.update_local_route().await?;
            let new_route = app.our_route;

            assert!(old_route != new_route);
            old_route = new_route
        }

        Ok(())
    }
}
