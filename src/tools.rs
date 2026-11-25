use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, RwLock},
};

use rmcp::{
    ServerHandler,
    handler::server::{router::prompt::PromptRouter, tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ErrorData, GetPromptResult, Implementation, ListResourceTemplatesResult,
        ListResourcesResult, PromptMessage, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ResourceTemplate, Role, ServerCapabilities,
        ServerConfig,
    },
    prompt, prompt_handler, prompt_router, tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::telemetry::{ProcessSort, SampleHistory, TelemetryProvider};

const ALLOWED_BASES_ENV: &str = "SYS_PULSE_ALLOWED_BASES";
const DEFAULT_PROCESS_LIMIT: usize = 10;
const MAX_PROCESS_LIMIT: usize = 100;
const MAX_PORTS_PER_REQUEST: usize = 64;
const WORKSPACE_CACHES: [&str; 4] = ["target", "node_modules", ".venv", "dist"];

/// Empty input for the system overview tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SystemOverviewInput {}

/// Empty input for the mounted disk usage tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiskUsageInput {}

/// Input for looking up a Git repository beneath an allowed base directory.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RepoStatusInput {
    /// Path to a repository, relative to one of the configured allowed base directories.
    pub path: String,
}

/// Arguments for the bottleneck diagnosis prompt.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiagnoseBottleneckArguments {
    /// Describe the system symptom or workload to investigate.
    pub symptom: String,
    /// Optional workload details such as workload type or when it occurs.
    pub workload: Option<String>,
}

/// Arguments for the pre-push review prompt.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrePushCheckArguments {
    /// Repository path relative to an allowed base directory.
    pub path: String,
}

/// Input for listing the busiest processes.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TopProcessesInput {
    /// Maximum number of processes to return, from 1 to 100. Defaults to 10.
    pub limit: Option<usize>,
    /// Sort by `cpu` or `memory`. Defaults to `cpu`.
    pub sort: Option<ProcessSort>,
    /// Return a token-efficient summary instead of process details.
    pub compact: Option<bool>,
}

/// Input for checking local TCP listening ports.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PortCheckInput {
    /// TCP ports to check. A request may include at most 64 ports.
    pub ports: Vec<u16>,
    /// Return only port states and owner PIDs, omitting process command lines.
    pub compact: Option<bool>,
}

/// Input for GPU telemetry.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GpuTelemetryInput {
    /// Return a token-efficient summary with just per-device utilization.
    pub compact: Option<bool>,
}

/// Input for a repository and build-cache workspace summary.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkspacePulseInput {
    /// Repository path relative to an allowed base directory.
    pub path: String,
    /// Return cache totals rather than per-cache directory details.
    pub compact: Option<bool>,
}

/// Read-only system inspection tools exposed over MCP.
#[derive(Clone)]
pub struct SysPulseServer {
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
    allowed_bases: Vec<PathBuf>,
    sample_history: Arc<RwLock<SampleHistory>>,
    provider: Arc<dyn TelemetryProvider>,
}

impl SysPulseServer {
    pub fn with_provider(provider: Arc<dyn TelemetryProvider>) -> Result<Self, String> {
        let configured = match env::var_os(ALLOWED_BASES_ENV) {
            Some(value) => env::split_paths(&value).collect(),
            None => vec![
                env::current_dir()
                    .map_err(|error| format!("Could not determine current directory: {error}"))?,
            ],
        };
        Self::with_allowed_bases_and_provider(configured, provider)
    }

    pub fn with_allowed_bases_and_provider(
        allowed_bases: Vec<PathBuf>,
        provider: Arc<dyn TelemetryProvider>,
    ) -> Result<Self, String> {
        if allowed_bases.is_empty() {
            return Err("At least one allowed base directory must be configured.".to_owned());
        }
        let allowed_bases = allowed_bases
            .into_iter()
            .map(|base| {
                let canonical = fs::canonicalize(&base)
                    .map_err(|error| format!("Could not resolve allow directory: {error}"))?;
                if !canonical.is_dir() {
                    return Err(format!(
                        "Allow directory is not a directory: {}",
                        canonical.display()
                    ));
                }
                Ok(canonical)
            })
            .collect::<Result<Vec<_>, String>>()?;

        Ok(Self {
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
            allowed_bases,
            sample_history: Arc::new(RwLock::new(SampleHistory::default())),
            provider,
        })
    }

