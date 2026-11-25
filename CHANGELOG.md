# Changelog

## Milestone 1 — MCP Resources & Prompts

- feat(mcp): expose a live `sys://telemetry/snapshot` resource and an allowlisted repository status resource template.
- feat(mcp): advertise tool, resource, and prompt capabilities to MCP clients.
- feat(mcp): add typed `diagnose_bottleneck` and `pre_push_check` prompt workflows with structured messages.

## Milestone 2 — Diagnostics & Configurable Allowlist

- feat(diagnostics): report top local processes sorted by CPU or resident memory, with a bounded optional result limit.
- feat(diagnostics): check bounded sets of TCP ports on loopback without probing remote hosts.
- feat(diagnostics): expose NVIDIA GPU VRAM, temperature, and utilization through NVML with a structured unavailable error.
- feat(config): accept repeatable `--allow-dir` paths and validate allowlist directories at startup, with `SYS_PULSE_ALLOWED_BASES` and the current directory as fallbacks.
