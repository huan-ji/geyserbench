use crate::utils::{Comparator, TransactionData, percentile};
use comfy_table::{ContentArrangement, Table};

#[cfg(target_os = "windows")]
#[inline]
fn table_preset() -> &'static str {
    comfy_table::presets::ASCII_FULL
}

#[cfg(not(target_os = "windows"))]
#[inline]
fn table_preset() -> &'static str {
    comfy_table::presets::UTF8_FULL
}
use std::collections::HashMap;
use std::time::Duration;

#[derive(Default)]
pub struct EndpointStats {
    pub first_detections: usize,
    pub total_valid_transactions: usize,
    pub delays: Vec<f64>,
    pub backfill_transactions: usize,
}

#[derive(Debug, Default, Clone)]
pub struct EndpointSummary {
    pub name: String,
    pub first_share: f64,
    pub p50_delay_ms: Option<f64>,
    pub p95_delay_ms: Option<f64>,
    pub p99_delay_ms: Option<f64>,
    pub valid_transactions: usize,
    pub first_detections: usize,
    pub backfill_transactions: usize,
}

#[derive(Debug, Clone)]
pub struct RunSummary {
    pub endpoints: Vec<EndpointSummary>,
    pub fastest_endpoint: Option<String>,
    pub has_data: bool,
}

pub fn compute_run_summary(comparator: &Comparator, endpoint_names: &[String]) -> RunSummary {
    let mut endpoint_stats: HashMap<String, EndpointStats> = HashMap::new();

    for endpoint_name in endpoint_names {
        endpoint_stats.insert(endpoint_name.clone(), EndpointStats::default());
    }

    for sig_entry in comparator.iter() {
        let sig_data = sig_entry.value();

        let is_historical = sig_data
            .values()
            .any(|tx| tx.wallclock_secs < tx.start_wallclock_secs);

        if is_historical {
            for endpoint in sig_data.keys() {
                if let Some(stats) = endpoint_stats.get_mut(endpoint) {
                    stats.backfill_transactions += 1;
                }
            }
            continue;
        }

        if let Some((first_endpoint, first_tx)) =
            sig_data.iter().min_by_key(|(_, tx)| tx.elapsed_since_start)
        {
            if let Some(stats) = endpoint_stats.get_mut(first_endpoint) {
                stats.first_detections += 1;
                stats.total_valid_transactions += 1;
            }

            for (endpoint, tx) in sig_data.iter() {
                if endpoint != first_endpoint
                    && let Some(stats) = endpoint_stats.get_mut(endpoint)
                {
                    let delay_ms = diff_ms(tx, first_tx);
                    stats.delays.push(delay_ms);
                    stats.total_valid_transactions += 1;
                }
            }
        }
    }

    let total_first_detections: usize = endpoint_stats
        .values()
        .map(|stats| stats.first_detections)
        .sum();

    let endpoints: Vec<EndpointSummary> = endpoint_stats
        .into_iter()
        .map(|(endpoint, stats)| build_summary(endpoint, stats, total_first_detections))
        .collect();

    let has_data = endpoints
        .iter()
        .any(|summary| summary.valid_transactions > 0);

    let fastest_endpoint = endpoints
        .iter()
        .filter(|summary| summary.valid_transactions > 0)
        .min_by(|a, b| match (a.p50_delay_ms, b.p50_delay_ms) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(lhs), Some(rhs)) => lhs.partial_cmp(&rhs).unwrap_or(std::cmp::Ordering::Equal),
        })
        .map(|summary| summary.name.clone());

    RunSummary {
        endpoints,
        fastest_endpoint,
        has_data,
    }
}

