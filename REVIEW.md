# Review guide

This is an experimental order-flow research fork of [Niketion/flowdepth](https://github.com/Niketion/flowdepth). The active branch is `feat/delta-table`; the shared history and license are preserved.

## Start here

| Document | Purpose |
|---|---|
| [Project guide](docs/project-guide.md) | Codebase map and main execution paths |
| [Core architecture](docs/core-architecture.md) | Extraction plan and architectural contracts |
| [Current state](docs/state.md) | Completed work and remaining limitations |
| [Quantity contract](docs/quantity-contract.md) | Market units and conversion boundaries |
| [Profile core batch](docs/profile-core-batch.md) | Shared profile calculations, coverage and snapshots |
| [Workspace direction](docs/terminal-workspace.md) | Terminal interaction and layout goals |
| [Order-flow roadmap](docs/orderflow-roadmap.md) | Study definitions and feature direction |

Detailed engineering notes are primarily in Russian. The README provides an English overview; code and symbols are in English.

## Review priorities

- Compare volume and delta across live ingestion, historical loading, cache replay and compaction.
- Check base/quote conversions for spot, linear and inverse markets. Canonical storage independent of display units is still unfinished.
- Verify that incomplete or approximate inputs cannot publish falsely confirmed levels into signals.
- Inspect cancellation, request ownership, stale results and source-generation changes.
- Keep calculation lifecycles independent of rendering, and preserve saved-layout compatibility.
- Review signal confirmation timing and whether historical markers depend on subsequent bars.

Architecture work is incremental. Passing tests do not establish correctness against every exchange dataset or validate a trading strategy.

## Reproduce the checks

Run from the workspace root using the pinned Rust toolchain:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --release --workspace --locked
```

The recorded pre-publication workspace run completed with **548 passed, 15 ignored, 0 failed**. Ignored tests include GPU-dependent and separate stress checks. This is a local result, not a claim that hosted CI or strict Clippy is green. Strict Clippy has outstanding findings; standalone package builds may expose existing dependency-feature unification issues.

Use an [isolated profile](docs/profiles.md) for interactive testing. Personal layouts, market caches, logs and executables are not bundled. Local paths in engineering notes describe the maintainer's environment and are not required directory structures.

## Useful findings

Please include the affected commit, source market, symbol, timeframe, **UTC timestamp**, quantity unit, actual behavior and expected result. Small synthetic fixtures are preferable for calculation bugs. Explain what was observed separately from assumptions about an exchange.

Report ordinary bugs through [Issues](https://github.com/Mixannik/flowdepth/issues). Follow [SECURITY.md](SECURITY.md) for sensitive reports. Never attach credentials or unredacted saved state.
