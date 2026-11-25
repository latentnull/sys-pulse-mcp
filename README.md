# sys-pulse-mcp

A read-only, stdio-based [Model Context Protocol](https://modelcontextprotocol.io/)
server for local system telemetry, Git status, and practical diagnostics. Built
with Rust, [`rmcp`](https://crates.io/crates/rmcp), Tokio, and `sysinfo`.

## Features

- System CPU, memory, operating-system, and uptime telemetry.
- Mounted disk usage and top-process inspection by CPU or resident memory.
- Local loopback TCP listening-port checks and optional NVIDIA GPU telemetry.
- Allowlisted, read-only Git repository status and a corresponding MCP resource.
- Structured diagnosis and pre-push prompt workflows.
- Stdio JSON-RPC is kept on stdout; diagnostic logs are sent to stderr.

## Requirements

- Rust 1.95 or newer.
- Git on `PATH` for repository tools and resources.
- NVIDIA drivers/NVML and a supported NVIDIA GPU only for `gpu_telemetry`.
  Other tools work without NVIDIA hardware.

## Build and run

```powershell
cargo build --release
cargo run --release -- --allow-dir C:\Work\Projects
```

`--allow-dir <path>` may be repeated to authorize multiple repository roots.
When no CLI paths are supplied, the server reads `SYS_PULSE_ALLOWED_BASES`
(platform-separated paths); if that variable is unset, it uses the process
working directory. Configured directories are canonicalized and validated at
startup. Repository lookups reject absolute paths, parent traversal, and paths
that resolve outside those bases.

The server communicates over standard input/output and is intended to be
launched by an MCP client. Do not pipe other application output to stdout.
Tracing output is configured to stderr.

## MCP tools

All tools are read-only and use object-shaped JSON input schemas.

| Tool | Input | Result |
| --- | --- | --- |
| `system_overview` | `{}` | CPU count and usage, total/available RAM, OS details, host name, and uptime. |
| `disk_usage` | `{}` | Mounted disks with filesystem, mount point, and total/available bytes. |
| `repo_status` | `{"path":"relative/repo"}` | Canonical path, branch, dirty-file count, and latest commit summary. |
| `top_processes` | `{"limit":10,"sort":"cpu"}` | Up to 100 processes, ordered by `cpu` or `memory`; limit defaults to 10. |
| `port_check` | `{"ports":[80,443]}` | Whether each requested port accepts TCP connections on `127.0.0.1`. |
| `gpu_telemetry` | `{}` | NVIDIA GPU name, VRAM, temperature, and utilization using NVML. |

`port_check` only tests loopback and accepts at most 64 ports per call; it does
not probe remote hosts. `gpu_telemetry` returns a structured tool-level error
(`GPU_TELEMETRY_UNAVAILABLE`) when NVML or a supported NVIDIA GPU is unavailable.
Likewise, invalid or non-repository paths return structured tool-level errors
instead of failing the MCP protocol request.

## MCP resources

- `sys://telemetry/snapshot` provides a fresh JSON snapshot of CPU, memory, OS,
  host, and uptime data.
- The `sys://repositories/{path}` resource template provides JSON Git status
  for a repository beneath an allowed base directory. The `{path}` value must
  be relative and cannot traverse outside configured roots.

## MCP prompts

- `diagnose_bottleneck` accepts required `symptom` and optional `workload`
  arguments. It combines the described symptom with current telemetry and
  guides a focused, read-only investigation.
- `pre_push_check` accepts a repository `path` relative to an allowed base and
  creates a read-only review prompt from the current branch, dirty-file count,
  and latest commit.

Both prompts return structured MCP `GetPromptResult` messages. They suggest
analysis and checks; they do not run shell commands or modify repositories.

## MCP client configuration

Example stdio-server configuration for an MCP client that accepts `command` and
`args`:

```json
{
  "mcpServers": {
    "sys-pulse": {
      "command": "C:\\Work\\Projects\\sys-pulse-mcp\\target\\release\\sys-pulse-mcp.exe",
      "args": [
        "--allow-dir",
        "C:\\Work\\Projects"
      ]
    }
  }
}
```

Repeat the `--allow-dir`, `<path>` pair in `args` to authorize additional
directories. Alternatively, omit those arguments and configure
`SYS_PULSE_ALLOWED_BASES` in the MCP client's `env` object.

## Architecture

- `src/main.rs` configures stderr-only tracing, parses CLI allowlist options,
  creates the MCP service, and serves it over rmcp's stdio transport.
- `src/tools.rs` contains the typed tool and prompt routers, MCP resource
  handlers, telemetry/Git inspection, and path-validation logic.
- `SYS_PULSE_ALLOWED_BASES` and repeatable `--allow-dir` options define the
  repository access boundary.

The server performs no writes to inspected repositories and invokes Git only
with argument arrays (never through a shell). Process and port diagnostics are
restricted to local observation; port checks connect only to IPv4 loopback.

## Development

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

GitHub Actions runs formatting, Clippy, and tests on pushes and pull requests.
See [CHANGELOG.md](CHANGELOG.md) for the milestone-by-milestone feature history.
