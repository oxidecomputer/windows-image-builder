# Windows Image Builder

This repo contains the `wimsy` command-line tool for constructing generic
Windows Server images that can be imported into an Oxide rack and used to
create new Windows-based instances.

`wimsy` runs on Linux (tested on Ubuntu 20.04) and illumos systems. It works by
running a VM to which it attaches Windows installation media and other disks
containing scripts that tell Windows Setup how to operate and drivers that
Windows should install.

# Usage

## Supported Windows versions

`wimsy` can set up images for Windows Server 2019 and Windows Server 2022
guests. Windows Server 2016 is not yet supported. Earlier versions of Windows
Server and client editions of Windows are also not supported.

## Prerequisites

### Host machine configuration

When using Oxide's default setup scripts, the guest VM must be able to reach the
Internet to download guest software, so a networked host is required. See
[Default image configuration](#default-image-configuration) for more information
about what these scripts install.

### Tools

Run `install_prerequisites.sh` from the repo or release tarball to install the
tools `wimsy` invokes to create disks and run VMs. On Linux hosts, a Debian
(aptitude-based) package manager is required. Linux systems use the following
tools and packages:

* `qemu` and `ovmf` to run the Windows installer in a virtual machine
* `qemu-img` and `libguestfs-tools` to create and manage virtual disks and their
  filesystems
* `sgdisk` to modify virtual disks' GUID partition tables
* `genisoimage` to create an ISO containing the unattended setup scripts

### Installation media & drivers

`wimsy` requires an ISO disk image containing Windows installation media, an ISO
disk image containing signed virtio drivers, and a UEFI guest firmware image to
use when running the setup VM.

Oxide tests Windows guests using the [driver
images](https://github.com/virtio-win/virtio-win-pkg-scripts/blob/master/README.md)
created by the Fedora Project. If you use another driver ISO, the drivers must
be arranged in the same directory structure used by this project.

On a Linux system with virtualization tools installed, a guest firmware image
from the OVMF project can generally be found in `/usr/share/OVMF/OVMF_CODE.fd`.

### Setup scripts

`wimsy` supplies Windows Setup with scripts that allow it to run unattended.
Oxide tests images using lightly modified version of the scripts in the
`unattend` directory in this repo. These scripts must all reside in a single
directory.

## Running `wimsy`

### From a release tarball

Unpack the tarball and install the prerequisite tools, then run `wimsy`,
substituting the appropriate paths to your input ISOs and output disk image:

```bash
install_prerequisites.sh

wimsy \
--work-dir /tmp \
--output-image $OUTPUT_IMAGE_PATH \
create-guest-disk-image \
--windows-iso $WINDOWS_SETUP_ISO_PATH \
--virtio-iso $VIRTIO_DRIVER_ISO_PATH \
--unattend-dir unattend \
--ovmf-path /usr/share/OVMF/OVMF_CODE.fd \
```

For more information, run `wimsy --help` or `wimsy create-guest-disk-image
--help`. 

### Building from source

Build with `cargo` and view the command-line help as follows:

```bash
cargo build --release
target/release/wimsy create-guest-disk-image --help
```

If you are using the default unattend scripts from this repo, ensure they are in
a single flat directory before proceeding. The `unattend` directory in the repo
has some generic and some OS-specific scripts; to create a flat directory from
it with the appropriate scripts for your host OS, run

```bash
make_unattend.sh
```

Then invoke `wimsy` with your desired arguments, e.g.:

```bash
target/release/wimsy \
--work-dir /tmp \
--output-image ./wimsy-ws2022.img \
create-guest-disk-image \
--windows-iso ./WS2022_SERVER_EVAL_x64FRE_en-us.iso \
--virtio-iso ./virtio-win-0.1.240.iso \
--unattend-dir ./out/unattend \
--ovmf-path /usr/share/OVMF/OVMF_CODE.fd \
```

## Additional options

`wimsy` runs an unattended Windows Setup session driven by the files and scripts
in the directory passed to `--unattend-dir`. You can modify these files directly
to customize your image, but `wimsy` provides some command line switches to
apply common modifications:

- The `--unattend-image-index` switch changes the image index specified in
  `Autounattend.xml`, which changes the Windows edition Setup will attempt to
  install (e.g. selecting between Server Standard and Server Datacenter with or
  without a Desktop Experience Pack).
- The `--windows-version` switch rewrites the driver paths in `Autounattend.xml`
  to install virtio drivers corresponding to a specific Windows version.

When running on Linux, adding the `--vga-console` switch directs QEMU to run
with a VGA console attached to the guest so that you can watch and interact with
Windows Setup visually.

# Default image configuration

`wimsy` and the unattend scripts in this repo create
[generalized](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/sysprep--generalize--a-windows-installation?view=windows-11)
images that can be uploaded to the rack and used to create multiple VMs. These
images contain the following drivers, software, and settings:

- **Drivers**: `virtio-net` and `virtio-block` device drivers will be installed.
- **Remote access**:
  - The [Emergency Management Services
    console](https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/boot-parameters-to-enable-ems-redirection)
    is enabled and accessible over COM1. This console will be accessible through
    the Oxide web console and CLI.
  - [OpenSSH for
    Windows](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse?tabs=powershell)
    is installed via PowerShell cmdlet. This operation requires Internet access.
  - The guest is configured to allow Remote Desktop connections, and the guest
    firewall is configured to accept connections on port 3389. **Note:** VMs
    using these images must also have their firewall rules set to accept
    connections on this port for RDP to be accessible.
- **In-guest agents**: The scripts install an Oxide-compatible
  [fork](https://github.com/luqmana/cloudbase-init/tree/oxide) of
  [cloudbase-init](https://cloudbase-init.readthedocs.io/en/latest/) that
  initializes new VMs when they are run for the first time. `cloudbase-init` is
  configured with the following settings and plugins:
  - Instance metadata will be read from the no-cloud configuration drive the
    Oxide control plane attaches to each running instance.
  - The instance's computer name will be set to its Oxide instance hostname on
    first boot.
  - The built-in administrator account is disabled. An `oxide` account is
    in the Local Administrators group is created in its place. Any SSH keys
    provided in the instance's metadata will be added to this user's
    `authorized_keys`.
  - The OS installation volume is automatically extended to include the entire
    boot disk, even if it is larger than the original image.
