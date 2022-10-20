#!/bin/bash
set -eux

packages=(
    'pkg:/system/kvm' # qemu-img
)

rc=0
{ pfexec pkg install -v "${packages[@]}" || rc=$?; }
# Return codes:
#  0: Normal Success
#  4: Failure because we're already up-to-date. Also acceptable.
if [[ "$rc" -ne 4 ]] && [[ "$rc" -ne 0 ]]; then
    exit "$rc"
fi

# Install package for qemu-img
# (rc=4 if package already installed)
rc=0;
{ pfexec pkg install -v pkg:/system/kvm || rc=$?; }
if [[ "$rc" -ne 4 ]] && [[ "$rc" -ne 0 ]]; then
    exit "$rc"
fi
# We also want qemu-img in the next pass so just copy it over
# we're going from one helios box to another so it's prolly fine...
cp $(which qemu-img) /work/bin

pushd /work/tmp

REMOVE_DIRS=()

# Build popt (dependency of sgdisk)
wget https://mirrors.omnios.org/popt/popt-1.14.tar.gz
tar xf popt-1.14.tar.gz
rm popt-1.14.tar.gz
pushd popt-1.14
REMOVE_DIRS+=($(pwd))
./configure --disable-shared
gmake -j
popd # popt-1.14

# Build sgdisk
wget https://download.sourceforge.net/project/gptfdisk/gptfdisk/1.0.9/gptfdisk-1.0.9.tar.gz
tar xf gptfdisk-1.0.9.tar.gz
rm gptfdisk-1.0.9.tar.gz
pushd gptfdisk-1.0.9
REMOVE_DIRS+=($(pwd))
CXXFLAGS="-I../popt-1.14 -D_UUID_UUID_H" gmake sgdisk LDFLAGS="-L/lib -luuid -L../popt-1.14/.libs" -j
mv sgdisk /work/bin
popd # gptfdisk-1.0.9

# Build fuse kernel driver
# Workaround buggy fuse driver by building from source
wget https://mirrors.omnios.org/fuse/Version-1.4.tar.gz -O illumos-fusefs-Version-1.4.tar.gz
gtar xf illumos-fusefs-Version-1.4.tar.gz # gtar because tar throws some warning
rm illumos-fusefs-Version-1.4.tar.gz
pushd illumos-fusefs-Version-1.4
REMOVE_DIRS+=($(pwd))
pushd kernel/amd64

# Build the amd64 module
CFLAGS="-fident -fno-builtin -fno-asm -nodefaultlibs -Wall -Wno-unknown-pragmas -Wno-unused -fno-inline-functions -m64 -mcmodel=kernel -g -O2 -fno-inline -ffreestanding -fno-strict-aliasing -Wpointer-arith -gdwarf-2 -std=gnu99 -mno-red-zone -D_KERNEL -D__SOLARIS__ -mindirect-branch=thunk-extern -mindirect-branch-register"
PATH=$PATH:/opt/onbld/bin/i386 dmake CC=gcc CFLAGS="$CFLAGS"

# Copy the driver, install and load it
pfexec cp ../fuse.conf /usr/kernel/drv/
pfexec cp fuse /usr/kernel/drv/amd64/
pfexec chmod +x /usr/kernel/drv/amd64/fuse
pfexec bash ../pkgdefs/SUNWfusefs/postinstall
pfexec modload fuse

popd # kernel/amd64
popd # illumos-fusefs-Version-1.4

# Also build ntfs-3g from source because the package relies on the buggy fuse driver and would pull that in
wget https://mirrors.omnios.org/ntfs-3g/ntfs-3g_ntfsprogs-2017.3.23AR.6.tgz
tar xf ntfs-3g_ntfsprogs-2017.3.23AR.6.tgz
rm ntfs-3g_ntfsprogs-2017.3.23AR.6.tgz
pushd ntfs-3g_ntfsprogs-2017.3.23AR.6
REMOVE_DIRS+=($(pwd))
./configure --enable-really-static
gmake -j
cp ntfsprogs/mkntfs /work/bin
cp src/ntfs-3g /work/bin
popd # ntfs-3g_ntfsprogs-2017.3.23AR.6

# Build propolis
git clone https://github.com/oxidecomputer/propolis.git
pushd propolis
REMOVE_DIRS+=($(pwd))
cargo build --release --bin propolis-standalone
mv target/release/propolis-standalone /work/bin
popd # propolis

popd # /work/tmp

rm -rf ${REMOVE_DIRS[@]}