#!/usr/bin/env bash
set -euo pipefail

echo "📁 Validating image build environment and inputs..."

# Load config
ENV_FILE="imgbuild.env"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "❌ Configuration file '$ENV_FILE' not found."
  echo "   Please create it and define required paths."
  exit 1
fi

# shellcheck disable=SC1090
source "$ENV_FILE"

# Required variables
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
  echo "❌ Missing required environment variables in '$ENV_FILE':"
  printf '  - %s\n' "${missing_vars[@]}"
  exit 1
fi

# File/path checks
check_path() {
  local label="$1"
  local path="$2"
  local type="$3"  # 'file' or 'dir'
  if [[ "$type" == "file" && ! -f "$path" ]]; then
    echo "❌ Missing file: $label ($path)"
    return 1
  elif [[ "$type" == "dir" && ! -d "$path" ]]; then
    echo "❌ Missing directory: $label ($path)"
    return 1
  fi
  return 0
}

errors=()
check_path "Windows ISO" "$WINDOWS_ISO" "file" || errors+=("WINDOWS_ISO")
check_path "VirtIO ISO" "$VIRTIO_ISO" "file" || errors+=("VIRTIO_ISO")
check_path "Unattended Directory" "$UNATTEND_DIR" "dir" || errors+=("UNATTEND_DIR")
check_path "OVMF Firmware" "$OVMF_PATH" "file" || errors+=("OVMF_PATH")
check_path "Working Directory" "$WORK_DIR" "dir" || errors+=("WORK_DIR")

if (( ${#errors[@]} )); then
  echo "🚫 One or more required paths are missing or invalid:"
  printf '  - %s\n' "${errors[@]}"
  exit 1
else
  echo "✅ All required input paths are present."
fi

