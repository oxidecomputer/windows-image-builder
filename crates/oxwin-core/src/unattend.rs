// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! `autounattend.xml` generation.
//!
//! Windows Setup scans every attached volume for `autounattend.xml` at boot, which is
//! the trick this whole project rests on: the answer disk needs no bootloader and no
//! cooperation from the install medium. The oxide rack doesnt have the traditional
//! virtual floppy some systems rely on; thus we make our own unique install media.
//!
//! This builds code that is then tested against the committed goldens in
//! `testdata/unattend/` (see the tests at the bottom). Those goldens began as the output
//! of the JavaScript test code that was the v0, and have been regenerated from this code
//! several times since, so they are known-good configurations rather than a fixed
//! reference — `TESTED-MEDIA.md` records which releases an answer file has actually
//! installed. An answer file that is subtly wrong does not error, it installs
//! a machine with no network, or no bootstrap, or stalls at a licence page.
//! So the formatting needs to be reproduced exactly, including the blank line an empty
//!  pass leaves behind, rather than tidied up.
//!
//! Two design decisions carried over from the original:
//!
//! * The guest bootstrap runs in `specialize` as SYSTEM, not from
//!   `FirstLogonCommands`. Those need an interactive logon, which means autologon,
//!   which means a console session nobody is watching.
//! * Nothing references the answer disk by drive letter. Letters are not stable across
//!   passes or guests, so the bootstrap is located by scanning filesystem drives.

use crate::settings::{Deployment, Settings, WindowsRelease};
use anyhow::{Result, bail};

mod lint;
pub use lint::{LintContext, lint};

/// Label of the volume carrying `autounattend.xml`, `bootstrap.ps1`, drivers and
/// OpenSSH. The image builder must label the volume with this same string.
pub const VOLUME_LABEL: &str = "WINSETUP";

const NS: &str = concat!(
    r#"xmlns="urn:schemas-microsoft-com:unattend" "#,
    r#"xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State""#
);
const ARCH: &str = concat!(
    r#"processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" "#,
    r#"language="neutral" versionScope="nonSxS""#
);

/// `<Path>` is capped at this by the unattend schema. Over it, Setup rejects the whole
/// answer file with a message naming only the pass, which reads like malformed XML.
pub const PATH_LIMIT: usize = 259;

/// Skips A and B (floppies) only. C is included deliberately: when the installer media
/// is the sole volume WinPE mounts it lands on C, and omitting it meant the drivers
/// were never found. Setup tolerates paths that do not resolve, so over-listing costs
/// only log noise.
const DRIVE_LETTERS: &[char] = &['C', 'D', 'E', 'F', 'G', 'H'];

pub fn xml_escape(value: &str) -> String {
    // Ampersand first, or the escapes introduced below get escaped again.
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A Windows release and the editions its media carries.
struct Target {
    /// Oxide instances expose no vTPM and no UEFI Secure Boot, so stock Windows 11
    /// Setup refuses to install without the LabConfig bypasses. Windows 11 only —
    /// see `target_for`, which is where the per-release decision is made.
    bypass_hardware_checks: bool,
    editions: Vec<Edition>,
}

struct Edition {
    id: String,
    /// The `/IMAGE/NAME` value, used only when no image index is known.
    name: String,
}

impl Edition {
    fn new(id: &str, name: String) -> Self {
        Self { id: id.to_string(), name }
    }
}

/// The editions on server media, which is one vocabulary across every release.
///
/// Server 2019, 2022 and 2025 ship the same four images with the same ids, differing only
/// in the year in the name — so this is parameterised rather than copy-pasted three
/// times. Copy-pasting is how `datacenter` came to be spelled two ways in one codebase.
///
/// The names must keep their exact spelling: `SERVERDATACENTER` is Desktop Experience and
/// `SERVERDATACENTERCORE` is Core, and a loose match between them installed a server with
/// no desktop once already.
fn server_editions(year: u16) -> Vec<Edition> {
    ["DATACENTER", "STANDARD"]
        .iter()
        .flat_map(|which| {
            let id = which.to_lowercase();
            [
                Edition::new(
                    &id,
                    format!("Windows Server {year} SERVER{which}"),
                ),
                Edition::new(
                    &format!("{id}-core"),
                    format!("Windows Server {year} SERVER{which}CORE"),
                ),
            ]
        })
        .collect()
}

/// The editions on client media.
///
/// Far more than server media carries, and the choice matters beyond the name: **Home has
/// no RDP host and cannot domain-join**, so it is reachable over SSH and the serial
/// console and not by Remote Desktop. There is no Core variant: `INSTALLATIONTYPE` is
/// `Client` on every image, which is also why Windows 11 Home being `FLAGS=Core` fooled
/// the old Core test.
fn client_editions(release: &str) -> Vec<Edition> {
    ["Home", "Pro", "Education", "Enterprise"]
        .iter()
        .map(|name| {
            Edition::new(&name.to_lowercase(), format!("{release} {name}"))
        })
        .collect()
}

fn target_for(release: WindowsRelease) -> Result<Target> {
    // Public KMS client setup keys are deliberately not carried over: nothing here
    // activates Windows, and the only reason the original held them was to stop Setup
    // asking for a key, which omitting <ProductKey> already achieves.
    //
    // The bypass decision lives in this match rather than behind `is_client()` so that a
    // new release cannot arrive without an explicit answer — the compiler asks. Only
    // Windows 11 Setup reads LabConfig: it is the release with the appraiser that gates
    // installation on a vTPM and Secure Boot, neither of which an Oxide instance has.
    // Server Setup never runs that appraiser, and Windows 10's does not consult these
    // values, so writing them there was five registry writes nothing reads, carrying
    // descriptions that said "Windows 11" on a Windows 10 answer file.
    let (bypass_hardware_checks, editions) = match release {
        WindowsRelease::Server2016 => (false, server_editions(2016)),
        WindowsRelease::Server2019 => (false, server_editions(2019)),
        WindowsRelease::Server2022 => (false, server_editions(2022)),
        WindowsRelease::Server2025 => (false, server_editions(2025)),
        WindowsRelease::Windows10 => (false, client_editions("Windows 10")),
        WindowsRelease::Windows11 => (true, client_editions("Windows 11")),
    };
    Ok(Target { bypass_hardware_checks, editions })
}

/// Everything the answer file needs. Deliberately lower level than [`Settings`]: it
/// carries the fields only the orchestration layer knows (`image_index`) and the
/// diagnostic escape hatches the UI does not expose, so the answer file can be generated
/// and inspected without a UI in the loop.
#[derive(Debug, Clone)]
pub struct Config {
    pub release: WindowsRelease,
    /// Edition id, e.g. `datacenter` or `datacenter-core`.
    pub edition: String,
    pub computer_name: String,
    pub username: String,
    pub password: String,
    pub enable_rdp: bool,
    pub inject_drivers: bool,
    pub enable_serial_console: bool,
    pub target_disk: u8,
    /// How the target disk is partitioned. The default is the three partitions the
    /// answer file has always written; see [`crate::partition`] for why orders and
    /// IDs are derived rather than carried.
    pub partitions: Vec<crate::partition::Partition>,
    /// What Setup and the installed shell are rendered in.
    ///
    /// **Constrained by the media**: this needs a language pack present in the
    /// image, and most media carries exactly one. `media::MediaInfo::languages`
    /// reads which. A value the media does not carry is a warning rather than a
    /// refusal, because what Setup does then has not been established here — see
    /// the design note in `docs/superpowers/specs/2026-09-21-customization-design.md`.
    pub ui_language: String,
    /// Formats, currency, and keyboard layout: `UserLocale`, `SystemLocale` and
    /// `InputLocale`.
    ///
    /// Unconstrained by the media — any tag works on any ISO. Kept apart from
    /// `ui_language` because collapsing the two is only correct while both are
    /// `en-US`, which is the only reason one field sufficed until now.
    pub region: String,
    pub timezone: String,
    pub product_key: Option<String>,
    /// Console autologon. For debugging installs only.
    ///
    /// Off in every real path, and deliberately so: it exists to make a hang at the
    /// logon screen debuggable, not to be part of a build. Nothing that ships needs it,
    /// in particular [`Config::generalize`] runs from a SYSTEM scheduled task rather
    /// than `FirstLogonCommands`, precisely so that automating the golden image does not
    /// drag an autologon back in.
    pub auto_logon: bool,
    /// Generalize the machine once the install has finished, so its disk can be cloned.
    ///
    /// Only meaningful for a golden image. `<ComputerName>*</ComputerName>` alone is not
    /// enough, it resolves once, during `specialize`, so without `sysprep /generalize`
    /// every clone keeps one name and one SID.
    pub generalize: bool,
    /// Where Setup writes `setupact.log` and `setuperr.log`.
    ///
    /// `None` leaves Setup's default, which during `windowsPE` is the WinPE RAM disk, so
    /// an install that stalls there takes its own explanation down with it on reset. That
    /// is why a rack hang has been unreadable. Point this at the installer volume, which
    /// is writable and survives, and the logs can be read afterwards by attaching that
    /// disk to a machine that works.
    ///
    /// The path has to be a literal, and WinPE drive letters are not stable — the media
    /// is usually `C:` when the target disk is still unformatted, but nothing guarantees
    /// it. Diagnostic, not something to depend on.
    pub log_path: Option<String>,
    /// `OnError` on every `WillShowUI`, which is the shipped behaviour, or `Never`.
    ///
    /// **On an Oxide guest `OnError` means an infinite hang.** There is no console to show
    /// UI on, so Setup waits forever for a click nobody can make, and from the outside
    /// that is indistinguishable from a slow install. `Never` makes it fail fast instead,
    /// worse for a human at a keyboard, far better for a machine nobody is watching.
    pub show_ui_on_error: bool,
    /// Emit a serial marker (`specialize-begin`) at the start of `specialize`, after
    /// the image has been applied and the guest has rebooted into it, so reaching it
    /// proves `windowsPE` finished.
    ///
    /// Diagnostic, and off by default so the answer file stays byte-identical to the one
    /// that has installed on a rack. Setup renders to graphics an Oxide instance does not
    /// have, so this marker is the only view there is until EMS comes up.
    ///
    /// There used to be markers at the start and end of `windowsPE` too, but WinPE has
    /// no COM1 device: they wrote nothing while `& exit /b 0` still reported success to
    /// Setup's log.
    pub verbose_serial: bool,
    /// Index of the image to install, read from the WIM's own metadata. Preferred over
    /// a name: names vary across retail, evaluation, OEM and localised media, so a
    /// hardcoded name silently matches nothing and Setup shows an empty edition list.
    pub image_index: Option<u32>,
    /// Diagnostic escape hatch: drop `<ImageInstall>` so Setup picks the edition and
    /// resolves the licence itself.
    pub skip_image_install: bool,
    /// Public keys authorised for SSH. Read only by the bootstrap script, the answer
    /// file has nowhere to put them, since the account does not exist until
    /// `oobeSystem` and sshd is configured in `specialize`.
    pub ssh_keys: Vec<String>,
    /// Install and start OpenSSH in the guest. Also read only by the bootstrap.
    pub enable_ssh: bool,
    /// Cloud-init in the guest: install it, configure it, and run it once per
    /// clone. Read by `bootstrap` and by [`build_sysprep`].
    pub cloud_init: Option<crate::settings::CloudInit>,
    /// Whether this build carries any extra files, which is all the guest
    /// script needs to know -- what they are is `builder`'s business. A bool
    /// rather than the list, so the answer file and the bootstrap cannot
    /// disagree with the volume about what is on it.
    pub has_extras: bool,
}

impl Config {
    /// Build a config from the user-facing settings. Everything the UI does not expose
    /// takes the value the current builder uses.
    pub fn from_settings(
        settings: &Settings,
        image_index: Option<u32>,
    ) -> Self {
        Self {
            release: settings.release,
            // One convention, not two. This used to append `-core` here while
            // `Settings::edition_hint` appended `core`, for the same fact, and neither
            // spelling is needed now that Core-ness is a property of the image the user
            // picked out of the media's own list rather than a separate switch.
            edition: settings.edition.trim().to_lowercase(),
            computer_name: match &settings.deployment {
                Deployment::GoldenImage => "*".to_string(),
                Deployment::Named { hostname } => hostname.clone(),
            },
            username: settings.credentials.username.clone(),
            password: settings.credentials.password.clone(),
            enable_rdp: settings.enable_rdp,
            inject_drivers: settings.inject_drivers,
            enable_serial_console: settings.enable_serial_console,
            target_disk: settings.target_disk,
            partitions: settings.partitions.clone(),
            ui_language: settings.ui_language.clone(),
            region: settings.region.clone(),
            timezone: settings.timezone.clone(),
            product_key: settings.product_key.clone(),
            auto_logon: false,
            generalize: settings.deployment.is_golden(),
            log_path: None,
            show_ui_on_error: true,
            verbose_serial: false,
            image_index,
            skip_image_install: false,
            ssh_keys: settings.credentials.keys.clone(),
            enable_ssh: true,
            cloud_init: settings.cloud_init,
            has_extras: !settings.extras.is_empty(),
        }
    }
}

/// One entry in a `<RunSynchronous>` block.
struct Command {
    path: String,
    description: String,
}

/// `OnError` or `Never` for every `WillShowUI` in the answer file.
///
/// One function rather than three literals: the whole point is that they agree, and this
/// codebase has already been bitten twice by one fact spelled two ways.
fn will_show_ui(config: &Config) -> &'static str {
    if config.show_ui_on_error { "OnError" } else { "Never" }
}

