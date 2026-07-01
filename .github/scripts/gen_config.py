#!/usr/bin/env python3
"""Generate config.toml from environment variables for CI builds.

Usage:
    python3 .github/scripts/gen_config.py > config.toml

Recognized environment variables:
    WIFI_SSID, WIFI_PASSWORD,
    TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID

Optional:
    UI_LOCALE  — "en" (default) or "zh"
"""

import os
import sys


def toml_str(s: str) -> str:
    """Return a TOML basic string literal with backslash and quote escaping."""
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def env(name: str, default: str = "") -> str:
    return os.environ.get(name, default)

chat_id_str = env("TELEGRAM_CHAT_ID", "0")
try:
    chat_id = int(chat_id_str)
except ValueError:
    print(f"error: TELEGRAM_CHAT_ID must be an integer, got {chat_id_str!r}", file=sys.stderr)
    sys.exit(1)

lines = [
    "[wifi]",
    f"ssid     = {toml_str(env('WIFI_SSID'))}",
    f"password = {toml_str(env('WIFI_PASSWORD'))}",
    "",
    "[im]",
    f"bot_token = {toml_str(env('TELEGRAM_BOT_TOKEN'))}",
    f"chat_id   = {chat_id}",
    "",
    "[modem]",
    "uart_tx           = 26",
    "uart_rx           = 27",
    "uart_baud         = 115200",
    "pwrkey            = 4",
    "cellular_data     = false",
    "cellular_fallback = false",
    'apn               = ""',
    'apn_user          = ""',
    'apn_pass          = ""',
    "",
    "[bridge]",
    "max_failures_before_reboot = 8",
    "poll_interval_ms           = 3000",
    "",
    "[ui]",
    f"locale = {toml_str(env('UI_LOCALE', 'en'))}",
]

print("\n".join(lines))
