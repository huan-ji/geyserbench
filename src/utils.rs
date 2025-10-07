use dashmap::DashMap;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct TransactionData {
    pub wallclock_secs: f64,
    pub elapsed_since_start: Duration,
    pub start_wallclock_secs: f64,
}

#[derive(Debug)]
pub struct Comparator {
    data: DashMap<String, HashMap<String, TransactionData>>,
}

impl Comparator {
    pub fn new() -> Self {
        Self {
            data: DashMap::new(),
        }
    }

    pub fn add_batch(&self, from: &str, transactions: HashMap<String, TransactionData>) {
        for (signature, data) in transactions {
            let mut entry = self.data.entry(signature).or_default();
            entry.insert(from.to_owned(), data);
        }
    }

    pub fn iter(&self) -> dashmap::iter::Iter<'_, String, HashMap<String, TransactionData>> {
        self.data.iter()
    }
}

pub fn get_current_timestamp() -> f64 {
    let now = SystemTime::now();
    let since_epoch: Duration = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d,
        Err(e) => {
            // System clock went backwards; log and clamp to 0
            log::warn!("SystemTime error (clock skew): {}", e);
            Duration::from_secs(0)
        }
    };
    since_epoch.as_secs_f64()
}

pub fn percentile(sorted_data: &[f64], p: f64) -> f64 {
    if sorted_data.is_empty() {
        return 0.0;
    }
    let index = (p * (sorted_data.len() - 1) as f64).round() as usize;
    sorted_data[index]
}

pub fn open_log_file(name: &str) -> std::io::Result<impl Write> {
    let safe_name = sanitize_filename(name);
    let log_filename = format!("transaction_log_{}.txt", safe_name);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_filename)
}

pub fn write_log_entry(
    file: &mut impl Write,
    timestamp: f64,
    endpoint_name: &str,
    signature: &str,
) -> std::io::Result<()> {
    let log_entry = format!("[{:.3}] [{}] {}\n", timestamp, endpoint_name, signature);
    file.write_all(log_entry.as_bytes())
}

fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    let trimmed = sanitized.trim_matches('.');
    if trimmed.is_empty() {
        "endpoint".to_string()
    } else {
        trimmed.to_string()
    }
}
