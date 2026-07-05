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
- NVS persistence for runtime config, cursor, reply mapping, and block list
- Outbound SMS queue with exponential-backoff retry
- Telegram-delivered OTA firmware updates over WiFi HTTPS
- Flash-backed `/log [offset]` event ring for SMS, boot, network, OTA, and user operations; pages show 16 entries with Telegram inline buttons for paging, and SMS log previews keep up to 160 chars before the fixed-size flash record is trimmed to fit
- ESP-IDF task watchdog plus Telegram poll/send software watchdogs
- Build commit hash embedded in `/status` output

## Quick Start

```bash
# 1. Install Xtensa Rust toolchain
cargo install espup && espup install

# 2. Copy and fill in config
cp config.toml.example config.toml
# Edit config.toml with WiFi/Telegram credentials, or leave them blank and provision over serial

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

Generate both OTA app images to send to the bot:

```bash
./tools/build_ota_images.sh
```

The script writes:

- `smsgate-ota-software-only.bin` — update firmware software only; keep the
  existing NVS `smsgcfg` runtime configuration.
- `smsgate-ota-with-config.bin` — update firmware software and, on first boot
  of the new image, write the compiled `config.toml` WiFi, Telegram, modem/APN/SIM,
  and bridge runtime configuration into NVS.

Send the chosen `.bin` to the configured Telegram chat with caption `/ota`.
OTA downloads use WiFi HTTPS only; cellular fallback mode will reject OTA.

When flashing over USB, keep the `--partition-table partitions_ota.csv` and
`--target-app-partition ota_0` flags, and erase `otadata`. The firmware uses a
custom OTA partition layout; omitting these flags can target the wrong app slot,
and stale OTA data can keep booting `ota_1` after `ota_0` was flashed.

Plain `cargo +esp build --release --target xtensa-esp32-espidf` keeps the USB
flash workflow as a software-and-config update when `config.toml` exists. To
build an image that preserves NVS runtime configuration, set
`SMSGATE_APPLY_COMPILED_CONFIG=0` for that build.

### USB Partition Migration Recovery

If a board was previously flashed with the old single-factory-app layout, a bare
`espflash flash <ELF> --port <PORT>` can leave the old partition table on flash.
The symptom is a boot log that lists only `nvs`, `phy_init`, and `factory`, plus
warnings such as `flash log partition not found: log_ring` or
`esp_ota_ops: not found otadata`.

For recovery, keep NVS intact and write the OTA layout explicitly:

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

Use `--baud 115200` if the CH9102 bridge reports intermittent communication
errors during `write-bin`. Do not erase the `0x9000` NVS partition unless the
device will be provisioned again. Verify the next boot lists `otadata`, `ota_0`,
`ota_1`, and `log_ring`, then confirm `smsgate starting... build=<hash>`,
`[boot] ... build=<hash>`, and `flash-backed log ring mounted`.

## Configuration

`config.toml` supplies compile-time defaults for WiFi, Telegram, modem pins, modem/APN/SIM settings, bridge timing, and UI locale. Runtime network, modem/SIM, and bridge settings are stored in NVS under the `smsgcfg` namespace. Images built with `SMSGATE_APPLY_COMPILED_CONFIG=1` overwrite those NVS runtime values with the compiled defaults on boot; images built with `SMSGATE_APPLY_COMPILED_CONFIG=0` preserve the existing NVS values. UI locale is still selected at compile time. See [`config.toml.example`](config.toml.example) for all options.

To build with Chinese UI strings, add to your `config.toml`:

```toml
[ui]
locale = "zh"
```

## Design Tradeoffs

**`serde_json` for Telegram API parsing** — The Telegram HTTP layer uses `serde_json`, which requires heap allocation. This is a deliberate tradeoff: the ESP32 has ample SRAM (320 KB + optional PSRAM), a typical Telegram API response is a few kilobytes, and `serde-json-core` (the `no_std` alternative) would add significant implementation complexity for marginal gain. If you port this to a more constrained MCU, swapping out `im/telegram/` is the only change needed.

**Configuration boundaries** — Hardware defaults and locale are compile-time settings. Runtime network, modem/SIM, and bridge settings can be baked into `config.toml` for simple deployments or provisioned over serial into NVS without rebuilding the firmware. OTA images choose whether to preserve or overwrite the NVS-backed runtime config through `SMSGATE_APPLY_COMPILED_CONFIG`.

**Runtime task split** — Telegram polling and Telegram outbound delivery run in separate worker threads. SMS and modem AT operations keep a single ordered UART owner so URCs, SMS reads/deletes, and `AT+CMGS` prompt handling do not interleave.

## Architecture

The system is built around core traits. Business logic depends on these abstractions rather than concrete Telegram, ESP-IDF, board, or NVS implementations:

| Trait | Abstracts |
|-------|-----------|
| `ModemPort` | AT commands, URC polling, PDU SMS send |
| `MessageSink` / `MessageSource` | Send/poll IM messages (Telegram) |
| `Store` | NVS key-value persistence |
| `Board` | Pin layout, power-on sequence, and modem construction |
| `Command` | Single bot command (name, description, handler) |

## USB Driver

The USB-to-serial chip varies by board. The reference board (T-A7670X) uses a **CH9102**.

- **Linux**: typically works out of the box (`/dev/ttyUSB0`); if not, load the appropriate kernel module (`ch341`, `cp210x`, etc.)
- **macOS**: install the driver matching your chip (e.g. [CH34x](https://www.wch-ic.com/downloads/CH34XSER_MAC_ZIP.html) for CH9102) and approve the kext in System Settings > Privacy & Security
- **Windows**: usually auto-detected; if not, install from the chip vendor (e.g. [WCH](https://www.wch-ic.com/downloads/CH343SER_ZIP.html) for CH9102)

## License

[MIT](LICENSE)
