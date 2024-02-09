#!/bin/bash

set -eu

install_linux_prerequisites() {
    local packages=(
    'gdisk'
    'genisoimage'
    'libguestfs-tools'
    'ovmf'
    'qemu-system-x86'
    'qemu-system-gui'
    'qemu-utils'
    )

    sudo apt-get update
    sudo apt-get install -y --no-install-recommends "${packages[@]}"

    sudo modprobe kvm
    sudo modprobe kvm_amd || sudo modprobe kvm_intel
}

install_illumos_prerequisites() {
    local BUILD_DIR=""
    local OUTPUT_DIR=""
    local REMOVE_DIRS=()
    local pkgs="pkg:/system/kvm pkg:/ooce/system/file-system/ntfs-3g pkg:/ooce/driver/fuse pkg:/compress/p7zip pkg:/ooce/system/gptfdisk"
    local rc=0;

    # shellcheck disable=SC2317
    illumos_cleanup() {
        cd "$(dirs -l -0)" && dirs -c
        rm -rf "${REMOVE_DIRS[@]}"
    }

    trap illumos_cleanup EXIT ERR

    while [ $# -gt 0 ]; do
        case "$1" in
            -b|--build-dir)
                BUILD_DIR="$2"
                shift 2
                ;;
            -o|--output-dir)
                OUTPUT_DIR="$2"
                shift 2
                ;;
            *)
                echo "Unexpected argument (use only --build-dir and --output-dir)"
                exit 1
                ;;
        esac
    done

    if [[ -z "$BUILD_DIR" ]]; then
        echo "--build-dir is required"
        exit 1
    fi

    if [[ -z "$OUTPUT_DIR" ]]; then
        echo "--output-dir is required"
        exit 1
    fi

    if [[ ! -d "$BUILD_DIR" ]]; then
        echo "build directory does not exist"
        exit 1
    fi

    if [[ ! -d "$OUTPUT_DIR" ]]; then
        echo "output directory does not exist"
        exit 1
    fi

    # Install required packages.
    { pfexec pkg install -v "$pkgs" || rc=$?; }
    # $rc is 4 if the package is already installed.
    if [[ "$rc" -ne 4 ]] && [[ "$rc" -ne 0 ]]; then
        exit "$rc"
    fi

    # Build propolis-standalone separately.
    pushd "$BUILD_DIR"
    git clone https://github.com/oxidecomputer/propolis.git
    pushd propolis
    REMOVE_DIRS+=("$(pwd)")
    cargo build --release --bin propolis-standalone
    mv target/release/propolis-standalone "$OUTPUT_DIR"
    popd # propolis
    popd # /work/tmp

}

case $(uname -s) in
    SunOS)
        install_illumos_prerequisites "$@"
        ;;
    Linux)
        install_linux_prerequisites
        ;;
    *)
        echo "unsupported host OS $(uname -s)"
        exit 1
        ;;
esac

exit 0
