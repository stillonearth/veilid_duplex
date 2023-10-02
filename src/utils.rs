use anyhow::Error;
use tokio::time::{sleep, Duration};
use tracing::info;
use veilid_core::{
    AttachmentState::{AttachedGood, AttachedStrong, AttachedWeak, FullyAttached, OverAttached},
    VeilidAPI,
};

pub async fn wait_for_attached(api: &VeilidAPI) {
    info!("awaiting attachment");
    loop {
        match api.get_state().await {
            Ok(state) => match state.attachment.state {
                AttachedWeak | AttachedGood | AttachedStrong | FullyAttached | OverAttached => {
                    break
                }
                _ => (),
            },
            _ => (),
        }
        sleep(Duration::from_millis(100)).await;
    }
    info!("awaiting attachment, done");
}

pub async fn wait_for_network_start(api: &VeilidAPI) {
    info!("awaiting network initialization");
    loop {
        match api.get_state().await {
            Ok(vs) => {
                if vs.network.started && !vs.network.peers.is_empty() {
                    info!(
                        "awaiting network initialization, done ({} peer(s))",
                        vs.network.peers.len()
                    );
                    break;
                }
            }
            Err(e) => {
                panic!("Getting state failed: {:?}", e);
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_for_public_internet_ready(api: &VeilidAPI) -> Result<(), Error> {
    info!("awaiting 'public_internet_ready'");
    loop {
        let state = api.get_state().await;
        match state {
            Ok(state) => {
                if state.attachment.public_internet_ready {
                    break;
                }
            }
            _ => (),
        }
        sleep(Duration::from_secs(5)).await;
    }
    info!("awaiting 'public_internet_ready', done");
    Ok(())
}
