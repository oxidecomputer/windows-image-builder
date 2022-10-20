#!/bin/bash
#:
#: name = "windows-server-2022-repack"
#: variety = "basic"
#: target = "helios"
#: rust_toolchain = "stable"
#:
#: output_rules = [
#:	"=/work/out/windows-server-2022-installer.raw",
#:	"=/work/bin/propolis-standalone",
#:	"=/work/bin/qemu-img",
#:	"=/work/bin/sgdisk",
#: ]

set -o errexit
set -o pipefail
set -o xtrace

cargo --version
rustc --version

mkdir -p /work/{bin,out,tmp}
ln -s $(pwd) /work/src

export PATH=$PATH:/work/bin

banner prereqs
ptime -m bash ./install_prerequisites.sh

# Create the repacked Windows Server 2022 Installer that includes the VirtIO drivers and unattended installation files
banner repack installer
ptime -m bash /work/src/create_windows_server_2022_image.sh repack /work/src/unattend /work/out/windows-server-2022-installer.raw