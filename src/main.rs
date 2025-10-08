pub use {
    bs58,
    bytes::Bytes,
    futures_util::stream::StreamExt,
    serde::{Deserialize, Serialize},
    std::{
        env,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::Instant,
    },
    tokio::{signal::ctrl_c, sync::broadcast, task},
};

mod analysis;
mod backend;
mod config;
mod providers;
mod utils;

use anyhow::{anyhow, Result};
use backend::{BackendStatus, StreamOptions};
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;
use utils::{get_current_timestamp, Comparator, ProgressTracker};
const DEFAULT_CONFIG_PATH: &str = "config.toml";
const DEFAULT_BACKEND_STREAM_URL: &str = "wss://gb.solstack.app/v1/benchmarks/stream";

struct CliArgs {
    config_path: Option<String>,
    disable_streaming: bool,
}

impl CliArgs {
    fn parse() -> Self {
        let mut args = env::args().skip(1);
        let mut parsed = CliArgs {
            config_path: None,
            disable_streaming: false,
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    let value = args.next().unwrap_or_else(|| {
                        eprintln!("Missing value for --config");
                        print_usage();
                        std::process::exit(1);
                    });
                    parsed.config_path = Some(value);
                }
                "--no-stream" => {
                    parsed.disable_streaming = true;
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("Unknown argument: {}", other);
                    print_usage();
                    std::process::exit(1);
                }
            }
        }

        parsed
    }
}

fn print_usage() {
    eprintln!("Usage: geyserbench [--config <PATH>] [--no-stream]");
}

#[tokio::main]
async fn main() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|err| anyhow!(err))?;

    let cli = CliArgs::parse();
    let config_path = cli.config_path.as_deref().unwrap_or(DEFAULT_CONFIG_PATH);
    let config = config::ConfigToml::load_or_create(config_path)?;
    info!(config_path = config_path, "Loaded configuration");

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let start_time = get_current_timestamp();
    let comparator = Arc::new(Comparator::new());
    let start_instant = Instant::now();
    let shared_counter = Arc::new(AtomicUsize::new(0));
    let shared_shutdown = Arc::new(AtomicBool::new(false));
    let aborted = Arc::new(AtomicBool::new(false));

    let mut backend_settings = config.backend.clone();
    if cli.disable_streaming {
        backend_settings.enabled = false;
    } else {
        backend_settings.enabled = true;
    }
    backend_settings.url = Some(DEFAULT_BACKEND_STREAM_URL.to_string());

    let mut backend_handle = None;
    let mut signature_sender = None;
    let mut backend_run_id = None;

    if backend_settings.enabled {
        let url = backend_settings
            .url
            .clone()
            .ok_or_else(|| anyhow!("backend streaming enabled but no URL configured"))?;
        let options = StreamOptions { url, summary: None };
        let handle = backend::connect_stream(options, &config.config, &config.endpoint).await?;
        let run_id = handle.run_id().to_string();
        info!(run_id = %run_id, "Streaming backend session initialised");
        backend_run_id = Some(run_id.clone());

        let mut status_rx = handle.status();
        let shutdown_for_backend = shutdown_tx.clone();
        let run_id_for_status = run_id.clone();
        tokio::spawn(async move {
            while status_rx.changed().await.is_ok() {
                match status_rx.borrow().clone() {
                    BackendStatus::Failed { message } => {
                        error!(run_id = %run_id_for_status, error = %message, "Backend streaming failed");
                        let _ = shutdown_for_backend.send(());
                        break;
                    }
                    BackendStatus::Completed { .. } => break,
                    BackendStatus::Ready { run_id } => {
                        debug!(run_id = %run_id, "Backend stream ready");
                    }
                    BackendStatus::Initializing => {}
                }
            }
        });

        signature_sender = Some(handle.signature_sender());
        backend_handle = Some(handle);
    } else {
        info!("Backend streaming disabled; collecting metrics locally");
    }

    let mut handles = Vec::new();
    let endpoint_names: Vec<String> = config.endpoint.iter().map(|e| e.name.clone()).collect();
    let global_target = if config.config.transactions > 0 {
        Some(config.config.transactions as usize)
    } else {
        None
    };
    let progress_tracker = global_target.map(|target| Arc::new(ProgressTracker::new(target)));

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
            signature_tx: signature_sender.clone(),
            shared_counter: shared_counter.clone(),
            shared_shutdown: shared_shutdown.clone(),
            target_transactions: global_target,
            total_producers,
            progress: progress_tracker.clone(),
        };

        handles.push(provider.process(endpoint, shared_config, context));
    }

    tokio::spawn({
        let shutdown_tx = shutdown_tx.clone();
        let shared_shutdown = shared_shutdown.clone();
        let aborted = aborted.clone();
        async move {
            match ctrl_c().await {
                Ok(()) => {
                    let already_aborting = aborted.swap(true, Ordering::AcqRel);
                    if already_aborting {
                        info!("Received additional Ctrl+C; shutdown already in progress");
                    } else {
                        info!("Received Ctrl+C; initiating shutdown");
                    }
                    shared_shutdown.store(true, Ordering::Release);
                    let _ = shutdown_tx.send(());
                }
                Err(err) => error!(error = %err, "Failed to listen for Ctrl+C"),
            }
        }
    });

    for handle in handles {
        match handle.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => error!(error = ?e, "Provider task returned error"),
            Err(e) => error!(error = ?e, "Provider join error"),
        }
    }

    drop(signature_sender);

    if let Some(handle) = backend_handle {
        if aborted.load(Ordering::Acquire) {
            if let Some(run_id) = backend_run_id.as_ref() {
                info!(run_id = %run_id, "Skipping backend finalisation due to user abort");
            } else {
                info!("Skipping backend finalisation due to user abort");
            }
            // Dropping the handle without calling finish() prevents the backend run from being saved.
        } else {
            let run_id = backend_run_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            match handle.finish().await {
                Ok(result) => {
                    info!(run_id = %run_id, "Backend completed run");
                    debug!(run_id = %run_id, response = %result.response, "Backend completion payload");
                }
                Err(err) => {
                    error!(run_id = %run_id, error = %err, "Backend streaming ended with error");
                }
            }
        }
    }

    let run_aborted = aborted.load(Ordering::Acquire);

    if !run_aborted {
        analysis::analyze_delays(comparator.as_ref(), &endpoint_names);

        if let Some(run_id) = backend_run_id {
            println!("🔗 Share this benchmark run: https://runs.solstack.app/run/{run_id}");
        }
    } else {
        info!("Benchmark aborted before completion; no results were generated");
    }

    Ok(())
}