pub fn display_run_summary(summary: &RunSummary) {
    println!("\nFinished test results");
    println!("--------------------------------------------");

    if !summary.has_data {
        println!("Not enough data");
    } else {
        let fastest_name_ref = summary.fastest_endpoint.as_deref();
        let mut summary_rows: Vec<&EndpointSummary> = summary.endpoints.iter().collect();
        summary_rows.sort_by(|a, b| {
            b.first_share
                .partial_cmp(&a.first_share)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| match (a.p50_delay_ms, b.p50_delay_ms) {
                    (Some(lhs), Some(rhs)) => {
                        lhs.partial_cmp(&rhs).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.name.cmp(&b.name),
                })
        });

        for summary in summary_rows {
            if summary.valid_transactions == 0 {
                println!("{}: Not enough data", summary.name);
                continue;
            }

            let raw_win_rate = format_percent(summary.first_share);
            let win_rate = if raw_win_rate == "—" {
                raw_win_rate
            } else {
                format!("{}%", raw_win_rate)
            };
            let is_fastest = fastest_name_ref == Some(summary.name.as_str());

            if is_fastest {
                println!(
                    "{}: Win rate {}, p50 0.00ms (fastest)",
                    summary.name, win_rate,
                );
            } else {
                let p50_delay = summary
                    .p50_delay_ms
                    .map(|v| format!("{:.2}ms", v))
                    .unwrap_or_else(|| "—".to_string());
                println!("{}: Win rate {}, p50 {}", summary.name, win_rate, p50_delay);
            }
        }
    }

    println!("\nDetailed test results");
    println!("--------------------------------------------");

    if !summary.has_data {
        println!("Not enough data");
        return;
    }

    let mut table_rows: Vec<&EndpointSummary> = summary.endpoints.iter().collect();
    table_rows.sort_by(|a, b| match (a.p50_delay_ms, b.p50_delay_ms) {
        (Some(lhs), Some(rhs)) => lhs.partial_cmp(&rhs).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });

    let mut table = Table::new();
    table.load_preset(table_preset());
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        "Endpoint", "First %", "P50 ms", "P95 ms", "P99 ms", "Valid Tx", "Firsts", "Backfill",
    ]);

    let fastest_name_ref = summary.fastest_endpoint.as_deref();
    for summary in table_rows {
        let is_fastest = fastest_name_ref == Some(summary.name.as_str());
        table.add_row(vec![
            summary.name.clone(),
            format_percent(summary.first_share),
            format_latency_value(summary.p50_delay_ms, is_fastest),
            format_latency_value(summary.p95_delay_ms, is_fastest),
            format_latency_value(summary.p99_delay_ms, is_fastest),
            summary.valid_transactions.to_string(),
            summary.first_detections.to_string(),
            summary.backfill_transactions.to_string(),
        ]);
    }

    println!("{}", table);
}

fn diff_ms(tx: &TransactionData, first_tx: &TransactionData) -> f64 {
    let delta: Duration = tx
        .elapsed_since_start
        .saturating_sub(first_tx.elapsed_since_start);
    delta.as_secs_f64() * 1_000.0
}

fn build_summary(
    endpoint: String,
    stats: EndpointStats,
    total_first_detections: usize,
) -> EndpointSummary {
    let mut summary = EndpointSummary {
        name: endpoint,
        valid_transactions: stats.total_valid_transactions,
        first_detections: stats.first_detections,
        backfill_transactions: stats.backfill_transactions,
        ..Default::default()
    };

    if total_first_detections > 0 {
        summary.first_share = stats.first_detections as f64 / total_first_detections as f64;
    }

    if !stats.delays.is_empty() {
        let mut sorted = stats.delays.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        summary.p50_delay_ms = Some(percentile(&sorted, 0.5));
        summary.p95_delay_ms = Some(percentile(&sorted, 0.95));
        summary.p99_delay_ms = Some(percentile(&sorted, 0.99));
    }

    summary
}

fn format_latency_value(value: Option<f64>, is_fastest: bool) -> String {
    if is_fastest {
        "-".to_string()
    } else {
        value
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "—".to_string())
    }
}

fn format_percent(value: f64) -> String {
    if value.is_finite() {
        format!("{:.2}", value * 100.0)
    } else {
        "—".to_string()
    }
}
