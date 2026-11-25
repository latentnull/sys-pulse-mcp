# sys-pulse-mcp

[![CI](https://github.com/Zepyrnex/sys-pulse-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/Zepyrnex/sys-pulse-mcp/actions/workflows/ci.yml)

A read-only, stdio-based [Model Context Protocol](https://modelcontextprotocol.io/)
server for local system telemetry, Git status, and practical diagnostics. Built
with Rust, [`rmcp`](https://crates.io/crates/rmcp), Tokio, and `sysinfo`.

## Features

- Current CPU, memory, swap, operating-system, and uptime telemetry.
- A background sampler with a bounded 150-sample history (about five minutes
  at the default two-second interval), rolling statistics, CPU p95, and memory
  growth rate.
- Mounted disk usage and top-process inspection by CPU or resident memory.
- Local TCP listening-port attribution to PIDs, process names, and command
  lines, with a graceful fallback when the operating system withholds details.
- Optional NVIDIA GPU telemetry through NVML.
- Allowlisted Git repository status, build-cache sizing, and unpushed commit
  counts.
- Compact output options for verbose diagnostic tools.
- Structured diagnosis and pre-push prompt workflows.
- Stdio JSON-RPC is kept on stdout; diagnostic and sampler logs go to stderr.

## Requirements

- Rust 1.95 or newer.
- Git on `PATH` for repository tools, resources, and prompts.
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

Use `--sample-interval-secs <seconds>` to change the background sampling
interval (1–3600 seconds; default 2). The equivalent environment variable is
`SYS_PULSE_SAMPLE_INTERVAL_SECS`.

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
| `top_processes` | `{"limit":10,"sort":"cpu","compact":true}` | Up to 100 processes, ordered by `cpu` or `memory`; limit defaults to 10. `compact` is optional. |
| `port_check` | `{"ports":[80,443],"compact":true}` | Listening state and process attribution for requested local TCP ports. `compact` omits names and command lines. |
| `gpu_telemetry` | `{"compact":true}` | NVIDIA GPU name, VRAM, temperature, and utilization using NVML. `compact` returns a short per-device summary. |
| `workspace_pulse` | `{"path":"relative/repo","compact":true}` | Allowlisted Git status, unpushed count when an upstream is configured, and sizes of common build caches. |

`port_check` inspects local listening sockets and their owners; it does not
probe remote hosts. At most 64 ports may be requested per call. If the operating
system does not expose a socket's PID or process details, the result indicates
that elevated permissions may be required. `gpu_telemetry` returns a structured
tool-level error (`GPU_TELEMETRY_UNAVAILABLE`) when NVML or a supported NVIDIA
GPU is unavailable. Invalid or non-repository paths likewise return structured
tool-level errors instead of failing the MCP protocol request.

`workspace_pulse` reports Git branch and dirty state, the number of commits
ahead of the configured upstream (or an explicit unconfigured status), and
sizes for `target`, `node_modules`, `.venv`, and `dist`. It only inspects these
directories and does not execute arbitrary shell commands or follow symlinks.

## MCP resources

- `sys://telemetry/snapshot` provides a fresh JSON snapshot of CPU, memory, OS,
  host, uptime, and rolling trend statistics.
- `sys://telemetry/trends` provides the bounded recent CPU, memory, and swap
  samples and aggregate statistics, including CPU p95 and memory delta in
  MB/minute.
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
        "C:\\Work\\Projects",
        "--sample-interval-secs",
        "2"
      ]
    }
  }
}
```

Repeat the `--allow-dir`, `<path>` pair in `args` to authorize additional
directories. Alternatively, omit those arguments and configure
`SYS_PULSE_ALLOWED_BASES` in the MCP client's `env` object.

## Architecture

- `src/main.rs` configures stderr-only tracing, parses CLI options, creates a
  shared telemetry provider, starts the background sampler, and serves the MCP
  service over rmcp's stdio transport.
- `src/telemetry.rs` defines the `TelemetryProvider` boundary, the host-backed
  `SystemTelemetryProvider`, the test-only deterministic mock provider, and
  the bounded sample history and trend calculations.
- `src/tools.rs` contains typed tool and prompt routers, MCP resource handlers,
  allowlisted repository inspection, workspace cache sizing, and compact
  response formatting.

The provider boundary keeps host-dependent inspection out of MCP handlers so
their behavior can be tested deterministically without relying on a particular
machine or an NVIDIA GPU. The server performs no writes to inspected
repositories and invokes Git with argument arrays, never through a shell.

## Development

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

GitHub Actions runs formatting, Clippy, and tests on both Ubuntu and Windows.
See [CHANGELOG.md](CHANGELOG.md) for the versioned feature history.
