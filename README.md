# Windows Image Builder

This repo contains the `wimsy` command-line tool for constructing generic
Windows Server images that can be imported into an Oxide rack and used to
create new Windows-based instances.

# Usage

`wimsy` runs on Linux (tested on Ubuntu 20.04) and illumos systems. It works by
running a VM to which it attaches Windows installation media and other disks
containing scripts that tell Windows Setup how to operate and drivers that
Windows should install.

## Install prerequisite binaries

On Linux distros with Debian (aptitude-based) package managers, run
`linux/install_prerequisites.sh` to ensure the necessary tools and packages are
installed. The following packages and tools are required:

* `qemu` and `ovmf` to run the Windows installer in a virtual machine
* `qemu-img` and `libguestfs-tools` to create and manage virtual disks and their
  filesystems
* `sgdisk` to modify virtual disks' GUID partition tables
* `genisoimage` to create an ISO containing the unattended setup scripts

## Obtain installation media and virtio drivers

`wimsy` requires the locations of a few files:

- An ISO disk image containing Windows installation media
- An ISO disk image containing appropriately signed virtio drivers, arranged in
  the directory structure used in the [driver images](https://github.com/virtio-win/virtio-win-pkg-scripts/blob/master/README.md) created by the Fedora Project
- A UEFI guest firmware image; on a Linux system with virtualization tools
  installed, this is typically found in `/usr/share/OVMF/OVMF_CODE.fd`

`wimsy` also requires a set of Windows Setup answer files and accompanying
scripts to pass to the setup process. The answer files and scripts Oxide uses
for its internal test images are in this repository in the `linux/unattend`
directory.

## Run `wimsy`

Build with `cargo` and view the command-line help as follows:

```bash
cargo build --release
target/release/wimsy create-guest-disk-image --help
```

An example invocation might be

```bash
target/release/wimsy \
--work-dir /tmp \
--output-image ./wimsy-ws2022.img \
create-guest-disk-image \
--windows-iso ./WS2022_SERVER_EVAL_x64FRE_en-us.iso \
--virtio-iso ./virtio-win-0.1.217.iso \
--unattend-dir ./linux/unattend \
--ovmf-path /usr/share/OVMF/OVMF_CODE.fd \
```

The installation is driven using the files and scripts in the directory passed
to `--unattend-dir`. You can modify these files directly to customize your
image, but `wimsy` provides some command line switches to apply common
modifications:

- The `--unattend-image-index` switch changes the image index specified in
  `Autounattend.xml`, which changes the Windows edition Setup will attempt to
  install (e.g. selecting between Server Standard and Server Datacenter with or
  without a Desktop Experience Pack).
- The `--windows-version` switch rewrites the driver paths in `Autounattend.xml`
  to install virtio drivers corresponding to a specific Windows version.

When running on Linux, adding the `--vga-console` switch directs QEMU to run
with a VGA console attached to the guest so that you can watch and interact with
Windows Setup visually.

## Other OSes

While the `wimsy` executable is only supported on Linux and illumos systems, the
installation method used by the Linux executable can be used in other
environments by creating a VM with the following attached devices and
configuration settings:

- A blank installation disk (at least 30 GiB)
- The following ISOs, attached as virtual CD-ROM drives or other removable
  media:
  - A Windows installation ISO
  - A virtio driver ISO
  - An ISO containing the contents of the `linux/unattend` directory
- A virtual network adapter
- UEFI-based guest firmware (e.g. a Hyper-V Generation 2 VM)

# Image configuration

Windows guests running on an Oxide rack work best with the following software
and settings:

- **Drivers**: The Oxide stack uses virtio network and block device drivers for
  its virtual network adapter and the cloud-init volumes it attaches to guests.
  Windows guests will need drivers for both of these devices.
- **Remote access**: Windows guests can be accessed via the Windows Emergency
  Management Services console (EMS) running over a virtual serial port, via
  Remote Desktop, or via SSH.
  - **EMS**: The EMS console must be explicitly
    [configured](https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/boot-parameters-to-enable-ems-redirection)
    to run over COM1.
  - **SSH**: To access an instance over SSH, the guest must have a running
    [SSH
    service](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse?tabs=gui),
    and the relevant user(s) must have valid passwords or SSH keys.
  - **Remote Desktop (RDP)**: To access an instance over RDP:
    - The guest's firewall rules must accept incoming TCP and UDP connections on
      port 3389.
    - The Oxide instance's firewall rules must also accept TCP and UDP
      connections on port 3389.
    - Remote Desktop sessions must be [enabled in the
      registry](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/unattend/microsoft-windows-terminalservices-localsessionmanager-fdenytsconnections). 

The scripts in this repo configure images as follows:

- The scripts install virtio network and block device drivers.
- The built-in administrator account is disabled, and an `oxide` account in the
  administrators group is added in its place. This account has a random password
  that must be re-set before the account can be accessed; generally this is done
  by supplying SSH keys at instance creation time, connecting over SSH, and
  using the `net user` command to change the `oxide` account password.
- The scripts configure the following settings:
  - The EMS console is enabled and can be accessed using the Oxide serial
    console API.
  - The guest firewall is configured to allow ping and Remote Desktop access.
  - The `fDenyTSConnections` registry value is set to 0 to allow incoming RDP
    connections.
- The scripts install OpenSSH and configure the SSH server service to start
  automatically on system startup.
- The scripts install a lightly modified
  [fork](https://github.com/luqmana/cloudbase-init/tree/oxide) of
  [cloudbase-init](https://cloudbase-init.readthedocs.io/en/latest/) that is
  configured to read `cloud-init` metadata from an attached VFAT-formatted disk.
  See cloudbase-init's documentation on [no-cloud configuration
  drives](https://cloudbase-init.readthedocs.io/en/latest/services.html#nocloud-configuration-drive)
  and [cloud config
  userdata](https://cloudbase-init.readthedocs.io/en/latest/userdata.html#cloud-config)
  for more information. The first time a disk based on one of these images is
  booted, the cloudbase-init config directs cloudbase-init to do the following:
  - The guest OS's hostname is set to the instance's hostname.
  - The `oxide` account's `authorized_keys` are set to the SSH keys specified
    when the instance was created.
  - The main OS installation volume is automatically extended to consume any
    unused data on the boot disk.
- Images produced by these scripts are generalized and can be used to create
  multiple distinct instances/boot disks.
