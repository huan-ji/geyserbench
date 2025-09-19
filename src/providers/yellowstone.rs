use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex},
};

use futures_util::{sink::SinkExt, stream::StreamExt};
use tokio::{sync::broadcast, task};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestPing},
    prelude::SubscribeRequestFilterTransactions,
    tonic::transport::ClientTlsConfig,
};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, Comparator, TransactionData},
};

use super::GeyserProvider;

pub struct YellowstoneProvider;

impl GeyserProvider for YellowstoneProvider {
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
            process_yellowstone_endpoint(
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

async fn process_yellowstone_endpoint(
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

    let mut client = GeyserGrpcClient::build_from_shared(endpoint.url)?
        .x_token(Some(endpoint.x_token))?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

    log::info!("[{}] Connected successfully", endpoint.name);

    let (mut subscribe_tx, mut stream) = client.subscribe().await?;
    let commitment: yellowstone_grpc_proto::geyser::CommitmentLevel = config.commitment.into();

    let mut transactions = HashMap::new();
    transactions.insert(
        "account".to_string(),
        SubscribeRequestFilterTransactions {
            account_include: vec![config.account.clone()],
            account_exclude: vec![],
            account_required: vec![],
            ..Default::default()
        },
    );

    subscribe_tx
        .send(SubscribeRequest {
            slots: HashMap::default(),
            accounts: HashMap::default(),
            transactions,
            transactions_status: HashMap::default(),
            entry: HashMap::default(),
            blocks: HashMap::default(),
            blocks_meta: HashMap::default(),
            commitment: Some(commitment as i32),
            accounts_data_slice: Vec::default(),
            ping: None,
            from_slot: None,
        })
        .await?;

    'ploop: loop {
        tokio::select! { biased;
            _ = shutdown_rx.recv() => {
                log::info!("[{}] Received stop signal...", endpoint.name);
                break;
            }

            message = stream.next() => {
                match message {
                    Some(Ok(msg)) => {
                        match msg.update_oneof {
                            Some(UpdateOneof::Transaction(tx_msg)) => {
                                if let Some(tx) = tx_msg.transaction.as_ref() {
                                    if let Some(msg) = tx.transaction.as_ref().and_then(|t| t.message.as_ref()) {
                                        let accounts = msg
                                            .account_keys
                                            .iter()
                                            .map(|key| bs58::encode(key).into_string())
                                            .collect::<Vec<String>>();

                                        if accounts.contains(&config.account) {
                                            let timestamp = get_current_timestamp();
                                            let signature = match tx.transaction.as_ref()
                                                .and_then(|t| t.signatures.first()) {
                                                Some(sig) => bs58::encode(sig).into_string(),
                                                None => {
                                                    log::warn!("[{}] Missing signature in transaction", endpoint.name);
                                                    continue;
                                                }
                                            };

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
                                                log::info!("Endpoint {} shutting down after {} transactions seen and {} completed",
                                                    endpoint.name, transaction_count, config.transactions);
                                                let _ = shutdown_tx.send(());
                                                break 'ploop;
                                            }

                                            log::info!("[{:.3}] [{}] {}", timestamp, endpoint.name, signature);
                                            transaction_count += 1;
                                        }
                                    }
                                }
                            },
                            Some(UpdateOneof::Ping(_)) => {
                                subscribe_tx
                                    .send(SubscribeRequest {
                                        ping: Some(SubscribeRequestPing { id: 1 }),
                                        ..Default::default()
                                    })
                                    .await?;
                            },
                            _ => {}
                        }
                    },
                    Some(Err(e)) => {
                        log::error!("[{}] Error receiving message: {:?}", endpoint.name, e);
                        break;
                    },
                    None => {
                        log::info!("[{}] Stream closed", endpoint.name);
                        break;
                    }
                }
            }
        }
    }

    log::info!("[{}] Stream closed", endpoint.name);
    Ok(())
}
