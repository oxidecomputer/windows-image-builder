#!/bin/bash
#:
#: name = "windows-server-2022-build"
#: variety = "basic"
#: target = "lab"
#: skip_clone = true
#:
#: [dependencies.repack]
#: job = "windows-server-2022-repack"
#:
#: output_rules = [
#:	"=/work/out/windows-server-2022-genericcloud-amd64.raw",
#:	"=/work/out/windows-server-2022-genericcloud-amd64.raw.sha256.txt",
#: ]
#:
#: [[publish]]
#: series = "image"
#: name = "windows-server-2022-genericcloud-amd64.raw"
#: from_output = "/work/out/windows-server-2022-genericcloud-amd64.raw"
#:
#: [[publish]]
#: series = "image"
#: name = "windows-server-2022-genericcloud-amd64.raw.sha256.txt"
#: from_output = "/work/out/windows-server-2022-genericcloud-amd64.raw.sha256.txt"

set -o errexit
set -o pipefail
set -o xtrace

# Artifacts from the previous step
REPACK_OUTPUTS=/input/repack

mkdir -p /work/{out,tmp}

export PATH=$PATH:$REPACK_OUTPUTS/work/bin

# Create the image
win_image="/work/out/windows-server-2022-genericcloud-amd64.raw"
ptime -m bash /work/src/create_windows_server_2022_image.sh build $REPACK_OUTPUTS/work/out/windows-server-2022-installer.raw $win_image
digest -a sha256 $win_image > $win_image.sha256.txt