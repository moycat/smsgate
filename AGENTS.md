# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project

ESP32 firmware in Rust — bridges SMS and IM (Telegram and others); two-way forwarding.
Designed to support any ESP32 board paired with an AT-command cellular modem via the `Board` trait.
Reference hardware: LilyGo T-A7670X (A7670G modem, CH9102 USB bridge).
This branch (`main`) is hardware-tested and boots to working state on real hardware.

## Commands

```bash
# Host tests — no hardware needed; use after every change
cargo test --no-default-features --features testing

# Single test file
cargo test --no-default-features --features testing --test <name>

# Build firmware (requires Xtensa toolchain — see Toolchain Setup below)
cargo +esp build --release --target xtensa-esp32-espidf
# Windows: ESP-IDF has path-length limits; use a short target dir:
#   CARGO_TARGET_DIR=C:\t cargo +esp build --release --target xtensa-esp32-espidf

# Flash + monitor
# PORT: /dev/ttyUSB0 (Linux), /dev/cu.wchusbserial* (macOS), COM3 (Windows)
espflash flash target/xtensa-esp32-espidf/release/smsgate --port <PORT> --partition-table partitions_ota.csv --target-app-partition ota_0 --erase-parts otadata
espflash monitor --port <PORT> --non-interactive

# Fuzz smoke (nightly, run after touching PDU/URC/command parsers)
# Note: requires cargo-fuzz and Linux/macOS (Windows DLL issue with libFuzzer)
cargo +nightly fuzz run pdu_decode    -- -max_total_time=60
cargo +nightly fuzz run urc_parse     -- -max_total_time=60
cargo +nightly fuzz run command_parse -- -max_total_time=60
```

## Commit Messages

Use Conventional Commits for all commits. Prefer the smallest accurate type:

- `feat: ...` for user-visible firmware behavior
- `fix: ...` for bug fixes
- `docs: ...` for documentation-only changes
- `test: ...` for tests and fuzz targets
- `build: ...` for Cargo, ESP-IDF, partition, or toolchain build changes
- `ci: ...` for GitHub Actions and release automation
- `chore: ...` for maintenance that does not affect firmware behavior

Use an optional scope when it adds clarity, for example `fix(modem): ...` or
`docs(agents): ...`. Mark breaking changes with `!` and include a
`BREAKING CHANGE:` footer when behavior or configuration compatibility changes.

## Language Policy

Repository documents, code comments, commit messages, branch names, and PR text must be
written in English. The only exception is user-visible i18n content, which belongs under
the locale-specific translation files and should use the target language.

Chat with users in the language they use. If the user writes in Chinese, reply in Chinese;
keep repository edits in English.

## Rust Engineering Practices

Use stable Rust unless this repository already requires nightly for that specific task
(for example, fuzzing). Prefer current stable language features and standard-library APIs
when they make the code simpler, but do not change the Rust edition, MSRV expectations, or
ESP toolchain assumptions without an explicit migration plan and both host and firmware
verification.

The repository currently uses Rust 2021. Treat Rust 2024 features as an edition migration,
not as an opportunistic refactor. If a migration is ever needed, update the toolchain notes,
run `cargo fix --edition`, verify generated diffs carefully, and confirm the Xtensa ESP-IDF
build still works.

Keep the firmware dependency-inverted and host-testable:

- Business logic should depend on the core traits in `bridge/`, `commands/`, `sms/`, `im/`,
  `modem/`, and `persist/`, not concrete Telegram, ESP-IDF, or board implementations.
- Prefer small modules and functions with explicit responsibilities. Split large
  implementations by domain instead of adding catch-all utility files.
- Use strong types for IDs, timestamps, phone numbers, and command names when that prevents
  invalid states from crossing module boundaries.
- Prefer explicit error enums for library/domain code. Use broad error aggregation only at
  composition boundaries such as startup, command-line tooling, or tests.
- Avoid `unwrap()` and `expect()` in firmware paths unless the invariant is local, obvious,
  and documented. Tests may use them freely when failure output remains useful.
- Avoid unbounded allocation or queue growth in long-running firmware loops. Prefer bounded
  buffers, fixed capacities, and backpressure when memory or latency matters.
