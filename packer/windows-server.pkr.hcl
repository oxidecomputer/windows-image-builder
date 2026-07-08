packer {
  required_plugins {
    qemu = {
      version = "~> 1"
      source  = "github.com/hashicorp/qemu"
    }
  }
}

source "qemu" "windows-server" {
  # ISO configuration. The Windows ISO is also referenced directly in
  # qemuargs below; iso_url is still used so Packer verifies its checksum.
  iso_url      = var.windows_iso_path
  iso_checksum = var.windows_iso_checksum

  # UEFI firmware. efi_boot makes the plugin copy efi_firmware_vars to
  # <output_directory>/efivars.fd before launching QEMU, giving each build a
  # private writable copy of the EFI variable store. The plugin's own pflash
  # arguments are overridden by the -drive entries in qemuargs, so equivalent
  # entries are re-specified there.
  efi_boot          = true
  efi_firmware_code = var.ovmf_code_path
  efi_firmware_vars = var.ovmf_vars_path
  efi_drop_efivars  = true

  # When qemuargs includes -drive entries, the plugin drops all of its
  # auto-generated drives (main disk, CD-ROMs, EFI pflash) — everything must
  # be specified here. The plugin re-adds its own -netdev (with the WinRM
  # host-forward) and appends the virtio-net device automatically because no
  # -device entry below names it.
  #
  # The VM configuration mirrors what wimsy used, and what an Oxide rack
  # presents to guests: the boot disk is an NVMe device with 512-byte
  # logical/physical sectors, and the NIC is virtio-net.
  qemuargs = [
    # Match wimsy's CPU configuration, including the Hyper-V enlightenments
    # that speed up Windows considerably.
    ["-cpu", "host,kvm=off,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time"],
    # Windows expects the RTC to be in local time.
    ["-rtc", "base=localtime"],
    # UEFI firmware.
    ["-drive", "if=pflash,format=raw,readonly=on,file=${var.ovmf_code_path}"],
    ["-drive", "if=pflash,format=raw,file={{ .OutputDir }}/efivars.fd"],
    # Output disk, attached as NVMe exactly as wimsy attached it. discard
    # passthrough lets the guest's Optimize-Volume punch holes in the file.
    ["-device", "nvme,drive=drivec,serial=01de01de,physical_block_size=512,logical_block_size=512,discard_granularity=512,bootindex=1"],
    ["-drive", "if=none,id=drivec,file={{ .OutputDir }}/{{ .Name }},format=raw,discard=unmap"],
    # Windows Server ISO (boot CD-ROM).
    ["-device", "ide-cd,drive=win-disk,bus=ide.0,unit=0,bootindex=2"],
    ["-drive", "file=${var.windows_iso_path},if=none,id=win-disk,media=cdrom"],
    # VirtIO drivers ISO (second CD-ROM); drivers are staged into the image
    # during the offlineServicing pass of Autounattend.xml.
    ["-device", "ide-cd,drive=virtio-disk,bus=ide.1,unit=0"],
    ["-drive", "file=${var.virtio_iso_path},if=none,id=virtio-disk,media=cdrom"],
    # Serial port for EMS console output during the build.
    ["-serial", "stdio"],
  ]

  # Disk configuration. The plugin still creates the backing file referenced
  # by the -drive entry above.
  disk_size        = var.disk_size
  format           = "raw"
  output_directory = var.output_directory
  vm_name          = "windows-server"

  # VM configuration. wimsy used the i440fx ("pc") machine type; it is also
  # what provides the floppy controller used below (q35 has none).
  accelerator  = "kvm"
  machine_type = "pc"
  cpus         = var.cpus
  memory       = var.memory
  headless     = var.headless
  net_device   = "virtio-net"

  # Windows Setup reads Autounattend.xml from the floppy (A:\) automatically.
  # The other files are used by the provisioners and the sysprep task.
  # Autounattend.xml is rendered from a template so the Windows version
  # (virtio driver paths), image index, and build password are configurable.
  floppy_content = {
    "Autounattend.xml" = templatefile("${path.root}/answer_files/Autounattend.pkrtpl.hcl", {
      windows_version = var.windows_version
      image_index     = var.image_index
      admin_password  = var.winrm_password
    })
  }
  floppy_files = [
    "${path.root}/scripts/setup-winrm.ps1",
    "${path.root}/scripts/sysprep.ps1",
    "${path.root}/../unattend/specialize-unattend.xml",
    "${path.root}/../unattend/cloudbase-init.conf",
    "${path.root}/../unattend/cloudbase-init-unattend.conf",
  ]

  # Boot: press Enter to boot from CD-ROM when prompted by the firmware.
  boot_wait    = "5s"
  boot_command = ["<enter><wait><enter><wait><enter>"]

  # WinRM communicator — Packer uses this to run provisioners. Credentials
  # match what the rendered Autounattend.xml configures.
  communicator   = "winrm"
  winrm_username = "Administrator"
  winrm_password = var.winrm_password
  winrm_timeout  = "60m"
  winrm_use_ssl  = false
  winrm_insecure = true

  # De-provision and sysprep via an elevated scheduled task. WinRM basic auth
  # gets a filtered (non-elevated) token due to UAC, so a scheduled task is
  # used to run with full privileges; the command returns immediately and
  # Packer waits for the sysprep-initiated shutdown.
  shutdown_command = "cmd /c schtasks /create /tn packer-sysprep /tr \"powershell.exe -NoProfile -ExecutionPolicy Bypass -File A:\\sysprep.ps1\" /sc once /st 00:00 /rl highest /f && schtasks /run /tn packer-sysprep"
  shutdown_timeout = "30m"
}

