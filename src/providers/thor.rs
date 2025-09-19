use std::{
    error::Error,
    sync::{Arc, Mutex},
};

use crate::{
    config::{Config, Endpoint},
    utils::{get_current_timestamp, open_log_file, write_log_entry, Comparator, TransactionData},
};
use futures_util::stream::StreamExt;

use prost::Message;
use tokio::{sync::broadcast, task};
use tonic::{metadata::MetadataValue, transport::Channel, Request, Streaming};

use super::GeyserProvider;

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
        shutdown_tx: broadcast::Sender<()>,
        shutdown_rx: broadcast::Receiver<()>,
        start_time: f64,
        comparator: Arc<Mutex<Comparator>>,
    ) -> task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> {
        task::spawn(async move {
            process_thor_endpoint(
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

async fn process_thor_endpoint(
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

    let grpc_url = &endpoint.url;
    let grpc_token = &endpoint.x_token;
    // Connect to the gRPC server
    let uri = grpc_url.parse::<tonic::transport::Uri>()?;
    let channel = Channel::from_shared(uri.to_string())?.connect().await?;
    let add_auth = move |mut req: Request<()>| {
        let meta = MetadataValue::try_from(grpc_token.as_str())
            .expect("Invalid token string for metadata");
        req.metadata_mut().insert("authorization", meta);
        Ok(req)
    };

    let mut publisher_client = EventPublisherClient::with_interceptor(channel, add_auth);
    log::info!("[{}] Connected successfully", endpoint.name);

    let mut stream: Streaming<StreamResponse> = publisher_client
        .subscribe_to_transactions(())
        .await?
        .into_inner();

    'ploop: loop {
        tokio::select! { biased;
            _ = shutdown_rx.recv() => {
                log::info!("[{}] Received stop signal...", endpoint.name);
                break;
            }

            message = stream.next() => {
                if let Some(Ok(msg)) = message {
                    if let Ok(message_wrapper) = MessageWrapper::decode(&*msg.data) {
                        if let Some(EventMessage::Transaction(transaction_event_wrapper)) = message_wrapper.event_message {
                            if let Some(transaction_event) = transaction_event_wrapper.transaction {
                                if let Some(transaction) = transaction_event.transaction.as_ref() {
                                    if let Some(message) = transaction.message.as_ref() {
                                        let accounts: Vec<String> = message.account_keys
                                            .iter()
                                            .map(|key| bs58::encode(key).into_string())
                                            .collect();

                                        if accounts.contains(&config.account) {
                                            let timestamp = get_current_timestamp();
                                            let signature = bs58::encode(&transaction_event.signature).into_string();

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
                }
            }
        }
    }

    log::info!("[{}] Stream closed", endpoint.name);
    Ok(())
}