- Use `unsafe` only at ESP-IDF/HAL boundaries that cannot be expressed safely. Every unsafe
  block must have a nearby `SAFETY:` comment explaining the invariant being upheld.
- Update tests with behavior changes. Parser changes to PDU, URC, or bot command handling
  also require the fuzz smoke commands listed above.

## Required Quality Gates

Run the relevant gate before committing. For Rust or build-affecting changes, the default
local gate is:

```bash
cargo fmt --all -- --check
cargo clippy --no-default-features --features testing --all-targets -- -D warnings
cargo test --no-default-features --features testing
cargo +esp build --release --target xtensa-esp32-espidf
```

If `rustfmt` or `clippy` is missing, install the Rust components instead of skipping the
gate:

```bash
rustup component add rustfmt clippy
```

For documentation-only changes, `git diff --check` is sufficient unless the documentation
changes commands, build behavior, or hardware procedures that should be verified directly.

For parser changes, also run the fuzz smoke commands in the Commands section. For dependency
changes, use dependency hygiene tools when available: `cargo deny check` for advisories,
licenses, bans, and source policy; `cargo machete` as an advisory unused-dependency check
because it can produce false positives. Do not introduce new dependency-policy config without
reviewing and committing the config with the change.

The checked-in GitHub CI covers `rustfmt`, `clippy -D warnings`, and host tests.
Local agents are still responsible for running the broader gate that matches the
change, including the ESP firmware build when Rust or build-affecting files change.

## Toolchain Setup

```bash
# 1. Install Xtensa Rust toolchain (all platforms)
cargo install espup && espup install
# Linux/macOS: source ~/export-esp.sh in each session (or add to shell profile)
# Windows:     source ~/export-esp.ps1 in each session

# 2. Flash tool
cargo install espflash ldproxy
```

On **Windows**, if `espup install` does not set the environment automatically,
set these before building:

```powershell
$env:LIBCLANG_PATH = "$env:USERPROFILE\.rustup\toolchains\esp\xtensa-esp32-elf-clang\esp-clang\bin\libclang.dll"
$env:PATH = "$env:USERPROFILE\.rustup\toolchains\esp\xtensa-esp32-elf-clang\esp-clang\bin;$env:USERPROFILE\.rustup\toolchains\esp\xtensa-esp-elf\bin;$env:PATH"
```

### Local Setup Notes

After installing the host Rust toolchain, verify the ESP tools before building:

```bash
cargo +esp --version
espflash --version
ldproxy --version
```

The first `cargo +esp build --release --target xtensa-esp32-espidf` can take several
minutes. `esp-idf-sys` creates a managed ESP-IDF checkout and Python environment under
`.embuild/espressif/`, downloads ESP-IDF submodules, installs Python packages, and then
builds the ESP-IDF C/C++ side before linking the Rust firmware.

The first build needs network access to crates.io/static.crates.io, GitHub,
PyPI, and `dl.espressif.com`. In sandboxed agent environments, run the firmware
build outside the network sandbox instead of repeatedly retrying DNS failures.

`config.toml` is not required for host tests and the firmware can still compile
without it, but the resulting binary uses empty compile-time credentials and will
enter serial provisioning unless credentials already exist in NVS. For normal
firmware builds, copy `config.toml.example` to `config.toml` and fill in real
values before building.

`[modem].sim_pin` is an optional compile-time SIM unlock PIN. Leave it empty for
an unlocked SIM or a SIM with PIN disabled. If the SIM reports `SIM PIN` during
startup, the value must be a 4-8 digit string; the firmware sends it once with
`AT+CPIN` before network registration and SMS setup. Never log or hardcode real
SIM PIN values.

## Architecture

The system is built around core traits. All business logic depends only on these;
nothing in `bridge/`, `commands/`, or `sms/` imports a concrete implementation.

| Trait | Defined in | Abstracts |
|-------|-----------|-----------|
| `ModemPort` | `modem/mod.rs` | AT commands, URC polling, PDU SMS send |
| `MessageSink` | `im/mod.rs` | outbound delivery to Telegram |
| `MessageSource` | `im/mod.rs` | inbound command polling (Telegram only) |
| `Store` | `persist/mod.rs` | NVS key-value persistence |
| `Board` | `boards/mod.rs` | pin layout, power-on sequence, builds `ModemPort` |
| `Command` | `commands/mod.rs` | single bot command (name, description, handler) |

