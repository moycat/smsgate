#!/usr/bin/env bash
set -euo pipefail

TARGET_TRIPLE="xtensa-esp32-espidf"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
ELF="${TARGET_DIR}/${TARGET_TRIPLE}/release/smsgate"
OUT_DIR="${1:-.}"

if [[ ! -f config.toml ]]; then
  echo "error: config.toml is required to build the with-config OTA image." >&2
  echo "       Copy config.toml.example to config.toml and fill in real values." >&2
  exit 1
fi

mkdir -p "${OUT_DIR}"

build_ota_image() {
  local apply_compiled_config="$1"
  local output_name="$2"

  echo "Building ${output_name} with SMSGATE_APPLY_COMPILED_CONFIG=${apply_compiled_config}"
  SMSGATE_APPLY_COMPILED_CONFIG="${apply_compiled_config}" \
    cargo +esp build --release --target "${TARGET_TRIPLE}"

  espflash save-image \
    --chip esp32 \
    --flash-size 4mb \
    --partition-table partitions_ota.csv \
    --target-app-partition ota_0 \
    "${ELF}" \
    "${OUT_DIR}/${output_name}"
}

build_ota_image 0 "smsgate-ota-software-only.bin"
build_ota_image 1 "smsgate-ota-with-config.bin"
