#!/bin/bash
set -eu
trap "trap - SIGTERM && kill -- -$$" SIGINT SIGTERM EXIT

WIN_IMAGE=${1:-"windows-server-2022-genericcloud-amd64.raw"}
BLOCK_SZ=${2:-512}

WINDOWS_SERVER_ISO="https://software-static.download.prss.microsoft.com/sg/download/888969d5-f34g-4e03-ac9d-1f9786c66749/SERVER_EVAL_x64FRE_en-us.iso"
WIN_ISO=windows-installer.iso

VIRTIO_DRIVERS_ISO="https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso"
VIRTIO_ISO=virtio-win.iso

UNATTEND_ISO=unattend.iso

banner "Grabbing Windows Server 2022 ISO"
wget --progress=dot:giga $WINDOWS_SERVER_ISO -O $WIN_ISO

banner "Grabbing VirtIO Drivers ISO"
wget --progress=dot:giga $VIRTIO_DRIVERS_ISO -O $VIRTIO_ISO

banner "Creating blank image"
qemu-img create -f raw $WIN_IMAGE 32G

banner "Creating Unattended scripts & config ISO"
genisoimage -J -R -o $UNATTEND_ISO unattend

QEMU_ARGS=(
	-nodefaults
    -enable-kvm
    -M pc
    -m 2048
    -cpu host,kvm=off,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time
    -smp 2,sockets=1,cores=2
    -rtc base=localtime
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE.fd

    -netdev user,id=net0
    -device virtio-net-pci,netdev=net0

    -device nvme,drive=drivec,serial=deadbeef,physical_block_size=$BLOCK_SZ,logical_block_size=$BLOCK_SZ,discard_granularity=$BLOCK_SZ
    -drive if=none,id=drivec,file=$WIN_IMAGE,format=raw

    -device ide-cd,drive=win-disk,id=cd-disk0,unit=0,bus=ide.0
    -drive file=$WIN_ISO,if=none,id=win-disk,media=cdrom

    -device ide-cd,drive=virtio-disk,id=cd-disk1,unit=0,bus=ide.1
    -drive file=$VIRTIO_ISO,if=none,id=virtio-disk,media=cdrom

    -device ide-cd,drive=unattend-disk,id=cd-disk3,unit=1,bus=ide.0
    -drive file=$UNATTEND_ISO,if=none,id=unattend-disk,media=cdrom

    -serial stdio
    -monitor telnet:localhost:8888,server,nowait
    -display none
)
qemu-system-x86_64 "${QEMU_ARGS[@]}" &
QEMU_PID=$!

banner "Waiting for QEMU to start"
while ! nc -z localhost 8888; do
    sleep 1
done

banner "Starting Windows Server 2022 install"
# Get past "Press any key to boot from CD or DVD" prompt
for i in {1..20}; do
    echo "sendkey ret" | nc -N localhost 8888 >/dev/null || true
    sleep 1
done

# Wait for QEMU to finish
wait $QEMU_PID

banner "Shrinking image"
# Find the bounds of the last partition (OS partition)
SECTOR_SZ=$(sgdisk -p $WIN_IMAGE | grep -i "Sector size" | awk '{ print $4; }')
OS_PART_END_SECTOR=$(sgdisk -i 4 $WIN_IMAGE | grep "Last sector" | awk '{ print $3; }')
OS_PART_END=$(echo "$OS_PART_END_SECTOR * $SECTOR_SZ" | bc)
# Resize the image to the end of the last partition + 33 sectors for the secondary GPT table at the end of the disk
NEW_SIZE=$(echo "($OS_PART_END + ($SECTOR_SZ - ($OS_PART_END % $SECTOR_SZ))) + 33 * $SECTOR_SZ" | bc)
qemu-img resize -f raw --shrink $WIN_IMAGE $NEW_SIZE
# Repair secondary GPT table
sgdisk -e $WIN_IMAGE
# Sparsify the image
sudo virt-sparsify --in-place $WIN_IMAGE

banner "Done!"