    pub fn with_sample_history(mut self, history: Arc<RwLock<SampleHistory>>) -> Self {
        self.sample_history = history;
        self
    }

    #[cfg(test)]
    fn with_mock_provider(allowed_bases: Vec<PathBuf>) -> Result<Self, String> {
        Self::with_allowed_bases_and_provider(
            allowed_bases,
            Arc::new(crate::telemetry::MockTelemetryProvider::default()),
        )
    }
}

#[tool_router]
impl SysPulseServer {
    /// Report CPU, memory, operating system, and uptime details.
    #[tool(
        description = "Return CPU count and usage, total and available RAM, OS details, and uptime.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn system_overview(
        &self,
        Parameters(_input): Parameters<SystemOverviewInput>,
    ) -> CallToolResult {
        match self.provider.system_overview() {
            Ok(data) => CallToolResult::structured(data),
            Err(message) => CallToolResult::structured_error(json!({
                "error_code": "SYSTEM_INSPECTION_FAILED",
                "message": message,
            })),
        }
    }

    /// List top processes, sorted by CPU or resident memory.
    #[tool(
        description = "List the top local processes by CPU usage or resident memory. Results are limited to at most 100 entries.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn top_processes(&self, Parameters(input): Parameters<TopProcessesInput>) -> CallToolResult {
        let limit = input.limit.unwrap_or(DEFAULT_PROCESS_LIMIT);
        if !(1..=MAX_PROCESS_LIMIT).contains(&limit) {
            return CallToolResult::structured_error(json!({
                "error_code": "INVALID_LIMIT",
                "message": format!("limit must be between 1 and {MAX_PROCESS_LIMIT}."),
            }));
        }

        match self
            .provider
            .top_processes(limit, input.sort.unwrap_or_default())
        {
            Ok(processes) => {
                let sort = input.sort.unwrap_or_default();
                let result = json!({
                    "sort": sort,
                    "processes": processes,
                });
                CallToolResult::structured(if input.compact.unwrap_or(false) {
                    compact_process_result(&result)
                } else {
                    result
                })
            }
            Err(message) => CallToolResult::structured_error(json!({
                "error_code": "PROCESS_INSPECTION_FAILED",
                "message": message,
            })),
        }
    }

    /// Check whether local TCP ports accept connections on loopback.
    #[tool(
        description = "Check a bounded list of TCP ports on local loopback only; this does not contact remote hosts.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn port_check(&self, Parameters(input): Parameters<PortCheckInput>) -> CallToolResult {
        if input.ports.is_empty() || input.ports.len() > MAX_PORTS_PER_REQUEST {
            return CallToolResult::structured_error(json!({
                "error_code": "INVALID_PORTS",
                "message": format!("Provide 1 to {MAX_PORTS_PER_REQUEST} TCP ports."),
            }));
        }
        match self.provider.port_owners(&input.ports) {
            Ok(ports) => {
                let result = json!({ "ports": ports });
                CallToolResult::structured(if input.compact.unwrap_or(false) {
                    compact_port_result(&result)
                } else {
                    result
                })
            }
            Err(message) => CallToolResult::structured_error(json!({
                "error_code": "PORT_LOOKUP_FAILED",
                "message": message,
            })),
        }
    }

    /// Return NVIDIA GPU metrics when NVML and a compatible device are available.
    #[tool(
        description = "Return NVIDIA GPU name, VRAM usage, temperature, and utilization using NVML.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn gpu_telemetry(&self, Parameters(_input): Parameters<GpuTelemetryInput>) -> CallToolResult {
        let compact = _input.compact.unwrap_or(false);
        match self.provider.gpu_telemetry() {
            Ok(gpus) => {
                let result = json!({ "gpus": gpus });
                CallToolResult::structured(if compact {
                    compact_gpu_result(&result)
                } else {
                    result
                })
            }
            Err(message) => gpu_unavailable_result(message),
        }
    }

    /// Summarize repository status and common build-cache disk usage.
    #[tool(
        description = "Report Git branch, dirty state, unpushed commit count, and sizes of common build caches beneath an allowlisted repository.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn workspace_pulse(
        &self,
        Parameters(input): Parameters<WorkspacePulseInput>,
    ) -> CallToolResult {
        match workspace_pulse_data(
            &input.path,
            &self.allowed_bases,
            input.compact.unwrap_or(false),
        ) {
            Ok(result) => CallToolResult::structured(result),
            Err(error) => CallToolResult::structured_error(json!({
                "error_code": error.code,
                "message": error.message,
            })),
        }
    }

    /// List mounted disks and their capacity.
    #[tool(
        description = "List mounted disks and partitions with filesystem, mount point, total space, and available space.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn disk_usage(&self, Parameters(_input): Parameters<DiskUsageInput>) -> CallToolResult {
        match self.provider.disk_usage() {
            Ok(disks) => CallToolResult::structured(disks),
            Err(error) => CallToolResult::structured_error(json!({
                "error_code": "DISK_INSPECTION_FAILED",
                "message": error,
            })),
        }
    }

    /// Return the current branch, dirty-file count, and latest commit for a repository.
    #[tool(
        description = "Inspect a Git repository located beneath an allowed base path. Paths must be relative and cannot traverse outside configured bases.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn repo_status(&self, Parameters(input): Parameters<RepoStatusInput>) -> CallToolResult {
        match inspect_repo(&input.path, &self.allowed_bases) {
            Ok(status) => CallToolResult::structured(status),
            Err(error) => CallToolResult::structured_error(json!({
                "error_code": error.code,
                "message": error.message,
            })),
        }
    }
}