`Board` is used only during startup in `main.rs` to produce a `ModemPort`.
After that, `Board` disappears from the call graph entirely.

Large implementations are split into subdirectories (`modem/a76xx/`, `im/telegram/`,
`commands/builtin/`). Small ones are single files. Trait definitions always live
in the parent `mod.rs`.

The `"smsgate"` NVS namespace stores exactly four keys: `im_cursor` (i64),
`reply_map` (blob), `block_list` (blob), `fwd_enabled` (bool). Runtime credentials
live in the separate `"smsgcfg"` namespace and override compile-time defaults from
`config.toml`.

### Partition Table

The project uses `partitions_ota.csv`, a custom 4 MB flash layout with:

- `nvs` at `0x9000`, size `0x6000`
- `otadata` at `0xf000`, size `0x2000`
- `phy_init` at `0x11000`, size `0x1000`
- `ota_0` at `0x20000`, size `0x1E0000`
- `ota_1` at `0x200000`, size `0x1E0000`
- `log_ring` at `0x3E0000`, size `0x20000` (128 KiB, flash-backed `/log` event ring)

ESP-IDF generates the binary partition table during `cargo +esp build` and writes it to:

```bash
target/xtensa-esp32-espidf/release/partition-table.bin
```

Normal flashing must pass the tracked CSV and target app partition explicitly:

```bash
espflash flash target/xtensa-esp32-espidf/release/smsgate --port <PORT> --partition-table partitions_ota.csv --target-app-partition ota_0 --erase-parts otadata
```

Do not omit these flags for the OTA layout. Direct `espflash flash <ELF> --port <PORT>`
can produce misleading app-size output and may not target the intended OTA app slot.
Do not omit `--erase-parts otadata` for USB recovery/development flashes: if OTA data
still selects `ota_1`, flashing `ota_0` alone leaves the device booting the old slot.

When migrating a board from the old single-factory-app layout, verify the serial boot
partition table after flashing. If it still lists only `nvs`, `phy_init`, and `factory`,
the custom partition table was not written. Other symptoms are
`flash log partition not found: log_ring` and `esp_ota_ops: not found otadata`.
Do not erase the whole flash unless NVS credentials can be reprovisioned. Use this
manual recovery flow instead:

```bash
cargo +esp build --release --target xtensa-esp32-espidf

espflash save-image --chip esp32 --flash-size 4mb --flash-mode dio --flash-freq 40mhz \
  --partition-table partitions_ota.csv --partition-table-offset 0x8000 \
  --target-app-partition ota_0 \
  target/xtensa-esp32-espidf/release/smsgate \
  target/xtensa-esp32-espidf/release/smsgate-ota0.bin

espflash save-image --chip esp32 --flash-size 4mb --flash-mode dio --flash-freq 40mhz \
  --partition-table partitions_ota.csv --partition-table-offset 0x8000 \
  --target-app-partition ota_1 \
  target/xtensa-esp32-espidf/release/smsgate \
  target/xtensa-esp32-espidf/release/smsgate-ota1.bin

espflash erase-region --port <PORT> --after no-reset 0xf000 0x3000
espflash erase-region --port <PORT> --after no-reset 0x3e0000 0x20000
espflash write-bin --port <PORT> --baud 115200 --after no-reset 0x1000 \
  target/xtensa-esp32-espidf/release/bootloader.bin
espflash write-bin --port <PORT> --baud 115200 --after no-reset 0x8000 \
  target/xtensa-esp32-espidf/release/partition-table.bin
espflash write-bin --port <PORT> --baud 115200 --after no-reset 0x20000 \
  target/xtensa-esp32-espidf/release/smsgate-ota0.bin
espflash write-bin --port <PORT> --baud 115200 --after hard-reset 0x200000 \
  target/xtensa-esp32-espidf/release/smsgate-ota1.bin
```

The explicit `write-bin` flow writes both OTA slots because stale `otadata` may select
either one. Use `--baud 115200` when the CH9102 bridge reports intermittent
communication errors. The expected verification log is a partition table containing
`otadata`, `ota_0`, `ota_1`, and `log_ring`, followed by
`smsgate starting... build=<hash>`, `[boot] ... build=<hash>`, and
`flash-backed log ring mounted`.