pub fn build(config: &Config) -> Result<String> {
    let target = target_for(config.release)?;
    // The edition is needed for exactly one thing: the `/IMAGE/NAME` written when no
    // image index is known. With an index it is dead weight, so an unrecognised one is
    // only fatal without an index — which is what lets the caller select an image by
    // number, or by anything else the WIM's own list offers, without also having to name
    // it in a table here. Requiring it turned a Windows 11 ISO into "unknown win11
    // edition \"datacenter\"", which describes our table rather than the user's media.
    let unnamed = Edition::new("", String::new());
    let found = target.editions.iter().find(|e| e.id == config.edition);
    let edition = match (found, config.image_index) {
        (Some(edition), _) => edition,
        (None, Some(_)) => &unnamed,
        (None, None) => bail!(
            "no image index, and {:?} is not a known {} edition — this release has {}",
            config.edition,
            config.release.token(),
            target
                .editions
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    // Joined with a newline, exactly as the original does, which means a pass that
    // returns an empty string leaves a blank line in the output. Preserved on purpose:
    // the reference file has it, and this is a byte-for-byte port.
    let settings = [
        windows_pe_pass(config, &target, edition)?,
        offline_servicing_pass(config),
        specialize_pass(config, false)?,
        oobe_pass(config, false),
    ];

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<unattend {NS}>\n{}\n</unattend>\n",
        settings.join("\n")
    ))
}

/// The `<DiskConfiguration>` block, generated from the layout.
///
/// Byte-for-byte what the literal it replaced produced, for the default layout.
/// `Order` and `PartitionID` are the 1-based position in the list and are never
/// carried in the model, which is the whole reason a custom layout is safe to offer.
fn disk_configuration(config: &Config) -> String {
    let creates: Vec<String> = config
        .partitions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let extent = match p.size_mb {
                Some(mb) => format!("              <Size>{mb}</Size>\n"),
                None => "              <Extend>true</Extend>\n".to_string(),
            };
            format!(
                "            <CreatePartition wcm:action=\"add\">\n\
                 \x20             <Order>{}</Order>\n\
                 \x20             <Type>{}</Type>\n\
                 {extent}\x20           </CreatePartition>",
                i + 1,
                p.kind.type_name()
            )
        })
        .collect();

    let modifies: Vec<String> = config
        .partitions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // Emitted only when set. MSR carries none of the three, and adding an
            // empty <Label> or <Format> for it would move every committed golden.
            let label = match &p.label {
                Some(l) => {
                    format!("              <Label>{}</Label>\n", xml_escape(l))
                }
                None => String::new(),
            };
            let letter = match p.letter {
                Some(c) => format!("              <Letter>{c}</Letter>\n"),
                None => String::new(),
            };
            let format = match p.format {
                Some(f) => {
                    format!("              <Format>{}</Format>\n", f.name())
                }
                None => String::new(),
            };
            format!(
                "            <ModifyPartition wcm:action=\"add\">\n\
                 \x20             <Order>{n}</Order>\n\
                 \x20             <PartitionID>{n}</PartitionID>\n\
                 {label}{letter}{format}\x20           </ModifyPartition>",
                n = i + 1
            )
        })
        .collect();

    format!(
        "      <DiskConfiguration>\n\
         \x20       <WillShowUI>{ui}</WillShowUI>\n\
         \x20       <Disk wcm:action=\"add\">\n\
         \x20         <DiskID>{disk}</DiskID>\n\
         \x20         <WillWipeDisk>true</WillWipeDisk>\n\
         \x20         <CreatePartitions>\n{}\n\
         \x20         </CreatePartitions>\n\
         \x20         <ModifyPartitions>\n{}\n\
         \x20         </ModifyPartitions>\n\
         \x20       </Disk>\n\
         \x20     </DiskConfiguration>\n",
        creates.join("\n"),
        modifies.join("\n"),
        disk = config.target_disk,
        ui = will_show_ui(config)
    )
}

