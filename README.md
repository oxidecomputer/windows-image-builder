# Windows Image Builder

This repo contains the `wimsy` command-line tool for constructing generic
Windows Server images that can be imported into an Oxide rack and used to
create new Windows-based instances. This tool sets up Windows in a VM running
on your computer, automatically customizes that installation using scripts that
you supply, and minimizes the size of the installation disk once setup is
complete. You can then upload the installation disk to an Oxide rack and attach
it to a VM or use it as the source disk for a new disk image.

`wimsy` runs on Linux (tested on Ubuntu 20.04) and illumos systems and supports
creating Windows Server 2019 and Windows Server 2022 images. Windows Server
2016 is not yet fully supported (but it's on the roadmap). Earlier versions of
Windows Server and client editions of Windows are not supported. It may be
possible to use `wimsy` to generate images for these versions, but Oxide has
not tested them, so your mileage may vary.

# Usage

## Pre-flight checklist

To set up a host machine to use `wimsy`:

* Run `install_prerequisites.sh` from the repo or release tarball to install
  [required tools and packages](#required-tools).
* Ensure the host has a copy of your [installation media and of a driver
  ISO](#installation-media-and-drivers).
* Ensure the host has a network connection that allows VM guests to access the
  public Internet. This is needed to download software into the guest after
  Windows Setup runs. See [CONFIGURING.md](CONFIGURING.md) for more details.

### Required tools

The `install_prerequisites.sh` script installs the tools `wimsy` uses to create
disks and run VMs. On Linux hosts, a Debian (aptitude-based) package manager is
required. Linux systems use the following tools and packages:

* `qemu` and `ovmf` to run the Windows installer in a virtual machine
* `qemu-img` and `libguestfs-tools` to create and manage virtual disks and their
  filesystems
* `sgdisk` to modify virtual disks' GUID partition tables
* `genisoimage` to create an ISO containing the unattended setup scripts

### Installation media and drivers

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

`wimsy` uses a number of scripts to run an unattended Windows Setup process and
customize an image's software and settings. Oxide tests images using lightly
modified version of the scripts in the `unattend` directory in this repo, but
you can modify these or provide custom scripts. At a minimum, an
`Autounattend.xml` answer file is required to run Windows Setup unattended. See
Microsoft's documentation of the [Windows Setup
process](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/windows-setup-installation-process?view=windows-11)
and the [Unattended Windows Setup
Reference](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/unattend/)
for details.

`wimsy` expects all the unattend scripts it will inject to reside in a single
flat directory. The `unattend` directory in this repo contains a set of scripts
that apply the [default image configuration](#default-image-configuration)
described below.

## Running `wimsy`

### From a release tarball

Unpack the tarball and install the prerequisite tools, then run `wimsy`,
substituting the appropriate paths to your input ISOs and output disk image:

```bash
./install_prerequisites.sh

./wimsy \
--work-dir /tmp \
--output-image $OUTPUT_IMAGE_PATH \
create-guest-disk-image \
--windows-iso $WINDOWS_SETUP_ISO_PATH \
--virtio-iso $VIRTIO_DRIVER_ISO_PATH \
--unattend-dir unattend \
--ovmf-path /usr/share/OVMF/OVMF_CODE.fd \
```

### Building from source

Build with `cargo` and view the command-line help as follows:

```bash
cargo build --release
target/release/wimsy create-guest-disk-image --help
```

Then invoke `wimsy` with your desired arguments, e.g.:

```bash
target/release/wimsy \
--work-dir /tmp \
--output-image $OUTPUT_IMAGE_PATH \
create-guest-disk-image \
--windows-iso $WINDOWS_SETUP_ISO_PATH \
--virtio-iso $VIRTIO_DRIVER_ISO_PATH \
--unattend-dir ./unattend \
--ovmf-path /usr/share/OVMF/OVMF_CODE.fd \
```

### Running on illumos

Running on illumos requires some extra configuration:

- If you are using the setup scripts in the `unattend` directory, copy them to
  another directory, then replace `Autounattend.xml` and `prep.cmd` with
  `illumos/Autounattend.xml` and `illumos/prep.cmd` from the repo.
- You'll need to run `wimsy build-installation-disk` before running `wimsy
  create-guest-disk-image`. See the command-line help for more information.

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
- **User accounts**: The local administrator account is disabled. An account
  with username `oxide` will be created and added to the Local Administrators
  group. Any SSH keys that are associated with an instance when that instance is
  created will be added to the `oxide` user's authorized keys. By default, this
  account has no password; to set a password, access the machine via SSH and use
  `net user oxide *`.
- **Remote access**:
  - The [Emergency Management Services
    console](https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/boot-parameters-to-enable-ems-redirection)
    is enabled and accessible over COM1. This console will be accessible through
    the Oxide web console and CLI.
  - [OpenSSH for
    Windows](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse?tabs=powershell)
    is installed via PowerShell cmdlet (Windows Server 2019 and 2022) or by
    downloading the latest
    [release](https://github.com/PowerShell/Win32-OpenSSH/releases/) from
    GitHub. This operation requires the guest to have Internet access.
  - The guest is configured to allow Remote Desktop connections, and the guest
    firewall is configured to accept connections on port 3389. **Note:** VMs
    using these images must also have their firewall rules set to accept
    connections on this port for RDP to be accessible.
- **In-guest agents**: The scripts install an Oxide-compatible
  [fork](https://github.com/luqmana/cloudbase-init/tree/oxide) of
  [cloudbase-init](https://cloudbase-init.readthedocs.io/en/latest/) that
  initializes new VMs when they are run for the first time. This operation
  requires Internet access. `cloudbase-init` is configured with the following
  settings and plugins:
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

# Determining the `/IMAGE/INDEX` for your Windows version

The index used for a given Windows version will vary by iso file.
You can use the `wimtools` package to find the versions available on your image:

```sh
# On a debian-based Linux host
$ sudo apt-get install wimtools 7zip
$ 7z e '-ir!install.wim' <WIN_ISO>
$ wiminfo sources/install.wim
```

# Configuring the output image

See [CONFIGURING.md](CONFIGURING.md) to learn more about how to customize the
images `wimsy` produces.

# Troubleshooting

## `wimsy` gets stuck at "waiting for guest to complete installation"

Usually, this means either that Windows Setup failed to install Windows or that
the image prep script, `OxidePrepBaseImage.ps1`, did not run to completion.

When using a Linux host, you can determine where the setup process has stopped
by adding the `--vga-console` switch to `wimsy create-guest-disk-image`.

## Windows Setup is waiting for someone to select an edition to install

This can occur if the edition chosen in `Autounattend.xml` or on the command
line is invalid, or if it conflicts with other settings in `Autounattend.xml`.
See [CONFIGURING.md](CONFIGURING.md) for information about selecting an edition
to install.

## Setup is displaying a command prompt with "Press any key to continue..."

This usually indicates there was a problem running the `OxidePrepBaseImage.ps1`
setup script after installing Windows. This script's last step shuts down the
guest; if the script fails early, this won't happen, and `prep.cmd` will not
exit.

To investigate, look in the directory you passed to `wimsy --work-dir` for the
output from `qemu-system-x86_64`:

```sh
$ ls *qemu-system-x86_64.stdio.log
4.qemu-system-x86_64.stdio.log
```

By default, when `prep.cmd` runs `OxidePrepBaseImage.ps1` in the guest, it
redirects the script's output to a guest serial port, and `wimsy` asks QEMU to
write this output to QEMU's stdout. You can use the script outputs in this file
to determine where the script failed.
