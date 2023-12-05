// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Commands for creating a Windows installation disk and a generic Windows
//! image using illumos and propolis-standalone.
//!
//! Because Propolis doesn't support presenting virtual disks as removable disks
//! (at least at the time of this writing), the only way to inject an answer
//! file into the installation process is to stick it on the installation disk
//! proper. wimsy does this by creating a blank raw disk, partitioning it with a
//! GPT, and distributing the installation files into the partitions on the new
//! install disk: WinPE and all the unattend files go onto a FAT32 setup
//! partition, and the `install.wim` installable image goes onto a separate NTFS
//! partition.
//!
//! Once this disk is created, creating a generic Oxide Windows image is the
//! same as on other platforms: create a VM, attach a blank installation target
//! disk and the installation media, and then boot the VM; Windows Setup will
//! take care of the rest.

use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand};

use crate::{autounattend::VirtioDriverVersion, runner::Script};

use self::{
    build_installation_disk::BuildInstallationDiskScript,
    create_guest_disk_image::CreateGuestDiskImageScript,
};

mod build_installation_disk;
mod create_guest_disk_image;

#[derive(Parser)]
pub struct App {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates a Windows installation disk containing all of the answer files
    /// and drivers needed to create an Oxide-compatible generalized Windows
    /// guest image
    BuildInstallationDisk {
        #[command(flatten)]
        args: BuildInstallationDiskArgs,
    },

    /// Runs a VM with a blank disk and a Windows installation image to create
    /// an Oxide-compatible base image.
    CreateGuestDiskImage {
        #[command(flatten)]
        args: CreateGuestDiskImageArgs,
    },
}

#[derive(Args, Clone)]
struct BuildInstallationDiskArgs {
    /// The path to a directory to use for temporary files created by this
    /// workflow.
    #[arg(long)]
    work_dir: Utf8PathBuf,

    /// The path to the Windows installer ISO to repack into a customized
    /// installation disk.
    #[arg(long)]
    windows_iso: Utf8PathBuf,

    /// The path to an ISO containing signed virtio-net and virtio-block drivers
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
    virtio_iso: Utf8PathBuf,

    /// The path to a directory containing the unattend files to inject into the
    /// image.
    #[arg(long)]
    unattend_dir: Utf8PathBuf,

    /// An optional image index to write into the Microsoft-Windows-Setup
    /// component's ImageInstall/OSImage/InstallFrom elements in
    /// Autounattend.xml. This index determines the edition of Windows that will
    /// be installed when the installation media contains multiple editions
    /// (e.g. Server Standard, Standard with the Desktop Experience Pack, etc.).
    /// If not specified, the index in the Autounattend.xml specified by
    /// --unattend-dir is used.
    #[arg(long)]
    unattend_image_index: Option<u32>,

    /// An optional Windows Server version that specifies the driver
    /// installation paths to specify in Autounattend.xml. If set, this
    /// substitutes the appropriate versioned directory name ("2k16", "2k19", or
    /// "2k22") when copying virtio drivers into the installation disk. If not
    /// specified, Windows Server 2022 is used.
    #[arg(long, value_enum)]
    windows_version: Option<VirtioDriverVersion>,

    /// The path at which to create the repacked installation disk.
    #[arg(long)]
    output_image: Utf8PathBuf,
}

impl BuildInstallationDiskArgs {
    fn file_prerequisites(&self) -> Vec<Utf8PathBuf> {
        vec![
            self.windows_iso.clone(),
            self.virtio_iso.clone(),
            self.unattend_dir.clone(),
        ]
    }
}

#[derive(Args, Clone)]
struct CreateGuestDiskImageArgs {
    /// The path to a directory to use for temporary files created by this
    /// workflow.
    #[arg(long)]
    work_dir: Utf8PathBuf,

    /// The name of the physical link on the host machine to which the
    /// installation VM's VNIC should be bound.
    #[arg(long)]
    vnic_link: String,

    /// The path to the installation disk to use. This will generally have been
    /// created with the `build-installation-disk` subcommand.
    #[arg(long)]
    installer_image: Utf8PathBuf,

    /// The path to which to write the output Windows image.
    #[arg(long)]
    output_image: Utf8PathBuf,

    /// The path to the Propolis bootrom (guest firmware image) to supply to the
    /// installation VM.
    #[arg(long)]
    propolis_bootrom: Utf8PathBuf,
}

impl App {
    pub fn get_script(&self) -> Box<dyn Script> {
        match &self.command {
            Command::BuildInstallationDisk { args } => {
                Box::new(BuildInstallationDiskScript::new(args.clone()))
            }
            Command::CreateGuestDiskImage { args } => {
                Box::new(CreateGuestDiskImageScript::new(args.clone()))
            }
        }
    }
}
