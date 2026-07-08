# Packer template for Oxide Windows images

This directory contains a [Packer](https://developer.hashicorp.com/packer)
template that builds a generalized Windows Server image suitable for import
into an Oxide rack. It is the successor to the `wimsy` Rust tool in this
repository and reproduces its build flow:

1. Create a blank raw disk and boot Windows Setup in a QEMU/KVM guest that
   matches what an Oxide rack presents: an NVMe boot disk (512-byte sectors)
   and a virtio-net NIC.
2. Drive an unattended installation via a rendered `Autounattend.xml`
   (delivered on a virtual floppy), staging the virtio NetKVM/viostor drivers
   from the driver ISO during the `offlineServicing` pass.
3. Provision over WinRM: enable the EMS serial console, ping, and RDP;
   install OpenSSH and the Oxide fork of cloudbase-init; run disk cleanup and
   shrink the OS partition.
4. De-provision (reset WinRM to defaults, remove autologon credentials,
   scramble the build password) and generalize with sysprep, which disables
   the Administrator account on first boot and hands configuration over to
   cloudbase-init.
5. Trim the raw image down to the end of the OS partition, rebuild the
   secondary GPT, and sparsify. The result is `output/windows-server.raw`
   plus a `.sha256` checksum ready for `oxide disk import`.

## Prerequisites

The build must run on a Linux host with KVM. Required tools:

* `packer` (>= 1.10) with the QEMU plugin (installed by `packer init`,
  which `build.sh` runs for you)
* `qemu-system-x86_64` and `qemu-img`
* `sgdisk` (from `gdisk`) to trim the output image
* OVMF UEFI firmware (`edk2-ovmf` / `ovmf` package)

You also need:

* A Windows Server ISO (2016/2019/2022/2025). See the repository README for
  ISO requirements.
* A virtio driver ISO using the Fedora directory layout
  (`NetKVM/<version>/amd64`, `viostor/<version>/amd64`), e.g.
  [virtio-win.iso](https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/).

The guest needs outbound Internet access (Packer's user-mode networking) to
download OpenSSH and cloudbase-init.

## Usage

```sh
./build.sh \
  -var windows_iso_path=/path/to/windows_server_2022.iso \
  -var virtio_iso_path=/path/to/virtio-win.iso
```

Commonly overridden variables (see [variables.pkr.hcl](variables.pkr.hcl) for
the full list):

| Variable | Default | Purpose |
| --- | --- | --- |
| `windows_iso_path` | (required) | Windows Server installation ISO |
| `virtio_iso_path` | (required) | virtio driver ISO |
| `windows_iso_checksum` | `none` | ISO checksum verification (`sha256:...`) |
| `windows_version` | `2k22` | virtio driver directory (`2k16`/`2k19`/`2k22`/`2k25`) |
| `image_index` | `2` | Windows edition index in the ISO |
| `ovmf_code_path` / `ovmf_vars_path` | Arch paths | OVMF firmware location |
| `headless` | `true` | Set `false` to watch the installer |
| `winrm_password` | `Packer!build0` | Build-time Administrator password (scrambled before capture) |

To debug a failing build, pass `-var headless=false` to watch the console,
and add `-on-error=ask` to keep the VM around on failure. EMS serial output
is forwarded to Packer's stdout during the build.

## Notes and differences from wimsy

* The final image contains no build credentials or build machinery: the
  Administrator password is scrambled, autologon/WinRM build settings are
  reset, and the sysprep scheduled task and provisioner temp files are
  removed by [scripts/sysprep.ps1](scripts/sysprep.ps1) before sysprep runs.
  A `C:\Users\Administrator` profile folder exists in the image, as it did
  in wimsy-built images (wimsy's audit-mode session signed in as
  Administrator too); the account itself is disabled on first boot in both
  flows.
* The unattend collateral in [../unattend](../unattend) (`specialize-unattend.xml`,
  cloudbase-init configs) is shared with wimsy and delivered on the build
  floppy.
* Building images on illumos hosts (wimsy's `build-installation-disk` /
  Propolis flow) is not supported by this template; use wimsy for that.
