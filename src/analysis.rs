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

#[derive(Debug, Default)]
struct EndpointSummary {
    name: String,
    first_share: f64,
    avg_delay_ms: Option<f64>,
    median_delay_ms: Option<f64>,
    p95_delay_ms: Option<f64>,
    min_delay_ms: Option<f64>,
    max_delay_ms: Option<f64>,
    valid_transactions: usize,
    first_detections: usize,
    backfill_transactions: usize,
}

pub fn analyze_delays(comparator: &Comparator, endpoint_names: &[String]) {
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

    let summaries: Vec<EndpointSummary> = endpoint_stats
        .into_iter()
        .map(|(endpoint, stats)| build_summary(endpoint, stats, total_first_detections))
        .collect();

    let mut summary_rows: Vec<&EndpointSummary> = summaries.iter().collect();
    summary_rows.sort_by(|a, b| {
        b.first_share
            .partial_cmp(&a.first_share)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| match (a.avg_delay_ms, b.avg_delay_ms) {
                (Some(lhs), Some(rhs)) => {
                    lhs.partial_cmp(&rhs).unwrap_or(std::cmp::Ordering::Equal)
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            })
    });

    println!("\nFinished test results");
    println!("--------------------------------------------");

    let has_data = summary_rows
        .iter()
        .any(|summary| summary.valid_transactions > 0);

    if !has_data {
        println!("Not enough data");
    } else {
        let fastest_summary = summary_rows
            .iter()
            .copied()
            .find(|summary| summary.valid_transactions > 0);
        let fastest_name = fastest_summary.map(|summary| summary.name.as_str());

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
            let mut avg_delay = summary.avg_delay_ms.unwrap_or(0.0);
            let is_fastest = fastest_name == Some(summary.name.as_str());

            if is_fastest {
                avg_delay = 0.0;
                println!(
                    "{}: Win rate {}, avg delay {:.2}ms (fastest)",
                    summary.name, win_rate, avg_delay
                );
            } else {
                println!(
                    "{}: Win rate {}, avg delay {:.2}ms",
                    summary.name, win_rate, avg_delay
                );
            }
        }
    }

    println!("\nDetailed test results");
    println!("--------------------------------------------");

    if !has_data {
        println!("Not enough data");
        return;
    }

    let mut table_rows: Vec<&EndpointSummary> = summaries.iter().collect();
    table_rows.sort_by(|a, b| match (a.avg_delay_ms, b.avg_delay_ms) {
        (Some(lhs), Some(rhs)) => lhs.partial_cmp(&rhs).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });

    let mut table = Table::new();
    table.load_preset(table_preset());
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        "Endpoint",
        "First %",
        "Avg ms",
        "Median ms",
        "P95 ms",
        "Min/Max ms",
        "Valid Tx",
        "Firsts",
        "Backfill",
    ]);

    for summary in table_rows {
        table.add_row(vec![
            summary.name.clone(),
            format_percent(summary.first_share),
            format_option(summary.avg_delay_ms),
            format_option(summary.median_delay_ms),
            format_option(summary.p95_delay_ms),
            format_min_max(summary.min_delay_ms, summary.max_delay_ms),
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
        summary.avg_delay_ms = Some(sorted.iter().sum::<f64>() / sorted.len() as f64);
        summary.median_delay_ms = Some(percentile(&sorted, 0.5));
        summary.p95_delay_ms = Some(percentile(&sorted, 0.95));
        summary.min_delay_ms = Some(*sorted.first().unwrap());
        summary.max_delay_ms = Some(*sorted.last().unwrap());
    }

    summary
}

fn format_option(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "—".to_string())
}

fn format_percent(value: f64) -> String {
    if value.is_finite() {
        format!("{:.2}", value * 100.0)
    } else {
        "—".to_string()
    }
}

fn format_min_max(min: Option<f64>, max: Option<f64>) -> String {
    match (min, max) {
        (Some(mi), Some(ma)) => format!("{:.2}/{:.2}", mi, ma),
        _ => "—".to_string(),
    }
}