build {
  sources = ["source.qemu.windows-server"]

  # Enable serial console (EMS on COM1).
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    inline = [
      "Write-Host 'Enabling Serial Console'",
      "bcdedit /ems on",
      "bcdedit /emssettings EMSPORT:1 EMSBAUDRATE:115200",
    ]
  }

  # Enable ping.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    inline = [
      "Write-Host 'Enabling Ping'",
      "New-NetFirewallRule -DisplayName 'Allow Inbound ICMPv4' -Direction Inbound -Protocol ICMPv4 -IcmpType 8 -RemoteAddress Any -Action Allow",
    ]
  }

  # Enable RDP.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    inline = [
      "Write-Host 'Enabling RDP'",
      "Set-ItemProperty 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\' -Name 'fDenyTSConnections' -Value 0",
      "Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'",
    ]
  }

  # Install OpenSSH.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    script            = "${path.root}/scripts/install-ssh.ps1"
  }

  # Install Cloudbase-init (Oxide fork).
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    script            = "${path.root}/scripts/install-cloudbase-init.ps1"
  }

  # Cleanup and defrag.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    inline = [
      "Write-Host 'Cleaning up disk'",
      "Dism.exe /online /Cleanup-Image /StartComponentCleanup /ResetBase",
      "Optimize-Volume -DriveLetter C",
    ]
  }

  # Shrink OS partition to minimize output image size.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = var.winrm_password
    inline = [
      "Write-Host 'Shrinking OS partition'",
      "$osPartition = Get-Partition -DriveLetter C",
      "$resizeInfo = Get-PartitionSupportedSize -DriveLetter C",
      "$minSz = $resizeInfo.SizeMin",
      "$maxSz = $resizeInfo.SizeMax",
      "$curSz = $osPartition.Size",
      "$newSz = $minSz + 3GB",
      "$diff = $curSz - $newSz",
      "if ($newSz -lt $maxSz) { Resize-Partition -DriveLetter C -Size $newSz; Write-Host \"New Partition Size: $newSz\"; Write-Host \"Free'd $diff\" }",
    ]
  }

  # Trim the unused tail of the output image, as wimsy did: resize the raw
  # file down to the end of the OS partition (plus room for the secondary
  # GPT), rebuild the secondary GPT at the new end of disk, then sparsify.
  post-processor "shell-local" {
    inline = [
      "set -e",
      "command -v sgdisk >/dev/null 2>&1 || { echo 'ERROR: sgdisk is required to shrink the output image' >&2; exit 1; }",
      "img='${var.output_directory}/windows-server'",
      "out='${var.output_directory}/windows-server.raw'",
      "echo 'Trimming unused sectors from output image...'",
      "sector_size=$(sgdisk -p \"$img\" | awk '/^Sector size/ {print $4}')",
      "last_sector=$(sgdisk -i 4 \"$img\" | awk '/^Last sector/ {print $3}')",
      "new_size=$(( (last_sector + 34) * sector_size ))",
      "echo \"Sector size: $sector_size, OS partition last sector: $last_sector, new size: $new_size bytes\"",
      "qemu-img resize --shrink -f raw \"$img\" \"$new_size\"",
      "sgdisk -e \"$img\"",
      "echo 'Sparsifying output image...'",
      "qemu-img convert -f raw -O raw \"$img\" \"$out\"",
      "rm \"$img\"",
      "if command -v sha256sum >/dev/null 2>&1; then sha256sum \"$out\" > \"$out.sha256\"; else shasum -a 256 \"$out\" > \"$out.sha256\"; fi",
      "echo \"Done. Output image: $out\"",
    ]
  }
}
