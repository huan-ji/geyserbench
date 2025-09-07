use futures_util::stream::StreamExt;
use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex},
};
use tokio::{sync::broadcast, task};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, Comparator, TransactionData},
};

use super::GeyserProvider;

pub mod shreder_proto {
    include!(concat!(env!("OUT_DIR"), "/shreder.rs"));
}

use shreder_proto::shreder_service_client::ShrederServiceClient;

pub struct ShrederProvider;

impl GeyserProvider for ShrederProvider {
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

    let mut client = ShrederServiceClient::connect(endpoint.url).await?;
    log::info!("[{}] Connected successfully", endpoint.name);

    let mut transactions: HashMap<String, shreder_proto::SubscribeRequestFilterTransactions> =
        HashMap::new();
    transactions.insert(
        String::from("account"),
        shreder_proto::SubscribeRequestFilterTransactions {
            account_exclude: vec![],
            account_include: vec![],
            account_required: vec![config.account.clone()],
        },
    );

    let request = shreder_proto::SubscribeTransactionsRequest { transactions };
    // Send a single initial request via a stream
    let in_stream = tokio_stream::iter(vec![request]);
    let mut stream = client.subscribe_transactions(in_stream).await?.into_inner();

    'ploop: loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::info!("[{}] Received stop signal...", endpoint.name);
                break;
            }

            message = stream.next() => {
                if let Some(Ok(msg)) = message {
                    if let Some(tx_update) = msg.transaction.as_ref() {
                        if let Some(tx) = tx_update.transaction.as_ref() {
                            if let Some(message) = tx.message.as_ref() {
                                let accounts = message
                                    .account_keys
                                    .iter()
                                    .map(|key| bs58::encode(key).into_string())
                                    .collect::<Vec<String>>();

                                if accounts.contains(&config.account) {
                                    let timestamp = get_current_timestamp();
                                    let signature = tx.signatures.first()
                                        .map(|s| bs58::encode(s).into_string())
                                        .unwrap_or_default();

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
                                        TransactionData { timestamp, signature: signature.clone(), start_time },
                                    );

                                    if comp.get_all_seen_count() >= config.transactions as usize {
                                        log::info!(
                                            "Endpoint {} shutting down after {} transactions seen and {} by all workers",
                                            endpoint.name, transaction_count, config.transactions
                                        );
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
        }
    }

    log::info!("[{}] Stream closed", endpoint.name);
    Ok(())
}
