// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Creating the instance that installs Windows.
//!
//! Three rack-specific facts shape everything here:
//!
//! - **Boot order belongs to the control plane.** `boot_disk` is pinned at creation, and
//!   there is no fallthrough to the next disk. The SDK says as much: an instance with no
//!   boot disk "may result in an instance that only boots to the EFI shell".
//! - **The system disk must be a whole number of GiB**, at least one.
//! - **An instance is unreachable without an external IP.** Nothing in the guest can fix
//!   that afterwards.
//!
//! And one that cannot be fixed from here at all: **the default VPC allows only tcp/22
//! and ICMP**. Enabling RDP in the answer file is necessary and not sufficient, because
//! 3389 is dropped before it reaches Windows. Adding that rule is a control-plane action;
//! [`Created::warnings`] says so rather than letting it be discovered on the rack.

use crate::upload::Rack;
use anyhow::{Context, Result};
use oxide::{ClientDisksExt, ClientInstancesExt};
use oxwin_core::progress::Reporter;

/// One gibibyte.
const GIB: u64 = 1024 * 1024 * 1024;

/// What to build.
#[derive(Debug, Clone)]
pub struct InstanceSpec {
    pub name: String,
    pub description: String,
    /// Defaults to `name` when empty. A hostname has a narrower character set than an
    /// instance name, so they are allowed to differ.
    pub hostname: String,
    pub ncpus: u16,
    pub memory_gib: u64,
    /// The already-uploaded installer. Pinned as the boot disk.
    pub installer_disk: String,
    /// Blank disk for Windows to install onto, created here.
    ///
    /// `None` for a machine that needs no second disk, which is what a clone made
    /// from a finished image is: the image *is* its system disk. Creating a
    /// throwaway 1 GiB disk to satisfy this field would leave a resource nobody
    /// asked for on every clone check.
    pub system_disk: Option<String>,
    pub system_disk_gib: u64,
    /// Block size for the system disk. 4096 is what a rack normally uses; the *installer*
    /// is the one that must be 512.
    pub system_block_size: i64,
    /// Start it immediately. False leaves it stopped, which is what a script wants when
    /// it intends to attach something else first.
    pub start: bool,
}

/// What the media turned out to be, for the description the rack shows.
///
/// `oxide instance list` shows the description and nothing else about the guest, so a
/// fixed string was the one place someone could have learned which Windows this is and
/// it said nothing — on a rack carrying several of these, all of them alike.
///
/// Every field here was **read off the media**, never asserted: `builder::assemble`
/// overwrites the caller's release with the detected one, so this describes what went on
/// the disk rather than what was asked for. Each is optional because each is separately
/// absent on real media, and [`Installed::description`] has to read as a sentence either
/// way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    /// `WindowsRelease::label`, e.g. `Windows Server 2022`.
    pub release: Option<String>,
    /// The **base** build the WIM reported, not the patch level: media whose filename
    /// says 19045 reports 19041.
    pub build: Option<u32>,
    /// `EDITIONID` of the image that was applied, e.g. `ServerDatacenterEval`. The
    /// image index is deliberately not here: it selects, it does not describe, and it
    /// means nothing to someone reading a list of instances.
    pub edition: Option<String>,
}

impl Installed {
    /// `Windows Server 2022 (20348), ServerDatacenterEval`.
    pub fn description(&self) -> String {
        /// Blank is absent. The GUI holds raw strings, so an empty field reaches here
        /// rather than a `None`.
        fn field(v: &Option<String>) -> Option<&str> {
            v.as_deref().map(str::trim).filter(|s| !s.is_empty())
        }
        // "Windows" rather than nothing, so the description is never empty and never
        // opens with a comma. This is also the whole description on the one path with
        // no media in hand, `oxwin instance` against an already-uploaded disk.
        let mut out = field(&self.release).unwrap_or("Windows").to_string();
        if let Some(build) = self.build {
            out.push_str(&format!(" ({build})"));
        }
        if let Some(edition) = field(&self.edition) {
            out.push_str(&format!(", {edition}"));
        }
        out
    }

    /// The same, for the disk the media was uploaded to: `Installer for Windows Server
    /// 2022 (20348), ServerDatacenterEval`.
    ///
    /// A word in front rather than a second format, because the two resources sit
    /// beside each other in a project and disagreeing about what the media is would be
    /// worse than saying it twice.
    pub fn installer_description(&self) -> String {
        format!("Installer for {}", self.description())
    }
}

