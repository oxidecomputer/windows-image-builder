#!/bin/bash
set -eu
trap "trap - SIGTERM && kill -- -$$" SIGINT SIGTERM EXIT

WIN_IMAGE=/work/out/windows-server-2022-genericcloud-amd64.raw

WIN_ISO=windows-installer.iso
WIN_REPACK=windows-installer.raw
VIRTIO_ISO=virtio-win.iso
OVMF_PATH=OVMF_CODE.fd
WIN_TOML=windows-server-2022.toml

WINDOWS_SERVER_ISO="https://software-static.download.prss.microsoft.com/sg/download/888969d5-f34g-4e03-ac9d-1f9786c66749/SERVER_EVAL_x64FRE_en-us.iso"
VIRTIO_DRIVERS_ISO="https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso"
OVMF_BLOB="https://oxide-omicron-build.s3.amazonaws.com/OVMF_CODE_20220922.fd"

pushd /work/tmp

banner "OVMF"
wget --progress=dot:giga $OVMF_BLOB -O $OVMF_PATH

banner "VirtIO"
wget --progress=dot:giga $VIRTIO_DRIVERS_ISO -O $VIRTIO_ISO

banner "Windows Server 2022"
wget --progress=dot:giga $WINDOWS_SERVER_ISO -O $WIN_ISO

# Begin re-packing the ISO

# Create blank 5G image for the Windows Setup
qemu-img create -f raw $WIN_REPACK 5G

# Create GPT structures and a single partition w/ type 'Microsoft Basic Data'
sgdisk -og $WIN_REPACK
sgdisk -n=1:0:0 -t 1:0700 $WIN_REPACK

# Create loopback
WIN_INST_LOFI=${$(pfexec lofiadm -l -a $WIN_REPACK)/p0/s0}

# Format partition as FAT32
yes | pfexec mkfs -F pcfs -o fat=32 ${WIN_INST_LOFI/dsk/rdsk}

# Mount partition
mkdir iso-mount
pfexec mount -F pcfs $WIN_INST_LOFI iso-mount

# Extract original ISO into the mounted disk iamge
# (Excluding install.wim)
7z x '-xr!install.wim' $WIN_ISO -oiso-mount

# Extract install.wim separately and split it across multiple files
# so we don't hit the FAT32 limits
7z e '-ir!install.wim' $WIN_ISO
rm $WIN_ISO
wimlib-imagex split install.wim iso-mount/sources/install.swm 3000
rm install.wim

# Autounattend.xml will drive the setup without any user input
cp /work/src/unattend/Autounattend.xml iso-mount/

# Copy any files we want to be installed alongside the OS
#   $OEM$/$1/ corresponds to the root drive Windows will be installed to (i.e. C:\)
OEM_PATH=iso-mount/sources/\$OEM\$/\$1/oxide
mkdir -p $OEM_PATH

# Setup script
cp /work/src/unattend/OxidePrepBaseImage.ps1 $OEM_PATH/

# Drivers:
# We don't need this past `offlineServicing` which is still during Windows Setup
# so no need to copy to $OEM_PATH. Just keep it on the install disk.
7z e $VIRTIO_ISO -oiso-mount/virtio-drivers/ {viostor,NetKVM}/2k22/amd64/\*.{cat,inf,sys}

# Cloudbase-init config
mkdir $OEM_PATH/cloudbase/
cp -r /work/src/unattend/cloudbase-* $OEM_PATH/cloudbase/

pfexec umount iso-mount
pfexec lofiadm -d $WIN_INST_LOFI

# Create blank image we'll install Windows to
qemu-img create -f raw $WIN_IMAGE 32G

cat << EOF >$WIN_TOML
[main]
name = "windows-server-2022"
cpus = 2
memory = 2048
bootrom = "$OVMF_PATH"

[block_dev.win_image]
type = "file"
path = "$WIN_IMAGE"
[dev.block0]
driver = "pci-nvme"
block_dev = "win_image"
pci-path = "0.16.0"

[block_dev.win_iso]
type = "file"
path = "$WIN_REPACK"
[dev.block1]
driver = "pci-nvme"
block_dev = "win_iso"
pci-path = "0.17.0"

[dev.net0]
driver = "pci-virtio-viona"
vnic = "vnic0"
pci-path = "0.8.0"
EOF

banner "Creating image"
pfexec propolis-standalone $WIN_TOML &
PROPOLIS_PID=$!

# Kick off propolis and dump the VM's COM1 output
nc -Ud ttya &

# Wait for the installation to finish
wait $PROPOLIS_PID

# Find the bounds of the last partition (OS partition)
SECTOR_SZ=$(sgdisk -p $WIN_IMAGE | grep -i "Sector size" | awk '{ print $4; }')
OS_PART_END_SECTOR=$(sgdisk -i 4 $WIN_IMAGE | grep "Last sector" | awk '{ print $3; }')
OS_PART_END=$(echo "$OS_PART_END_SECTOR * $SECTOR_SZ" | bc)
# Resize the image to the end of the last partition + 33 sectors for the secondary GPT table at the end of the disk
NEW_SIZE=$(echo "($OS_PART_END + ($SECTOR_SZ - ($OS_PART_END % $SECTOR_SZ))) + 33 * $SECTOR_SZ" | bc)
qemu-img resize -f raw $WIN_IMAGE $NEW_SIZE
# Repair secondary GPT table
sgdisk -e $WIN_IMAGE

popd # /work/tmp

banner "Done!"