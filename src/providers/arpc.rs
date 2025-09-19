use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex},
};

use futures::{channel::mpsc::unbounded, SinkExt};
use futures_util::stream::StreamExt;
use solana_pubkey::Pubkey;
use tokio::{sync::broadcast, task};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, Comparator, TransactionData},
};

use super::GeyserProvider;

#[allow(clippy::all, dead_code)]
pub mod arpc {
    include!(concat!(env!("OUT_DIR"), "/arpc.rs"));
}

use arpc::{
    arpc_service_client::ArpcServiceClient, SubscribeRequest as ArpcSubscribeRequest,
    SubscribeRequestFilterTransactions,
};

pub struct ArpcProvider;

impl GeyserProvider for ArpcProvider {
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
            process_arpc_endpoint(
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

async fn process_arpc_endpoint(
    endpoint: Endpoint,
    config: Config,
    shutdown_tx: broadcast::Sender<()>,
    mut shutdown_rx: broadcast::Receiver<()>,
    start_time: f64,
    comparator: Arc<Mutex<Comparator>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut transaction_count = 0;

    let account_pubkey = config.account.parse::<Pubkey>()?;

    let mut log_file = open_log_file(&endpoint.name)?;

    log::info!(
        "[{}] Connecting to endpoint: {}",
        endpoint.name,
        endpoint.url
    );

    let mut client = ArpcServiceClient::connect(endpoint.url).await?;
    log::info!("[{}] Connected successfully", endpoint.name);

    let mut transactions = HashMap::new();
    transactions.insert(
        String::from("account"),
        SubscribeRequestFilterTransactions {
            account_include: vec![config.account.clone()],
            account_exclude: vec![],
            account_required: vec![],
        },
    );

    let request = ArpcSubscribeRequest {
        transactions,
        ping_id: Some(0),
    };

    let (mut subscribe_tx, subscribe_rx) = unbounded::<ArpcSubscribeRequest>();
    subscribe_tx.send(request).await?;
    let mut stream = client.subscribe(subscribe_rx).await?.into_inner();

    'ploop: loop {
        tokio::select! { biased;
            _ = shutdown_rx.recv() => {
                log::info!("[{}] Received stop signal...", endpoint.name);
                break;
            }

            message = stream.next() => {
                if let Some(Ok(msg)) = message {
                    if let Some(tx) = msg.transaction {
                        let has_account = tx
                            .account_keys
                            .iter()
                            .any(|key| key.as_slice() == account_pubkey.as_ref());

                        if has_account {
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
