// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand};

use crate::autounattend::WindowsVersion;

#[derive(Parser)]
pub struct App {
    /// The directory in which to store temporary files.
    #[arg(long)]
    pub work_dir: Utf8PathBuf,

    /// The path to the tool's output disk image (i.e. the generated all-in-one
    /// installation disk or guest disk image).
    #[arg(long)]
    pub output_image: Utf8PathBuf,

    /// Forces the tool to run in an interactive or non-interactive mode. If not
    /// set, the tool infers whether to run interactively from whether it is
    /// running in an interactive terminal.
    #[arg(long, default_value = Option::None)]
    pub interactive: Option<bool>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Builds from a set of source files an installation disk suitable for use
    /// with the create-guest-disk-image command.
    #[cfg(target_os = "illumos")]
    BuildInstallationDisk {
        #[command(flatten)]
        sources: ImageSources,
    },

    /// Creates a disk image containing a generalized Windows installation
    /// suitable for use as an image in an Oxide rack.
    CreateGuestDiskImage {
        /// The name of the physical link on the host machine to which the
        /// installation VM's VNIC should be bound.
        #[cfg(target_os = "illumos")]
        #[cfg_attr(target_os = "illumos", arg(long))]
        vnic_link: String,

        /// The path to the repacked installation disk (created with the
        /// build-installation-disk subcommand) to use to install Windows.
        #[cfg(target_os = "illumos")]
        #[cfg_attr(target_os = "illumos", arg(long))]
        installer_image: Utf8PathBuf,

        /// The path to the Propolis bootrom (guest firmware image) to supply to
        /// the installation VM.
        #[cfg(target_os = "illumos")]
        #[cfg_attr(target_os = "illumos", arg(long))]
        propolis_bootrom: Utf8PathBuf,

        #[cfg(target_os = "linux")]
        #[cfg_attr(target_os = "linux", command(flatten))]
        sources: ImageSources,

        /// The path to the OVMF bootrom to supply to QEMU for use as a guest
        /// firmware image.
        #[cfg(target_os = "linux")]
        #[cfg_attr(target_os = "linux", arg(long))]
        ovmf_path: Utf8PathBuf,

        /// Displays a graphical console for the setup VM.
        #[cfg(target_os = "linux")]
        #[cfg_attr(target_os = "linux", arg(long, default_value_t = false))]
        vga_console: bool,
    },
}

#[derive(Args, Clone)]
pub struct ImageSources {
    /// The path to the Windows setup ISO to use for this operation.
    #[arg(long)]
    pub windows_iso: Utf8PathBuf,

    /// A path to an ISO containing signed virtio-net and virtio-block drivers
    /// to install. The drivers on this disk must have the directory structure
    /// the Fedora project uses in its virtio driver disks:
    ///
    /// - Top-level directories named `viostor` and `NetKVM`
    ///
    /// - Within each of these directories, subdirectories named `2k16`, `2k19`,
    ///   and `2k22`
    ///
    /// - Within each of these directories, an `amd64` subdirectory, which
    ///   contains `.cat`, `.inf`, and `.sys` files (i.e. the driver collateral
    ///   itself)
    #[arg(long)]
    pub virtio_iso: Utf8PathBuf,

    /// The path to a directory containing the unattend files to inject into the
    /// image.
    #[arg(long)]
    pub unattend_dir: Utf8PathBuf,

    /// An optional image index to write into the Microsoft-Windows-Setup
    /// component's ImageInstall/OSImage/InstallFrom elements in
    /// Autounattend.xml. This index determines the edition of Windows that will
    /// be installed when the installation media contains multiple editions
    /// (e.g. Server Standard, Standard with the Desktop Experience Pack, etc.).
    /// If not specified, the index in the Autounattend.xml specified by
    /// --unattend-dir is used.
    #[arg(long)]
    pub unattend_image_index: Option<u32>,

    /// An optional Windows Server version that specifies the driver
    /// installation paths to specify in Autounattend.xml. If set, this
    /// substitutes the appropriate versioned directory name ("2k16", "2k19", or
    /// "2k22") into the DriverPaths specified in the template Autounattend.xml
    /// specified by --unattend-dir. If not specified, the existing driver paths
    /// in that Autounattend.xml are used.
    #[arg(long, value_enum)]
    pub windows_version: Option<WindowsVersion>,
}
