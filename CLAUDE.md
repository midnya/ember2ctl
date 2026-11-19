# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

`ember2ctl` is a Rust CLI for controlling an **Ember² smart mug** over Bluetooth Low Energy. The crate is a working tool, not a scaffold — `src/main.rs` parses subcommands via `clap` derive, and `src/lib.rs` exposes the BLE service/characteristic UUIDs, typed command builders, and async readers.

## Repository layout

Two independent Cargo packages exist in the tree, **not** combined as a workspace:

- `./` — the `ember2ctl` crate (Rust edition 2024). **This is the only crate to modify.**
- `bta30ctl/` — a separate, nested git repository included **only as a reference template**. It is a previously-built BLE CLI for a different device (Fiio BTA 30 audio adapter). It is not a submodule, not a workspace member, and is excluded from the parent repo.

There is no `[workspace]` in the root `Cargo.toml`, so `cargo` commands at the repo root operate on `ember2ctl` only.

### Hard rule: `bta30ctl/` is read-only reference

**Never edit, create, delete, build, test, or install anything under `bta30ctl/`.** It exists purely so its source can be read for architectural guidance. All changes must be confined to the root `ember2ctl` crate.

If a task seems to require modifying `bta30ctl/`, stop and confirm with the user first.

## Build / run (ember2ctl, from repo root)

```
cargo build
cargo run -- --help
cargo run -- info
cargo test
cargo check           # fast type-check while iterating
cargo clippy -- -D warnings
```

The CLI uses `env_logger`; set `RUST_LOG=debug` for verbose output. `EMBER2CTL_CONNECT_RETRIES` overrides the default 5 connect attempts.

## Subcommands

`clap` generates `--help` for each. Today's set:

- `pair` — bond with the mug via `bluetoothctl` (one-time; mug must be in pairing mode).
- `info` — dump every readable characteristic.
- `watch [secs]` — repeatedly print `info`, default 2 s.
- `name` / `set-name <name>`
- `temp` — current liquid temperature (Celsius).
- `target` / `set-target <celsius>` — 0 disables heating; otherwise 50.0–100.0.
- `off` — alias for setting target to 0.
- `unit` / `set-unit <c|f>` — display unit.
- `battery` — percent, charging state, internal temperature.
- `level` — liquid level (empty / not empty / intermediate raw).
- `state` — liquid state (empty/filling/cooling/heating/stable).
- `color` / `set-color <rrggbb[aa]>` — LED color; alpha defaults to `FF`.

## Code shape

### `src/lib.rs`

- UUIDs and target `Characteristic`s are `std::sync::LazyLock` statics; only the service UUID is `pub` (`EMBER_SERVICE_UUID`).
- `CommandKind::Request` → `WriteType::WithResponse`; `CommandKind::Command` → `WriteType::WithoutResponse`. Pick per the actual GATT property of the target characteristic.
- `Command` carries a `&'static Characteristic`, a `CommandKind`, and a `Vec<u8>` payload. `send_command(peripheral, command)` is the single write entry point.
- Command builders return `Result<Command, CommandBuilderError>`. `CommandBuilderError` implements `Display` + `std::error::Error`, so the binary just `?`s it.
- Readers return `Result<Result<T, CommandBuilderError>, btleplug::Error>` — outer for the BLE transport, inner for payload decode. `read_mug_name` is the one exception (no decode failure mode beyond UTF-8 lossy).

### `src/main.rs`

- `#[derive(Parser)]` + `#[derive(Subcommand)]` define the CLI surface. A `ValueEnum` (`CliUnit`) handles `c|f`. `parse_color` is a clap value parser used directly by `set-color`.
- `find_mug` checks `central.peripherals()` first (the OS-known list for previously bonded mugs) before scanning. The mug only advertises in pairing mode, so this matters.
- `connect_with_retry` retries `connect()` with a 2 s delay; count configurable via `EMBER2CTL_CONNECT_RETRIES`.
- `run_pair` shells out to `bluetoothctl pair`/`trust`. Linux-only (BlueZ).
- `watch_loop` redraws on a `tokio::time::interval` tick and exits cleanly on `Ctrl-C` via `tokio::select!`.

## External reference for the Ember² protocol

[`orlopau/ember-mug`](https://github.com/orlopau/ember-mug) documents the Ember mug BLE protocol. Use it to look up the service UUID, characteristic UUIDs, opcodes, and value encodings (e.g. temperature scaling, LED color format). The R/G/B/A byte ordering follows the documented *example* (`BF FF 00 FF` = lime-green) rather than the documented *table*, which lists `R, B, G, A` and disagrees with the example.

## Dependencies

See `Cargo.toml` for the authoritative list. Current major dependencies: `tokio`, `btleplug`, `clap` (derive), `futures-util`, `uuid`, `log`, `env_logger`.

## Misc

- `.gitignore` only excludes `/target`.
- No CI or lint config is committed.
- No tests exist yet; the pure-logic functions in `lib.rs` (encode/decode temperature, `set_mug_name` validation, `from_byte` decoders) and `parse_color` in `main.rs` are the easy first targets if tests are added.
- Edition mismatch between crates is intentional: root is `edition = "2024"`, the reference crate is `edition = "2021"`.
