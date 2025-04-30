#!/usr/bin/env bash
set -euo pipefail

echo "🛠️  Checking system compatibility..."

errors=()

# Check if script is run as root or with sudo when needed
if [[ "$(id -u)" -ne 0 ]]; then
  echo "⚠️  Some checks will require elevated permissions (sudo)."
  echo "   This is needed to:"
  echo "   - Verify and set iptables NAT rules"
  echo "   - Enable IP forwarding if needed"
  echo "   - Inspect system-wide configurations"
  echo ""
fi

# Check KVM availability
if [[ ! -e /dev/kvm ]]; then
    errors+=("❌ KVM device not found. Is virtualization enabled in your BIOS?")
fi

if ! groups | grep -q '\bkvm\b'; then
    errors+=("❌ You are not in the 'kvm' group. Add yourself with:")
    errors+=("   sudo usermod -aG kvm $USER && newgrp kvm")
fi

# Check QEMU availability
if ! command -v qemu-system-x86_64 >/dev/null; then
    errors+=("❌ qemu-system-x86_64 not found. Please install QEMU.")
fi

# Check Rust / Cargo availability
if ! command -v cargo >/dev/null || ! command -v rustc >/dev/null; then
    errors+=("❌ Rust and/or Cargo are not installed or not in your PATH.")
    errors+=("   Please visit https://www.rust-lang.org/tools/install to install Rust.")
fi

# Determine which packages are required
required_pkgs=(qemu-system-x86 qemu-utils genisoimage wimtools gdisk curl unzip)
missing_pkgs=()

for pkg in "${required_pkgs[@]}"; do
    if ! dpkg -s "$pkg" &>/dev/null; then
        missing_pkgs+=("$pkg")
    fi
done

if (( ${#missing_pkgs[@]} )); then
    echo "📦 The following packages are missing and will be installed:"
    printf '  - %s\n' "${missing_pkgs[@]}"
    if [[ -x "./install_prerequisites.sh" ]]; then
        echo "🔧 Running install_prerequisites.sh..."
        ./install_prerequisites.sh
    else
        errors+=("❌ Missing required packages: ${missing_pkgs[*]}")
        errors+=("   And install_prerequisites.sh was not found or executable.")
    fi
fi

# Ensure iptables MASQUERADE rule exists
#if ! sudo iptables -t nat -C POSTROUTING -s 10.0.0.0/8 -j MASQUERADE 2>/dev/null; then
    #echo "⚠️  Adding MASQUERADE rule for 10.0.0.0/8..."
    #sudo iptables -t nat -A POSTROUTING -s 10.0.0.0/8 -j MASQUERADE
#fi

# Ensure IP forwarding is enabled
#if [[ "$(sysctl -n net.ipv4.ip_forward)" -ne 1 ]]; then
    #echo "⚠️  Enabling IP forwarding..."
    #sudo sysctl -w net.ipv4.ip_forward=1
#fi

# Report results
if (( ${#errors[@]} )); then
    echo ""
    echo "🚫 Some system requirements are not met:"
    for err in "${errors[@]}"; do
        echo "  $err"
    done
    exit 1
else
    echo "✅ System check passed."
fi