Telegram OTA uses the ESP app image, not the ELF passed to `espflash flash`.
Generate the `.bin` to send to the bot with:

```bash
espflash save-image --chip esp32 --flash-size 4mb --partition-table partitions_ota.csv --target-app-partition ota_0 target/xtensa-esp32-espidf/release/smsgate smsgate-ota.bin
```

### CI / Release Notes

The checked-in CI workflow is `.github/workflows/ci.yml`, which runs `rustfmt`,
`clippy -D warnings`, and host tests.

Production workflows that use `.github/scripts/gen_config.py` should provide these GitHub
Secrets (Settings -> Secrets and variables -> Actions). Empty credential values build a
firmware image that enters serial provisioning unless credentials already exist in NVS:

| Secret | Example |
|--------|---------|
| `WIFI_SSID` | `MyNetwork` |
| `WIFI_PASSWORD` | `hunter2` |
| `TELEGRAM_BOT_TOKEN` | `123456:ABC-DEF...` |
| `TELEGRAM_CHAT_ID` | `8024680950` |
| `UI_LOCALE` *(optional)* | `zh` (default) or `en` |

## Agent Workflow and Context Management

Use subagents when there are two or more independent work streams, such as codebase research,
hardware/toolchain research, test investigation, or documentation review. Keep each subagent
prompt self-contained and narrow: include exact files, commands, expected output, and the
decision the main agent needs back.

Prefer explorer subagents for read-only research. Use worker subagents only for clearly
disjoint write scopes, and tell them not to revert unrelated changes. Continue useful
non-overlapping work while subagents run, then integrate their findings in the main context.

The main agent remains responsible for final decisions, repository edits that cross module
boundaries, verification, git staging, and commits. Do not delegate destructive git commands,
hardware flashing, secret handling, or tightly coupled architectural choices.

Close completed subagents when the tool supports it so stale context does not leak into later
work.

## Task Recipes

### Add a bot command (hard cap: 10 — check count first)
1. `src/commands/builtin/<name>.rs` — implement `Command` trait
2. `src/commands/builtin/mod.rs` — `pub use <name>::<Name>Command;`
3. `src/main.rs` `build_registry()` — `registry.register(Box::new(<Name>Command));`
4. `tests/test_commands.rs` and/or `tests/test_poller.rs` — add tests using
   `RecordingMessenger` + `MemStore`
5. `cargo test --no-default-features --features testing --test test_commands --test test_poller`

### Add a board
1. `src/boards/<board>.rs` — implement `Board` trait
2. `src/boards/mod.rs` — export the board module behind the appropriate cfg
3. `src/main.rs` — select the board during startup, or add an explicit feature switch
4. `config.toml.example` — document default pins for this board

### Add a test scenario
1. Pick or create a file in `tests/`
2. `Scenario::new("...").modem_urc(...).expect_im_sent(...).run()`
3. Real hardware recording → add `serial_capture/<description>.txt`

## Hardware Testing

**Every change must be tested on real hardware before the task is considered done.**
Host tests (`cargo test`) verify logic; they cannot catch UART timing issues, modem
power sequencing, NVS partition behaviour, or FreeRTOS scheduling interactions.

Minimum verification on board after any change:
1. Flash and confirm clean boot log (see Boot Sequence Timing below)
2. Send an SMS to the device — confirm it forwards to Telegram
3. Send `/status` from Telegram — confirm it replies

## Board and Runtime Stability

Reference hardware is the LilyGo T-A7670X with an A7670G/A76xx AT-command modem and CH9102
USB bridge. Verify the exact board SKU, modem model, and modem firmware when debugging
hardware-specific failures.

Reference board wiring and defaults:

- Keep the modem power rail enable GPIO high for the entire program (`MODEM_POWER_PIN`,
  GPIO12 on the reference board).
- The modem reset pin is active high (`MODEM_RESET_PIN`, GPIO5 on the reference board).
- The modem PWRKEY is GPIO4 on the reference board.
- The modem UART defaults to TX GPIO26, RX GPIO27, 115200 baud.

Do not simplify the cold-start power sequence, warm-reboot detection, reset timing, or AT
probe loop without hardware logs. These delays are part of modem bring-up stability, not just
startup cosmetics.