impl InstanceSpec {
    /// Sensible defaults for a Windows install, given the disk that was uploaded.
    ///
    /// The description says only `Windows`: every caller that knows what the media was
    /// overwrites it with an [`Installed::description`], and a caller that does not know
    /// must not claim otherwise.
    pub fn for_installer(name: &str, installer_disk: &str) -> Self {
        Self {
            name: name.to_string(),
            description: Installed::default().description(),
            hostname: name.to_string(),
            ncpus: 4,
            memory_gib: 8,
            installer_disk: installer_disk.to_string(),
            system_disk: Some(format!("{name}-system")),
            system_disk_gib: 100,
            system_block_size: 4096,
            start: true,
        }
    }
}

/// What was made, and what still needs a human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub instance: String,
    pub system_disk: Option<String>,
    /// Things that are true and cannot be fixed from here.
    pub warnings: Vec<String>,
}

/// Everything that exists on the rack because of a run that then failed.
///
/// Nothing is deleted automatically: a transient error after a twenty-minute upload
/// should not silently discard it, and a rollback that itself fails leaves a state
/// harder to reason about than a named resource. So the failure carries a list, and the
/// caller prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leftovers {
    pub disks: Vec<String>,
    pub instances: Vec<String>,
    /// A golden run snapshots the system disk before it makes an image.
    pub snapshots: Vec<String>,
    /// The product of a golden run. Present here only when a *later* step failed.
    pub images: Vec<String>,
}

impl Leftovers {
    pub fn is_empty(&self) -> bool {
        self.disks.is_empty()
            && self.instances.is_empty()
            && self.snapshots.is_empty()
            && self.images.is_empty()
    }

    /// What to run to clear it, in the order that works.
    ///
    /// Instances before disks, because a disk attached to an instance cannot be
    /// deleted and the other order hands the user a command that fails on the first
    /// line. Snapshots after disks, because a snapshot outlives the disk it came
    /// from; images last, because an image derived from a snapshot may pin it.
    pub fn cleanup_commands(&self, project: &str) -> Vec<String> {
        use crate::golden::Resource;
        let mut out = Vec::new();
        for n in &self.instances {
            out.push(Resource::Instance(n.clone()).delete_command(project));
        }
        for n in &self.disks {
            out.push(Resource::Disk(n.clone()).delete_command(project));
        }
        for n in &self.snapshots {
            out.push(Resource::Snapshot(n.clone()).delete_command(project));
        }
        for n in &self.images {
            out.push(Resource::Image(n.clone()).delete_command(project));
        }
        out
    }
}