#[prompt_router]
impl SysPulseServer {
    /// Guide a focused diagnosis using live system telemetry.
    #[prompt(
        description = "Create a structured diagnostic workflow for investigating a system bottleneck."
    )]
    fn diagnose_bottleneck(
        &self,
        Parameters(arguments): Parameters<DiagnoseBottleneckArguments>,
    ) -> GetPromptResult {
        let telemetry = self
            .provider
            .system_overview()
            .map(|data| serde_json::to_string_pretty(&data).unwrap_or_else(|_| "{}".to_owned()))
            .unwrap_or_else(|error| format!("Telemetry unavailable: {error}"));
        let workload = arguments
            .workload
            .map(|value| format!("\nWorkload details: {value}"))
            .unwrap_or_default();
        GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Diagnose this system symptom: {}\n\
                 Distinguish CPU, memory, disk, and workload causes. Separate observations \
                 from hypotheses, suggest safe read-only checks, and ask for missing evidence \
                 rather than assuming a cause.{workload}\n\n\
                 Current telemetry snapshot:\n{telemetry}",
                arguments.symptom
            ),
        )])
        .with_description("A telemetry-grounded, read-only bottleneck investigation.")
    }

    /// Prepare a structured pre-push review of an allowlisted Git repository.
    #[prompt(
        description = "Review the current status of an allowlisted Git repository before pushing."
    )]
    fn pre_push_check(
        &self,
        Parameters(arguments): Parameters<PrePushCheckArguments>,
    ) -> GetPromptResult {
        let status = inspect_repo(&arguments.path, &self.allowed_bases)
            .map(|value| serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_owned()))
            .unwrap_or_else(|error| {
                format!(
                    "Repository status unavailable ({}): {}",
                    error.code, error.message
                )
            });
        GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Review this repository before a push. Check the branch, working-tree changes, \
                 and latest commit; identify any obvious release or verification follow-ups. \
                 Do not run commands or modify files. Report findings and suggested checks.\n\n\
                 Repository status:\n{status}"
            ),
        )])
        .with_description("A read-only pre-push repository review.")
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for SysPulseServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            "sys-pulse-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Read-only local system telemetry, allowlisted repository inspection, and diagnostic prompts.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("sys://telemetry/snapshot", "System telemetry snapshot")
                .with_description(
                    "A fresh snapshot of CPU, memory, operating system, uptime, and rolling trends.",
                )
                .with_mime_type("application/json"),
            Resource::new("sys://telemetry/trends", "Telemetry trends")
                .with_description(
                    "Bounded recent CPU, memory, and swap samples with rolling statistics.",
                )
                .with_mime_type("application/json"),
        ]))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("sys://repositories/{path}", "Allowlisted repository status")
                .with_description(
                    "Read-only Git status for a repository beneath SYS_PULSE_ALLOWED_BASES.",
                )
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = if request.uri == "sys://telemetry/snapshot" {
            let mut snapshot = self.provider.system_overview().map_err(|error| {
                ErrorData::internal_error(format!("Could not collect telemetry: {error}"), None)
            })?;
            let trends = self.sample_history.read().map_err(|error| {
                ErrorData::internal_error(
                    format!("Could not read telemetry history: {error}"),
                    None,
                )
            })?;
            if let Some(snapshot) = snapshot.as_object_mut() {
                snapshot.insert("trends".to_owned(), trends.trends());
            }
            let text = serde_json::to_string_pretty(&snapshot).map_err(|error| {
                ErrorData::internal_error(format!("Could not serialize telemetry: {error}"), None)
            })?;
            ResourceContents::text(text, request.uri)
        } else if request.uri == "sys://telemetry/trends" {
            let trends = self.sample_history.read().map_err(|error| {
                ErrorData::internal_error(
                    format!("Could not read telemetry history: {error}"),
                    None,
                )
            })?;
            let text = serde_json::to_string_pretty(&trends.trends()).map_err(|error| {
                ErrorData::internal_error(format!("Could not serialize trends: {error}"), None)
            })?;
            ResourceContents::text(text, request.uri)
        } else if let Some(path) = request.uri.strip_prefix("sys://repositories/") {
            let status = inspect_repo(path, &self.allowed_bases).map_err(|error| {
                ErrorData::resource_not_found(
                    format!("Repository resource is unavailable: {}", error.message),
                    Some(json!({ "error_code": error.code })),
                )
            })?;
            let text = serde_json::to_string_pretty(&status).map_err(|error| {
                ErrorData::internal_error(
                    format!("Could not serialize repository status: {error}"),
                    None,
                )
            })?;
            ResourceContents::text(text, request.uri)
        } else {
            return Err(ErrorData::resource_not_found(
                "Unknown sys-pulse resource URI.",
                None,
            ));
        };

        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

