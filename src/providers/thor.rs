use std::{error::Error, sync::atomic::Ordering};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, TransactionData},
};
use futures_util::stream::StreamExt;

use prost::Message;
use solana_pubkey::Pubkey;
use tokio::task;
use tonic::{metadata::MetadataValue, transport::Channel, Request, Streaming};

use super::{
    common::{fatal_connection_error, TransactionAccumulator},
    GeyserProvider, ProviderContext,
};

#[allow(clippy::all, dead_code)]
pub mod thor_streamer {
    include!(concat!(env!("OUT_DIR"), "/thor_streamer.types.rs"));
}

#[allow(clippy::all, dead_code)]
pub mod publisher {
    include!(concat!(env!("OUT_DIR"), "/publisher.rs"));
}

use publisher::{event_publisher_client::EventPublisherClient, StreamResponse};
use thor_streamer::{message_wrapper::EventMessage, MessageWrapper};

pub struct ThorProvider;

impl GeyserProvider for ThorProvider {
    fn process(
        &self,
        endpoint: Endpoint,
        config: Config,
        context: ProviderContext,
    ) -> task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> {
        task::spawn(async move { process_thor_endpoint(endpoint, config, context).await })
    }
}

async fn process_thor_endpoint(
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
    let auth_header = endpoint
        .x_token
        .as_ref()
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .map(|token| {
            MetadataValue::try_from(token)
                .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err))
        });

    log::info!(
        "[{}] Connecting to endpoint: {}",
        endpoint_name,
        endpoint_url
    );

    // Connect to the gRPC server
    let uri = endpoint_url
        .parse::<tonic::transport::Uri>()
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    let channel = Channel::from_shared(uri.to_string())
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err))
        .connect()
        .await
        .unwrap_or_else(|err| fatal_connection_error(&endpoint_name, err));
    let mut publisher_client =
        EventPublisherClient::with_interceptor(channel, move |mut req: Request<()>| {
            if let Some(ref token) = auth_header {
                req.metadata_mut().insert("authorization", token.clone());
            }
            Ok(req)
        });
    log::info!("[{}] Connected successfully", endpoint_name);

    let mut stream: Streaming<StreamResponse> = publisher_client
        .subscribe_to_transactions(())
        .await?
        .into_inner();

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
                    if let Ok(message_wrapper) = MessageWrapper::decode(&*msg.data) {
                        if let Some(EventMessage::Transaction(transaction_event_wrapper)) = message_wrapper.event_message {
                            if let Some(transaction_event) = transaction_event_wrapper.transaction {
                                if let Some(transaction) = transaction_event.transaction.as_ref() {
                                    if let Some(message) = transaction.message.as_ref() {
                                        let has_account = message
                                            .account_keys
                                            .iter()
                                            .any(|key| key.as_slice() == account_pubkey.as_ref());

                                        if has_account {
                                            let wallclock = get_current_timestamp();
                                            let elapsed = start_instant.elapsed();
                                            let signature = bs58::encode(&transaction_event.signature).into_string();

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
