# Contributing to Flowdepth

This repository is an independently maintained research fork. The active development and pull-request target branch is **`feat/delta-table`**. The binary remains named `flowsurface` for compatibility.

## Development setup

Follow the platform requirements in the [README](README.md#getting-started), then:

```sh
git clone --branch feat/delta-table https://github.com/Mixannik/flowdepth.git
cd flowdepth
cargo build --release --locked
```

Use the pinned Rust toolchain and an [isolated application profile](docs/profiles.md) when testing changes to state, cache or layout behavior.

## Checks

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --release --workspace --locked
```

Strict Clippy has known outstanding findings. Avoid introducing new warnings and document any check you could not complete. Existing hosted workflows do not constitute validation of every working-branch commit.

## Changes and pull requests

- Describe the concrete problem and resulting behavior; include reproduction steps and validation.
- Keep changes focused and explain compatibility effects on layouts, cache formats and units.
- Add small regression fixtures for calculation and lifecycle bugs where useful.
- Include real screenshots for UI changes, with private information removed.
- Discuss major architectural changes in an issue before implementing them.

For market-data discrepancies, specify the exchange, market, symbol, timeframe, **UTC timestamp** and quantity unit. Distinguish source gaps from processing errors. Read the [review guide](REVIEW.md) for current priorities and [indicator documentation](docs/indicators.md) for module registration and settings.

## Data and attribution

Do not commit personal saved state, credentials, private datasets, market caches, logs or build artifacts. Use synthetic or redistributable fixtures, documenting their source and license as needed.

Preserve upstream attribution. Contributions are distributed under the repository's [GPLv3-or-later license](LICENSE). Follow the [Code of Conduct](CODE_OF_CONDUCT.md) and [security policy](SECURITY.md).