fn gpu_unavailable_result(message: String) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error_code": "GPU_TELEMETRY_UNAVAILABLE",
        "message": format!("NVIDIA GPU telemetry is unavailable: {message}"),
    }))
}

fn compact_process_result(result: &Value) -> Value {
    let processes = result["processes"].as_array().cloned().unwrap_or_default();
    let sort = result["sort"].as_str().unwrap_or("cpu");
    let top_process = processes.first().map(|process| {
        json!({
            "pid": process["pid"],
            "name": process["name"],
            "primary_value": if sort == "memory" {
                process["memory_bytes"].clone()
            } else {
                process["cpu_usage_percent"].clone()
            },
        })
    });
    json!({
        "sort": sort,
        "returned_count": processes.len(),
        "top_process": top_process,
    })
}

fn compact_port_result(result: &Value) -> Value {
    let ports = result["ports"].as_array().cloned().unwrap_or_default();
    json!({
        "ports": ports.iter().map(|port| json!({
            "port": port["port"],
            "listening": port["listening"],
            "owner_pids": port["owners"].as_array().into_iter()
                .flatten()
                .filter_map(|owner| owner["pid"].as_u64())
                .collect::<Vec<_>>(),
            "attribution_available": port["attribution_available"],
        })).collect::<Vec<_>>(),
    })
}

