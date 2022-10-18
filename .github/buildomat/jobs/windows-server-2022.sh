#!/bin/bash
#:
#: name = "Windows Server 2022"
#: variety = "basic"
#: target = "lab"
#: rust_toolchain = "stable"
#: output_rules = [
#:	"=/work/out/windows-server-2022-genericcloud-amd64.raw",
#:	"=/work/out/windows-server-2022-genericcloud-amd64.sha256.txt",
#: ]
#:
#: [[publish]]
#: series = "image"
#: name = "windows-server-2022-genericcloud-amd64.raw"
#: from_output = "/work/out/windows-server-2022-genericcloud-amd64.raw"
#:
#: [[publish]]
#: series = "image"
#: name = "windows-server-2022-genericcloud-amd64.sha256.txt"
#: from_output = "/work/out/windows-server-2022-genericcloud-amd64.sha256.txt"

set -o errexit
set -o pipefail
set -o xtrace

cargo --version
rustc --version

mkdir /work/{bin,out,tmp}
ln -s $(pwd) /work/src

export PATH=$PATH:/work/bin

# TMP_DEV=$(pfexec ramdiskadm -a work 12g)
# yes | pfexec newfs $TMP_DEV
# pfexec mount $TMP_DEV /work/tmp
# pfexec chmod 1777 /work/tmp

banner prerequisites
ptime -m bash ./install_prerequisites.sh

# Create the image
banner Creating Windows Server 2022 Image
ptime -m bash ./create_windows_server_2022_image.sh

cd /work/out
digest -a sha256 windows-server-2022-genericcloud-amd64.raw > windows-server-2022-genericcloud-amd64.sha256.txt