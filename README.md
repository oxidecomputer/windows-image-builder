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

On Linux, run `linux/install_prerequisites.sh` to ensure the necessary tools and
packages are installed.

## Obtain installation media and virtio drivers

`wimsy` requires the locations of a few files:

- An ISO disk image containing Windows installation media
- An ISO disk image containing appropriately signed virtio drivers, such as the
  [images](https://github.com/virtio-win/virtio-win-pkg-scripts/blob/master/README.md)
  supplied by the Fedora Project
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
target/release/wimsy create-guest-disk-image \
--work-dir /tmp \
--windows-iso ./WS2022_SERVER_EVAL_x64FRE_en-us.iso \
--virtio-iso ./virtio-win-0.1.217.iso \
--unattend-dir ./linux/unattend \
--ovmf-path /usr/share/OVMF/OVMF_CODE.fd \
--output-image ./wimsy-ws2022.img
```

`wimsy` will set up an installation VM in QEMU and drive the setup process
automatically. The VM's serial console output will appear in the terminal while
the VM is running. This process will take several minutes to complete.

# Image configuration

The default configuration scripts set up an image with the following properties:

- **Drivers**: The scripts install virtio-net and virtio-block device drivers.
- **Software**: The scripts install a lightly modified version of
  [cloudbase-init](https://cloudbase-init.readthedocs.io/en/latest/) that is
  configured to read `cloud-init` metadata from an attached VFAT-formatted disk.
  See cloudbase-init's documentation on [no-cloud configuration
  drives](https://cloudbase-init.readthedocs.io/en/latest/services.html#nocloud-configuration-drive)
  and [cloud config
  userdata](https://cloudbase-init.readthedocs.io/en/latest/userdata.html#cloud-config)
  for more information. The scripts also install an OpenSSH daemon and configure
  it to start automatically on Windows startup.
- **User accounts**: The built-in administrator account is disabled. The system
  creates an account named `oxide` with a random password and copies SSH keys
  from the cloud config metadata into the user's `authorized_keys` file. Note
  that you must log in via SSH and use `net user` to change the `oxide` user's
  password in order to log into an interactive console session (e.g. via Remote
  Desktop).
- **Other configuration**: 
  - The Remote Desktop service is enabled and configured to allow terminal
    service connections.
  - The Emergency Management Console (EMS) is enabled and configured to connect
    to COM1. It is accessible via the serial console functions in the Oxide API.
- **Activation**: `wimsy` images don't have license keys and aren't activated by
  default. Users of these images must supply the appropriate license information
  or set up a key management server that their Windows instances can access.
- **Generalized images**: The scripts run `sysprep /generalize` after running
  the setup process, producing a "generalized" image that can be used as the
  base image for multiple Oxide disks. When a new VM based on a `wimsy` image
  boots for the first time, it will need to perform some final setup tasks
  (including additional reboots) before it is ready for use.

# Known issues

- Although `server2016` is a valid option for the `--windows-version` flag,
  Server 2016 images don't provision correctly because the method of installing
  OpenSSH used in OxidePrepBaseImage.ps1 is only supported on Server 2019 and
  Server 2022.
- Changing the image ID using the `--unattend-image-index` flag may not work
  with Windows Server 2022 installation media.
