use futures_util::stream::StreamExt;
use std::{error::Error, sync::atomic::Ordering};
use tokio::task;

use solana_pubkey::Pubkey;

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, TransactionData},
};

use super::{
    common::{fatal_connection_error, TransactionAccumulator},
    GeyserProvider, ProviderContext,
};

#[allow(clippy::all, dead_code)]
pub mod shredstream {
    include!(concat!(env!("OUT_DIR"), "/shredstream.rs"));
}

pub struct ShredstreamProvider;

impl GeyserProvider for ShredstreamProvider {
    fn process(
        &self,
        endpoint: Endpoint,
        config: Config,
        context: ProviderContext,
    ) -> task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> {
        task::spawn(async move { process_shredstream_endpoint(endpoint, config, context).await })
    }
}

async fn process_shredstream_endpoint(
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

    let mut client = shredstream::shredstream_proxy_client::ShredstreamProxyClient::connect(
        endpoint_url.clone(),
    )
    .await
    .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    log::info!("[{}] Connected successfully", endpoint_name);

    let request = shredstream::SubscribeEntriesRequest {};
    let mut stream = client.subscribe_entries(request).await?.into_inner();

    let mut accumulator = TransactionAccumulator::new();
    let mut reached_target = false;

    let mut transaction_count = 0usize;

    loop {
        tokio::select! { biased;
        _ = shutdown_rx.recv() => {
            log::info!("[{}] Received stop signal...", endpoint_name);
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
                    let has_account = tx
                        .message
                        .static_account_keys()
                        .iter()
                        .any(|key| key == &account_pubkey);

                    if !has_account {
                        continue;
                    }

                    let wallclock = get_current_timestamp();
                    let elapsed = start_instant.elapsed();
                    let signature = tx.signatures[0].to_string();

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
