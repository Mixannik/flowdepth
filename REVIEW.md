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

The shared runner is `python -B scripts/quality_gate.py --mode fast` (Python 3.10+, standard library). Use `--mode full` for market-data/recovery/cache changes and broad validation; full adds the existing release workspace tests and both indicator catalogs. No application GUI, packaging or release is launched. Cargo uses the pinned toolchain, `--locked` and offline dependencies by default; `--online` permits dependency downloads, as in PR CI.

Fast includes formatting, release all-target compilation, existing FD-001 WS-01/02/03 bounded/unbounded regressions (required counts/named scenarios), and four real profile-isolation/subprocess smoke tests. These create temporary synthetic files; the runner sets a fresh `.local/quality-gate/<run>/profile` as an additional safety fence. It does not invoke test-profile.mjs, copy working layouts, replace executables or access the application's shared credential store.

`--allow-docs-only` skips Rust only when all changed paths are allowlisted Markdown; otherwise the requested Rust mode runs. Local selection includes staged/unstaged/untracked files against HEAD. CI supplies `--base SHA --head SHA`, uses merge-base diff and the same classifier. Check links/paths of changed documents as well; whitespace diff and runner tests remain automatic. The stable PR quality job completes even for documentation-only PRs. Release guards in develop-ci remain unchanged.

Each run writes commands, return codes, durations, logs and `report.json` under the ignored `.local/quality-gate/` directory. HEAD and a working-tree fingerprint identify the checked inputs. Source changes during a run yield NOT VERIFIED. Exit codes are 0 for PASS, 1 for FAIL and 2 for NOT VERIFIED. The default `--require automated` covers machine checks. `--require all` includes outstanding UI/live checks and skipped workspace tests in the verdict. The report always includes both verdicts. The workspace row describes tests that ran. Its ignored tests get a separate NOT VERIFIED row, with names in `workspace.log`. An ignored FD-001 or profile smoke regression prevents an automated PASS.

Manual FD-001 minimum: in a NEW explicit isolated profile, select BTCUSDT spot/perp, exercise reconnect/recorded overflow, change symbol/tab and reopen normally. Check that warnings persist through partial/error/unsupported recovery, stale generations do not clear them, and only supported proved scopes recover. Raw detectors, liquidation losses and historical depth must not appear repaired by aggregate backfill/current snapshot. Record exact build, UTC interval, units and observations. The gate cannot safely induce exchange overflow, verify rendering or confirm live recovery without an isolated interactive scenario; these remain NOT VERIFIED. Do not change shared keychain, touch the working instance, or forcibly close it. Targeted regressions establish synthetic correctness, not live exchange completeness.

The gate is extensible through `plan()` in scripts/quality_gate.py: add task-specific existing regressions with a positive minimum and named critical tests. Keep FD task statuses in DEV_TASKS; a passing automated gate alone must not mark FD-001 Done. To stop a stuck check the runner terminates only its owned Cargo/test process tree after `--timeout` seconds (default 1800); it never starts an interactive application. Concurrent gate runs are rejected; inspect the PID before removing a stale run.lock after an abnormal runner exit.

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