/// A failure, plus what it left behind.
#[derive(Debug)]
pub struct Failure {
    pub error: anyhow::Error,
    pub leftovers: Leftovers,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl Rack {
    /// Create the blank system disk and the instance, booting from the installer.
    pub fn create_instance(
        &self,
        spec: &InstanceSpec,
        reporter: &Reporter,
    ) -> std::result::Result<Created, Failure> {
        let mut leftovers = Leftovers::default();

        if let Some(system_disk) = &spec.system_disk {
            reporter.phase(
                "instance",
                format!(
                    "creating system disk {} ({} GiB)",
                    system_disk, spec.system_disk_gib
                ),
            );
            if let Err(error) = self.create_system_disk(spec, system_disk) {
                return Err(Failure { error, leftovers });
            }
            leftovers.disks.push(system_disk.clone());
        }

        reporter.phase("instance", format!("creating instance {}", spec.name));
        match self.create_the_instance(spec) {
            Ok(()) => {}
            Err(error) => return Err(Failure { error, leftovers }),
        }

        Ok(Created {
            instance: spec.name.clone(),
            system_disk: spec.system_disk.clone(),
            warnings: warnings_for(spec),
        })
    }

    fn create_system_disk(
        &self,
        spec: &InstanceSpec,
        system_disk: &str,
    ) -> Result<()> {
        // Whole GiB, at least one. Without this the first real upload is rejected.
        let gib = spec.system_disk_gib.max(1);
        let body = oxide::types::DiskCreate {
            name: name(system_disk)?,
            description: format!("System disk for {}", spec.name),
            size: oxide::types::ByteCount(gib * GIB),
            disk_backend: oxide::types::DiskSource::Blank {
                block_size: spec.system_block_size.try_into().map_err(|e| {
                    anyhow::anyhow!(
                        "system disk block size {}: {e}",
                        spec.system_block_size
                    )
                })?,
            }
            .into(),
        };
        self.block_on(
            self.client()
                .disk_create()
                .project(self.project())
                .body(body)
                .send(),
        )
        .with_context(|| format!("creating disk {system_disk}"))?;
        Ok(())
    }

    fn create_the_instance(&self, spec: &InstanceSpec) -> Result<()> {
        let hostname =
            if spec.hostname.is_empty() { &spec.name } else { &spec.hostname };

        let body = oxide::types::InstanceCreate {
            name: name(&spec.name)?,
            description: spec.description.clone(),
            hostname: hostname
                .parse()
                .map_err(|e| anyhow::anyhow!("hostname {hostname:?}: {e}"))?,
            ncpus: oxide::types::InstanceCpuCount(spec.ncpus),
            memory: oxide::types::ByteCount(spec.memory_gib.max(1) * GIB),
            // Pinned, not implied. Without it the instance follows its own UEFI
            // settings, which on this media means sitting at the EFI shell.
            boot_disk: Some(oxide::types::InstanceDiskAttachment::Attach {
                name: name(&spec.installer_disk)?,
            }),
            disks: match &spec.system_disk {
                Some(disk) => {
                    vec![oxide::types::InstanceDiskAttachment::Attach {
                        name: name(disk)?,
                    }]
                }
                None => Vec::new(),
            },
            // Without this the instance has no route in at all, and nothing inside the
            // guest can add one afterwards.
            external_ips: vec![oxide::types::ExternalIpCreate::Ephemeral {
                pool_selector: oxide::types::PoolSelector::Auto {
                    ip_version: None,
                },
            }],
            network_interfaces:
                oxide::types::InstanceNetworkInterfaceAttachment::DefaultIpv4,
            start: spec.start,
            anti_affinity_groups: Vec::new(),
            auto_restart_policy: None,
            cpu_platform: None,
            enable_jumbo_frames: false,
            multicast_groups: Vec::new(),
            ssh_public_keys: None,
            user_data: String::new(),
        };

        self.block_on(
            self.client()
                .instance_create()
                .project(self.project())
                .body(body)
                .send(),
        )
        .with_context(|| format!("creating instance {}", spec.name))?;
        Ok(())
    }
}

/// What is true about this instance that the app cannot fix.
pub fn warnings_for(spec: &InstanceSpec) -> Vec<String> {
    let mut warnings = vec![format!(
        "Remote Desktop will time out until the VPC allows inbound tcp/3389. \
             The default VPC permits only tcp/22 and ICMP, so enabling RDP in the \
             guest is necessary but not sufficient."
    )];
    // The installer is safe to leave attached, the chooser boots an installed Windows
    // once there is one; but only if it really is the pinned boot disk.
    warnings.push(format!(
        "{} stays attached and remains the boot disk. That is deliberate: the media's \
         UEFI chooser boots the installed Windows once the install finishes, so there \
         is no window to catch. Detach it whenever convenient.",
        spec.installer_disk
    ));
    warnings
}

fn name(raw: &str) -> Result<oxide::types::Name> {
    raw.parse().map_err(|e| anyhow::anyhow!("name {raw:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pin_the_installer_as_the_boot_disk_and_name_a_system_disk() {
        let spec = InstanceSpec::for_installer("windows", "ws2022-installer");
        assert_eq!(spec.installer_disk, "ws2022-installer");
        assert_eq!(spec.system_disk.as_deref(), Some("windows-system"));
        assert!(spec.system_disk_gib >= 1, "a disk must be at least 1 GiB");
        assert!(spec.start);
    }

    /// A golden run can leave four kinds of thing behind, and the order they have
    /// to be removed in is the whole reason this function exists.
    #[test]
    fn cleanup_orders_instances_disks_snapshots_images() {
        let leftovers = Leftovers {
            disks: vec!["d1".into()],
            instances: vec!["i1".into()],
            snapshots: vec!["s1".into()],
            images: vec!["m1".into()],
        };
        let commands = leftovers.cleanup_commands("danb");
        assert_eq!(commands.len(), 4);
        assert!(commands[0].contains("instance delete"), "{commands:?}");
        assert!(commands[1].contains("disk delete"), "{commands:?}");
        assert!(commands[2].contains("snapshot delete"), "{commands:?}");
        assert!(commands[3].contains("image delete"), "{commands:?}");
        assert!(commands.iter().all(|c| c.contains("--project danb")));
    }

    /// Instances before disks. A disk attached to an instance cannot be deleted, so the
    /// other order hands the user commands that fail on the first one.
    #[test]
    fn cleanup_deletes_instances_before_disks() {
        let leftovers = Leftovers {
            disks: vec!["d1".into()],
            instances: vec!["i1".into()],
            ..Default::default()
        };
        let commands = leftovers.cleanup_commands("danb");
        assert_eq!(commands.len(), 2);
        assert!(commands[0].contains("instance delete"), "{commands:?}");
        assert!(commands[1].contains("disk delete"), "{commands:?}");
        assert!(commands.iter().all(|c| c.contains("--project danb")));
    }

    #[test]
    fn nothing_created_means_nothing_to_clean_up() {
        assert!(Leftovers::default().is_empty());
        assert!(Leftovers::default().cleanup_commands("danb").is_empty());
    }

    /// Everything the media said, in the order someone reads it.
    #[test]
    fn a_description_names_the_release_the_build_and_the_edition() {
        let installed = Installed {
            release: Some("Windows Server 2022".into()),
            build: Some(20348),
            edition: Some("ServerDatacenterEval".into()),
        };
        assert_eq!(
            installed.description(),
            "Windows Server 2022 (20348), ServerDatacenterEval"
        );
    }

    /// Each field is separately absent on real media: a build with no release is an
    /// unfamiliar build, and an edition with no build is media omitting the tag. None
    /// of them may turn into an empty pair of brackets or a dangling comma.
    #[test]
    fn a_description_degrades_one_field_at_a_time() {
        let full = Installed {
            release: Some("Windows Server 2022".into()),
            build: Some(20348),
            edition: Some("ServerDatacenterEval".into()),
        };
        assert_eq!(
            Installed { build: None, ..full.clone() }.description(),
            "Windows Server 2022, ServerDatacenterEval"
        );
        assert_eq!(
            Installed { edition: None, ..full.clone() }.description(),
            "Windows Server 2022 (20348)"
        );
        assert_eq!(
            Installed { release: None, ..full.clone() }.description(),
            "Windows (20348), ServerDatacenterEval"
        );
        assert_eq!(
            Installed { release: None, build: None, ..full }.description(),
            "Windows, ServerDatacenterEval"
        );
    }

    /// The disk is described by what it installs, so it is the same string with a word
    /// in front of it: an installer disk and the instance that boots it sit next to each
    /// other in a project, and they should agree about what is on the media.
    #[test]
    fn an_installer_disk_is_described_by_what_it_installs() {
        let installed = Installed {
            release: Some("Windows Server 2022".into()),
            build: Some(20348),
            edition: Some("ServerDatacenterEval".into()),
        };
        assert_eq!(
            installed.installer_description(),
            "Installer for Windows Server 2022 (20348), ServerDatacenterEval"
        );
        assert_eq!(
            Installed::default().installer_description(),
            "Installer for Windows"
        );
    }

    /// The `oxwin instance` path has no media in hand at all, so this is what it gets
    /// when the user names no description.
    #[test]
    fn a_description_of_nothing_is_still_a_description() {
        assert_eq!(Installed::default().description(), "Windows");
        assert_eq!(
            InstanceSpec::for_installer("w", "installer").description,
            "Windows"
        );
    }

    /// Blank is not the same as absent anywhere else in this workspace, and it is not
    /// here either: the GUI's `Draft` holds raw strings, so an empty edition reaches
    /// this rather than a `None`.
    #[test]
    fn a_blank_field_is_an_absent_field() {
        let installed = Installed {
            release: Some(String::new()),
            build: None,
            edition: Some("  ".into()),
        };
        assert_eq!(installed.description(), "Windows");
    }

    /// The RDP warning is not optional detail. It is the single most common surprise on
    /// a rack, and it cannot be fixed from the answer file or from inside the guest.
    #[test]
    fn the_rdp_firewall_warning_is_always_present() {
        let warnings =
            warnings_for(&InstanceSpec::for_installer("w", "installer"));
        assert!(
            warnings.iter().any(|w| w.contains("3389")),
            "no warning names the port that has to be opened: {warnings:?}"
        );
    }
}
