#!/bin/bash
set -eu

packages=(
    'pkg:/system/kvm'
)

rc=0
{ pfexec pkg install -v "${packages[@]}" || rc=$?; }
# Return codes:
#  0: Normal Success
#  4: Failure because we're already up-to-date. Also acceptable.
if [[ "$rc" -ne 4 ]] && [[ "$rc" -ne 0 ]]; then
    exit "$rc"
fi

pushd /work/tmp

REMOVE_DIRS=()

wget https://mirrors.omnios.org/popt/popt-1.14.tar.gz
tar xf popt-1.14.tar.gz
rm popt-1.14.tar.gz
pushd popt-1.14
REMOVE_DIRS+=($(pwd))
./configure --disable-shared
gmake -j
popd # popt-1.14

wget https://download.sourceforge.net/project/gptfdisk/gptfdisk/1.0.9/gptfdisk-1.0.9.tar.gz
tar xf gptfdisk-1.0.9.tar.gz
rm gptfdisk-1.0.9.tar.gz
pushd gptfdisk-1.0.9
REMOVE_DIRS+=($(pwd))
CXXFLAGS="-I../popt-1.14 -D_UUID_UUID_H" gmake sgdisk LDFLAGS="-L/lib -luuid -L../popt-1.14/.libs" -j
mv sgdisk /work/bin
popd # gptfdisk-1.0.9

wget https://wimlib.net/downloads/wimlib-1.13.6.tar.gz
tar xf wimlib-1.13.6.tar.gz
rm wimlib-1.13.6.tar.gz
pushd wimlib-1.13.6
REMOVE_DIRS+=($(pwd))
./configure --disable-shared --without-ntfs-3g
gmake -j
mv wimlib-imagex /work/bin/wimlib-imagex
popd # wimlib-1.13.6

git clone https://github.com/oxidecomputer/propolis.git
pushd propolis
REMOVE_DIRS+=($(pwd))
cargo build --release --bin propolis-standalone
mv target/release/propolis-standalone /work/bin
popd # propolis

NIC_NAME="vnic0"
NIC_MAC="02:08:20:ac:e9:30"
NIC_LINK="$(dladm show-phys -po LINK | tail -1)"
pfexec dladm create-vnic -t -l $NIC_LINK -m $NIC_MAC $NIC_NAME

popd # /work/tmp

rm -rf ${REMOVE_DIRS[@]}