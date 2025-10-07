use std::{collections::HashMap, error::Error, sync::atomic::Ordering};

use futures_util::{sink::SinkExt, stream::StreamExt};
use solana_pubkey::Pubkey;
use tokio::task;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestPing},
    prelude::SubscribeRequestFilterTransactions,
    tonic::transport::ClientTlsConfig,
};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, TransactionData},
};

use super::{
    common::{fatal_connection_error, TransactionAccumulator},
    GeyserProvider, ProviderContext,
};

pub struct YellowstoneProvider;

impl GeyserProvider for YellowstoneProvider {
    fn process(
        &self,
        endpoint: Endpoint,
        config: Config,
        context: ProviderContext,
    ) -> task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> {
        task::spawn(async move { process_yellowstone_endpoint(endpoint, config, context).await })
    }
}

async fn process_yellowstone_endpoint(
    endpoint: Endpoint,
    config: Config,
    context: ProviderContext,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let ProviderContext {
        shutdown_tx,
        mut shutdown_rx,
        start_wallclock_secs,
        start_instant,
        comparator,
        target_transactions,
        completion_counter,
        total_producers,
    } = context;

    let account_pubkey = config.account.parse::<Pubkey>()?;
    let endpoint_name = endpoint.name.clone();
    let mut log_file = if log::log_enabled!(log::Level::Trace) {
        Some(open_log_file(&endpoint_name)?)
    } else {
        None
    };

    let endpoint_url = endpoint.url.clone();
    let endpoint_token = endpoint.x_token.clone();

    log::info!(
        "[{}] Connecting to endpoint: {}",
        endpoint_name,
        endpoint_url
    );

    let builder = GeyserGrpcClient::build_from_shared(endpoint_url.clone())
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    let builder = builder
        .x_token(Some(endpoint_token))
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    let builder = builder
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    let mut client = builder
        .connect()
        .await
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));

    log::info!("[{}] Connected successfully", endpoint_name);

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

    let mut accumulator = TransactionAccumulator::new();
    let mut reached_target = false;
    let mut transaction_count = 0usize;

    loop {
        tokio::select! { biased;
            _ = shutdown_rx.recv() => {
                log::info!("[{}] Received stop signal...", endpoint_name);
                break;
            }

            message = stream.next() => {
                match message {
                    Some(Ok(msg)) => {
                        match msg.update_oneof {
                    Some(UpdateOneof::Transaction(tx_msg)) => {
                                if let Some(tx) = tx_msg.transaction.as_ref() {
                                    if let Some(msg) = tx.transaction.as_ref().and_then(|t| t.message.as_ref()) {
                                        let has_account = msg
                                            .account_keys
                                            .iter()
                                            .any(|key| key.as_slice() == account_pubkey.as_ref());

                                        if has_account {
                                            let wallclock = get_current_timestamp();
                                            let elapsed = start_instant.elapsed();
                                            let signature = match tx.transaction.as_ref()
                                                .and_then(|t| t.signatures.first()) {
                                                Some(sig) => bs58::encode(sig).into_string(),
                                                None => {
                                                    log::warn!("[{}] Missing signature in transaction", endpoint_name);
                                                    continue;
                                                }
                                            };

                                            if let Some(file) = log_file.as_mut() {
                                                write_log_entry(file, wallclock, &endpoint_name, &signature)?;
                                            }

                                            accumulator.record(
                                                signature,
                                                TransactionData {
                                                    wallclock_secs: wallclock,
                                                    elapsed_since_start: elapsed,
                                                    start_wallclock_secs,
                                                },
                                            );

                                            transaction_count += 1;
                                            if let Some(target) = target_transactions {
                                                if !reached_target && transaction_count >= target {
                                                    reached_target = true;
                                                    let completed = completion_counter
                                                        .fetch_add(1, Ordering::AcqRel)
                                                        + 1;
                                                    let required = total_producers.max(1);
                                                    if completed >= required {
                                                        log::info!(
                                                            "All endpoints reached target {}; broadcasting shutdown",
                                                            target
                                                        );
                                                        let _ = shutdown_tx.send(());
                                                    }
                                                }
                                            }
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
                        log::error!("[{}] Error receiving message: {:?}", endpoint_name, e);
                        break;
                    },
                    None => {
                        log::info!("[{}] Stream closed", endpoint_name);
                        break;
                    }
                }
            }
        }
    }

    let unique_signatures = accumulator.len();
    let collected = accumulator.into_inner();
    comparator.add_batch(&endpoint_name, collected);
    log::info!(
        "[{}] Stream closed after dispatching {} transactions (unique signatures: {})",
        endpoint_name,
        transaction_count,
        unique_signatures
    );
    Ok(())
}
