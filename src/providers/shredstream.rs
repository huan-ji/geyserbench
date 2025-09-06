use futures_util::stream::StreamExt;
use std::{
    error::Error,
    sync::{Arc, Mutex},
};
use tokio::{sync::broadcast, task};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, Comparator, TransactionData},
};

use super::GeyserProvider;

pub mod shredstream_proto {
    include!(concat!(env!("OUT_DIR"), "/shredstream.rs"));
}

pub struct ShredstreamProvider;

impl GeyserProvider for ShredstreamProvider {
    fn process(
        &self,
        endpoint: Endpoint,
        config: Config,
        shutdown_tx: broadcast::Sender<()>,
        shutdown_rx: broadcast::Receiver<()>,
        start_time: f64,
        comparator: Arc<Mutex<Comparator>>,
    ) -> task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> {
        task::spawn(async move {
            process_shredstream_endpoint(
                endpoint,
                config,
                shutdown_tx,
                shutdown_rx,
                start_time,
                comparator,
            )
            .await
        })
    }
}

async fn process_shredstream_endpoint(
    endpoint: Endpoint,
    config: Config,
    shutdown_tx: broadcast::Sender<()>,
    mut shutdown_rx: broadcast::Receiver<()>,
    start_time: f64,
    comparator: Arc<Mutex<Comparator>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut transaction_count = 0;

    let mut log_file = open_log_file(&endpoint.name)?;

    log::info!(
        "[{}] Connecting to endpoint: {}",
        endpoint.name,
        endpoint.url
    );

    let mut client =
        shredstream_proto::shredstream_proxy_client::ShredstreamProxyClient::connect(endpoint.url)
            .await?;
    log::info!("[{}] Connected successfully", endpoint.name);

    let request = shredstream_proto::SubscribeEntriesRequest {};
    let mut stream = client.subscribe_entries(request).await?.into_inner();

    'ploop: loop {
        tokio::select! {
        _ = shutdown_rx.recv() => {
            log::info!("[{}] Received stop signal...", endpoint.name);
            break;
        }

        Some(Ok(slot_entry)) = stream.next() => {
            let entries = match bincode::deserialize::<Vec<solana_entry::entry::Entry>>(
                &slot_entry.entries,
            ) {
                Ok(e) => e,
                Err(e) => {
                    log::error!("Deserialization failed with err: {e}");
                    continue;
                }
            };

            for entry in entries {
            for tx in entry.transactions {
                    let accounts = tx.message.static_account_keys()
                        .iter()
                        .map(|key| bs58::encode(key).into_string())
                        .collect::<Vec<String>>();

                    if accounts.contains(&config.account) {
                        let timestamp = get_current_timestamp();
                        let signature = bs58::encode(&tx.signatures[0]).into_string();

                        write_log_entry(&mut log_file, timestamp, &endpoint.name, &signature)?;

                        let mut comp = match comparator.lock() {
                            Ok(g) => g,
                            Err(e) => {
                                log::error!("Comparator mutex poisoned: {}", e);
                                e.into_inner()
                            }
                        };

                        comp.add(
                            endpoint.name.clone(),
                            TransactionData {
                                timestamp,
                                signature: signature.clone(),
                                start_time,
                            },
                        );

                        if comp.get_all_seen_count() >= config.transactions as usize {
                            log::info!("Endpoint {} shutting down after {} transactions seen and {} by all workers",
                                endpoint.name, transaction_count, config.transactions);
                            let _ = shutdown_tx.send(());
                            break 'ploop;
                        }

                        log::info!("[{:.3}] [{}] {}", timestamp, endpoint.name, signature);
                        transaction_count += 1;
                    }
                }
            }
            }
        }
    }

    log::info!("[{}] Stream closed", endpoint.name);
    Ok(())
}
