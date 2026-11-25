use std::{
    collections::BTreeMap,
    collections::VecDeque,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState, get_sockets_info};
use nvml_wrapper::{Nvml, enum_wrappers::device::TemperatureSensor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sysinfo::{
    Disks, MINIMUM_CPU_UPDATE_INTERVAL, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind,
};

pub const DEFAULT_HISTORY_CAPACITY: usize = 150;

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessSort {
    #[default]
    Cpu,
    Memory,
}

pub trait TelemetryProvider: Send + Sync + 'static {
    fn system_overview(&self) -> Result<Value, String>;
    fn disk_usage(&self) -> Result<Value, String>;
    fn top_processes(&self, limit: usize, sort: ProcessSort) -> Result<Vec<Value>, String>;
    fn gpu_telemetry(&self) -> Result<Vec<Value>, String>;
    fn port_owners(&self, ports: &[u16]) -> Result<Vec<Value>, String>;
    fn sample(&self) -> Result<TelemetrySample, String>;
}

pub struct SystemTelemetryProvider {
    system: Mutex<System>,
}

impl SystemTelemetryProvider {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            system: Mutex::new(System::new().map_err(|error| error.to_string())?),
        })
    }

    fn with_system<T>(
        &self,
        operation: impl FnOnce(&mut System) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut system = self
            .system
            .lock()
            .map_err(|error| format!("System telemetry lock is poisoned: {error}"))?;
        operation(&mut system)
    }
}

impl TelemetryProvider for SystemTelemetryProvider {
    fn system_overview(&self) -> Result<Value, String> {
        self.with_system(|system| {
            system.refresh_cpu_all();
            std::thread::sleep(Duration::from_millis(200));
            system.refresh_cpu_all();
            system.refresh_memory();
            Ok(json!({
                "cpu_count": system.cpus().len(),
                "cpu_usage_percent": system.global_cpu_usage(),
                "memory_total_bytes": system.total_memory(),
                "memory_available_bytes": system.available_memory(),
                "os_name": System::name().map_err(|error| error.to_string())?,
                "os_version": System::os_version().map_err(|error| error.to_string())?,
                "os_long_version": System::long_os_version().map_err(|error| error.to_string())?,
                "host_name": System::host_name().map_err(|error| error.to_string())?,
                "uptime_seconds": System::uptime().map_err(|error| error.to_string())?,
            }))
        })
    }

    fn disk_usage(&self) -> Result<Value, String> {
        let disks = Disks::new_with_refreshed_list().map_err(|error| error.to_string())?;
        let entries: Vec<Value> = disks
            .iter()
            .map(|disk| {
                json!({
                    "name": disk.name().to_string_lossy(),
                    "file_system": disk.file_system().to_string_lossy(),
                    "mount_point": disk.mount_point().to_string_lossy(),
                    "total_space_bytes": disk.total_space(),
                    "available_space_bytes": disk.available_space(),
                })
            })
            .collect();
        Ok(json!({ "disks": entries }))
    }