Keep `sdkconfig.defaults` aligned with the board and firmware behavior:

- 4 MB flash size for the reference board.
- Custom dual-OTA partition table with a 128 KiB `log_ring` data partition.
- 240 MHz CPU frequency for the reference ESP32.
- Bluetooth disabled; the firmware does not use BT controller or host features.
- Dual-core FreeRTOS enabled; do not enable unicore builds for the reference board.
- TLS certificate bundle enabled and insecure TLS disabled.
- Main task stack large enough for current firmware paths.
- Watchdog timeouts high enough for modem, flash, and TLS operations while still catching
  stuck tasks.

Long blocking operations such as modem HTTP, SMS send, and flash erase/write must yield
or reset watchdog state often enough for ESP-IDF watchdogs. Avoid long critical sections and
avoid blocking interrupts around UART or flash work.

Telegram polling (`tg-poll`) and outbound Telegram delivery (`tg-send`) are separate runtime
threads. Main/SMS code should not own a WiFi TLS client directly. Keep modem UART operations
single-owner and ordered; do not let multiple tasks issue AT commands concurrently.

After changing the partition layout, run a firmware build so ESP-IDF regenerates
`target/xtensa-esp32-espidf/release/partition-table.bin`, then flash with
`espflash flash --partition-table partitions_ota.csv --target-app-partition ota_0 --erase-parts otadata`.
Use `espflash write-bin` only for manual recovery workflows.

For modem issues, check LilyGo/SIMCom firmware notes and known issues before changing the init
sequence. Preserve the existing SMS storage behavior unless tests and hardware logs prove the
new sequence is safe.

SIM PIN unlock runs after the basic AT probe and `ATE0`, before PDU mode, CNMI setup,
storage checks, and network registration. Preserve this ordering: locked SIMs cannot
register, and SMS setup may fail or behave inconsistently before `+CPIN: READY`.

Cellular fallback is opt-in only. Runtime WiFi failure or Telegram poll staleness may switch
Telegram transport to modem HTTP only when `[modem].cellular_fallback = true` and APN is
configured. If fallback is disabled, keep retrying WiFi and do not attach PDP.

## Key Invariants

Verify after every change:

- PDU roundtrip: `encode(decode(x)) == x`
- Blocked numbers produce zero IM messages
- `FakeClock` u32 wraparound fires all timers correctly
- `ScriptedModem` unconsumed steps → test failure (no silent pass)
- Command count ≤ 10 (hard cap — adding one requires removing one)
- `"smsgate"` NVS key set unchanged (4 keys only: im_cursor, reply_map, block_list, fwd_enabled)
- `"smsgcfg"` NVS credential keys unchanged (7 keys: wifi_ssid/pass, bot_token, chat_id, apn/user/pass)

## sdkconfig.defaults Known Quirk

`ESP_IDF_SDKCONFIG_DEFAULTS` in `.cargo/config.toml` must be an **absolute path**. A relative
path is resolved against the esp-idf-sys crate directory in `~/.cargo/registry`, not the project
root, and silently produces the wrong (default) sdkconfig values.

If you change `sdkconfig.defaults` and the change doesn't seem to take effect, delete the cached
sdkconfig to force kconfgen to regenerate from scratch:
```bash
rm target/xtensa-esp32-espidf/release/build/esp-idf-sys-*/out/sdkconfig
# (adjust path if you used a custom CARGO_TARGET_DIR)
```
Then rebuild. The `sdkconfig.defaults` is applied as a *seed* (lower priority than existing
sdkconfig), so deleting the cache is required for changes to take effect.

## Boot Sequence Timing

Milestones below are for the reference board (T-A7670X) on a cold start; exact timings vary by board and modem:
- `t≈645ms`: smsgate starting
- `t≈3545ms`: RESET_PIN configured
- `t≈6745ms`: Board power-on sequence complete (modem booted)
- `t≈12900ms`: Modem responded to AT probe
- `t≈15500ms`: **Network registered** (typical; within 30s window)
- `t≈19500ms`: WiFi DHCP IP assigned
- `t≈21000ms`: Sweeping existing SMS
- `t≈22000ms`: smsgate ready

