use std::{
    env, fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
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
use sysinfo::{Disks, System};

const ALLOWED_BASES_ENV: &str = "SYS_PULSE_ALLOWED_BASES";

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

/// Read-only system inspection tools exposed over MCP.
#[derive(Clone)]
pub struct SysPulseServer {
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl SysPulseServer {
    /// Create a server with its tool and prompt routers.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
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
        match system_overview_data() {
            Ok(data) => CallToolResult::structured(data),
            Err(message) => CallToolResult::structured_error(json!({
                "error_code": "SYSTEM_INSPECTION_FAILED",
                "message": message,
            })),
        }
    }

    /// List mounted disks and their capacity.
    #[tool(
        description = "List mounted disks and partitions with filesystem, mount point, total space, and available space.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn disk_usage(&self, Parameters(_input): Parameters<DiskUsageInput>) -> CallToolResult {
        let disks = match Disks::new_with_refreshed_list() {
            Ok(disks) => disks,
            Err(error) => {
                return CallToolResult::structured_error(json!({
                    "error_code": "DISK_INSPECTION_FAILED",
                    "message": error.to_string(),
                }));
            }
        };
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

        CallToolResult::structured(json!({ "disks": entries }))
    }

    /// Return the current branch, dirty-file count, and latest commit for a repository.
    #[tool(
        description = "Inspect a Git repository located beneath an allowed base path. Paths must be relative and cannot traverse outside configured bases.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    fn repo_status(&self, Parameters(input): Parameters<RepoStatusInput>) -> CallToolResult {
        match inspect_repo(&input.path) {
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
        let telemetry = system_overview_data()
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
        let status = inspect_repo(&arguments.path)
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
                .with_description("A fresh snapshot of CPU, memory, operating system, and uptime.")
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
            let snapshot = system_overview_data().map_err(|error| {
                ErrorData::internal_error(format!("Could not collect telemetry: {error}"), None)
            })?;
            let text = serde_json::to_string_pretty(&snapshot).map_err(|error| {
                ErrorData::internal_error(format!("Could not serialize telemetry: {error}"), None)
            })?;
            ResourceContents::text(text, request.uri)
        } else if let Some(path) = request.uri.strip_prefix("sys://repositories/") {
            let status = inspect_repo(path).map_err(|error| {
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

pub(crate) fn system_overview_data() -> Result<Value, String> {
    let mut system = System::new().map_err(|error| error.to_string())?;
    system.refresh_cpu_all();
    std::thread::sleep(std::time::Duration::from_millis(200));
    system.refresh_cpu_all();
    system.refresh_memory();

    let os_name = System::name().map_err(|error| error.to_string())?;
    let os_version = System::os_version().map_err(|error| error.to_string())?;
    let os_long_version = System::long_os_version().map_err(|error| error.to_string())?;
    let host_name = System::host_name().map_err(|error| error.to_string())?;
    let uptime_seconds = System::uptime().map_err(|error| error.to_string())?;

    Ok(json!({
        "cpu_count": system.cpus().len(),
        "cpu_usage_percent": system.global_cpu_usage(),
        "memory_total_bytes": system.total_memory(),
        "memory_available_bytes": system.available_memory(),
        "os_name": os_name,
        "os_version": os_version,
        "os_long_version": os_long_version,
        "host_name": host_name,
        "uptime_seconds": uptime_seconds,
    }))
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

fn inspect_repo(requested_path: &str) -> Result<Value, RepoStatusError> {
    let bases = allowed_bases()?;
    let repository = resolve_repo_path(requested_path, &bases)?;

    let probe = run_git(&repository, ["rev-parse", "--is-inside-work-tree"])?;
    if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Err(RepoStatusError::new(
            "NOT_A_GIT_REPOSITORY",
            "The requested directory is not inside a Git working tree.",
        ));
    }

    let branch_output = run_git(&repository, ["branch", "--show-current"])?;
    if !branch_output.status.success() {
        return Err(git_command_error(&branch_output));
    }
    let branch = nonempty_stdout(&branch_output);

    let status_output = run_git(
        &repository,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status_output.status.success() {
        return Err(git_command_error(&status_output));
    }
    let dirty_file_count = count_dirty_files(&status_output.stdout);

    let commit_output = run_git(&repository, ["log", "-1", "--pretty=format:%h %s"])?;
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

fn allowed_bases() -> Result<Vec<PathBuf>, RepoStatusError> {
    let configured = env::var_os(ALLOWED_BASES_ENV);
    let bases: Vec<PathBuf> = match configured {
        Some(value) => env::split_paths(&value).collect(),
        None => vec![env::current_dir().map_err(|error| {
            RepoStatusError::new(
                "PATH_CONFIGURATION_ERROR",
                format!("Could not determine the server working directory: {error}"),
            )
        })?],
    };

    if bases.is_empty() {
        return Err(RepoStatusError::new(
            "PATH_CONFIGURATION_ERROR",
            format!("{ALLOWED_BASES_ENV} must contain at least one directory."),
        ));
    }

    bases
        .into_iter()
        .map(|base| {
            let canonical = fs::canonicalize(&base).map_err(|error| {
                RepoStatusError::new(
                    "PATH_CONFIGURATION_ERROR",
                    format!("Could not resolve an allowed base directory: {error}"),
                )
            })?;
            if !canonical.is_dir() {
                return Err(RepoStatusError::new(
                    "PATH_CONFIGURATION_ERROR",
                    "Every allowed base path must be a directory.",
                ));
            }
            Ok(canonical)
        })
        .collect()
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
        DiagnoseBottleneckArguments, PrePushCheckArguments, RepoStatusError, SysPulseServer,
        count_dirty_files, resolve_repo_path,
    };
    use rmcp::{ServerHandler, handler::server::wrapper::Parameters};
    use std::{
        fs,
        path::PathBuf,
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
        let server = SysPulseServer::new();
        let info = server.get_info();

        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.prompts.is_some());
        assert!(info.capabilities.resources.is_some());
    }

    #[test]
    fn bottleneck_prompt_includes_symptom_and_optional_workload() {
        let server = SysPulseServer::new();
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
        let server = SysPulseServer::new();
        let prompt = server.pre_push_check(Parameters(PrePushCheckArguments {
            path: "../outside".to_owned(),
        }));
        let output = serde_json::to_string(&prompt).expect("prompt serializes");

        assert!(output.contains("read-only"));
        assert!(output.contains("INVALID_PATH"));
    }
}