    fn top_processes(&self, limit: usize, sort: ProcessSort) -> Result<Vec<Value>, String> {
        self.with_system(|system| {
            let refresh = ProcessRefreshKind::nothing().with_cpu().with_memory();
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
            std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);

            let mut processes: Vec<Value> = system
                .processes()
                .iter()
                .map(|(pid, process)| {
                    json!({
                        "pid": pid.as_u32(),
                        "name": process.name().to_string_lossy(),
                        "cpu_usage_percent": process.cpu_usage(),
                        "memory_bytes": process.memory(),
                    })
                })
                .collect();
            match sort {
                ProcessSort::Cpu => processes.sort_by(|left, right| {
                    right["cpu_usage_percent"]
                        .as_f64()
                        .unwrap_or_default()
                        .total_cmp(&left["cpu_usage_percent"].as_f64().unwrap_or_default())
                }),
                ProcessSort::Memory => processes.sort_by_key(|process| {
                    std::cmp::Reverse(process["memory_bytes"].as_u64().unwrap_or_default())
                }),
            }
            processes.truncate(limit);
            Ok(processes)
        })
    }

    fn gpu_telemetry(&self) -> Result<Vec<Value>, String> {
        let nvml = Nvml::init().map_err(|error| error.to_string())?;
        let count = nvml.device_count().map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("NVML found no NVIDIA GPUs.".to_owned());
        }

        (0..count)
            .map(|index| {
                let device = nvml
                    .device_by_index(index)
                    .map_err(|error| error.to_string())?;
                let memory = device.memory_info().map_err(|error| error.to_string())?;
                let temperature = device
                    .temperature(TemperatureSensor::Gpu)
                    .map_err(|error| error.to_string())?;
                let utilization = device
                    .utilization_rates()
                    .map_err(|error| error.to_string())?;
                Ok(json!({
                    "index": index,
                    "name": device.name().map_err(|error| error.to_string())?,
                    "vram_total_bytes": memory.total,
                    "vram_used_bytes": memory.used,
                    "vram_free_bytes": memory.free,
                    "temperature_celsius": temperature,
                    "gpu_utilization_percent": utilization.gpu,
                    "memory_utilization_percent": utilization.memory,
                }))
            })
            .collect()
    }

    fn port_owners(&self, ports: &[u16]) -> Result<Vec<Value>, String> {
        let sockets = get_sockets_info(
            AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
            ProtocolFlags::TCP,
        )
        .map_err(|error| format!("Could not inspect local TCP sockets: {error}"))?;
        self.with_system(|system| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
            );
            Ok(ports
                .iter()
                .map(|port| {
                    let mut owners = BTreeMap::<u32, Value>::new();
                    let listening = sockets.iter().any(|socket| {
                        matches!(
                            &socket.protocol_socket_info,
                            ProtocolSocketInfo::Tcp(tcp)
                                if tcp.local_port == *port && tcp.state == TcpState::Listen
                        )
                    });
                    for socket in &sockets {
                        let ProtocolSocketInfo::Tcp(tcp) = &socket.protocol_socket_info else {
                            continue;
                        };
                        if tcp.local_port != *port || tcp.state != TcpState::Listen {
                            continue;
                        }
                        for pid in &socket.associated_pids {
                            let process = system.processes().iter().find(|(candidate, _)| {
                                candidate.as_u32() == *pid
                            });
                            let owner = match process {
                                Some((_, process)) => json!({
                                    "pid": pid,
                                    "process_name": process.name().to_string_lossy(),
                                    "command_line": process.cmd().iter()
                                        .map(|part| part.to_string_lossy())
                                        .collect::<Vec<_>>()
                                        .join(" "),
                                    "attribution_status": "available",
                                }),
                                None => json!({
                                    "pid": pid,
                                    "process_name": null,
                                    "command_line": null,
                                    "attribution_status": "process_details_unavailable",
                                }),
                            };
                            owners.insert(*pid, owner);
                        }
                    }
                    let owners: Vec<Value> = owners.into_values().collect();
                    json!({
                        "port": port,
                        "listening": listening,
                        "owners": owners,
                        "attribution_available": !owners.is_empty(),
                        "attribution_note": if owners.is_empty() && listening {
                            Some("The socket is listening, but the operating system did not expose its PID. Elevated permissions may be required.")
                        } else {
                            None
                        },
                    })
                })
                .collect())
        })
    }

    fn sample(&self) -> Result<TelemetrySample, String> {
        self.with_system(|system| {
            system.refresh_cpu_all();
            system.refresh_memory();
            let timestamp_unix_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| format!("System clock is before Unix epoch: {error}"))?
                .as_millis()
                .min(u64::MAX as u128) as u64;
            Ok(TelemetrySample {
                timestamp_unix_ms,
                cpu_usage_percent: f64::from(system.global_cpu_usage()),
                memory_used_bytes: system.used_memory(),
                memory_total_bytes: system.total_memory(),
                swap_used_bytes: system.used_swap(),
                swap_total_bytes: system.total_swap(),
            })
        })
    }
}