If network registration doesn't appear within 30s, a warning is logged and boot continues.
SMS delivery still works — the modem registers in the background.

## Modem Driver Notes

**`is_urc` deliberately excludes `+CREG:` / `+CGREG:` / `+CEREG:`**. With `AT+CREG=0`
(default — no URC mode), these prefixes appear only as responses to `AT+CREG?`. If they were
classified as URCs, `send_at("+CREG?")` would siphon the response into the URC buffer and
registration checks would always return `false`. Do not add them back to `is_urc` unless
`AT+CREG=1` (or `=2`) is also added to the modem init sequence.

**`AT+CNMI=2,1,0,0,0`** (store + `+CMTI` notify) is the required setting because it preserves
slot retry/delete semantics. Direct `+CMT` two-line parsing exists as a defensive fallback,
but do not switch modem init to `mt=2` without tests and hardware validation for two-line
delivery, storage cleanup, retries, and UART buffering.

`AT+CMGF=0` is volatile modem state, not a permanent firmware invariant. Reassert PDU mode
before `AT+CMGR` and `AT+CMGL`; if the modem has reset or drifted back to text mode, text-mode
headers such as `+CMGR: "REC UNREAD","10086",...` contain correct sender/timestamp metadata while
the following UCS2 hex line is not a PDU. `AT+CMGL="ALL"` does not switch modes; it is only a
text-mode list argument accepted when `CMGF=1`. Keep the text-mode parser as a fallback, but prefer
PDU mode so multipart SMS retains UDH and can be reassembled correctly.

Boot sweep tries `AT+CMGL=4` first, then falls back to `AT+CMGL="ALL"` for SIMCom firmware
that reports `+CMS ERROR: Invalid text mode parameter` for the numeric list form. Keep this
fallback unless hardware logs prove both board and modem firmware accept only one form. New
SMS delivery still relies on stored-slot `+CMTI` plus `AT+CMGR=<index>`.

**`AT+CPIN?` / SIM PIN**: startup checks SIM readiness before SMS and registration setup.
When the modem reports `+CPIN: SIM PIN`, `[modem].sim_pin` must contain a 4-8 digit PIN;
the firmware sends `AT+CPIN` once, waits for `+CPIN: READY`, and never logs the PIN. If
the modem reports `SIM PUK`, do not attempt PIN unlock in firmware; recover the SIM with
carrier tooling first.

**`AT+CPMS` and storage memory**: Some modems (e.g. A7670G on T-A7670X) return `+CMS ERROR`
to `AT+CPMS?` because the SIM doesn't support SMS management queries. When that happens the
modem defaults to `"ME"` (device flash) for all three memory slots. `+CMTI` notifications will say `+CMTI: "ME",<index>`. The modem
driver passes the memory name through `Urc::NewSms { mem, index }` and calls `AT+CPMS=<mem>`
before each `AT+CMGR` to guarantee the read uses the correct storage. SMS is deleted only after
a successful Telegram forward — if `forward_sms` fails the slot stays occupied and sweep on
next boot retries. Do not add `AT+CPMS="SM","SM","SM"` to modem init: it silently triggers
CMTI notifications for all stored SMS which can overflow the 256-byte UART Rx buffer during init.

Do not send Telegram messages while holding the modem mutex. Read modem slots and perform
hang-up/status/CMGS operations under the mutex, then release it before IM delivery; reacquire
only for short follow-up operations such as `AT+CMGD` after successful forwarding.

## Forbidden Patterns

- `>=` / `<` on raw `u32` timestamps — use `elapsed_since()` / `is_past()` only
- Literal WiFi password, bot token, or pin numbers anywhere in `src/`
- Importing `im::telegram` (or any concrete backend) from `bridge/`, `commands/`, `sms/`, `persist/`
- Adding a fifth NVS key to `"smsgate"` without updating the Key Invariants section above
- ASCII art diagrams in documentation — use Mermaid instead
- Adding `+CREG:` back to `is_urc` without also enabling `AT+CREG=1` in modem init
- Non-English repository documents, code, comments, commit messages, branch names, or PR text
  outside locale-specific i18n content
- Nightly-only Rust features, Rust edition changes, or toolchain requirement changes without
  explicit approval plus host and ESP firmware verification
