pub use {
    bs58,
    bytes::Bytes,
    env_logger,
    futures_util::stream::StreamExt,
    log,
    serde::{Deserialize, Serialize},
    std::{
        env,
        sync::{atomic::AtomicUsize, Arc},
        time::Instant,
    },
    tokio::{signal::ctrl_c, sync::broadcast, task},
};

mod analysis;
mod config;
mod providers;
mod utils;

use anyhow::Result;
use utils::{get_current_timestamp, Comparator};
const CONFIG_PATH: &str = "config.toml";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = config::ConfigToml::load_or_create(CONFIG_PATH)?;
    log::info!("Loaded configuration");

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let start_time = get_current_timestamp();
    let comparator = Arc::new(Comparator::new());
    let start_instant = Instant::now();

    let mut handles = Vec::new();
    let endpoint_names: Vec<String> = config.endpoint.iter().map(|e| e.name.clone()).collect();
    let completion_counter = Arc::new(AtomicUsize::new(0));
    let global_target = if config.config.transactions > 0 {
        Some(config.config.transactions as usize)
    } else {
        None
    };

    let total_producers = config.endpoint.len();
    for endpoint in config.endpoint.clone() {
        let provider = providers::create_provider(&endpoint.kind);
        let shared_config = config.config.clone();
        let context = providers::ProviderContext {
            shutdown_tx: shutdown_tx.clone(),
            shutdown_rx: shutdown_tx.subscribe(),
            start_wallclock_secs: start_time,
            start_instant,
            comparator: comparator.clone(),
            target_transactions: global_target,
            completion_counter: completion_counter.clone(),
            total_producers,
        };

        handles.push(provider.process(endpoint, shared_config, context));
    }

    tokio::spawn({
        let shutdown_tx = shutdown_tx.clone();
        async move {
            match ctrl_c().await {
                Ok(()) => {
                    log::info!("Received Ctrl+C signal; initiating shutdown");
                    let _ = shutdown_tx.send(());
                }
                Err(err) => log::error!("Failed to listen for Ctrl+C: {}", err),
            }
        }
    });

    for handle in handles {
        match handle.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => log::error!("Provider error: {:?}", e),
            Err(e) => log::error!("Task join error: {:?}", e),
        }
    }

    analysis::analyze_delays(comparator.as_ref(), &endpoint_names);

    Ok(())
}