fn compact_gpu_result(result: &Value) -> Value {
    let gpus = result["gpus"].as_array().cloned().unwrap_or_default();
    json!({
        "gpu_count": gpus.len(),
        "gpus": gpus.iter().map(|gpu| json!({
            "index": gpu["index"],
            "name": gpu["name"],
            "gpu_utilization_percent": gpu["gpu_utilization_percent"],
        })).collect::<Vec<_>>(),
    })
}

fn workspace_pulse_data(
    requested_path: &str,
    bases: &[PathBuf],
    compact: bool,
) -> Result<Value, RepoStatusError> {
    let repository = resolve_repo_path(requested_path, bases)?;
    let mut status = inspect_repo_at(&repository)?;
    let upstream = run_git(
        &repository,
        [
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    let (unpushed_commit_count, upstream_status) = if upstream.status.success() {
        let output = run_git(&repository, ["rev-list", "--count", "@{upstream}..HEAD"])?;
        if !output.status.success() {
            return Err(git_command_error(&output));
        }
        (
            nonempty_stdout(&output).and_then(|count| count.parse::<u64>().ok()),
            Some("configured".to_owned()),
        )
    } else {
        (None, Some("not_configured".to_owned()))
    };

    let caches = workspace_cache_sizes(&repository)?;
    let cache_total_bytes = caches
        .values()
        .filter_map(|cache| cache["size_bytes"].as_u64())
        .sum::<u64>();
    status["unpushed_commit_count"] = unpushed_commit_count
        .map(Value::from)
        .unwrap_or(Value::Null);
    status["upstream_status"] = json!(upstream_status);
    if compact {
        Ok(json!({
            "path": status["path"],
            "branch": status["branch"],
            "dirty_file_count": status["dirty_file_count"],
            "unpushed_commit_count": status["unpushed_commit_count"],
            "upstream_status": status["upstream_status"],
            "cache_count": caches.values().filter(|cache| cache["exists"] == true).count(),
            "cache_total_bytes": cache_total_bytes,
        }))
    } else {
        status["build_caches"] = json!(caches);
        status["cache_total_bytes"] = json!(cache_total_bytes);
        Ok(status)
    }
}

fn workspace_cache_sizes(repository: &Path) -> Result<BTreeMap<String, Value>, RepoStatusError> {
    WORKSPACE_CACHES
        .iter()
        .map(|name| {
            let path = repository.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    let size = directory_size_without_symlinks(&path)?;
                    Ok((
                        (*name).to_owned(),
                        json!({
                            "exists": true,
                            "size_bytes": size,
                        }),
                    ))
                }
                Ok(_) => Ok((
                    (*name).to_owned(),
                    json!({
                        "exists": false,
                        "size_bytes": 0,
                    }),
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
                    (*name).to_owned(),
                    json!({
                        "exists": false,
                        "size_bytes": 0,
                    }),
                )),
                Err(error) => Err(RepoStatusError::new(
                    "CACHE_INSPECTION_FAILED",
                    format!("Could not inspect {}: {error}", path.display()),
                )),
            }
        })
        .collect()
}

fn directory_size_without_symlinks(root: &Path) -> Result<u64, RepoStatusError> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0u64;
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            RepoStatusError::new(
                "CACHE_INSPECTION_FAILED",
                format!("Could not read {}: {error}", directory.display()),
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                RepoStatusError::new(
                    "CACHE_INSPECTION_FAILED",
                    format!("Could not inspect a cache entry: {error}"),
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                RepoStatusError::new(
                    "CACHE_INSPECTION_FAILED",
                    format!("Could not inspect {}: {error}", entry.path().display()),
                )
            })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total = total.saturating_add(
                    entry
                        .metadata()
                        .map_err(|error| {
                            RepoStatusError::new(
                                "CACHE_INSPECTION_FAILED",
                                format!(
                                    "Could not read metadata for {}: {error}",
                                    entry.path().display()
                                ),
                            )
                        })?
                        .len(),
                );
            }
        }
    }
    Ok(total)
}

