packer {
  required_plugins {
    qemu = {
      version = "~> 1"
      source  = "github.com/hashicorp/qemu"
    }
  }
}

source "qemu" "windows-server" {
  # ISO configuration.
  iso_url      = var.windows_iso_path
  iso_checksum = var.windows_iso_checksum

  # When qemuargs includes -drive entries, the plugin does not
  # auto-generate its default disk or cdrom drives — specify everything.
  qemuargs = [
    # UEFI firmware.
    ["-drive", "if=pflash,format=raw,readonly=on,file=${var.ovmf_code_path}"],
    ["-drive", "if=pflash,format=raw,file=${path.root}/efivars.fd"],
    # Output disk (virtio-blk).
    ["-drive", "file={{ .OutputDir }}/packer-windows-server,if=virtio,cache=writeback,discard=ignore,format=raw"],
    # Windows Server ISO (boot CD-ROM).
    ["-cdrom", "${var.windows_iso_path}"],
    # VirtIO drivers ISO (second CD-ROM).
    ["-drive", "file=${var.virtio_iso_path},media=cdrom,index=1"],
    # Boot from CD-ROM.
    ["-boot", "order=d,menu=on"],
    # Serial port for EMS console output.
    ["-serial", "stdio"],
  ]

  # Disk configuration.
  disk_size        = var.disk_size
  disk_interface   = "virtio"
  format           = "raw"
  output_directory = var.output_directory

  # VM configuration.
  accelerator  = "kvm"
  machine_type = "q35"
  cpus         = var.cpus
  memory       = var.memory
  headless     = var.headless
  net_device   = "virtio-net"

  # Windows Setup reads Autounattend.xml from the floppy automatically.
  # Other unattend files are also placed on the floppy (A:\) for use by
  # provisioners and sysprep.
  floppy_files = [
    "${path.root}/answer_files/Autounattend.xml",
    "${path.root}/scripts/setup-winrm.ps1",
    "${path.root}/../unattend/specialize-unattend.xml",
    "${path.root}/../unattend/cloudbase-init.conf",
    "${path.root}/../unattend/cloudbase-init-unattend.conf",
  ]

  # Boot: press Enter to boot from CD-ROM when prompted by UEFI.
  boot_wait    = "5s"
  boot_command = ["<enter><wait><enter><wait><enter>"]

  # WinRM communicator — Packer uses this to run provisioners.
  # Credentials match what Autounattend.xml configures.
  communicator   = "winrm"
  winrm_username = "Administrator"
  winrm_password = "Packer!build0"
  winrm_timeout  = "60m"
  winrm_use_ssl  = false
  winrm_insecure = true

  # Run sysprep elevated via a scheduled task. WinRM basic auth gets a
  # filtered (non-elevated) token due to UAC, so we create a scheduled
  # task to run sysprep with full privileges, then return immediately.
  shutdown_command = "cmd /c schtasks /create /tn packer-sysprep /tr \"C:\\Windows\\System32\\Sysprep\\sysprep.exe /generalize /oobe /shutdown /unattend:A:\\specialize-unattend.xml\" /sc once /st 00:00 /rl highest /f && schtasks /run /tn packer-sysprep"
  shutdown_timeout = "30m"
}

build {
  sources = ["source.qemu.windows-server"]

  # Enable serial console (EMS on COM1).
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
    inline = [
      "Write-Host 'Enabling Serial Console'",
      "bcdedit /ems on",
      "bcdedit /emssettings EMSPORT:1 EMSBAUDRATE:115200",
    ]
  }

  # Enable ping.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
    inline = [
      "Write-Host 'Enabling Ping'",
      "New-NetFirewallRule -DisplayName 'Allow Inbound ICMPv4' -Direction Inbound -Protocol ICMPv4 -IcmpType 8 -RemoteAddress Any -Action Allow",
    ]
  }

  # Enable RDP.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
    inline = [
      "Write-Host 'Enabling RDP'",
      "Set-ItemProperty 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\' -Name 'fDenyTSConnections' -Value 0",
      "Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'",
    ]
  }

  # Install OpenSSH.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
    script = "${path.root}/scripts/install-ssh.ps1"
  }

  # Install Cloudbase-init (Oxide fork).
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
    script = "${path.root}/scripts/install-cloudbase-init.ps1"
  }

  # Cleanup and defrag.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
    inline = [
      "Write-Host 'Cleaning up disk'",
      "Dism.exe /online /Cleanup-Image /StartComponentCleanup /ResetBase",
      "Optimize-Volume -DriveLetter C",
    ]
  }

  # Shrink OS partition to minimize output image size.
  provisioner "powershell" {
    elevated_user     = "Administrator"
    elevated_password = "Packer!build0"
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

  # Shrink the output image after the VM shuts down.
  post-processor "shell-local" {
    inline = [
      "echo 'Shrinking output image...'",
      "qemu-img convert -f raw -O raw '${var.output_directory}/packer-windows-server' '${var.output_directory}/windows-server.raw'",
      "rm -f '${var.output_directory}/packer-windows-server'",
      "echo 'Done. Output image: ${var.output_directory}/windows-server.raw'",
    ]
  }
}
