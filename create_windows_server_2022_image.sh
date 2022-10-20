#!/bin/bash
set -eux

check_bins() {
    while [ $# -gt 0 ]; do
        builtin type -P "$1" &>/dev/null || {
            echo "Missing required binary: $1"
            exit 1
        }
        shift
    done
}

download_blob() {
    if [[ $# -ne 2 ]]; then
        echo "Usage: download_blob: <ovmf|virtio-iso|server-2022-installer> <output path>"
        exit 1
    fi
    local item="$1"
    local path="$2"

    local url=""
    case "$item" in
        "ovmf") url="https://oxide-omicron-build.s3.amazonaws.com/OVMF_CODE_20220922.fd" ;;
        "server-2022-installer") url="https://software-static.download.prss.microsoft.com/sg/download/888969d5-f34g-4e03-ac9d-1f9786c66749/SERVER_EVAL_x64FRE_en-us.iso" ;;
        "virtio-iso") url="https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso" ;;
        *)
            echo "Unknown item: $item"
            exit 1
            ;;
    esac

    echo "Downloading $item"
    wget --progress=dot:giga $url -O $path
}

make_unattended() {
    if [[ $# -ne 3 ]]; then
        echo "Usage: make_unattended: <path to unattend scripts> <path to VirtIO ISO> <setup disk mountpoint>"
        exit 1
    fi
    local unattend="$1"
    local virtio_iso="$2"
    local setup_img="$3"

    echo "Copying unattend scripts..."

    # Windows Setup will find Autounattend.xml at the root and use
    # that to drive the installation without any user input.
    cp $unattend/Autounattend.xml $setup_img/

    # Setup script that will run on first boot after installing Windows
    # It will complete the rest of the unattended installation before
    # running sysprep /generalize to allow it to be used as a base image.
    cp $unattend/OxidePrepBaseImage.ps1 $setup_img/

    # The unattend xml used on first boot of the base image which will
    # enable skip the Out-of-Box-Experience (OOBE), enable cloudbase-init
    # and cleanup the provisioning scripts.
    cp $unattend/specialize-unattend.xml $setup_img/

    # Cloudbase-init config.
    # OxidePrepBaseImage.ps1 will place these in the proper location.
    mkdir -p $setup_img/cloudbase-init
    cp -r $unattend/cloudbase-*.conf $setup_img/cloudbase-init/

    # Copy the VirtIO drivers to the setup drive. Autounattend.xml will
    # direct Windows Setup to install them during setup.
    7z e $virtio_iso -o$setup_img/virtio-drivers/ {viostor,NetKVM}/2k22/amd64/\*.{cat,inf,sys}
}

repack_installer() {
    if [[ $# -ne 4 ]]; then
        echo "Usage: repack_installer <path to original installer ISO> <path to VirtIO ISO> <path to unattend scripts> <path to create repacked installer>"
        exit 1
    fi
    local win_iso="$1"
    local virtio_iso="$2"
    local unattend="$3"
    local repacked="$4"

    # Create a blank 5G image for the repacked Windows installer
    qemu-img create -f raw $repacked 5.5G

    # Format it as a GPT disk
    sgdisk -og $repacked

    # The Windows install image (install.wim) may be (is) larger than 4G and so can't be stored on a FAT32 partition.
    # But the Windows installer supports loading the install.wim from a separate partition.
    #   https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/winpe--use-a-single-usb-key-for-winpe-and-a-wim-file---wim?view=windows-11#option-1-create-a-multiple-partition-usb-drive
    # So we create a 1G FAT32 partition for the rest of the installer and a 4.5G NTFS partition for install.wim.
    #   0700 = Microsoft Basic Data partition
    sgdisk -n=1:0:+1G -t 1:0700 -n=2:0:0 -t 2:0700 $repacked

    # Assign specific GUIDs to the partitions so we can refer to them unambiguously from Autounattend.xml
    sgdisk -u 1:569CBD84-352D-44D9-B92D-BF25B852925B -u 2:A94E24F7-92C9-405C-82AA-9A1B45BA180C $repacked

    # Create loopback device
    local repack_loop=$(pfexec lofiadm -l -a $repacked)
    local repack_loop_setup=${repack_loop/p0/s0}    # s0 is the first (FAT32) partition
    local repack_loop_image=${repack_loop/p0/s1}    # s1 is the second (NTFS) partition

    # Format first partition as FAT32 and mount it
    yes | pfexec mkfs -F pcfs -o fat=32 ${repack_loop_setup/dsk/rdsk}

    local setup_mount=/work/tmp/setup-mount
    mkdir -p $setup_mount
    pfexec mount -F pcfs $repack_loop_setup $setup_mount

    # Copy everything from the original ISO except install.wim to the first partition
    7z x '-x!sources/install.wim' $win_iso -o$setup_mount

    # Add the unattended scripts and config to the first partition
    make_unattended $unattend $virtio_iso $setup_mount

    # Done with setup partition
    pfexec umount $setup_mount

    # Format second partition as NTFS and mount it
    local sector_sz=$(sgdisk -p $repacked | grep -i "Sector size" | awk '{ print $4; }')
    local part_sect_start=$(sgdisk -i 2 $repacked | grep "First sector" | awk '{ print $3; }')
    local num_sect=$(sgdisk -i 2 $repacked | grep "Partition size" | awk '{ print $3; }')
    pfexec mkntfs -Q -s $sector_sz -p $part_sect_start -H 16 -S 63 $repack_loop_image $num_sect

    local image_mount=/work/tmp/image-mount
    mkdir -p $image_mount
    pfexec ntfs-3g $repack_loop_image $image_mount

    # Extract install.wim into NTFS partition
    7z e '-i!sources/install.wim' $win_iso -o$image_mount

    # Unmount image partition and remove loopback device
    pfexec umount $image_mount
    sync; sleep 2 # lofiadm -d fails with "Device busy" if we're too quick?
    pfexec lofiadm -d $repack_loop
}

create_vnic() {
    if [[ $# -ne 2 ]]; then
        echo "Usage: create_vnic: <vNIC name> <LINK>"
        exit 1
    fi
    local nic_name="$1"
    local nic_link="$2"
    pfexec dladm create-vnic -t -l $nic_link $nic_name
}

case "${1-}" in
    "repack" )
        if [[ $# -ne 3 ]]; then
            echo "Usage: $0 repack <path to unattend scripts> <path to create repacked installer>"
            exit 1
        fi
        unattend_dir="$2"
        repacked_img="$3"

        # Make sure we have all the extra binaries we need to repack the installer
        check_bins mkntfs ntfs-3g qemu-img sgdisk

        mkdir -p /work/tmp

        # Download the VirtIO drivers
        virtio_iso="/work/tmp/virtio-win.iso"
        download_blob "virtio-iso" $virtio_iso

        # Download the Windows Server 2022 Eval ISO
        orig_win_iso="/work/tmp/windows-server-2022-installer.iso"
        download_blob "server-2022-installer" $orig_win_iso

        # Repack the installer to include the VirtIO drivers along with
        # the scripts & config for the unattended installation.
        repack_installer $orig_win_iso $virtio_iso $unattend_dir $repacked_img

        echo "Repacked: $repacked_img"

        rm $virtio_iso $orig_win_iso

        ;;

    "build" )
        if [[ $# -ne 3 ]]; then
            echo "Usage: $0 build <windows installer image> <path to create windows server 2022 image>"
            exit 1
        fi
        installer="$2"
        win_image="$3"

        # Make sure we have all the extra binaries we need to create the image
        check_bins propolis-standalone qemu-img sgdisk

        mkdir -p /work/tmp

        # Grab the OVMF bootrom
        ovmf_blob="/work/tmp/OVMF_CODE.fd"
        download_blob "ovmf" $ovmf_blob

        # Create a vNIC for the VM using the first available link
        vnic="vnic0"
        vnic_link=$(dladm show-phys -po LINK | tail -1)
        create_vnic $vnic $vnic_link

        # Create blank image we'll be installing to
        qemu-img create -f raw $win_image 32G

        # And the propolis config
        vm_toml="/work/tmp/vm.toml"
        cat <<EOF >$vm_toml
[main]
name = "windows-server-2022"
cpus = 2
memory = 2048
bootrom = "$ovmf_blob"

[block_dev.win_image]
type = "file"
path = "$win_image"
[dev.block0]
driver = "pci-nvme"
block_dev = "win_image"
pci-path = "0.16.0"

[block_dev.win_iso]
type = "file"
path = "$installer"
[dev.block1]
driver = "pci-nvme"
block_dev = "win_iso"
pci-path = "0.17.0"

[dev.net0]
driver = "pci-virtio-viona"
vnic = "$vnic"
pci-path = "0.8.0"
EOF

        # Start up the VM
        pfexec propolis-standalone $vm_toml &
        propolis_pid=$!

        # Wait for propolis to create the unix domain socket linked to the VM's COM1 output.
        while [ ! -e ttya ]; do sleep 1; done

        # Start dumping the VM's COM1 output which kicks off propolis and the unattended installation
        echo "Starting unattended installation. This'll take a moment..."
        nc -Ud ttya &

        # Wait for the installation to finish
        wait $propolis_pid
        echo "Installation finished."

        echo "Shrink disk image to fit OS."

        # Find the bounds of the last partition (OS partition)
        sector_sz=$(sgdisk -p $win_image | grep -i "Sector size" | awk '{ print $4; }')
        os_part_end_sector=$(sgdisk -i 4 $win_image | grep "Last sector" | awk '{ print $3; }')
        os_part_end=$(echo "$os_part_end_sector * $sector_sz" | bc)

        # Resize the image to the end of the last partition + 33 sectors for the secondary GPT table at the end of the disk
        new_sz=$(echo "($os_part_end + ($sector_sz - ($os_part_end % $sector_sz))) + 33 * $sector_sz" | bc)
        qemu-img resize -f raw $win_image $new_sz

        echo "Shrunk image to $new_sz bytes."

        # Repair secondary GPT table
        sgdisk -e $win_image

        ;;

    * )
        echo "Usage: $0 <build|repack> ..."
        exit 1
        ;;
esac