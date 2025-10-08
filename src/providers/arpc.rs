use std::{collections::HashMap, error::Error, sync::atomic::Ordering};

use futures::{channel::mpsc::unbounded, SinkExt};
use futures_util::stream::StreamExt;
use solana_pubkey::Pubkey;
use tokio::task;
use tracing::{info, warn, Level};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, TransactionData},
};

use super::{
    common::{build_signature_envelope, fatal_connection_error, TransactionAccumulator},
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
        signature_tx,
        shared_counter,
        shared_shutdown,
        target_transactions,
        total_producers,
    } = context;
    let signature_sender = signature_tx;
    let account_pubkey = config.account.parse::<Pubkey>()?;
    let endpoint_name = endpoint.name.clone();

    let mut log_file = if tracing::enabled!(Level::TRACE) {
        Some(open_log_file(&endpoint_name)?)
    } else {
        None
    };

    let endpoint_url = endpoint.url.clone();

    info!(endpoint = %endpoint_name, url = %endpoint_url, "Connecting");

    let mut client = ArpcServiceClient::connect(endpoint_url.clone())
        .await
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    info!(endpoint = %endpoint_name, "Connected");

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
    let mut transaction_count = 0usize;

    loop {
        tokio::select! { biased;
            _ = shutdown_rx.recv() => {
                info!(endpoint = %endpoint_name, "Received stop signal");
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

                            let tx_data = TransactionData {
                                wallclock_secs: wallclock,
                                elapsed_since_start: elapsed,
                                start_wallclock_secs,
                            };

                            let updated = accumulator.record(
                                signature.clone(),
                                tx_data.clone(),
                            );

                            if updated {
                                if let Some(envelope) = build_signature_envelope(
                                    &comparator,
                                    &endpoint_name,
                                    &signature,
                                    tx_data,
                                    total_producers,
                                ) {
                                    if let Some(target) = target_transactions {
                                        let shared = shared_counter
                                            .fetch_add(1, Ordering::AcqRel)
                                            + 1;
                                        if shared >= target
                                            && !shared_shutdown.swap(true, Ordering::AcqRel)
                                        {
                                            info!(endpoint = %endpoint_name, target, "Reached shared signature target; broadcasting shutdown");
                                            let _ = shutdown_tx.send(());
                                        }
                                    }

                                    if let Some(sender) = signature_sender.as_ref() {
                                        if let Err(err) = sender.send(envelope).await {
                                            warn!(endpoint = %endpoint_name, signature = %signature, error = %err, "Failed to queue signature for backend");
                                        }
                                    }
                                }
                            }

                            transaction_count += 1;
                        }
                    }
                }
            }
        }
    }

    let unique_signatures = accumulator.len();
    let collected = accumulator.into_inner();
    comparator.add_batch(&endpoint_name, collected);
    info!(
        endpoint = %endpoint_name,
        total_transactions = transaction_count,
        unique_signatures,
        "Stream closed after dispatching transactions"
    );
    Ok(())
}
