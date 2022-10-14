#!/bin/bash
set -eu

packages=(
    'gdisk'
    'genisoimage'
    'libguestfs-tools'
    'ovmf'
    'qemu-system-x86'
    'qemu-utils'
)

sudo apt-get update
sudo apt-get install -y --no-install-recommends ${packages[@]}

sudo modprobe kvm
sudo modprobe kvm_amd || sudo modprobe kvm_intel
