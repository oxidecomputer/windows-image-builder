#!/usr/bin/env bash
set -euo pipefail

# Load all modules
MODULE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/imgbuild.d" && pwd)"

# Default functions
function usage() {
  echo "Usage: $0 [command]"
  echo ""
  echo "Commands:"
  echo "  check-system   Run system checks (KVM, QEMU, firewall, dependencies)"
  echo "  build          Run the full image build process (wimsy)"
  echo "  validate-env   Validate required directories and files"
  echo "  build-image    Build the Windows image using the supplied config"
  echo "  run-all        Run all steps: check, validate, build app, build image"
  echo ""
  exit 1
}

# Parse command
CMD="${1:-}"
case "$CMD" in
  check-system)
    bash "$MODULE_DIR/check_system.sh"
    ;;
  build)
    bash "$MODULE_DIR/build_app.sh"
    ;;
  validate-env)
    bash "$MODULE_DIR/validate_inputs.sh"
    ;;
  build-image)
    bash "$MODULE_DIR/build_image.sh"
    ;;
  run-all)
    echo "==> [1/4] Checking system..."
    bash "$MODULE_DIR/check_system.sh"
    echo "==> [2/4] Validating inputs..."
    bash "$MODULE_DIR/validate_inputs.sh"
    echo "==> [3/4] Building wimsy CLI tool..."
    bash "$MODULE_DIR/build_app.sh"
    echo "==> [4/4] Building Windows image..."
    bash "$MODULE_DIR/build_image.sh"
    echo "✅ All steps completed successfully."
    ;;
  *)
    usage
    ;;
esac
