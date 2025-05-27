#!/usr/bin/env bash
set -euo pipefail

echo "📦 Checking for Rust application (wimsy)..."

WIMSY_BIN="./target/release/wimsy"

# Check if binary already exists
if [[ -x "$WIMSY_BIN" ]]; then
  echo "✅ Rust binary already built: $WIMSY_BIN"
  exit 0
fi

# Check if Cargo is available
if ! command -v cargo >/dev/null; then
  echo "❌ Cargo is not installed or not in PATH."
  echo "   Please install Rust: https://www.rust-lang.org/tools/install"
  exit 1
fi

# Build using cargo in release mode
echo "🔨 Building wimsy using: cargo build --release"
cargo build --release

# Confirm build result
if [[ -x "$WIMSY_BIN" ]]; then
  echo "✅ Build successful. Executable located at $WIMSY_BIN"
else
  echo "❌ Build failed or output binary not found."
  exit 1
fi

