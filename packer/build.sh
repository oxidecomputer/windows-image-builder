#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Default values.
OVMF_VARS="${OVMF_VARS:-/usr/share/edk2/x64/OVMF_VARS.4m.fd}"

# Copy OVMF_VARS so QEMU has a writable copy for EFI variable storage.
# Placed next to the template so the qemuargs pflash path resolves correctly.
cp "$OVMF_VARS" "$SCRIPT_DIR/efivars.fd"

# Forward all arguments to packer build.
packer build "$@" .
