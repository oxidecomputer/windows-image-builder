# Configuring `wimsy` Images

`wimsy` configures the Windows images it produces using a set of scripts that
automate the Windows setup process and some post-installation tasks.

The `--unattend-dir` parameter points `wimsy` to the configuration files and
scripts it should inject into the guest VM. The configuration files in the
`unattend` directory include the following:

* **Autounattend.xml** and **specialize-unattend.xml** provide unattended setup
  instructions to Windows Setup and the Windows sysprep utility.
* **prep.cmd** and **OxidePrepBaseImage.ps1** install additional software and
  configuration options to further prepare images for use in an Oxide rack
  (e.g., installing a `cloud-init` service).
* **cloudbase-init.conf** and **cloudbase-init-unattend.conf** configure the
  cloudbase-init service.

You can modify these scripts to customize your Windows images to your liking.
The sections below describe some common changes you might want to make. For
further details, see the following documentation:

* [Oxide's documentation for Windows
  VMs](https://docs.oxide.computer/guides/working-with-windows-vms)
* Microsoft's [Unattended Windows Setup
  Reference](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/unattend/)
* Documentation for
  [cloudbase-init](https://cloudbase-init.readthedocs.io/en/latest/), the
  `cloud-init` provider the default scripts install

# Common customizations

## Install drivers for the target Windows version

`wimsy` requires you to supply an ISO that contains virtio-net and virtio-block
drivers for your version of Windows. The Windows Setup answer file,
`Autounattend.xml`, needs to be configured to point to the correct drivers for
the version of Windows you're trying to install.

### Method 1: Use the command line

If you've downloaded a prebuilt driver ISO from the [Fedora
project](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/unattend/),
you can use the `--windows-version` switch to specify your target Windows
version:

```sh
./wimsy <ARGS> create-guest-disk-image --windows-version [2k16|2k19|2k22]
```

This option will make `wimsy` patch the installation's `Autounattend.xml` to
point to the expected driver path for the supplied Windows version.

### Method 2: Edit `Autounattend.xml`

You can also manually edit the driver paths in `Autounattend.xml` under the
`DriverPaths` tag in the `offlineServicing` pass:

```xml
  <settings pass="offlineServicing">
    <component name="Microsoft-Windows-PnpCustomizationsNonWinPE">
      <DriverPaths>
        <PathAndCredentials wcm:action="add" wcm:keyValue="1">
            <!-- change this path to the directory containing your drivers -->
            <Path>D:\NetKVM\2k22\amd64</Path>
        </PathAndCredentials>
```

Note that the driver ISO may be mounted with any of the drive letters D, E, or
F, so you should duplicate your driver paths to include all these drive letters.

## Select a Windows edition to install

Some Windows installation disks allow you to select an edition of Windows to
install, e.g. Server Standard or Server Datacenter with the Desktop Experience
Pack. Windows assigns an index to each of the editions in a Windows setup image.
You can either query the image for the index you want to install, or you can
supply Windows with a product key associated with a particular edition.

### Prerequisite: Determining the image indices for your setup disk

If you want to select an edition numerically, you can use the `wimage` tool from
the [`wimtools` package](https://wimlib.net/) to determine what options are
available:

```sh
$ sudo apt-get install wimtools 7zip
# Extract the Windows image file (WIM) from the ISO.
$ 7z -e '-ir!install.wim' $PATH_TO_ISO
$ wiminfo install.wim
```

This should produce output like the following:

```
Available Images:                                               
-----------------                                               
Index:                  1                                        
Name:                   Windows Server 2022 SERVERSTANDARDCORE
Description:            Windows Server 2022 SERVERSTANDARDCORE
Display Name:           Windows Server 2022 Standard Evaluation
```

### Method 1: Use the command line

Use the `--unattend-image-index` switch to have `wimsy` patch your image index
into `Autounattend.xml` automatically:

```sh
./wimsy <ARGS> create-guest-disk-image --unattend-image-index 1
```

### Method 2: Change the image index in `Autounattend.xml`

Edit the `InstallFrom\MetaData` tags in `Autounattend.xml` to specify the image
you'd like to install:

```xml
  <settings pass="WindowsPE">
    <component name="Microsoft-Windows-Setup">
      <ImageInstall>
        <OsImage>
          <InstallFrom>
            <MetaData wcm:action="add">
              <Key>/IMAGE/INDEX</Key>
              <!-- Change this value to the desired index -->
              <Value>1</Value>
```

### Method 3: Supply a product key in `Autounattend.xml`

Instead of selecting an index, you can specify a product key in the `UserData`
tag in `Autounattend.xml`. Windows will install the edition associated with that
product key:

```xml
  <settings pass="WindowsPE">
    <component name="Microsoft-Windows-Setup">
      <ImageInstall>
      </ImageInstall>
      <UserData>
        <AcceptEula>true</AcceptEula>
        <ProductKey>
          <!-- insert key here -->
          <Key>XXXXX-XXXXX-XXXXX-XXXXX-XXXXX</Key>
          <WillShowUI>Never</WillShowUI>
        </ProductKey>
      </UserData>
```

If you use this method, you should comment out or remove the
`OsImage\InstallFrom` tags in the Microsoft-Windows-Setup component. If those
tags exist, and the edition they specify conflicts with the one specified by the
product key, Windows Setup will stop and wait for user input instead of
proceeding unattended.

## Supply a product key to activate Windows after installing

You can direct `sysprep` to prepare an image to activate with a specific product
key when it is first used by editing `specialize-unattend.xml`:

```xml
  <settings pass="specialize">
    <component name="Microsoft-Windows-Shell-Setup">
      <ProductKey>XXXXX-XXXXX-XXXXX-XXXXX-XXXXX</ProductKey>
```

Note that the resulting generalized image will try to activate with this product
key every time it is used as the base image for a new VM.

# Default image configuration

The scripts in the repo's `unattend` directory installs the Server Standard
edition of Windows Server 2022 and includes the Desktop Experience Pack. The
scripts further customize the resulting image as follows:

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
