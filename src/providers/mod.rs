use std::{
    error::Error,
    sync::{atomic::AtomicUsize, Arc},
    time::Instant,
};
use tokio::sync::broadcast;

use crate::{
    config::{Config, Endpoint, EndpointKind},
    utils::Comparator,
};

pub mod arpc;
pub mod common;
pub mod jetstream;
pub mod shreder;
pub mod shredstream;
pub mod thor;
pub mod yellowstone;

pub trait GeyserProvider: Send + Sync {
    fn process(
        &self,
        endpoint: Endpoint,
        config: Config,
        context: ProviderContext,
    ) -> tokio::task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>;
}

pub fn create_provider(kind: &EndpointKind) -> Box<dyn GeyserProvider> {
    match kind {
        EndpointKind::Yellowstone => Box::new(yellowstone::YellowstoneProvider),
        EndpointKind::Arpc => Box::new(arpc::ArpcProvider),
        EndpointKind::Thor => Box::new(thor::ThorProvider),
        EndpointKind::Shreder => Box::new(shreder::ShrederProvider),
        EndpointKind::Shredstream => Box::new(shredstream::ShredstreamProvider),
        EndpointKind::Jetstream => Box::new(jetstream::JetstreamProvider),
    }
}

pub struct ProviderContext {
    pub shutdown_tx: broadcast::Sender<()>,
    pub shutdown_rx: broadcast::Receiver<()>,
    pub start_wallclock_secs: f64,
    pub start_instant: Instant,
    pub comparator: Arc<Comparator>,
    pub target_transactions: Option<usize>,
    pub completion_counter: Arc<AtomicUsize>,
    pub total_producers: usize,
}
