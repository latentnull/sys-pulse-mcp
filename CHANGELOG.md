# Changelog

## Initial Release — System Inspection

- feat(server): add a stdio MCP server with `system_overview`, `disk_usage`, and allowlisted `repo_status` tools.
- feat(security): validate repository paths against canonicalized allowed base directories and return structured tool errors.
- ci(github-actions): run formatting, Clippy, and unit tests on pushes and pull requests.

## v0.1 — MCP Resources & Prompts

- feat(mcp): expose a live `sys://telemetry/snapshot` resource and an allowlisted repository status resource template.
- feat(mcp): advertise tool, resource, and prompt capabilities to MCP clients.
- feat(mcp): add typed `diagnose_bottleneck` and `pre_push_check` prompt workflows with structured messages.

## v0.2 — Diagnostics & Configurable Allowlist

- feat(diagnostics): report top local processes sorted by CPU or resident memory, with a bounded optional result limit.
- feat(diagnostics): check bounded sets of TCP ports on loopback without probing remote hosts.
- feat(diagnostics): expose NVIDIA GPU VRAM, temperature, and utilization through NVML with a structured unavailable error.
- feat(config): accept repeatable `--allow-dir` paths and validate allowlist directories at startup, with `SYS_PULSE_ALLOWED_BASES` and the current directory as fallbacks.

## v0.3 — Documentation & Release History

- docs(readme): document all tools, resources, prompts, allowlist configuration, and MCP client setup.
- docs(changelog): organize feature history by version with Conventional Commit entries.

## v0.4 — Background Telemetry Trends

- feat(telemetry): sample CPU, memory, and swap on a configurable background interval into a bounded 150-sample ring buffer.
- feat(mcp): expose `sys://telemetry/trends` and enrich the snapshot resource with rolling CPU/memory/swap statistics, CPU p95, and memory growth rate.
- test(telemetry): cover ring-buffer eviction, rolling statistics, percentile calculation, and memory delta behavior.

## v0.5 — Workspace & Port Diagnostics

- feat(diagnostics): attribute local listening TCP ports to owning PIDs, process names, and command lines with a clear permission fallback.
- feat(workspace): report allowlisted Git status, upstream commit count, and sizes of common build caches without following symlinks.
- feat(diagnostics): add compact summaries for process, port, GPU, and workspace diagnostics.
- test(diagnostics): cover cache sizing, compact output, path validation, and local port lookup.

## v0.6 — Testable Providers & Cross-Platform CI

- refactor(core): route system, disk, process, GPU, port, and sampler telemetry through a shared `TelemetryProvider`.
- test(core): add a deterministic mock provider for stable MCP handler and prompt tests without host telemetry dependencies.
- ci(github-actions): run formatting, Clippy, and tests on Ubuntu and Windows.

## v0.7 — Documentation & Release Polish

- docs(readme): document telemetry trends, port attribution, workspace diagnostics, compact options, and provider architecture.
- docs(readme): add an Actions status badge and refresh the MCP client configuration and CLI guidance.
- docs(changelog): audit versioned release history and use consistent Conventional Commit bullets.
