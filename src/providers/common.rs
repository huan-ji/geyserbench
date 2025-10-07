use std::collections::HashMap;

use crate::utils::TransactionData;

#[derive(Default)]
pub struct TransactionAccumulator {
    entries: HashMap<String, TransactionData>,
}

impl TransactionAccumulator {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn record(&mut self, signature: String, data: TransactionData) {
        use std::collections::hash_map::Entry;

        match self.entries.entry(signature) {
            Entry::Vacant(entry) => {
                entry.insert(data);
            }
            Entry::Occupied(mut entry) => {
                if data.elapsed_since_start < entry.get().elapsed_since_start {
                    entry.insert(data);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn into_inner(self) -> HashMap<String, TransactionData> {
        self.entries
    }
}

pub fn fatal_connection_error(endpoint: &str, error: impl std::fmt::Display) -> ! {
    let message = format!("Failed to connect to endpoint {}: {}", endpoint, error);
    log::error!("{}", message);
    eprintln!("{}", message);
    std::process::exit(1);
}
