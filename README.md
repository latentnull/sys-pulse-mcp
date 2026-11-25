# sys-pulse-mcp

A read-only, stdio-based Model Context Protocol server for local system and Git
repository inspection. Built with Rust, [`rmcp`](https://crates.io/crates/rmcp),
Tokio, and `sysinfo`.

## Features

- Reports CPU, memory, operating-system, and uptime information.
- Lists mounted filesystems with total and available space.
- Inspects a repository's current branch, dirty-file count, and latest commit.
- Keeps MCP JSON-RPC frames on stdout and sends diagnostic logs only to stderr.
- Restricts repository lookups to explicitly allowed base directories.

## Requirements

- Rust 1.95 or newer.
- Git available on `PATH` for the `repo_status` tool.

## Build and run

```powershell
cargo build --release
cargo run --release
```

The server communicates over standard input/output and is intended to be
launched by an MCP client. Do not pipe other application output to stdout.
Tracing output is configured to stderr.

## Tools

All tools are read-only and use object-shaped JSON input schemas.

### `system_overview`

Input: `{}`.

Returns `cpu_count`, `cpu_usage_percent`, `memory_total_bytes`,
`memory_available_bytes`, OS name/version, host name, and `uptime_seconds`.

### `disk_usage`

Input: `{}`.

Returns a `disks` array. Each entry contains the disk name, filesystem,
mount point, `total_space_bytes`, and `available_space_bytes`.

### `repo_status`

Input:

```json
{ "path": "relative/path/to/repository" }
```

Returns the canonical repository path, current branch (or `null` when detached),
`dirty_file_count`, and the latest commit summary (or `null` for a repository
without commits). Invalid, missing, or non-repository paths produce an MCP
tool-level error with structured `error_code` and `message` fields; they do not
fail the JSON-RPC request.

By default, relative paths resolve beneath the server's working directory.
Set `SYS_PULSE_ALLOWED_BASES` to a platform-separated list of allowed base
directories to customize access. The server canonicalizes both bases and
requested paths, rejects absolute paths and parent traversal, and checks the
resolved path remains beneath an allowed base. On Windows, separate bases with
`;`; on Unix, use `:`.

## MCP client configuration

Example configuration for an MCP client that accepts a `command` and `args`
stdio server entry:

```json
{
  "mcpServers": {
    "sys-pulse": {
      "command": "C:\\Work\\Projects\\sys-pulse-mcp\\target\\release\\sys-pulse-mcp.exe",
      "args": [],
      "env": {
        "SYS_PULSE_ALLOWED_BASES": "C:\\Work\\Projects"
      }
    }
  }
}
```

Build the executable first with `cargo build --release`. Configure the allowed
base to the narrowest directory that contains repositories the client should
be able to inspect.

## Architecture

- `src/main.rs` configures stderr-only tracing, creates the MCP service, and
  serves it over rmcp's stdio transport.
- `src/tools.rs` owns the tool router, documented `JsonSchema` input types,
  system/disk inspections, Git status collection, and path validation.
- `SYS_PULSE_ALLOWED_BASES` is the access boundary for `repo_status`; it
  defaults to the process working directory.

The server disables Git's optional index writes and filesystem-monitor hook
while inspecting repositories, and invokes Git only with argument arrays
(never through a shell).

## Development

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

GitHub Actions runs the same formatting, lint, and test checks on pushes and
pull requests.