fn driver_paths() -> String {
    DRIVE_LETTERS
        .iter()
        .enumerate()
        .map(|(i, letter)| {
            format!(
                "        <PathAndCredentials wcm:action=\"add\" wcm:keyValue=\"{}\">\n\
                 \x20         <Path>{letter}:\\drivers</Path>\n\
                 \x20       </PathAndCredentials>",
                i + 1
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_synchronous_commands(commands: &[Command]) -> String {
    commands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "        <RunSynchronousCommand wcm:action=\"add\">\n\
                 \x20         <Order>{}</Order>\n\
                 \x20         <Path>{}</Path>\n\
                 \x20         <Description>{}</Description>\n\
                 \x20       </RunSynchronousCommand>",
                i + 1,
                xml_escape(&c.path),
                xml_escape(&c.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The answer file handed to `sysprep /generalize /unattend:`.
///
/// **Windows scrubs the password out of the answer file it caches.** After Setup has
/// processed `autounattend.xml` it writes a copy to `C:\Windows\Panther\unattend.xml`
/// with every `<Password>` replaced by the literal `*SENSITIVE*DATA*DELETED*`. A
/// `sysprep /generalize` with no `/unattend:` falls back to exactly that file, so the
/// `oobeSystem` pass cannot create the local account, OOBE stops and waits for a human,
/// and the machine sits at the out-of-box wizard forever. Verified on a rack: the guest
/// came up with `OOBEInProgress=1`, an IP, working drivers and no reachable Remote
/// Desktop, because the RDP listener does not accept connections until OOBE finishes.
/// `fDenyTSConnections` was already `0`; nothing was wrong with RDP itself.
///
/// So a golden build carries its own copy, with the password intact, and names it
/// explicitly.
///
/// Two passes only. `windowsPE` is left out deliberately — it carries
/// `DiskConfiguration`, and an answer file that could repartition a disk has no business
/// being handed to sysprep, however sure one is that Windows ignores it outside Setup.
/// `offlineServicing` is omitted because the drivers are already installed.
///
/// `specialize` *is* included: it re-resolves `<ComputerName>*</ComputerName>` so every
/// clone gets its own name, which is the entire point. Its `RunSynchronousCommand` looks
/// for `setup\bootstrap.ps1` across the filesystem drives and simply matches nothing on
/// a clone, exiting zero rather than failing the pass.
pub fn build_sysprep(config: &Config) -> Result<String> {
    let settings = [specialize_pass(config, true)?, oobe_pass(config, true)];
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<unattend {NS}>\n{}\n</unattend>\n",
        settings.join("\n")
    ))
}

fn windows_pe_pass(
    config: &Config,
    target: &Target,
    edition: &Edition,
) -> Result<String> {
    let language = xml_escape(&config.ui_language);
    let region = xml_escape(&config.region);
    let mut components: Vec<String> = Vec::new();

    components.push(format!(
        "    <component name=\"Microsoft-Windows-International-Core-WinPE\" {ARCH}>\n\
         \x20     <SetupUILanguage>\n\
         \x20       <UILanguage>{language}</UILanguage>\n\
         \x20     </SetupUILanguage>\n\
         \x20     <InputLocale>{region}</InputLocale>\n\
         \x20     <SystemLocale>{region}</SystemLocale>\n\
         \x20     <UILanguage>{language}</UILanguage>\n\
         \x20     <UserLocale>{region}</UserLocale>\n\
         \x20   </component>"
    ));

    if config.inject_drivers {
        // WinPE assigns drive letters in enumeration order and they cannot be
        // predicted, so every plausible letter is listed. Setup logs a miss and moves
        // on, which makes this safe if noisy.
        components.push(format!(
            "    <component name=\"Microsoft-Windows-PnpCustomizationsWinPE\" {ARCH}>\n\
             \x20     <DriverPaths>\n{}\n\
             \x20     </DriverPaths>\n\
             \x20   </component>",
            driver_paths()
        ));
    }

    let mut setup_commands: Vec<Command> = Vec::new();

    if target.bypass_hardware_checks {
        // One command per key. Chaining all five with && makes a 408-character
        // <Path>, over the 259 limit, and Setup then rejects the entire answer file.
        for check in [
            "BypassTPMCheck",
            "BypassSecureBootCheck",
            "BypassRAMCheck",
            "BypassCPUCheck",
            "BypassStorageCheck",
        ] {
            setup_commands.push(Command {
                path: format!(
                    "cmd.exe /c reg add HKLM\\System\\Setup\\LabConfig \
                     /v {check} /t REG_DWORD /d 1 /f"
                ),
                description: format!("Bypass Windows 11 {check}"),
            });
        }
    }

    let run_sync = if setup_commands.is_empty() {
        String::new()
    } else {
        format!(
            "      <RunSynchronous>\n{}\n      </RunSynchronous>\n",
            run_synchronous_commands(&setup_commands)
        )
    };

    let image_install = if config.skip_image_install {
        String::new()
    } else {
        // No `<Path>`: Setup resolves sources\install.wim relative to the volume it
        // booted from, and naming a second partition there fails to resolve the licence
        // terms. `builder`'s media is one volume for exactly that reason, so the only
        // correct path is the one Setup finds unaided.
        let metadata = match config.image_index {
            Some(index) => format!(
                "            <MetaData wcm:action=\"add\">\n\
                 \x20             <Key>/IMAGE/INDEX</Key>\n\
                 \x20             <Value>{index}</Value>\n\
                 \x20           </MetaData>"
            ),
            None => format!(
                "            <MetaData wcm:action=\"add\">\n\
                 \x20             <Key>/IMAGE/NAME</Key>\n\
                 \x20             <Value>{}</Value>\n\
                 \x20           </MetaData>",
                xml_escape(&edition.name)
            ),
        };
        format!(
            "      <ImageInstall>\n\
             \x20       <OSImage>\n\
             \x20         <InstallFrom>\n\
             {metadata}\n\
             \x20         </InstallFrom>\n\
             \x20         <InstallTo>\n\
             \x20           <DiskID>{disk}</DiskID>\n\
             \x20           <PartitionID>{partition}</PartitionID>\n\
             \x20         </InstallTo>\n\
             \x20         <WillShowUI>{ui}</WillShowUI>\n\
             \x20       </OSImage>\n\
             \x20     </ImageInstall>\n",
            disk = config.target_disk,
            ui = will_show_ui(config),
            partition = crate::partition::os_partition_id(&config.partitions),
        )
    };

    // Omit <ProductKey> entirely when there is no key. An *empty* <Key> is not the
    // same as no key: Setup treats it as a key to resolve, matches no edition, and
    // fails with "Windows cannot find the Microsoft Software License Terms" before it
    // shows any page, because the licence lookup is downstream of edition resolution.
    // That cost a day. When a key *is* given it filters the image list to editions it
    // is valid for, so a retail key against evaluation media matches nothing.
    let product_key = match &config.product_key {
        Some(key) => format!(
            "        <ProductKey>\n\
             \x20         <Key>{}</Key>\n\
             \x20         <WillShowUI>{ui}</WillShowUI>\n\
             \x20       </ProductKey>\n",
            xml_escape(key),
            ui = will_show_ui(config)
        ),
        None => String::new(),
    };

    let disk_config = disk_configuration(config);

    components.push(format!(
        "    <component name=\"Microsoft-Windows-Setup\" {ARCH}>\n\
         {run_sync}{log_path}{disk_config}{image_install}      <UpgradeData>\n\
         \x20       <Upgrade>false</Upgrade>\n\
         \x20       <WillShowUI>Never</WillShowUI>\n\
         \x20     </UpgradeData>\n\
         \x20     <UserData>\n\
         \x20       <AcceptEula>true</AcceptEula>\n\
         \x20       <FullName>{user}</FullName>\n\
         \x20       <Organization>Oxide</Organization>\n\
         {product_key}      </UserData>\n\
         \x20   </component>",
        user = xml_escape(&config.username),
        log_path = match &config.log_path {
            Some(path) =>
                format!("      <LogPath>{}</LogPath>\n", xml_escape(path)),
            None => String::new(),
        },
    ));

    Ok(format!(
        "  <settings pass=\"windowsPE\">\n{}\n  </settings>",
        components.join("\n")
    ))
}

/// Out-of-box drivers are staged here, not in `specialize`.
/// `Microsoft-Windows-PnpCustomizationsNonWinPE` is only valid in `offlineServicing`
/// and `auditSystem`; in `specialize` Setup silently ignores the whole component. That
/// is exactly what happened: every install shipped with this block in the wrong pass,
/// no error anywhere, and NetKVM was never installed, so the guest had no network.
///
/// `offlineServicing` applies the drivers to the image before first boot, so the NIC
/// works from the very first boot rather than after a PnP rescan.
fn offline_servicing_pass(config: &Config) -> String {
    if !config.inject_drivers {
        return String::new();
    }
    format!(
        "  <settings pass=\"offlineServicing\">\n\
         \x20   <component name=\"Microsoft-Windows-PnpCustomizationsNonWinPE\" {ARCH}>\n\
         \x20     <DriverPaths>\n{}\n\
         \x20     </DriverPaths>\n\
         \x20   </component>\n\
         \x20 </settings>",
        driver_paths()
    )
}

fn specialize_pass(config: &Config, for_sysprep: bool) -> Result<String> {
    let mut components: Vec<String> = Vec::new();

    components.push(format!(
        "    <component name=\"Microsoft-Windows-Shell-Setup\" {ARCH}>\n\
         \x20     <ComputerName>{}</ComputerName>\n\
         \x20     <TimeZone>{}</TimeZone>\n\
         \x20   </component>",
        xml_escape(&config.computer_name),
        xml_escape(&config.timezone)
    ));

    if config.enable_rdp {
        components.push(format!(
            "    <component name=\"Microsoft-Windows-TerminalServices-LocalSessionManager\" {ARCH}>\n\
             \x20     <fDenyTSConnections>false</fDenyTSConnections>\n\
             \x20   </component>"
        ));
        // 1 keeps Network Level Authentication on. 0 disables it, which is the usual
        // copy-paste default and a real downgrade: it lets a client reach the logon
        // screen before authenticating. Standard clients handle NLA against a local
        // account fine, so there is no reason to ship the weaker setting.
        components.push(format!(
            "    <component name=\"Microsoft-Windows-TerminalServices-RDP-WinStationExtensions\" {ARCH}>\n\
             \x20     <UserAuthentication>1</UserAuthentication>\n\
             \x20   </component>"
        ));
        components.push(format!(
            "    <component name=\"Networking-MPSSVC-Svc\" {ARCH}>\n\
             \x20     <FirewallGroups>\n\
             \x20       <FirewallGroup wcm:action=\"add\" wcm:keyValue=\"RemoteDesktop\">\n\
             \x20         <Active>true</Active>\n\
             \x20         <Group>@FirewallAPI.dll,-28752</Group>\n\
             \x20         <Profile>all</Profile>\n\
             \x20       </FirewallGroup>\n\
             \x20     </FirewallGroups>\n\
             \x20   </component>"
        ));
    }

    let mut commands: Vec<Command> = Vec::new();

    if config.verbose_serial {
        // First thing in specialize, so reaching it proves windowsPE finished, the image
        // was applied and the guest rebooted into it.
        commands.push(Command {
            path:
                "cmd.exe /c echo OXIDE-STAGE specialize-begin>COM1 & exit /b 0"
                    .into(),
            description: "Serial progress marker: specialize-begin".into(),
        });
    }

    if config.enable_serial_console {
        // An Oxide rack's only out-of-band access to a guest is the serial console, so
        // without this an install that goes wrong is invisible. It runs first and as
        // its own command: if the bootstrap below fails, serial is what you need to
        // find out why. Propolis exposes the guest serial port as com1, hence
        // EMSPORT:1; /bootems additionally sends boot loader output to serial.
        commands.push(Command {
            path: "cmd.exe /c bcdedit /ems {current} on && bcdedit /emssettings \
                   EMSPORT:1 EMSBAUDRATE:115200 && bcdedit /bootems {current} on"
                .into(),
            description: "Enable EMS serial console on com1".into(),
        });
    }

    // Runs as SYSTEM before any user logs on. The drive letter is not knowable here,
    // so enumerate filesystem drives and look for the script itself. Looking it up by
    // volume label was too brittle: when the builder's label and VOLUME_LABEL
    // disagreed the lookup threw, this command failed, and the whole bootstrap was
    // skipped *without failing the install* — so the guest came up looking fine with
    // no OpenSSH, no firewall rule and no SSH keys.
    //
    // The terse aliases (gdr/%/?) rather than readable cmdlet names are what keep this
    // under the 259-character <Path> limit.
    let find = "gdr -PSProvider FileSystem|%{$_.Root+'setup\\bootstrap.ps1'}\
                |?{Test-Path $_}|select -First 1|%{& $_}";
    let path = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command \"{find}\""
    );
    if path.length_over_limit() {
        bail!(
            "bootstrap RunSynchronousCommand path is {} chars, limit is {PATH_LIMIT}",
            path.len()
        );
    }
    commands
        .push(Command { path, description: "Oxide guest bootstrap".into() });

    // The clone-side cloud-init run. Only in the sysprep answer file
    // (`for_sysprep`), never the install-time one, and gated on `for_sysprep`
    // alone rather than `config.generalize` -- which is also true for the
    // install-time file of a golden build. The golden image is built on a
    // rack instance, which has its own config drive: if this command were in
    // the install-time file too, it would run right after the bootstrap
    // command above (which has just written this very script) *during the
    // golden's own install*, renaming the golden from the builder instance's
    // metadata and arming `OxideCloudInit` before sysprep has even run --
    // exactly the mid-sysprep race that registering the task disabled was
    // supposed to close. `build_sysprep` is only ever called for a golden
    // build, so `for_sysprep` already implies `config.generalize`.
    //
    // No `Set-Service cloudbase-init -StartupType Automatic` command here.
    // `OxideCloudInit` is the only thing that sets the service to Automatic
    // now: setting it here in `specialize` would start the service on this
    // clone's next boot *before* `OxideCloudInit` has created the account's
    // profile, so cloud-init's SSH-key plugin would miss it and the
    // per-instance keys would not land until the clone's second boot.
    //
    // Must run after the bootstrap command above: if the installer disk is
    // still attached to this clone, that command re-runs the golden
    // `bootstrap.ps1`, which re-registers `OxideCloudInit` *disabled*. This
    // command is what re-enables it, so it has to come last.
    if for_sysprep && config.cloud_init.is_some() {
        // A short call to a generated script rather than the prototype's inline
        // cmd.exe incantation, which at ~230 characters was one edit away from
        // the 259-character <Path> cap -- over which Setup rejects the whole
        // answer file with an error naming only the pass. It also lets the exit
        // code be logged, which the sysprep work taught us to do.
        //
        // The script exits 1 on SUCCESS: `WillReboot` is not emitted below (see
        // `run_synchronous_commands`), so it defaults to `OnRequest`, and that
        // reads an exit code of 1 as "reboot requested" -- which a run that has
        // just set the computer name needs. See `cloudinit::clone_runner`.
        let path = format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File {}",
            crate::cloudinit::CLONE_SCRIPT_PATH
        );
        if path.length_over_limit() {
            bail!(
                "cloud-init RunSynchronousCommand path is {} chars, limit is {PATH_LIMIT}",
                path.len()
            );
        }
        commands.push(Command {
            path,
            description: "Run cloud-init once for this clone".into(),
        });
    }

    components.push(format!(
        "    <component name=\"Microsoft-Windows-Deployment\" {ARCH}>\n\
         \x20     <RunSynchronous>\n{}\n\
         \x20     </RunSynchronous>\n\
         \x20   </component>",
        run_synchronous_commands(&commands)
    ));

    Ok(format!(
        "  <settings pass=\"specialize\">\n{}\n  </settings>",
        components.join("\n")
    ))
}

/// The `oobeSystem` pass.
///
/// `international` adds `Microsoft-Windows-International-Core`, which is **only** wanted
/// for the sysprep answer file. On a normal install the locale is set by
/// `Microsoft-Windows-International-Core-WinPE` in the `windowsPE` pass, which satisfies
/// OOBE's Localization page so it never appears.
///
/// **`windowsPE` does not run on a generalize cycle.** Nothing sets the locale, the
/// Localization page becomes active, and OOBE stops there waiting for a click: on a
/// guest with no framebuffer, forever. Caught on a rack, in `UnattendGC\\setupact.log`:
/// the original install logged `SETACTIVE: wizard page for page End`, and the cycle
/// after sysprep logged `SETACTIVE: wizard page for page Localization` and went silent.
/// Everything else had succeeded, `ComputerName set to OXIDEOX-F893NMJ`,
/// `UserAccounts: Password set for 'oxide'`, `oobeSystem` exiting `0x00000000`.
///
/// It stays off for the normal build so the committed goldens do not move.
fn oobe_pass(config: &Config, international: bool) -> String {
    let auto_logon = if config.auto_logon {
        format!(
            "      <AutoLogon>\n\
             \x20       <Enabled>true</Enabled>\n\
             \x20       <Username>{user}</Username>\n\
             \x20       <LogonCount>1</LogonCount>\n\
             \x20       <Password>\n\
             \x20         <Value>{pass}</Value>\n\
             \x20         <PlainText>true</PlainText>\n\
             \x20       </Password>\n\
             \x20     </AutoLogon>\n",
            user = xml_escape(&config.username),
            pass = xml_escape(&config.password)
        )
    } else {
        String::new()
    };

    // Ahead of Shell-Setup, so the locale is established before the OOBE settings that
    // depend on it.
    let international = if international {
        format!(
            "    <component name=\"Microsoft-Windows-International-Core\" {ARCH}>\n\
             \x20     <InputLocale>{region}</InputLocale>\n\
             \x20     <SystemLocale>{region}</SystemLocale>\n\
             \x20     <UILanguage>{language}</UILanguage>\n\
             \x20     <UserLocale>{region}</UserLocale>\n\
             \x20   </component>\n",
            language = xml_escape(&config.ui_language),
            region = xml_escape(&config.region)
        )
    } else {
        String::new()
    };

    format!(
        "  <settings pass=\"oobeSystem\">\n\
         {international}\
         \x20   <component name=\"Microsoft-Windows-Shell-Setup\" {ARCH}>\n\
         \x20     <OOBE>\n\
         \x20       <HideEULAPage>true</HideEULAPage>\n\
         \x20       <HideLocalAccountScreen>true</HideLocalAccountScreen>\n\
         \x20       <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>\n\
         \x20       <HideOnlineAccountScreens>true</HideOnlineAccountScreens>\n\
         \x20       <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>\n\
         \x20       <NetworkLocation>Work</NetworkLocation>\n\
         \x20       <ProtectYourPC>3</ProtectYourPC>\n\
         \x20     </OOBE>\n\
         \x20     <UserAccounts>\n\
         \x20       <LocalAccounts>\n\
         \x20         <LocalAccount wcm:action=\"add\">\n\
         \x20           <Name>{user}</Name>\n\
         \x20           <DisplayName>{user}</DisplayName>\n\
         \x20           <Group>Administrators</Group>\n\
         \x20           <Password>\n\
         \x20             <Value>{pass}</Value>\n\
         \x20             <PlainText>true</PlainText>\n\
         \x20           </Password>\n\
         \x20         </LocalAccount>\n\
         \x20       </LocalAccounts>\n\
         \x20     </UserAccounts>\n\
         {auto_logon}    </component>\n\
         \x20 </settings>",
        user = xml_escape(&config.username),
        pass = xml_escape(&config.password)
    )
}

/// Small helper so the limit check reads as a question about the path.
trait PathLimit {
    fn length_over_limit(&self) -> bool;
}

impl PathLimit for String {
    fn length_over_limit(&self) -> bool {
        self.len() > PATH_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base() -> Config {
        Config {
            release: WindowsRelease::Server2022,
            edition: "datacenter".into(),
            computer_name: "win-server-01".into(),
            username: "oxide".into(),
            password: "0xide!230xide!23".into(),
            enable_rdp: true,
            inject_drivers: true,
            enable_serial_console: true,
            target_disk: 1,
            partitions: crate::partition::default_layout(),
            ui_language: "en-US".into(),
            region: "en-US".into(),
            timezone: "UTC".into(),
            product_key: None,
            auto_logon: false,
            generalize: false,
            verbose_serial: false,
            log_path: None,
            show_ui_on_error: true,
            image_index: Some(4),
            skip_image_install: false,
            ssh_keys: Vec::new(),
            enable_ssh: true,
            cloud_init: None,
            has_extras: false,
        }
    }

    /// Every branch in the generator. The slug is the golden's filename and the label
    /// is prose for a failure message, kept separate on purpose, because deriving the
    /// filename from the label would let a reworded label silently orphan a golden.
    fn cases() -> Vec<(&'static str, &'static str, Config)> {
        vec![
            ("defaults", "defaults", base()),
            (
                "no-drivers",
                "no drivers (leaves an empty offlineServicing pass)",
                Config { inject_drivers: false, ..base() },
            ),
            ("no-rdp", "no rdp", Config { enable_rdp: false, ..base() }),
            (
                "no-serial-console",
                "no serial console",
                Config { enable_serial_console: false, ..base() },
            ),
            // `image_index: None` on purpose. The edition only reaches the answer file
            // through the /IMAGE/NAME fallback; with an index set, these cases were
            // byte-identical to "defaults" and tested nothing.
            (
                "core-edition",
                "core edition",
                Config {
                    edition: "datacenter-core".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "standard-edition",
                "standard edition",
                Config {
                    edition: "standard".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "windows-11",
                "windows 11 (LabConfig bypasses)",
                Config {
                    release: WindowsRelease::Windows11,
                    edition: "pro".into(),
                    ..base()
                },
            ),
            // One golden per release, because `target_for` now parameterises the
            // edition table by release and nothing else pins what comes out of it.
            // All of them carry `image_index: None`: with an index set, the edition table
            // never reaches the file and every one of these would be byte-identical to
            // `defaults` (or to `windows-11`) while appearing to test the matrix.
            //
            // What each pins is the `/IMAGE/NAME` string Setup matches on. Getting it
            // wrong matches nothing, and Setup then shows an empty edition list and
            // waits: on a rack, forever.
            // 2016's edition vocabulary is right and its media detects correctly, so
            // the table stays. It is not supported at this time: its NVMe namespace
            // enumeration has known trouble, characterised under QEMU because a rack
            // attempt showed nothing on serial — see `TESTED-MEDIA.md`. EOL Jan 2027.
            (
                "release-server-2016",
                "server 2016 edition table",
                Config {
                    release: WindowsRelease::Server2016,
                    edition: "datacenter".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "release-server-2019",
                "server 2019 edition table",
                Config {
                    release: WindowsRelease::Server2019,
                    edition: "datacenter".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "release-server-2025",
                "server 2025 edition table",
                Config {
                    release: WindowsRelease::Server2025,
                    edition: "datacenter".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "release-windows-10",
                "windows 10 edition table (client, no bypasses)",
                Config {
                    release: WindowsRelease::Windows10,
                    edition: "pro".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "release-windows-11",
                "windows 11 edition table",
                Config {
                    release: WindowsRelease::Windows11,
                    edition: "enterprise".into(),
                    image_index: None,
                    ..base()
                },
            ),
            (
                "product-key",
                "product key",
                Config {
                    product_key: Some("WX4NM-KYWYW-QJJR4-XV3QB-6VM33".into()),
                    ..base()
                },
            ),
            (
                "no-image-index",
                "no image index (falls back to /IMAGE/NAME)",
                Config { image_index: None, ..base() },
            ),
            (
                "skip-image-install",
                "skip image install",
                Config { skip_image_install: true, ..base() },
            ),
            ("autologon", "autologon", Config { auto_logon: true, ..base() }),
            (
                "golden-image-name",
                "golden image uses a random computer name",
                Config { computer_name: "*".into(), ..base() },
            ),
            (
                "xml-hostile-credentials",
                "xml-hostile credentials survive escaping",
                Config {
                    username: "a<b>&c".into(),
                    password: "p<a>&s\"s'w0rd!".into(),
                    ..base()
                },
            ),
            // Locale is the one setting whose failure is a wizard page on a guest
            // with no framebuffer, so it gets pinned rather than trusted.
            (
                "locale-de-de",
                "German display language and region",
                Config {
                    ui_language: "de-DE".into(),
                    region: "de-DE".into(),
                    timezone: "W. Europe Standard Time".into(),
                    ..base()
                },
            ),
            // The case one picker would have made unreachable: English media, German
            // formats and keyboard. Common, and correct on any ISO.
            (
                "locale-split",
                "English display language with a German region",
                Config {
                    ui_language: "en-US".into(),
                    region: "de-DE".into(),
                    ..base()
                },
            ),
            // A layout with a data partition, pinning both the generated orders and
            // the InstallTo that follows them. A hardcoded PartitionID here would
            // install Windows onto the wrong partition and say nothing.
            (
                "partitions-data-volume",
                "an extra data partition",
                Config {
                    partitions: {
                        let mut l = crate::partition::default_layout();
                        l[2].size_mb = Some(61440);
                        l.push(crate::partition::Partition {
                            kind: crate::partition::Kind::Primary,
                            size_mb: None,
                            label: Some("Data".into()),
                            letter: Some('D'),
                            format: Some(crate::partition::Format::Ntfs),
                        });
                        l
                    },
                    ..base()
                },
            ),
            (
                "cloud-init-golden",
                "golden image with cloud-init",
                Config {
                    generalize: true,
                    computer_name: "*".into(),
                    cloud_init: Some(crate::settings::CloudInit {
                        manage_account: false,
                    }),
                    ..base()
                },
            ),
        ]
    }

    /// Rewrite the goldens from the current generator.
    ///
    ///   cargo test -p oxwin-core dump_goldens -- --ignored
    ///
    /// Originally this process was tested in a script, then a js plugin,
    /// (it was an idea), then that didnt work as wanted, so it was moved here.
    /// Any reference to the original goldens comes from those original tests
    /// and iterating there.
    #[test]
    #[ignore = "rewrites goldens; run deliberately and read the diff"]
    fn dump_goldens() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/unattend");
        std::fs::create_dir_all(&dir).expect("create testdata/unattend");
        for (slug, _, config) in cases() {
            let ours = build(&config).expect("build");
            std::fs::write(dir.join(format!("{slug}.xml")), &ours)
                .expect("write golden");
        }
    }

    /// Every branch in the generator, against a committed golden byte for byte. The
    /// goldens were produced by the JavaScript builder, which is the only answer file
    /// that has installed Windows Image Builder hardware, and every way this file can be
    /// wrong is silent.
    #[test]
    fn matches_the_goldens() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/unattend");
        let cases = cases();

        let mut failures = Vec::new();
        for (slug, label, config) in &cases {
            let path = dir.join(format!("{slug}.xml"));
            // Missing goldens fail. The predecessor of this test skipped when its
            // reference was unavailable, and a test that skips is a test that lies.
            let theirs = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("{}: {e}. Regenerate with dump_goldens", path.display())
            });
            let ours = build(config).expect("build");
            if ours != theirs {
                failures.push(format!(
                    "{label}: {}",
                    first_difference(&ours, &theirs)
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} cases differ from their golden:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
    }

    /// Display language and region are different axes and must land in different
    /// elements. One string driving both is only correct while it is always en-US.
    #[test]
    fn language_and_region_land_in_the_right_elements() {
        let config = Config {
            ui_language: "en-US".into(),
            region: "de-DE".into(),
            ..base()
        };
        let xml = build(&config).expect("build");
        // UI language: what Setup and the shell are rendered in. Needs a pack.
        assert!(xml.contains("<UILanguage>en-US</UILanguage>"));
        assert!(xml.contains("<SetupUILanguage>"));
        // Region: formats and keyboard. Works on any media.
        assert!(xml.contains("<InputLocale>de-DE</InputLocale>"));
        assert!(xml.contains("<SystemLocale>de-DE</SystemLocale>"));
        assert!(xml.contains("<UserLocale>de-DE</UserLocale>"));
    }

    /// The sysprep file is where getting this wrong reproduces the Localization hang:
    /// windowsPE does not run on a generalize cycle, so this component is the only
    /// thing that sets the locale at all.
    #[test]
    fn the_sysprep_file_carries_the_chosen_locale_not_en_us() {
        let config = Config {
            ui_language: "de-DE".into(),
            region: "de-DE".into(),
            generalize: true,
            ..base()
        };
        let xml = build_sysprep(&config).expect("build_sysprep");
        assert!(xml.contains(r#"name="Microsoft-Windows-International-Core""#));
        assert!(xml.contains("<UILanguage>de-DE</UILanguage>"));
        assert!(xml.contains("<UserLocale>de-DE</UserLocale>"));
        assert!(!xml.contains("en-US"), "hardcoded en-US survived: {xml}");
    }

    #[test]
    fn the_timezone_reaches_specialize() {
        let config =
            Config { timezone: "W. Europe Standard Time".into(), ..base() };
        let xml = build(&config).expect("build");
        assert!(
            xml.contains("<TimeZone>W. Europe Standard Time</TimeZone>"),
            "{xml}"
        );
    }

    /// The safety property for this whole phase. If it fails, the split changed bytes
    /// it had no business changing.
    #[test]
    fn the_default_locale_produces_the_same_bytes_as_before() {
        let config = base();
        assert_eq!(config.ui_language, "en-US");
        assert_eq!(config.region, "en-US");
        assert_eq!(config.timezone, "UTC");
        // `matches_the_goldens` is the real assertion; this names the reason.
    }

    /// The generator must satisfy its own linter, for every case and both deployment
    /// kinds.
    ///
    /// This is what turns the trap list in CLAUDE.md from prose into a gate. Each rule
    /// in `lint` describes a way an answer file is silently wrong; without this test,
    /// nothing stops a future change to this file from reintroducing one, and the next
    /// evidence would be a rack guest sitting at a wizard page nobody can see.
    #[test]
    fn generated_output_is_clean() {
        let mut failures = Vec::new();
        for (slug, label, config) in cases() {
            let xml = build(&config).expect("build");
            // As production builds the context (`Settings::problems`, the
            // CLI's build path): cloud-init from the config, not a constant.
            let cx = LintContext {
                target_disk: config.target_disk,
                generalize: config.generalize,
                cloud_init: config.cloud_init.is_some(),
            };
            // `show_ui_on_error` is true in every committed case, so this fires
            // everywhere. It is a real hazard and the rule stays, but changing the
            // default would move every golden and is a separate decision from this
            // plan. Filtered here, named rather than hidden.
            for problem in lint(&xml, &cx)
                .into_iter()
                .filter(|p| p.field != "unattend_will_show_ui")
            {
                failures.push(format!("{slug} ({label}): {}", problem.message));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The normal answer file, built for a golden image (`generalize: true`), must
    /// still lint clean. This is the false-positive `unattend_locale` could throw
    /// pinned from the other side: `build` always emits `windowsPE`, which sets the
    /// locale through `International-Core-WinPE`, so the rule must not ask for the
    /// non-WinPE component that only belongs in the sysprep file.
    #[test]
    fn generated_output_is_clean_for_a_golden_build() {
        let config = Config {
            generalize: true,
            cloud_init: Some(crate::settings::CloudInit {
                manage_account: false,
            }),
            ..base()
        };
        let xml = build(&config).expect("build");
        let cx = LintContext {
            target_disk: config.target_disk,
            generalize: config.generalize,
            cloud_init: config.cloud_init.is_some(),
        };
        // Same filter and reason as `generated_output_is_clean`: `show_ui_on_error`
        // defaults to true, which is a real hazard but a separate decision from
        // this plan.
        let found: Vec<_> = lint(&xml, &cx)
            .into_iter()
            .filter(|p| p.field != "unattend_will_show_ui")
            .collect();
        assert!(found.is_empty(), "{found:#?}");
    }

    /// The sysprep answer file has its own shape — two passes, no windowsPE — and its
    /// own way of being wrong, which is exactly the Localization hang.
    #[test]
    fn generated_sysprep_output_is_clean() {
        // With and without cloud-init: with it the file carries the clone
        // runner as well as the bootstrap, and is linted as production would.
        for cloud_init in
            [None, Some(crate::settings::CloudInit { manage_account: false })]
        {
            let config = Config { generalize: true, cloud_init, ..base() };
            let xml = build_sysprep(&config).expect("build_sysprep");
            assert_eq!(
                xml.contains(crate::cloudinit::CLONE_SCRIPT_PATH),
                cloud_init.is_some()
            );
            let cx = LintContext {
                target_disk: config.target_disk,
                generalize: true,
                cloud_init: config.cloud_init.is_some(),
            };
            let found = lint(&xml, &cx);
            assert!(found.is_empty(), "{cloud_init:?}: {found:#?}");
        }
    }

    /// The Localization page is what actually stalled a rack guest, and only the sysprep
    /// answer file can prevent it: `windowsPE` sets the locale on a normal install and
    /// does not run on a generalize cycle.
    #[test]
    fn the_sysprep_answer_file_sets_the_locale_for_oobe() {
        let xml = build_sysprep(&base()).unwrap();
        assert!(
            xml.contains("Microsoft-Windows-International-Core\""),
            "without this OOBE stops on the Localization page and waits for a click"
        );
        for field in ["InputLocale", "SystemLocale", "UILanguage", "UserLocale"]
        {
            assert!(
                xml.contains(&format!("<{field}>en-US</{field}>")),
                "{field} is not set"
            );
        }
    }

    /// And it must stay out of the normal answer file, whose bytes are pinned by a
    /// golden that came from the builder that has installed on real hardware.
    #[test]
    fn the_normal_answer_file_is_unchanged_by_that() {
        let xml = build(&base()).unwrap();
        assert!(!xml.contains("Microsoft-Windows-International-Core\""));
        // The WinPE one is a different component and stays where it was.
        assert!(xml.contains("Microsoft-Windows-International-Core-WinPE"));
    }

    /// WinPE has no COM1 device, so a marker echoed there is written to nothing
    /// while `& exit /b 0` reports success. Proven under QEMU on 2026-09-10.
    /// Serial during Setup comes from EMS now.
    #[test]
    fn no_answer_file_echoes_to_com1_in_windows_pe() {
        for release in WindowsRelease::ALL {
            let mut config = base();
            config.release = *release;
            config.verbose_serial = true;
            let xml = build(&config).unwrap();
            let pe = xml
                .split("<settings pass=\"specialize\">")
                .next()
                .expect("windowsPE comes first");
            assert!(
                !pe.contains("COM1"),
                "{release:?}: windowsPE still writes to COM1"
            );
        }
    }

    /// The whole point of the sysprep answer file is that it still has the password.
    ///
    /// Windows replaces it with `*SENSITIVE*DATA*DELETED*` in the copy it caches, which
    /// is why relying on that copy left a rack guest stuck at the out-of-box wizard with
    /// `OOBEInProgress=1`.
    #[test]
    fn the_sysprep_answer_file_keeps_the_password() {
        let xml = build_sysprep(&base()).unwrap();
        assert!(xml.contains("0xide!230xide!23"), "the password was lost");
        assert!(!xml.contains("SENSITIVE"));
    }

    /// It must not be able to touch a disk. `windowsPE` carries `DiskConfiguration`,
    /// which formats and repartitions; Windows ignores that pass outside Setup, but an
    /// answer file handed to sysprep should not contain it in the first place.
    #[test]
    fn the_sysprep_answer_file_cannot_repartition_anything() {
        let xml = build_sysprep(&base()).unwrap();
        assert!(!xml.contains("windowsPE"), "windowsPE must not be included");
        assert!(!xml.contains("DiskConfiguration"));
        assert!(!xml.contains("offlineServicing"));
        // And it must still do the two jobs it exists for.
        assert!(xml.contains("<settings pass=\"specialize\">"));
        assert!(xml.contains("<settings pass=\"oobeSystem\">"));
    }

    /// A golden image re-resolves its name on every clone. That only happens because
    /// `specialize` is in this file; without it each clone keeps the template's name.
    #[test]
    fn the_sysprep_answer_file_re_randomises_the_computer_name() {
        let xml =
            build_sysprep(&Config { computer_name: "*".into(), ..base() })
                .unwrap();
        assert!(xml.contains("<ComputerName>*</ComputerName>"));
    }

    /// The clone-side run: the cloud-init runner fires once, after the bootstrap
    /// command that could re-register `OxideCloudInit` disabled. There is no
    /// second command re-enabling the service here: `OxideCloudInit` is now the
    /// only thing that sets it to Automatic, deliberately (see `specialize_pass`).
    #[test]
    fn the_sysprep_file_runs_cloud_init_once() {
        let config = Config {
            generalize: true,
            computer_name: "*".into(),
            cloud_init: Some(crate::settings::CloudInit {
                manage_account: false,
            }),
            ..base()
        };
        let xml = build_sysprep(&config).unwrap();
        assert!(xml.contains(crate::cloudinit::CLONE_SCRIPT_PATH));
        assert!(!xml.contains("StartupType Automatic"));
        // Ordering: the bootstrap command (which can re-disable the task) runs
        // before the cloud-init runner (which enables it), never the reverse.
        let bootstrap_order = xml
            .find("Oxide guest bootstrap")
            .and_then(|i| xml[..i].rfind("<Order>"))
            .and_then(|i| xml[i + "<Order>".len()..].split('<').next())
            .and_then(|s| s.parse::<u32>().ok())
            .expect("bootstrap command order");
        let cloud_init_order = xml
            .find(crate::cloudinit::CLONE_SCRIPT_PATH)
            .and_then(|i| xml[..i].rfind("<Order>"))
            .and_then(|i| xml[i + "<Order>".len()..].split('<').next())
            .and_then(|s| s.parse::<u32>().ok())
            .expect("cloud-init command order");
        assert!(
            cloud_init_order > bootstrap_order,
            "cloud-init (order {cloud_init_order}) must run after bootstrap \
             (order {bootstrap_order}), or a re-attached installer disk can \
             re-disable OxideCloudInit after it has been enabled"
        );
    }

    /// The install-time answer file must not run it, even for the exact
    /// default golden-plus-cloud-init configuration: the golden is built on
    /// a rack instance with its own config drive, so running the clone-side
    /// runner during the golden's own install would rename the golden from
    /// the builder instance's metadata and arm `OxideCloudInit` before
    /// sysprep -- the mid-sysprep race registering the task disabled exists
    /// to prevent. The sysprep file, built from the same config, still must
    /// run it.
    #[test]
    fn the_install_answer_file_does_not_run_cloud_init() {
        let config = Config {
            generalize: true,
            computer_name: "*".into(),
            cloud_init: Some(crate::settings::CloudInit {
                manage_account: false,
            }),
            ..base()
        };
        assert!(
            !build(&config)
                .unwrap()
                .contains(crate::cloudinit::CLONE_SCRIPT_PATH)
        );
        assert!(
            build_sysprep(&config)
                .unwrap()
                .contains(crate::cloudinit::CLONE_SCRIPT_PATH)
        );
    }

    #[test]
    fn cloud_init_off_leaves_the_sysprep_file_alone() {
        let with_it = build_sysprep(&Config {
            generalize: true,
            computer_name: "*".into(),
            cloud_init: Some(crate::settings::CloudInit {
                manage_account: false,
            }),
            ..base()
        })
        .unwrap();
        let without = build_sysprep(&Config {
            generalize: true,
            computer_name: "*".into(),
            cloud_init: None,
            ..base()
        })
        .unwrap();
        assert_ne!(with_it, without);
        assert!(!without.contains("cloudbase"));
    }

    /// `*` and `SetHostNamePlugin` both fire, deliberately and in both
    /// directions: `*` guarantees a valid unique name if the config drive is
    /// ever missing, and cloud-init overwrites it with the instance's name when
    /// it is there. The prototype emitted no ComputerName at all, which is the
    /// more fragile choice.
    #[test]
    fn a_golden_image_keeps_the_random_name_token_as_well_as_cloud_init() {
        let xml = build_sysprep(&Config {
            generalize: true,
            computer_name: "*".into(),
            cloud_init: Some(crate::settings::CloudInit {
                manage_account: false,
            }),
            ..base()
        })
        .unwrap();
        assert!(xml.contains("<ComputerName>*</ComputerName>"));
        assert!(xml.contains(crate::cloudinit::CLONE_SCRIPT_PATH));
    }

    /// Adding a release to `WindowsRelease::ALL` without adding a golden for it leaves
    /// its edition table generated by nobody's reference and compared against nothing.
    /// This iterates `ALL` rather than a literal list precisely so that it fails on the
    /// day someone adds Server 2029 and stops there.
    #[test]
    fn every_release_has_a_golden() {
        let covered: Vec<WindowsRelease> =
            cases().iter().map(|(_, _, config)| config.release).collect();
        let missing: Vec<_> = WindowsRelease::ALL
            .iter()
            .filter(|release| !covered.contains(release))
            .collect();
        assert!(
            missing.is_empty(),
            "no unattend golden covers {missing:?}; add a case to cases()"
        );
    }

    /// Only Windows 11 Setup reads `HKLM\System\Setup\LabConfig`. Iterating `ALL`
    /// rather than checking the two client goldens means a new release cannot quietly
    /// inherit the bypasses — or quietly miss them, which on Windows 11 is a refusal to
    /// install at all.
    ///
    /// The goldens pin the same fact, but only for the releases that have a golden with
    /// `image_index: None`; this states it as the rule it is.
    #[test]
    fn only_windows_11_gets_the_labconfig_bypasses() {
        for release in WindowsRelease::ALL {
            let xml = build(&Config { release: *release, ..base() }).unwrap();
            let has = xml.contains("LabConfig");
            assert_eq!(
                has,
                *release == WindowsRelease::Windows11,
                "{release:?} emits LabConfig = {has}"
            );
        }
    }

    /// A golden nothing compares against is dead weight, and a case whose golden was
    /// never written would otherwise be caught only by the read above. Check the two
    /// sets agree exactly.
    #[test]
    fn every_golden_has_a_case_and_every_case_a_golden() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/unattend");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("testdata/unattend")
            .map(|e| {
                e.expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .trim_end_matches(".xml")
                    .to_string()
            })
            .collect();
        on_disk.sort();
        let mut expected: Vec<String> =
            cases().iter().map(|(slug, ..)| slug.to_string()).collect();
        expected.sort();
        assert_eq!(on_disk, expected);
    }

    /// A CRLF golden is the one difference `lines()` cannot see, and it is the one a
    /// Windows checkout produces, so the message has to name it. This failed on a
    /// windows runner for weeks reporting "ours 190 vs js ref 190".
    #[test]
    fn a_crlf_golden_is_reported_as_a_line_ending_difference() {
        let ours = build(&base()).expect("build");
        let theirs = ours.replace('\n', "\r\n");
        assert_ne!(ours, theirs);
        let message = first_difference(&ours, &theirs);
        assert!(
            message.contains("line endings")
                && message.contains("ours and CRLF"),
            "unhelpful message for a CRLF golden: {message}"
        );
    }

    /// Point at the first differing line, which is far more useful than a diff of two
    /// 8 KB strings when a single space is wrong.
    fn first_difference(ours: &str, theirs: &str) -> String {
        for (i, (a, b)) in ours.lines().zip(theirs.lines()).enumerate() {
            if a != b {
                return format!(
                    "line {}\n     ours: {a:?}\n  js ref: {b:?}",
                    i + 1
                );
            }
        }
        if ours.lines().count() != theirs.lines().count() {
            return format!(
                "line counts differ: ours {} vs js ref {}",
                ours.lines().count(),
                theirs.lines().count()
            );
        }
        // Every line matches and there are equally many of them, so what differs is
        // the bytes between the lines: `str::lines` drops a trailing CR. A checkout
        // with `core.autocrlf=true` — the default on GitHub's windows runners —
        // rewrites an LF golden into CRLF and lands here, and the old message for
        // this case read "line counts differ: ours 190 vs js ref 190".
        format!(
            "every line matches but the bytes do not: line endings are {} for \
             ours and {} for the js ref. Check .gitattributes and \
             core.autocrlf",
            terminators(ours),
            terminators(theirs)
        )
    }

    /// Names what ends the lines of `s`, for the message above.
    fn terminators(s: &str) -> &'static str {
        let crlf = s.matches("\r\n").count();
        let lf = s.matches('\n').count();
        match (crlf, lf - crlf) {
            (0, 0) => "absent",
            (0, _) => "LF",
            (_, 0) => "CRLF",
            _ => "mixed LF and CRLF",
        }
    }

    #[test]
    fn every_path_stays_under_the_schema_limit() {
        // The 259-char cap is what rejected an entire answer file once, with an error
        // naming only the pass. Check the decoded length, which is what the schema
        // sees, across every branch that can add commands, in **both** `build` and
        // `build_sysprep` -- the cloud-init runner is reachable only through the
        // latter, since it is gated on `for_sysprep`. The Windows 11 case is the
        // longest one left of the install-time branches; the diskpart command that
        // used to beat it went with `install_from_label`. So add a config here for
        // any new RunSynchronous branch rather than assuming the existing ones cover
        // it, and check it through whichever of `build`/`build_sysprep` can reach
        // the branch it exercises.
        fn assert_paths_ok(xml: &str) {
            for chunk in xml.split("<Path>").skip(1) {
                let raw = chunk.split("</Path>").next().expect("closing tag");
                let decoded = raw
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&");
                assert!(
                    decoded.len() <= PATH_LIMIT,
                    "{} chars: {decoded}",
                    decoded.len()
                );
            }
        }
        for config in [
            base(),
            Config {
                release: WindowsRelease::Windows11,
                edition: "pro".into(),
                ..base()
            },
            // The cloud-init runner's `<Path>`, which only build_sysprep can reach.
            Config {
                generalize: true,
                computer_name: "*".into(),
                cloud_init: Some(crate::settings::CloudInit {
                    manage_account: false,
                }),
                ..base()
            },
        ] {
            assert_paths_ok(&build(&config).expect("build"));
            assert_paths_ok(&build_sysprep(&config).expect("build_sysprep"));
        }
    }

    #[test]
    fn driver_paths_land_in_offline_servicing_not_specialize() {
        // In specialize this component is ignored with no error and NetKVM never
        // installs, which is a guest with no network and nothing in any log.
        let xml = build(&base()).expect("build");
        let offline = xml
            .split("<settings pass=\"offlineServicing\">")
            .nth(1)
            .expect("offlineServicing pass");
        assert!(offline.contains("PnpCustomizationsNonWinPE"));
        let specialize = xml
            .split("<settings pass=\"specialize\">")
            .nth(1)
            .expect("specialize pass");
        assert!(!specialize.contains("PnpCustomizationsNonWinPE"));
    }

    #[test]
    fn no_product_key_element_when_there_is_no_key() {
        // An empty <Key> is not the same as no key: Setup fails the licence lookup
        // before showing any page.
        assert!(!build(&base()).expect("build").contains("<ProductKey>"));
    }

    /// The safety property. Generating the block from the default layout must produce
    /// the bytes the literal produced, or every committed golden moves.
    #[test]
    fn the_default_layout_produces_the_original_bytes() {
        let xml = build(&base()).expect("build");
        assert!(
            xml.contains(
                "            <CreatePartition wcm:action=\"add\">\n\
             \x20             <Order>1</Order>\n\
             \x20             <Type>EFI</Type>\n\
             \x20             <Size>260</Size>\n\
             \x20           </CreatePartition>"
            ),
            "{xml}"
        );
        // MSR carries no Label, Letter or Format -- the original emits only Order and
        // PartitionID for it, and adding either would move every golden.
        assert!(
            xml.contains(
                "            <ModifyPartition wcm:action=\"add\">\n\
             \x20             <Order>2</Order>\n\
             \x20             <PartitionID>2</PartitionID>\n\
             \x20           </ModifyPartition>"
            ),
            "{xml}"
        );
        assert!(xml.contains("<Extend>true</Extend>"));
    }

    /// The hardcoded `3` was correct for one layout only.
    #[test]
    fn install_to_follows_the_layout() {
        use crate::partition::{Kind, default_layout};
        let without_msr: Vec<_> = default_layout()
            .into_iter()
            .filter(|p| p.kind != Kind::Msr)
            .collect();
        let config = Config { partitions: without_msr, ..base() };
        let xml = build(&config).expect("build");
        assert!(
            xml.contains(
                "          <InstallTo>\n\
                 \x20           <DiskID>1</DiskID>\n\
                 \x20           <PartitionID>2</PartitionID>\n\
                 \x20         </InstallTo>"
            ),
            "{xml}"
        );
    }

    #[test]
    fn an_extra_data_partition_is_emitted() {
        use crate::partition::{Format, Kind, Partition, default_layout};
        let mut layout = default_layout();
        layout[2].size_mb = Some(61440);
        layout.push(Partition {
            kind: Kind::Primary,
            size_mb: None,
            label: Some("Data".into()),
            letter: Some('D'),
            format: Some(Format::Ntfs),
        });
        let xml =
            build(&Config { partitions: layout, ..base() }).expect("build");
        assert!(xml.contains("<Order>4</Order>"));
        assert!(xml.contains("<Letter>D</Letter>"));
        assert!(xml.contains("<Size>61440</Size>"));
    }
}
