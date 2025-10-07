use std::{collections::HashMap, error::Error, sync::atomic::Ordering};

use futures::{channel::mpsc::unbounded, SinkExt};
use futures_util::stream::StreamExt;
use solana_pubkey::Pubkey;
use tokio::task;

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, TransactionData},
};

use super::{
    common::{fatal_connection_error, TransactionAccumulator},
    GeyserProvider, ProviderContext,
};

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
        context: ProviderContext,
    ) -> task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> {
        task::spawn(async move { process_arpc_endpoint(endpoint, config, context).await })
    }
}

async fn process_arpc_endpoint(
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

    log::info!(
        "[{}] Connecting to endpoint: {}",
        endpoint_name,
        endpoint_url
    );

    let mut client = ArpcServiceClient::connect(endpoint_url.clone())
        .await
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    log::info!("[{}] Connected successfully", endpoint_name);

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
                if let Some(Ok(msg)) = message {
                    if let Some(tx) = msg.transaction {
                        let has_account = tx
                            .account_keys
                            .iter()
                            .any(|key| key.as_slice() == account_pubkey.as_ref());

                        if has_account {
                            let wallclock = get_current_timestamp();
                            let elapsed = start_instant.elapsed();
                            let signature = bs58::encode(&tx.signatures[0]).into_string();

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
                                    let completed =
                                        completion_counter.fetch_add(1, Ordering::AcqRel) + 1;
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
