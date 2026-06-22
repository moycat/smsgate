# smsgate

ESP32 firmware in Rust that bridges SMS/calls and Telegram. Receives SMS on a cellular modem, forwards to Telegram; reply to a Telegram message to send an SMS back.

## Hardware

Any ESP32 board paired with an AT-command cellular modem is supported via the `Board` trait.
Reference hardware: **LilyGo T-A7670X** (A7670G LTE modem, CH9102 USB bridge).

- A nano-SIM card with SMS service

## Features

- Two-way SMS forwarding (SMS to Telegram, Telegram reply to SMS)
- Incoming call notification with auto-hangup
- Multipart SMS reassembly (concatenated SMS)
- PDU-mode SMS encoding/decoding (GSM-7 + UCS-2)
- Bot commands: `/help`, `/status`, `/send`, `/block`, `/blocklist`, `/unblock`, `/pause`, `/resume`, `/log`, `/restart`
- i18n: English and Chinese (compile-time locale selection, zero overhead)
- NVS persistence for cursor, reply mapping, and block list
- Outbound SMS queue with exponential-backoff retry
- Telegram-delivered OTA firmware updates over WiFi HTTPS
- Flash-backed `/log [offset]` event ring for SMS, boot, network, OTA, and user operations; pages show 16 entries with Telegram inline buttons for paging, and SMS log previews keep up to 160 chars before the fixed-size flash record is trimmed to fit
- Hardware watchdog (120s timeout)
- Build commit hash embedded in `/status` output

## Quick Start

```bash
# 1. Install Xtensa Rust toolchain
cargo install espup && espup install

# 2. Copy and fill in config
cp config.toml.example config.toml
# Edit config.toml with your WiFi credentials, Telegram bot token, and chat ID

# 3. Run host tests (no hardware needed)
cargo test --no-default-features --features testing

# 4. Build firmware
cargo +esp build --release --target xtensa-esp32-espidf
# Windows note: ESP-IDF has path-length limits. Set a short target dir:
#   CARGO_TARGET_DIR=C:\t cargo +esp build --release --target xtensa-esp32-espidf

# 5. Flash
cargo install espflash
espflash flash target/xtensa-esp32-espidf/release/smsgate --port <PORT> --partition-table partitions_ota.csv --target-app-partition ota_0 --erase-parts otadata
# PORT is /dev/ttyUSB0 (Linux), /dev/cu.wchusbserial* (macOS), or COM3 (Windows)
```

## Telegram OTA

Generate the app image to send to the bot:

```bash
cargo +esp build --release --target xtensa-esp32-espidf
espflash save-image --chip esp32 --flash-size 4mb --partition-table partitions_ota.csv --target-app-partition ota_0 target/xtensa-esp32-espidf/release/smsgate smsgate-ota.bin
```

Send `smsgate-ota.bin` to the configured Telegram chat with caption `/ota`.
OTA downloads use WiFi HTTPS only; cellular fallback mode will reject OTA.

When flashing over USB, keep the `--partition-table partitions_ota.csv` and
`--target-app-partition ota_0` flags, and erase `otadata`. The firmware uses a
custom OTA partition layout; omitting these flags can target the wrong app slot,
and stale OTA data can keep booting `ota_1` after `ota_0` was flashed.

## Configuration

All configuration is in `config.toml` (compile-time, not runtime). See [`config.toml.example`](config.toml.example) for all options.

To build with Chinese UI strings, add to your `config.toml`:

```toml
[ui]
locale = "zh"
```

## Design Tradeoffs

**`serde_json` for Telegram API parsing** — The Telegram HTTP layer uses `serde_json`, which requires heap allocation. This is a deliberate tradeoff: the ESP32 has ample SRAM (320 KB + optional PSRAM), a typical Telegram API response is a few kilobytes, and `serde-json-core` (the `no_std` alternative) would add significant implementation complexity for marginal gain. If you port this to a more constrained MCU, swapping out `im/telegram/` is the only change needed.

**Compile-time configuration** — WiFi credentials, bot token, and pin assignments all live in `config.toml` and are baked into the binary at build time. Runtime configuration (e.g. over BLE or a captive portal) is out of scope for a single-owner personal device and would add substantial complexity.

**Runtime task split** — Telegram polling and Telegram outbound delivery run in separate worker threads. SMS and modem AT operations keep a single ordered UART owner so URCs, SMS reads/deletes, and `AT+CMGS` prompt handling do not interleave.

## Architecture

The system is built around four core traits. All business logic depends only on these abstractions:

| Trait | Abstracts |
|-------|-----------|
| `ModemPort` | AT commands, URC polling, PDU SMS send |
| `MessageSink` / `MessageSource` | Send/poll IM messages (Telegram) |
| `Store` | NVS key-value persistence |
| `Command` | Single bot command (name, description, handler) |

## USB Driver

The USB-to-serial chip varies by board. The reference board (T-A7670X) uses a **CH9102**.

- **Linux**: typically works out of the box (`/dev/ttyUSB0`); if not, load the appropriate kernel module (`ch341`, `cp210x`, etc.)
- **macOS**: install the driver matching your chip (e.g. [CH34x](https://www.wch-ic.com/downloads/CH34XSER_MAC_ZIP.html) for CH9102) and approve the kext in System Settings > Privacy & Security
- **Windows**: usually auto-detected; if not, install from the chip vendor (e.g. [WCH](https://www.wch-ic.com/downloads/CH343SER_ZIP.html) for CH9102)

## License

[MIT](LICENSE)