#[derive(Default)]
#[cfg(test)]
pub struct MockTelemetryProvider {
    sample_index: AtomicU64,
}

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
impl TelemetryProvider for MockTelemetryProvider {
    fn system_overview(&self) -> Result<Value, String> {
        Ok(json!({
            "cpu_count": 4,
            "cpu_usage_percent": 25.0,
            "memory_total_bytes": 1_073_741_824u64,
            "memory_available_bytes": 536_870_912u64,
            "os_name": "MockOS",
            "os_version": "1.0",
            "os_long_version": "MockOS 1.0",
            "host_name": "mock-host",
            "uptime_seconds": 3600,
        }))
    }

    fn disk_usage(&self) -> Result<Value, String> {
        Ok(json!({ "disks": [] }))
    }

    fn top_processes(&self, limit: usize, sort: ProcessSort) -> Result<Vec<Value>, String> {
        let mut processes = vec![
            json!({"pid": 7, "name": "mock-worker", "cpu_usage_percent": 12.5, "memory_bytes": 4096}),
            json!({"pid": 8, "name": "mock-indexer", "cpu_usage_percent": 25.0, "memory_bytes": 2048}),
        ];
        match sort {
            ProcessSort::Cpu => processes.sort_by(|left, right| {
                right["cpu_usage_percent"]
                    .as_f64()
                    .unwrap_or_default()
                    .total_cmp(&left["cpu_usage_percent"].as_f64().unwrap_or_default())
            }),
            ProcessSort::Memory => processes.sort_by_key(|process| {
                std::cmp::Reverse(process["memory_bytes"].as_u64().unwrap_or_default())
            }),
        }
        processes.truncate(limit);
        Ok(processes)
    }

    fn gpu_telemetry(&self) -> Result<Vec<Value>, String> {
        Ok(vec![json!({
            "index": 0,
            "name": "Mock GPU",
            "vram_total_bytes": 8_589_934_592u64,
            "vram_used_bytes": 2_147_483_648u64,
            "vram_free_bytes": 6_442_450_944u64,
            "temperature_celsius": 45,
            "gpu_utilization_percent": 25,
            "memory_utilization_percent": 10,
        })])
    }

    fn port_owners(&self, ports: &[u16]) -> Result<Vec<Value>, String> {
        Ok(ports
            .iter()
            .map(|port| {
                json!({
                    "port": port,
                    "listening": *port == 8080,
                    "owners": if *port == 8080 {
                        vec![json!({
                            "pid": 7,
                            "process_name": "mock-worker",
                            "command_line": "mock-worker --serve",
                            "attribution_status": "available",
                        })]
                    } else {
                        Vec::new()
                    },
                    "attribution_available": *port == 8080,
                    "attribution_note": null,
                })
            })
            .collect())
    }

    fn sample(&self) -> Result<TelemetrySample, String> {
        let index = self.sample_index.fetch_add(1, Ordering::Relaxed);
        Ok(TelemetrySample {
            timestamp_unix_ms: 1_000 + index * 2_000,
            cpu_usage_percent: 25.0,
            memory_used_bytes: 512 * 1024 * 1024 + index * 1024,
            memory_total_bytes: 1_073_741_824,
            swap_used_bytes: 64 * 1024 * 1024,
            swap_total_bytes: 256 * 1024 * 1024,
        })
    }
}

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

pub async fn run_sampler(
    provider: Arc<dyn TelemetryProvider>,
    history: Arc<RwLock<SampleHistory>>,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    tracing::info!(
        interval_seconds = interval.as_secs(),
        capacity = DEFAULT_HISTORY_CAPACITY,
        "Started telemetry sampler"
    );
    loop {
        ticker.tick().await;
        let sample = match provider.sample() {
            Ok(sample) => sample,
            Err(error) => {
                tracing::warn!(%error, "Could not collect telemetry sample");
                continue;
            }
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