#[derive(Debug)]
struct RepoStatusError {
    code: &'static str,
    message: String,
}

impl RepoStatusError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn inspect_repo(requested_path: &str, bases: &[PathBuf]) -> Result<Value, RepoStatusError> {
    let repository = resolve_repo_path(requested_path, bases)?;
    inspect_repo_at(&repository)
}

fn inspect_repo_at(repository: &Path) -> Result<Value, RepoStatusError> {
    let probe = run_git(repository, ["rev-parse", "--is-inside-work-tree"])?;
    if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Err(RepoStatusError::new(
            "NOT_A_GIT_REPOSITORY",
            "The requested directory is not inside a Git working tree.",
        ));
    }

    let branch_output = run_git(repository, ["branch", "--show-current"])?;
    if !branch_output.status.success() {
        return Err(git_command_error(&branch_output));
    }
    let branch = nonempty_stdout(&branch_output);

    let status_output = run_git(
        repository,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status_output.status.success() {
        return Err(git_command_error(&status_output));
    }
    let dirty_file_count = count_dirty_files(&status_output.stdout);

    let commit_output = run_git(repository, ["log", "-1", "--pretty=format:%h %s"])?;
    let recent_commit = if commit_output.status.success() {
        nonempty_stdout(&commit_output)
    } else if commit_output.status.code() == Some(128) {
        None
    } else {
        return Err(git_command_error(&commit_output));
    };

    Ok(json!({
        "path": repository,
        "branch": branch,
        "dirty_file_count": dirty_file_count,
        "recent_commit": recent_commit,
    }))
}

fn resolve_repo_path(requested_path: &str, bases: &[PathBuf]) -> Result<PathBuf, RepoStatusError> {
    let path = Path::new(requested_path);
    let traverses_parent = path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    if requested_path.trim().is_empty() || path.is_absolute() || traverses_parent {
        return Err(RepoStatusError::new(
            "INVALID_PATH",
            "Path must be a non-empty relative path without parent traversal.",
        ));
    }

    for base in bases {
        let candidate = match fs::canonicalize(base.join(path)) {
            Ok(candidate) => candidate,
            Err(_) => continue,
        };
        if candidate.starts_with(base) && candidate.is_dir() {
            return Ok(candidate);
        }
    }

    Err(RepoStatusError::new(
        "PATH_NOT_ALLOWED",
        "The path does not resolve to an existing directory beneath an allowed base.",
    ))
}

fn run_git<const N: usize>(
    repository: &Path,
    arguments: [&str; N],
) -> Result<Output, RepoStatusError> {
    Command::new("git")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| {
            RepoStatusError::new("GIT_UNAVAILABLE", format!("Could not run Git: {error}"))
        })
}

fn git_command_error(output: &Output) -> RepoStatusError {
    RepoStatusError::new(
        "GIT_COMMAND_FAILED",
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    )
}

