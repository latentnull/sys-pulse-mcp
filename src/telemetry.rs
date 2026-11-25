use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sysinfo::System;

pub const DEFAULT_HISTORY_CAPACITY: usize = 150;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TelemetrySample {
    pub timestamp_unix_ms: u64,
    pub cpu_usage_percent: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct SampleHistory {
    capacity: usize,
    samples: VecDeque<TelemetrySample>,
}

impl Default for SampleHistory {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_CAPACITY)
    }
}

impl SampleHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            samples: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn push(&mut self, sample: TelemetrySample) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn trends(&self) -> Value {
        let samples = &self.samples;
        if samples.is_empty() {
            return json!({
                "sample_count": 0,
                "samples": [],
                "summary": null,
            });
        }

        let mut cpu: Vec<f64> = samples
            .iter()
            .map(|sample| sample.cpu_usage_percent)
            .collect();
        cpu.sort_by(f64::total_cmp);
        let cpu_min = cpu[0];
        let cpu_max = cpu[cpu.len() - 1];
        let cpu_avg = cpu.iter().sum::<f64>() / cpu.len() as f64;
        let p95_index = ((cpu.len() as f64 * 0.95).ceil() as usize)
            .saturating_sub(1)
            .min(cpu.len() - 1);
        let memory = samples
            .iter()
            .map(|sample| sample.memory_used_bytes)
            .collect::<Vec<_>>();
        let memory_min = *memory.iter().min().expect("non-empty samples");
        let memory_max = *memory.iter().max().expect("non-empty samples");
        let memory_avg =
            memory.iter().map(|value| *value as u128).sum::<u128>() as f64 / memory.len() as f64;
        let memory_delta_mb_per_minute = memory_delta_mb_per_minute(samples);
        let rolling = json!({
            "cpu_usage_percent": {
                "min": cpu_min,
                "avg": cpu_avg,
                "max": cpu_max,
                "p95": cpu[p95_index],
            },
            "memory_used_bytes": {
                "min": memory_min,
                "avg": memory_avg,
                "max": memory_max,
                "delta_mb_per_minute": memory_delta_mb_per_minute,
            },
            "swap_used_bytes": {
                "min": samples.iter().map(|sample| sample.swap_used_bytes).min(),
                "avg": samples
                    .iter()
                    .map(|sample| sample.swap_used_bytes as u128)
                    .sum::<u128>() as f64
                    / samples.len() as f64,
                "max": samples.iter().map(|sample| sample.swap_used_bytes).max(),
            },
        });

        json!({
            "sample_count": samples.len(),
            "capacity": self.capacity,
            "sample_interval_note": "Timestamps indicate actual sample times.",
            "summary": rolling,
            "samples": samples,
        })
    }
}

fn memory_delta_mb_per_minute(samples: &VecDeque<TelemetrySample>) -> Option<f64> {
    let first = samples.front()?;
    let last = samples.back()?;
    let elapsed_ms = last
        .timestamp_unix_ms
        .saturating_sub(first.timestamp_unix_ms);
    if elapsed_ms == 0 || samples.len() < 2 {
        return None;
    }

    let delta_bytes = last.memory_used_bytes as i128 - first.memory_used_bytes as i128;
    Some(delta_bytes as f64 / (1024.0 * 1024.0) / (elapsed_ms as f64 / 60_000.0))
}

pub async fn run_sampler(history: Arc<RwLock<SampleHistory>>, interval: Duration) {
    let mut system = match System::new() {
        Ok(system) => system,
        Err(error) => {
            tracing::error!(%error, "Could not initialize telemetry sampler");
            return;
        }
    };
    system.refresh_cpu_all();
    system.refresh_memory();

    let mut ticker = tokio::time::interval(interval);
    tracing::info!(
        interval_seconds = interval.as_secs(),
        capacity = DEFAULT_HISTORY_CAPACITY,
        "Started telemetry sampler"
    );
    loop {
        ticker.tick().await;
        system.refresh_cpu_all();
        system.refresh_memory();
        let timestamp_unix_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_millis().min(u64::MAX as u128) as u64,
            Err(error) => {
                tracing::warn!(%error, "System clock is before Unix epoch; skipping telemetry sample");
                continue;
            }
        };
        let sample = TelemetrySample {
            timestamp_unix_ms,
            cpu_usage_percent: f64::from(system.global_cpu_usage()),
            memory_used_bytes: system.used_memory(),
            memory_total_bytes: system.total_memory(),
            swap_used_bytes: system.used_swap(),
            swap_total_bytes: system.total_swap(),
        };
        match history.write() {
            Ok(mut history) => history.push(sample),
            Err(error) => {
                tracing::error!(%error, "Telemetry history lock is poisoned; stopping sampler");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SampleHistory, TelemetrySample};

    fn sample(timestamp: u64, cpu: f64, memory: u64) -> TelemetrySample {
        TelemetrySample {
            timestamp_unix_ms: timestamp,
            cpu_usage_percent: cpu,
            memory_used_bytes: memory,
            memory_total_bytes: 100 * 1024 * 1024,
            swap_used_bytes: memory / 2,
            swap_total_bytes: 50 * 1024 * 1024,
        }
    }

    #[test]
    fn ring_buffer_evicts_oldest_samples_at_capacity() {
        let mut history = SampleHistory::new(2);
        history.push(sample(1_000, 10.0, 100));
        history.push(sample(2_000, 20.0, 200));
        history.push(sample(3_000, 30.0, 300));

        let trends = history.trends();
        let samples = trends["samples"].as_array().expect("samples array");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0]["timestamp_unix_ms"], 2_000);
        assert_eq!(samples[1]["timestamp_unix_ms"], 3_000);
    }

    #[test]
    fn trend_statistics_include_cpu_percentile_and_memory_delta() {
        let mut history = SampleHistory::new(10);
        for (index, cpu) in [1.0, 2.0, 3.0, 4.0, 100.0].into_iter().enumerate() {
            history.push(sample(
                index as u64 * 15_000,
                cpu,
                10 * 1024 * 1024 + index as u64 * 512 * 1024,
            ));
        }

        let trends = history.trends();
        assert_eq!(trends["summary"]["cpu_usage_percent"]["min"], 1.0);
        assert_eq!(trends["summary"]["cpu_usage_percent"]["max"], 100.0);
        assert_eq!(trends["summary"]["cpu_usage_percent"]["p95"], 100.0);
        assert_eq!(
            trends["summary"]["memory_used_bytes"]["delta_mb_per_minute"],
            2.0
        );
    }

    #[test]
    fn one_sample_reports_no_memory_delta() {
        let mut history = SampleHistory::new(2);
        history.push(sample(1_000, 20.0, 1_024));

        assert_eq!(
            history.trends()["summary"]["memory_used_bytes"]["delta_mb_per_minute"],
            serde_json::Value::Null
        );
    }
}
