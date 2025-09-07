pub use {
    bs58,
    bytes::Bytes,
    env_logger,
    futures_util::stream::StreamExt,
    log,
    serde::{Deserialize, Serialize},
    std::{
        env,
        sync::{Arc, Mutex},
    },
    tokio::{signal::ctrl_c, sync::broadcast, task},
};

mod analysis;
mod config;
mod providers;
mod utils;

use utils::{get_current_timestamp, Comparator};
const CONFIG_PATH: &str = "config.toml";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = config::ConfigToml::load_or_create(CONFIG_PATH)?;
    log::info!("Loaded configuration");

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let endpoint_count = config.endpoint.len();

    let start_time = get_current_timestamp();
    // Expect each endpoint to eventually report; use endpoint count
    let comparator = Arc::new(Mutex::new(Comparator::new(endpoint_count)));

    let mut handles = Vec::new();
    let endpoint_names: Vec<String> = config.endpoint.iter().map(|e| e.name.clone()).collect();

    for endpoint in config.endpoint.clone() {
        let provider = providers::create_provider(&endpoint.kind);
        let shared_config = config.config.clone();
        let stx = shutdown_tx.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let shared_comparator = comparator.clone();

        handles.push(provider.process(
            endpoint,
            shared_config,
            stx,
            shutdown_rx,
            start_time,
            shared_comparator,
        ));
    }

    tokio::spawn(async move {
        if (ctrl_c().await).is_ok() {
            println!("\nReceived Ctrl+C signal. Shutting down...");
            let _ = shutdown_tx.send(());
        }
    });

    for handle in handles {
        match handle.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => log::error!("Provider error: {:?}", e),
            Err(e) => log::error!("Task join error: {:?}", e),
        }
    }

    let comp_guard = match comparator.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            log::error!("Comparator mutex poisoned; continuing with inner value");
            poisoned.into_inner()
        }
    };
    analysis::analyze_delays(&comp_guard, endpoint_names);

    Ok(())
}
