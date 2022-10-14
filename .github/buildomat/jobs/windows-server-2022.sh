#!/bin/bash
#:
#: name = "Windows Server 2022"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:	"=/work/windows-server-2022-genericcloud-amd64.*",
#: ]
#:
#: [[publish]]
#: series = "image"
#: name = "windows-server-2022-genericcloud-amd64.raw"
#: from_output = "/work/windows-server-2022-genericcloud-amd64.raw"
#:
#: [[publish]]
#: series = "image"
#: name = "windows-server-2022-genericcloud-amd64.sha256.txt"
#: from_output = "/work/windows-server-2022-genericcloud-amd64.sha256.txt"

set -o errexit
set -o pipefail
set -o xtrace

# Install prerequisites
sudo ./install_prerequisites.sh

# Create the image
BLOCK_SZ=512
sudo ./create_windows_server_2022_image.sh /work/windows-server-2022-genericcloud-amd64.raw $BLOCK_SZ

cd /work
digest -a sha256 windows-server-2022-genericcloud-amd64.raw > windows-server-2022-genericcloud-amd64.sha256.txt