fn nonempty_stdout(output: &Output) -> Option<String> {
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn count_dirty_files(status: &[u8]) -> usize {
    let mut records = status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    let mut count = 0;
    while let Some(record) = records.next() {
        if record.len() < 3 {
            continue;
        }
        count += 1;
        if record[..2].contains(&b'R') || record[..2].contains(&b'C') {
            records.next();
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnoseBottleneckArguments, PrePushCheckArguments, ProcessSort, RepoStatusError,
        SysPulseServer, TopProcessesInput, compact_gpu_result, compact_port_result,
        compact_process_result, count_dirty_files, gpu_unavailable_result, resolve_repo_path,
        workspace_cache_sizes,
    };
    use rmcp::{ServerHandler, handler::server::wrapper::Parameters};
    use serde_json::json;
    use std::{
        fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temporary_directory(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sys-pulse-mcp-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        fs::canonicalize(path).expect("temporary directory should canonicalize")
    }

    fn error_code(result: Result<PathBuf, RepoStatusError>) -> &'static str {
        result.expect_err("path should fail validation").code
    }

    #[test]
    fn resolves_directory_within_allowed_base() {
        let root = temporary_directory("allowed");
        let child = root.join("repo");
        fs::create_dir(&child).expect("child directory should be created");

        let resolved = resolve_repo_path("repo", &[root.clone()]).expect("path should resolve");
        assert_eq!(
            resolved,
            fs::canonicalize(child).expect("child should canonicalize")
        );
        fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[test]
    fn rejects_parent_traversal_and_absolute_paths() {
        let root = temporary_directory("invalid");

        assert_eq!(
            error_code(resolve_repo_path("../outside", std::slice::from_ref(&root))),
            "INVALID_PATH"
        );
        assert_eq!(
            error_code(resolve_repo_path(
                root.to_string_lossy().as_ref(),
                std::slice::from_ref(&root)
            )),
            "INVALID_PATH"
        );

        fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[test]
    fn rejects_paths_that_escape_or_do_not_exist_under_the_base() {
        let root = temporary_directory("boundary");
        let outside = temporary_directory("outside");
        let outside_name = outside
            .file_name()
            .expect("temporary directory has a name")
            .to_string_lossy();

        assert_eq!(
            error_code(resolve_repo_path(
                &outside_name,
                std::slice::from_ref(&root)
            )),
            "PATH_NOT_ALLOWED"
        );
        assert_eq!(
            error_code(resolve_repo_path("missing", std::slice::from_ref(&root))),
            "PATH_NOT_ALLOWED"
        );

        fs::remove_dir_all(root).expect("test directory should be removed");
        fs::remove_dir_all(outside).expect("test directory should be removed");
    }

    #[test]
    fn counts_changed_files_in_nul_delimited_porcelain_output() {
        let status = b" M modified file\0?? untracked file\0R  renamed file\0old name\0";
        assert_eq!(count_dirty_files(status), 3);
    }

    #[test]
    fn advertises_tools_prompts_and_resources() {
        let server = SysPulseServer::with_mock_provider(vec![
            std::env::current_dir().expect("current directory should be available"),
        ])
        .expect("mock-backed server should use its default allowed base");
        let info = server.get_info();

        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.prompts.is_some());
        assert!(info.capabilities.resources.is_some());
    }

    #[test]
    fn bottleneck_prompt_includes_symptom_and_optional_workload() {
        let server = SysPulseServer::with_mock_provider(vec![
            std::env::current_dir().expect("current directory should be available"),
        ])
        .expect("mock-backed server should use its default allowed base");
        let prompt = server.diagnose_bottleneck(Parameters(DiagnoseBottleneckArguments {
            symptom: "slow builds".to_owned(),
            workload: Some("parallel Rust compilation".to_owned()),
        }));
        let output = serde_json::to_string(&prompt).expect("prompt serializes");

        assert!(output.contains("slow builds"));
        assert!(output.contains("parallel Rust compilation"));
        assert!(output.contains("Current telemetry snapshot"));
    }

    #[test]
    fn pre_push_prompt_reports_repository_status_or_validation_error() {
        let server = SysPulseServer::with_mock_provider(vec![
            std::env::current_dir().expect("current directory should be available"),
        ])
        .expect("mock-backed server should use its default allowed base");
        let prompt = server.pre_push_check(Parameters(PrePushCheckArguments {
            path: "../outside".to_owned(),
        }));
        let output = serde_json::to_string(&prompt).expect("prompt serializes");

        assert!(output.contains("read-only"));
        assert!(output.contains("INVALID_PATH"));
    }

    #[test]
    fn mock_provider_drives_process_and_port_handlers_deterministically() {
        let server = SysPulseServer::with_mock_provider(vec![
            std::env::current_dir().expect("current directory should be available"),
        ])
        .expect("mock-backed server should initialize");
        let processes = server.top_processes(Parameters(TopProcessesInput {
            limit: Some(1),
            sort: Some(ProcessSort::Cpu),
            compact: Some(false),
        }));
        let process_data = processes
            .structured_content
            .expect("structured process result");
        assert_eq!(process_data["processes"][0]["name"], "mock-indexer");
        assert_eq!(process_data["processes"].as_array().unwrap().len(), 1);

        let ports = server.port_check(Parameters(super::PortCheckInput {
            ports: vec![8080, 9000],
            compact: Some(true),
        }));
        let port_data = ports.structured_content.expect("structured port result");
        assert_eq!(port_data["ports"][0]["listening"], true);
        assert_eq!(port_data["ports"][0]["owner_pids"][0], 7);
        assert_eq!(port_data["ports"][1]["listening"], false);
    }

    #[test]
    fn rejects_empty_or_oversized_port_requests_before_provider_lookup() {
        let server = SysPulseServer::with_mock_provider(vec![
            std::env::current_dir().expect("current directory should be available"),
        ])
        .expect("mock-backed server should initialize");
        for ports in [Vec::new(), vec![1; 65]] {
            let result = server.port_check(Parameters(super::PortCheckInput {
                ports,
                compact: None,
            }));
            assert_eq!(result.is_error, Some(true));
        }
    }

    #[test]
    fn compact_port_result_omits_process_details() {
        let result = compact_port_result(&json!({
            "ports": [{
                "port": 8080,
                "listening": true,
                "owners": [{"pid": 7, "process_name": "secret", "command_line": "details"}],
                "attribution_available": true,
            }]
        }));

        assert_eq!(result["ports"][0]["owner_pids"][0], 7);
        assert!(result["ports"][0].get("owners").is_none());
    }

    #[test]
    fn unavailable_gpu_is_returned_as_a_structured_tool_error() {
        let result = gpu_unavailable_result("NVML is unavailable".to_owned());

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .structured_content
                .expect("structured error")
                .get("error_code"),
            Some(&serde_json::Value::String(
                "GPU_TELEMETRY_UNAVAILABLE".to_owned()
            ))
        );
    }

    #[test]
    fn server_rejects_nonexistent_allow_directories() {
        let error = SysPulseServer::with_allowed_bases_and_provider(
            vec![PathBuf::from("a-directory-that-does-not-exist")],
            Arc::new(crate::telemetry::MockTelemetryProvider::default()),
        )
        .err()
        .expect("missing base directory must fail startup");

        assert!(error.contains("Could not resolve allow directory"));
    }

    #[test]
    fn cache_sizing_counts_nested_regular_files_and_ignores_missing_caches() {
        let root = temporary_directory("cache-sizing");
        let target = root.join("target");
        fs::create_dir_all(target.join("nested")).expect("cache directories created");
        fs::write(target.join("one.bin"), [0; 3]).expect("cache file written");
        fs::write(target.join("nested").join("two.bin"), [0; 5])
            .expect("nested cache file written");

        let sizes = workspace_cache_sizes(&root).expect("cache sizes inspected");
        assert_eq!(sizes["target"]["exists"], true);
        assert_eq!(sizes["target"]["size_bytes"], 8);
        assert_eq!(sizes["node_modules"]["exists"], false);
        fs::remove_dir_all(root).expect("test directory removed");
    }

    #[test]
    fn compact_diagnostic_results_keep_only_summary_fields() {
        let process_result = compact_process_result(&json!({
            "sort": "memory",
            "processes": [
                {"pid": 42, "name": "compiler", "memory_bytes": 2048, "cpu_usage_percent": 5.0},
                {"pid": 43, "name": "worker", "memory_bytes": 1024, "cpu_usage_percent": 3.0}
            ]
        }));
        assert_eq!(process_result["returned_count"], 2);
        assert_eq!(process_result["top_process"]["name"], "compiler");
        assert!(process_result.get("processes").is_none());

        let gpu_result = compact_gpu_result(&json!({
            "gpus": [{
                "index": 0,
                "name": "GPU",
                "gpu_utilization_percent": 25,
                "memory_utilization_percent": 10,
                "vram_used_bytes": 1000
            }]
        }));
        assert_eq!(gpu_result["gpu_count"], 1);
        assert!(gpu_result["gpus"][0].get("vram_used_bytes").is_none());
    }
}
