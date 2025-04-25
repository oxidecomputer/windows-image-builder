#!/usr/bin/env bash
set -euo pipefail

echo "🖥️  Starting Windows image build process..."

# Load environment variables
ENV_FILE="imgbuild.env"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "❌ Configuration file '$ENV_FILE' not found."
  echo "   Please create it first."
  exit 1
fi

# shellcheck disable=SC1090
source "$ENV_FILE"

# Validate required variables
required_vars=(
  WORK_DIR
  OUTPUT_IMAGE
  WINDOWS_ISO
  VIRTIO_ISO
  UNATTEND_DIR
  OVMF_PATH
)

missing_vars=()

for var in "${required_vars[@]}"; do
  if [[ -z "${!var:-}" ]]; then
    missing_vars+=("$var")
  fi
done

if (( ${#missing_vars[@]} )); then
  echo "❌ Missing required environment variables:"
  printf '  - %s\n' "${missing_vars[@]}"
  exit 1
fi

# Check if wimsy binary exists
WIMSY_BIN="./target/release/wimsy"
if [[ ! -x "$WIMSY_BIN" ]]; then
  echo "❌ wimsy binary not found. Please build it first using ./build.sh build-rust."
  exit 1
fi

# Build the command line
CMD=(
  "$WIMSY_BIN"
  --work-dir "$WORK_DIR"
  --output-image "$OUTPUT_IMAGE"
  create-guest-disk-image
  --windows-iso "$WINDOWS_ISO"
  --virtio-iso "$VIRTIO_ISO"
  --unattend-dir "$UNATTEND_DIR"
  --ovmf-path "$OVMF_PATH"
)

# Handle VGA_CONSOLE if set
if [[ "${VGA_CONSOLE:-false}" == "true" ]]; then
  # Check if an X display is available
  if [[ -n "${DISPLAY:-}" ]]; then
    echo "🖥️  VGA console requested and X display found: $DISPLAY"
    CMD+=(--vga-console)
  else
    echo "⚠️  VGA console requested but no DISPLAY available. Skipping VGA console option."
  fi
fi

# Show the command for visibility
echo "🔧 Running: ${CMD[*]}"

# Run the build command
"${CMD[@]}"

echo "✅ Windows image build completed."

