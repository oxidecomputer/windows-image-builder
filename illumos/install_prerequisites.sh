#!/bin/bash
set -eux

BUILD_DIR=""
OUTPUT_DIR=""
REMOVE_DIRS=()

cleanup() {
    cd "$(dirs -l -0)" && dirs -c
    rm -rf "${REMOVE_DIRS[@]}"
}

trap cleanup EXIT ERR

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
pkgs="pkg:/system/kvm pkg:/ooce/system/file-system/ntfs-3g pkg:/ooce/driver/fuse pkg:/compress/p7zip"
rc=0;
{ pfexec pkg install -v $pkgs || rc=$?; }
# $rc is 4 if the package is already installed.
if [[ "$rc" -ne 4 ]] && [[ "$rc" -ne 0 ]]; then
    exit "$rc"
fi

pushd "$BUILD_DIR"

# Build popt (dependency of sgdisk)
wget https://mirrors.omnios.org/popt/popt-1.14.tar.gz
tar xf popt-1.14.tar.gz
rm popt-1.14.tar.gz
pushd popt-1.14
REMOVE_DIRS+=("$(pwd)")
./configure --disable-shared
gmake -j
popd # popt-1.14

# Build sgdisk
wget https://download.sourceforge.net/project/gptfdisk/gptfdisk/1.0.9/gptfdisk-1.0.9.tar.gz
tar xf gptfdisk-1.0.9.tar.gz
rm gptfdisk-1.0.9.tar.gz
pushd gptfdisk-1.0.9
REMOVE_DIRS+=("$(pwd)")
CXXFLAGS="-I../popt-1.14 -D_UUID_UUID_H" gmake sgdisk LDFLAGS="-L/lib -luuid -L../popt-1.14/.libs" -j
mv sgdisk "$OUTPUT_DIR"
popd # gptfdisk-1.0.9

# Build propolis
git clone https://github.com/oxidecomputer/propolis.git
pushd propolis
REMOVE_DIRS+=("$(pwd)")
cargo build --release --bin propolis-standalone
mv target/release/propolis-standalone "$OUTPUT_DIR"
popd # propolis

popd # /work/tmp
