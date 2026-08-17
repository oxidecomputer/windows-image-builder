// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The whole image, assembled.
//!
//! Everything underneath it: [`crate::exfat`] and [`crate::fat32`]
//! for the two volumes, [`crate::mbr`] for the partition table, [`crate::unattend`] and
//! [`crate::bootstrap`] for what Setup reads, [`crate::wim`] for choosing the edition,
//! and [`crate::udf`] for reading the ISO without mounting it.
//!
//! The layout, and why it is this shape:
//!
//! ```text
//!   p1  exFAT  the entire Windows media, install.wim copied verbatim,
//!              plus autounattend.xml, drivers and OpenSSH
//!   p2  FAT32  64 MiB at the end: UEFI Shell + startup.nsh chooser + UEFI:NTFS
//! ```
//!
//! Windows Setup insists `install.wim` sit on the volume it booted from — point
//! `<InstallFrom><Path>` at a second partition and it fails to resolve the licence
//! terms, and omit the path and it looks only on the boot volume. So the whole media has
//! to be one volume, and since `install.wim` is over FAT32's 4 GiB file limit, that
//! volume cannot be FAT32. UEFI firmware only guarantees a FAT driver, which is the
//! usual dead end; Rufus's UEFI:NTFS solves it with a tiny FAT partition whose
//! bootloader loads a filesystem driver for the big one.
//!
//! There is only an exFAT path. The builder this was ported from also had a FAT32
//! diagnostic mode that replaced `install.wim` with `wimlib`-split `.swm` parts; it was
//! not carried over because it needs an external tool, it answered a question `ei.cfg`
//! has since answered, and no media anyone booted came out of it.

use crate::bootstrap;
use crate::exfat;
use crate::fat32;
use crate::mbr;
use crate::progress::{Event, Reporter};
use crate::udf::{Entry, UdfImage};
use crate::unattend::{self, Config, VOLUME_LABEL};
use crate::wim;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SECTOR: u64 = crate::mbr::SECTOR as u64;
const GIB: u64 = 1024 * 1024 * 1024;

/// p2 is the partition the firmware boots: the UEFI Shell, a chooser script and the
/// UEFI:NTFS bootloader. The shell alone is ~1.1 MiB, and FAT32 needs at least 65525
/// clusters, so 64 MiB is the practical floor. The slack is all zeros and the importer
/// skips zero blocks, so it costs nothing to upload.
pub const BOOT_PART_SECTORS: u32 = 131_072;

/// Where the media comes from: an ISO read directly, or a directory it is mounted at.
///
/// The directory case is not legacy: it is how an ISO someone has already mounted, or a
/// tree they have edited by hand, gets built without being repacked first.
pub enum Media {
    Iso(PathBuf),
    Directory(PathBuf),
}

pub struct Request {
    pub media: Media,
    pub out: PathBuf,
    /// Everything Setup and the bootstrap script are told.
    pub config: Config,
    /// Overrides `config.edition` when choosing from the WIM's own image list.
    pub edition_hint: Option<String>,
    /// `None` derives it from the chosen image's `EDITIONID`; `Some("none")` writes no
    /// `ei.cfg` at all.
    pub ei_channel: Option<String>,
    /// Copy the media and nothing else: no answer file, no `ei.cfg`, no drivers. The
    /// control for "is the exFAT volume itself the problem", since a bare build differs
    /// from the vendor ISO only in filesystem and boot device.
    pub bare: bool,
    /// Where the third-party payload comes from: compiled in, or a directory.
    pub assets: crate::assets::Assets,
}

/// What the build produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub path: PathBuf,
    pub total_bytes: u64,
    /// Bytes of `install.wim` streamed in, which is what dominates the wall clock.
    pub copied_bytes: u64,
    pub image_index: u32,
    pub edition_id: String,
    pub media_files: usize,
}

/// The disk layout, in sectors. Separated out because it is arithmetic worth testing on
/// its own: every value here has a rule behind it that an Oxide rack enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub p1_start: u32,
    pub p1_sectors: u32,
    pub p2_start: u32,
    pub p2_sectors: u32,
    pub total_sectors: u64,
}

impl Layout {
    pub fn total_bytes(&self) -> u64 {
        self.total_sectors * SECTOR
    }

    /// Zero padding added to satisfy the whole-GiB rule.
    pub fn padding_bytes(&self) -> u64 {
        (self.total_sectors - (self.p2_start as u64 + self.p2_sectors as u64))
            * SECTOR
    }
}

/// Size the disk for `payload_bytes` of media plus image.
///
/// Two roundings, both of which an Oxide rack requires and neither of which the natural
/// layout produces:
///
/// - **p1 gets 10% headroom, rounded up to a whole GiB.** The headroom covers
///   filesystem metadata and the slack from 128 KiB clusters across ~900 mostly-small
///   files. Without it the volume fills and the build fails *after* the 4 GiB copy.
/// - **The whole image is rounded up to a whole GiB.** Oxide disks must be at least
///   1 GiB and a whole multiple of it; a 6 GiB volume plus the 1 MiB MBR gap and the
///   64 MiB boot partition is 6.064 GiB, which Nexus rejects. Padding here means the
///   artefact uploads as it is, and the trailing zeros cost nothing to transfer because
///   the importer skips all-zero blocks.
pub fn plan(payload_bytes: u64) -> Result<Layout> {
    let p1_start: u32 = 2048;
    // Deliberately f64. Integer arithmetic would be tidier and would round differently
    // wherever 1.1 is not exact, which would change the volume size — and the goldens,
    // and every image anyone has already booted. Left alone on purpose.
    let p1_gib = ((payload_bytes as f64 * 1.1) / GIB as f64).ceil();
    let p1_sectors = u32::try_from((p1_gib as u64) * GIB / SECTOR)
        .map_err(|_| anyhow::anyhow!("media volume is too large to address"))?;
    let p2_start = p1_start.checked_add(p1_sectors).ok_or_else(|| {
        anyhow::anyhow!("media volume is too large to address")
    })?;
    let content_sectors = p2_start as u64 + BOOT_PART_SECTORS as u64;
    let total_sectors = (content_sectors * SECTOR).div_ceil(GIB) * GIB / SECTOR;
    Ok(Layout {
        p1_start,
        p1_sectors,
        p2_start,
        p2_sectors: BOOT_PART_SECTORS,
        total_sectors,
    })
}

/// One file to copy onto the media volume.
struct MediaFile {
    /// Path as it will appear on the volume, with a leading slash.
    volume_path: String,
    size: u64,
    origin: Origin,
}

enum Origin {
    Host(PathBuf),
    Udf(Entry),
}

/// The media, opened.
enum Source {
    Iso(Box<UdfImage>),
    Directory(PathBuf),
}

impl Source {
    fn open(media: &Media) -> Result<Self> {
        match media {
            Media::Iso(path) => Ok(Self::Iso(Box::new(
                UdfImage::open(path)
                    .with_context(|| format!("reading {}", path.display()))?,
            ))),
            Media::Directory(path) => {
                if !path.is_dir() {
                    bail!("{} is not a directory", path.display());
                }
                Ok(Self::Directory(path.clone()))
            }
        }
    }

    /// Every file on the media, sorted by the path it will have on the volume.
    ///
    /// Sorted because the order decides the order clusters are handed out in, and so
    /// decides the bytes of the volume. `read_dir` and a UDF directory both return
    /// whatever order the source happens to hold, which is not the same order on a
    /// mounted ISO, an APFS copy and a Linux host. The sort key is the path the file
    /// will have on the volume, which is the only thing stable across all three.
    fn list(&mut self) -> Result<Vec<MediaFile>> {
        let mut files = match self {
            Self::Iso(udf) => udf
                .walk()?
                .into_iter()
                .filter(|e| !e.is_dir)
                .map(|e| MediaFile {
                    volume_path: format!("/{}", e.path),
                    size: e.size,
                    origin: Origin::Udf(e),
                })
                .collect(),
            Self::Directory(root) => {
                let mut out = Vec::new();
                walk_host(root, root, &mut out)?;
                out
            }
        };
        files.retain(|f| {
            let name = f.volume_path.rsplit('/').next().unwrap_or_default();
            // install.wim is added separately, by size, so its 4 GiB is never held in
            // memory. AppleDouble sidecars are macOS bookkeeping and not part of the
            // media; filtered here rather than per-source so the ISO and the mount
            // cannot disagree about what the media contains.
            !name.eq_ignore_ascii_case("install.wim") && !name.starts_with("._")
        });
        files.sort_by(|a, b| a.volume_path.cmp(&b.volume_path));
        Ok(files)
    }

    fn find(&mut self, volume_path: &str) -> Result<MediaFile> {
        match self {
            Self::Iso(udf) => {
                let want = volume_path.trim_start_matches('/');
                let entry = udf
                    .walk()?
                    .into_iter()
                    .find(|e| !e.is_dir && e.path.eq_ignore_ascii_case(want))
                    .with_context(|| {
                        format!("{volume_path} is not on this media")
                    })?;
                Ok(MediaFile {
                    volume_path: volume_path.to_string(),
                    size: entry.size,
                    origin: Origin::Udf(entry),
                })
            }
            Self::Directory(root) => {
                let path = root.join(volume_path.trim_start_matches('/'));
                let size = std::fs::metadata(&path)
                    .with_context(|| {
                        format!("{} is not on this media", path.display())
                    })?
                    .len();
                Ok(MediaFile {
                    volume_path: volume_path.to_string(),
                    size,
                    origin: Origin::Host(path),
                })
            }
        }
    }

    fn read(&mut self, file: &MediaFile) -> Result<Vec<u8>> {
        match (&mut *self, &file.origin) {
            (Self::Iso(udf), Origin::Udf(entry)) => udf.read_file(entry),
            (_, Origin::Host(path)) => std::fs::read(path)
                .with_context(|| format!("reading {}", path.display())),
            _ => bail!("{} does not belong to this media", file.volume_path),
        }
    }

    fn read_range(
        &mut self,
        file: &MediaFile,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        match (&mut *self, &file.origin) {
            (Self::Iso(udf), Origin::Udf(entry)) => {
                udf.read_range(entry, offset, len)
            }
            (_, Origin::Host(path)) => {
                let mut f = File::open(path)?;
                f.seek(SeekFrom::Start(offset))?;
                let mut buf = vec![0u8; len];
                let mut done = 0usize;
                while done < len {
                    match f.read(&mut buf[done..])? {
                        0 => break,
                        n => done += n,
                    }
                }
                buf.truncate(done);
                Ok(buf)
            }
            _ => bail!("{} does not belong to this media", file.volume_path),
        }
    }

    /// Stream a file into `out` at `at`, reporting bytes copied as it goes.
    fn stream_into(
        &mut self,
        file: &MediaFile,
        out: &mut File,
        at: u64,
        mut progress: impl FnMut(u64),
    ) -> Result<u64> {
        match (&mut *self, &file.origin) {
            (Self::Iso(udf), Origin::Udf(entry)) => {
                out.seek(SeekFrom::Start(at))?;
                udf.copy_file(entry, out, progress)
            }
            (_, Origin::Host(path)) => {
                let mut src = File::open(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                out.seek(SeekFrom::Start(at))?;
                let mut buf = vec![0u8; 32 * 1024 * 1024];
                let mut done = 0u64;
                loop {
                    let n = src.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n])?;
                    done += n as u64;
                    progress(done);
                }
                Ok(done)
            }
            _ => bail!("{} does not belong to this media", file.volume_path),
        }
    }
}

fn walk_host(dir: &Path, base: &Path, out: &mut Vec<MediaFile>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        // Unreadable entries are skipped rather than fatal: a mounted ISO can carry
        // entries stat(2) refuses, and none of them are media.
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            walk_host(&path, base, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .expect("walked from base")
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push(MediaFile {
                volume_path: format!("/{rel}"),
                size: meta.len(),
                origin: Origin::Host(path),
            });
        }
    }
    Ok(())
}

/// One driver or payload file from `payload-manifest.json`.
#[derive(Debug, serde::Deserialize)]
struct ManifestEntry {
    driver: String,
    name: String,
    path: String,
}

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    #[serde(rename = "openSsh")]
    open_ssh: String,
    drivers: std::collections::BTreeMap<String, Vec<ManifestEntry>>,
}

pub fn build(
    request: &Request,
    reporter: &Reporter,
    cancel: &crate::engine::Cancel,
) -> Result<Output> {
    let result = assemble(request, reporter, cancel);
    if result.is_err() {
        // A half-written image that looks like a finished one is worse than no image,
        // and the cancel path lands here too.
        let _ = std::fs::remove_file(&request.out);
    }
    result
}

fn assemble(
    request: &Request,
    reporter: &Reporter,
    cancel: &crate::engine::Cancel,
) -> Result<Output> {
    let mut source = Source::open(&request.media)?;

    // --- pick the edition from the WIM's own metadata ----------------------
    let wim_file = source.find("/sources/install.wim")?;
    let wim_size = wim_file.size;
    let images = {
        let mut read =
            |offset: u64, len: usize| source.read_range(&wim_file, offset, len);
        wim::read_images(&mut read).context("reading the WIM image list")?
    };
    let hint =
        request.edition_hint.clone().unwrap_or(request.config.edition.clone());
    let chosen = wim::select_image(&images, &hint).cloned();
    reporter.log("editions in this media:".to_string());
    for image in &images {
        let mark = if Some(image.index) == chosen.as_ref().map(|c| c.index) {
            "->"
        } else {
            "  "
        };
        reporter.log(format!(
            "  {mark} [{}] {}  ({})",
            image.index, image.name, image.edition_id
        ));
    }
    let Some(chosen) = chosen else {
        bail!(
            "no edition matched {hint:?}; pass an index, an EDITIONID, or part of a name"
        );
    };

    let mut config = request.config.clone();
    config.image_index = Some(chosen.index);
    // Evaluation media rejects retail and KMS keys, and supplying one makes Setup
    // filter every image out of the edition list.
    if chosen.edition_id.to_lowercase().contains("eval")
        && config.product_key.is_some()
    {
        reporter.log("  (evaluation media: dropping product key)".to_string());
        config.product_key = None;
    }

    // --- lay out the disk --------------------------------------------------
    let media_files = source.list()?;
    let media_bytes: u64 = media_files.iter().map(|f| f.size).sum();
    let layout = plan(media_bytes + wim_size)?;

    reporter.phase(
        "layout",
        format!("{} GiB image, exfat media volume", layout.total_bytes() / GIB),
    );
    reporter.log(format!(
        "p1 exFAT @{} {} GiB — full media",
        layout.p1_start,
        layout.p1_sectors as u64 * SECTOR / GIB
    ));
    reporter.log(format!(
        "p2 FAT32 @{} 64 MiB — UEFI Shell + chooser + UEFI:NTFS",
        layout.p2_start
    ));
    reporter.log(format!(
        "total {} GiB -> {} ({} MiB zero padding for Oxide's whole-GiB rule)",
        layout.total_bytes() / GIB,
        request.out.display(),
        layout.padding_bytes() / 1024 / 1024
    ));

    // --- the media volume --------------------------------------------------
    let mut p1 = exfat::ExFatBuilder::new(exfat::Options {
        size_bytes: layout.p1_sectors as u64 * SECTOR,
        label: VOLUME_LABEL.to_string(),
        partition_offset_lba: layout.p1_start as u64,
        ..exfat::Options::default()
    })?;

    for file in &media_files {
        cancel.check()?;
        let data = source.read(file)?;
        p1.add_file(&file.volume_path, data)?;
    }
    // Reserved, not loaded: a 4 GiB buffer plus the image holding it would need 8 GiB.
    p1.reserve_file("/sources/install.wim", wim_size)?;

    if !request.bare {
        p1.add_file(
            "/autounattend.xml",
            unattend::build(&config)?.into_bytes(),
        )?;
        p1.add_file(
            "/setup/bootstrap.ps1",
            bootstrap::build(&config).into_bytes(),
        )?;
    }

    // Setup resolves the EULA inside install.wim by edition and channel. With no
    // ei.cfg it guesses, and on evaluation media the guess misses:
    //   setuperr.log: Callback_License_LoadLicenseText: SkuGetImageEulaAsString
    //                 Failed to get the EULA; hr = 0x80070490
    // which surfaces as "Windows cannot find the Microsoft Software License Terms".
    // The licence lives at Licenses/<lang>/<Channel>/<EditionID>/license.rtf inside the
    // image, and evaluation media uses the "Eval" channel, not Retail — getting that
    // wrong is exactly what produced the error above.
    let default_channel = if chosen.edition_id.to_lowercase().ends_with("eval")
    {
        "Eval"
    } else {
        "Retail"
    };
    let channel =
        request.ei_channel.clone().unwrap_or(default_channel.to_string());
    if channel != "none" {
        let ei = format!(
            "[EditionID]\r\n{}\r\n[Channel]\r\n{channel}\r\n[VL]\r\n0\r\n",
            chosen.edition_id
        );
        p1.add_file("/sources/ei.cfg", ei.into_bytes())?;
        reporter.log(format!(
            "ei.cfg: EditionID={} Channel={channel}",
            chosen.edition_id
        ));
    }

    let manifest: Manifest =
        serde_json::from_slice(&request.assets.manifest()?)
            .context("parsing the payload manifest")?;
    let driver_dir = match request.config.release {
        crate::settings::WindowsRelease::Windows11 => "w11",
        _ => "2k22",
    };
    let drivers = manifest.drivers.get(driver_dir).with_context(|| {
        format!("the payload manifest has no drivers for {driver_dir}")
    })?;
    for entry in drivers {
        let data = request.assets.read(&entry.path)?.into_owned();
        p1.add_file(
            &format!("/drivers/{}/{}", entry.driver, entry.name),
            data,
        )?;
    }
    if config.enable_ssh {
        let data = request.assets.read(&manifest.open_ssh)?.into_owned();
        p1.add_file("/openssh/OpenSSH-Win64.zip", data)?;
    }

    cancel.check()?;
    let p1_image = p1.build()?;
    reporter.phase(
        "media",
        format!(
            "{} media files + unattend, drivers, OpenSSH",
            media_files.len()
        ),
    );

    // --- compose -----------------------------------------------------------
    let mut out = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&request.out)
        .with_context(|| format!("creating {}", request.out.display()))?;
    out.set_len(layout.total_bytes())?;

    let volume_base = layout.p1_start as u64 * SECTOR;
    for chunk in p1_image.non_zero_blocks() {
        out.seek(SeekFrom::Start(volume_base + chunk.offset))?;
        out.write_all(chunk.data)?;
    }
    drop(p1_image);

    let p2_image = boot_partition(&request.assets)?;
    for chunk in p2_image.non_zero_blocks() {
        out.seek(SeekFrom::Start(
            layout.p2_start as u64 * SECTOR + chunk.offset,
        ))?;
        out.write_all(chunk.data)?;
    }
    drop(p2_image);
    reporter.phase("boot", "UEFI Shell + startup.nsh chooser + UEFI:NTFS");

    // p2 is the bootable one: the firmware can only read FAT, so the shell is the entry
    // point and the chooser decides between the installed OS and Setup.
    let table = mbr::boot_sector(&[
        mbr::Partition {
            bootable: false,
            kind: mbr::kind::EXFAT,
            start_lba: layout.p1_start,
            sectors: layout.p1_sectors,
        },
        mbr::Partition {
            bootable: true,
            kind: mbr::kind::ESP,
            start_lba: layout.p2_start,
            sectors: layout.p2_sectors,
        },
    ]);
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&table)?;

    // --- stream install.wim into its reserved clusters ---------------------
    let placement = p1
        .placements()
        .iter()
        .find(|p| p.path.eq_ignore_ascii_case("/sources/install.wim"))
        .context("no reserved space was found for install.wim")?
        .clone();
    reporter.phase(
        "copy",
        format!(
            "copying {:.2} GiB of image payload",
            wim_size as f64 / GIB as f64
        ),
    );
    let at = volume_base + placement.offset;
    let copied = {
        let reporter = reporter.clone();
        source.stream_into(&wim_file, &mut out, at, move |done| {
            reporter.send(Event::Fraction {
                fraction: done as f32 / wim_size.max(1) as f32,
                detail: "install.wim".to_string(),
            });
        })?
    };
    if copied != wim_size {
        bail!("install.wim is {wim_size} bytes but only {copied} were copied");
    }
    out.flush()?;
    drop(out);

    reporter.send(Event::Done {
        artifact: request.out.clone(),
        bytes: layout.total_bytes(),
    });
    Ok(Output {
        path: request.out.clone(),
        total_bytes: layout.total_bytes(),
        copied_bytes: copied,
        image_index: chosen.index,
        edition_id: chosen.edition_id,
        media_files: media_files.len(),
    })
}

/// The UEFI Shell chooser script.
///
/// This is what makes the media safe to leave attached. An Oxide instance boots only its
/// configured `boot_disk` and never falls through to another disk — proven on real
/// hardware — so with the installer as `boot_disk` the reboot Setup performs after
/// applying the image lands back in Setup, which wipes the half-installed Windows and
/// starts over, forever.
///
/// Rather than have an external tool race that reboot to flip `boot_disk`, the media
/// decides for itself: if any FAT volume already has an installed Windows, boot it;
/// otherwise run Setup. That keeps the media self-sufficient, which matters most for an
/// air-gapped operator with only a serial console. p1 is exFAT and invisible to the
/// firmware, so its own `\efi` tree cannot produce a false positive.
pub fn chooser_script() -> String {
    [
        "@echo -off",
        "echo OXIDE-CHOOSER: scanning for an installed Windows",
        "for %d in fs0 fs1 fs2 fs3 fs4 fs5 fs6 fs7",
        "  if exist %d:\\EFI\\Microsoft\\Boot\\bootmgfw.efi then",
        "    echo OXIDE-CHOOSER: BOOT-INSTALLED %d",
        "    echo OXIDE-CHOOSER: BOOT-INSTALLED %d",
        // The launched image clears the screen immediately, and the serial console is a
        // screen scrape — so without a pause the one line saying which branch was taken
        // is overwritten before anything can read it. That cost a run's worth of
        // ambiguity. `stall` takes microseconds; three seconds survives.
        "    stall 3000000",
        "    %d:\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
        "  endif",
        "endfor",
        "echo OXIDE-CHOOSER: RUN-SETUP no installed Windows found",
        "echo OXIDE-CHOOSER: RUN-SETUP no installed Windows found",
        // The last thing anyone sees before the screen goes blank for minutes.
        // Without it the honest reading of a serial console is "it hung", and the
        // natural response to that is to reset the machine partway through an install.
        "echo Starting Windows Setup. The screen stays blank for a few minutes.",
        "echo Usually 2-3 minutes before Setup appears, sometimes longer.",
        "echo The install is unattended: nothing needs your input, and the",
        "echo machine reboots itself several times before it is done.",
        // Five seconds rather than three. There is more to read now, and the launched
        // image clears the screen the moment it starts.
        "stall 5000000",
        "EFI\\BOOT\\uefintfs.efi",
        "echo OXIDE-CHOOSER: FAILED Setup did not start; dropping to the shell",
        "stall 10000000",
    ]
    .join("\r\n")
        + "\r\n"
}

/// Build p2: the 64 MiB FAT32 EFI System Partition the firmware boots.
pub fn boot_partition(
    assets: &crate::assets::Assets,
) -> Result<crate::sparse::SparseImage> {
    let efi = |name: &str| -> Result<Vec<u8>> {
        Ok(assets.read(&format!("efi/{name}"))?.into_owned())
    };
    let mut p2 =
        fat32::Fat32Builder::new(fat32::Options::esp(BOOT_PART_SECTORS))?;
    p2.add_file("/EFI/BOOT/BOOTX64.EFI", efi("shellx64.efi")?)?;
    p2.add_file("/EFI/BOOT/uefintfs.efi", efi("uefintfs.efi")?)?;
    // UEFI:NTFS loads its filesystem driver at runtime from this exact path and fails
    // with "[14] Not Found" if it is missing — it never embeds one. Rufus's
    // uefi-ntfs.img ships it alongside the loader in EFI/Rufus, which is easy to miss.
    p2.add_file("/EFI/Rufus/exfat_x64.efi", efi("exfat_x64.efi")?)?;
    p2.add_file("/startup.nsh", chooser_script().into_bytes())?;
    p2.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout of `ws2022-win-server-01-install.img`, which installed Windows on a
    /// rack: 7 GiB total, p1 6 GiB at LBA 2048, p2 at 12584960.
    #[test]
    fn the_layout_matches_the_image_that_booted() {
        // 4.04 GiB of install.wim plus ~1.3 GiB of media, which needed 6 GiB after the
        // 10% headroom and the whole-GiB rounding.
        let layout = plan(4_340_202_461 + 1_400_000_000).unwrap();
        assert_eq!(layout.p1_start, 2048);
        assert_eq!(layout.p1_sectors, 12_582_912); // 6 GiB
        assert_eq!(layout.p2_start, 12_584_960);
        assert_eq!(layout.p2_sectors, 131_072);
        assert_eq!(layout.total_sectors, 14_680_064); // 7 GiB
        assert_eq!(layout.total_bytes(), 7 * GIB);
    }

    /// Oxide disks must be a whole number of GiB. The natural layout never is: p1 is
    /// whole, but the 1 MiB MBR gap and the 64 MiB boot partition are not.
    #[test]
    fn the_image_is_always_a_whole_number_of_gibibytes() {
        for payload in [
            1u64,
            100 * 1024 * 1024,
            GIB,
            4_340_202_461,
            5_700_000_000,
            20 * GIB,
        ] {
            let layout = plan(payload).unwrap();
            assert_eq!(
                layout.total_bytes() % GIB,
                0,
                "payload {payload} gave {} bytes",
                layout.total_bytes()
            );
            assert!(
                layout.total_bytes() >= GIB,
                "payload {payload} gave under 1 GiB"
            );
            // And the padding is real slack, never negative overlap.
            let content =
                (layout.p2_start as u64 + layout.p2_sectors as u64) * SECTOR;
            assert!(
                layout.total_bytes() >= content,
                "payload {payload} overlaps"
            );
        }
    }

    /// The exact sizes, pinned at payloads where the headroom factor is what decides
    /// the answer.
    ///
    /// This exists because the whole-image comparison **cannot** catch a wrong factor: our Server 2022 media is a 4.692 GiB payload,
    /// and every factor from 1.1 to 1.2 rounds it to the same 6 GiB volume. Mutating
    /// 1.1 to 1.15 produced a byte-identical 7 GiB image. Only the two payloads below
    /// separate them.
    #[test]
    fn the_headroom_factor_is_pinned_where_it_actually_decides() {
        // 0.9 GiB: ×1.05 fits in 1 GiB, ×1.1 fits in 1 GiB, ×1.15 needs 2.
        let layout = plan(966_367_641).unwrap();
        assert_eq!(layout.p1_sectors as u64 * SECTOR, GIB);
        assert_eq!(layout.total_bytes(), 2 * GIB);

        // The real media: ×1.05 would give 5 GiB, ×1.1 gives 6.
        let layout = plan(5_038_157_754).unwrap();
        assert_eq!(layout.p1_sectors as u64 * SECTOR, 6 * GIB);
        assert_eq!(layout.total_bytes(), 7 * GIB);

        // install.wim on its own, with no media around it.
        let layout = plan(4_340_202_461).unwrap();
        assert_eq!(layout.p1_sectors as u64 * SECTOR, 5 * GIB);
        assert_eq!(layout.total_bytes(), 6 * GIB);
    }

    /// The headroom is what keeps a build from failing after the 4 GiB copy.
    #[test]
    fn the_media_volume_has_room_for_the_payload_plus_slack() {
        let payload = 4_340_202_461 + 1_400_000_000;
        let layout = plan(payload).unwrap();
        let volume = layout.p1_sectors as u64 * SECTOR;
        assert!(volume >= payload, "the volume is smaller than its contents");
        assert!(
            volume as f64 >= payload as f64 * 1.1,
            "less than the 10% headroom"
        );
    }

    #[test]
    fn a_tiny_payload_still_gets_a_legal_disk() {
        let layout = plan(1).unwrap();
        // One GiB for p1, and the total rounds past it to fit p2.
        assert_eq!(layout.p1_sectors as u64 * SECTOR, GIB);
        assert_eq!(layout.total_bytes(), 2 * GIB);
    }

    /// The chooser is what makes the installer safe to leave attached, so the two
    /// branches and the marker lines are worth pinning.
    #[test]
    fn the_chooser_boots_an_installed_windows_before_running_setup() {
        let nsh = chooser_script();
        let installed = nsh.find("BOOT-INSTALLED").unwrap();
        let setup = nsh.find("RUN-SETUP").unwrap();
        assert!(installed < setup, "Setup is tried before the installed OS");
        assert!(nsh.contains(r"bootmgfw.efi"));
        assert!(nsh.contains(r"EFI\BOOT\uefintfs.efi"));
        // Every marker is printed twice, because the serial console is a screen scrape
        // and the launched image clears the screen.
        assert_eq!(
            nsh.matches("echo OXIDE-CHOOSER: BOOT-INSTALLED %d").count(),
            2
        );
        assert_eq!(nsh.matches("RUN-SETUP no installed").count(), 2);
        // And the operator is told what the blank screen means. Without this the
        // honest reading of a serial console is that the machine has hung, which
        // invites a reset partway through an install.
        assert!(nsh.contains("Starting Windows Setup"));
        assert!(nsh.contains("screen stays blank"));
        assert!(nsh.contains("nothing needs your input"));
        // Long enough to read it before the launched image clears the screen.
        let setup_branch = &nsh[setup..];
        assert!(setup_branch.contains("stall 5000000"));
        // CRLF, because the UEFI shell reads it on a FAT volume.
        assert!(nsh.ends_with("stall 10000000\r\n"));
    }
}

#[cfg(test)]
mod whole_image {
    use super::*;
    use crate::settings::WindowsRelease;
    use crate::unattend::Config;

    /// Found by walking up to a marker rather than by a fixed depth. This counted three
    /// levels while the crate lived at `<repo>/rust/crates/oxwin-core`, and pointed
    /// silently outside the repository the moment it moved up one.
    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|d| d.join("rust-toolchain.toml").is_file())
            .expect("repo root")
            .to_path_buf()
    }

    /// The gate on the composition: build the whole image by every route that reaches
    /// it and require identical bytes.
    ///
    /// ```text
    /// hdiutil attach ~/Desktop/SERVER_2022_x64FRE_en-us.iso
    /// OXWIN_TEST_MEDIA=/Volumes/SSS_X64FREE_EN-US_DV9 cargo test --release whole_image
    /// ```
    ///
    /// Needs a mounted ISO, the fetched payload, and ~21 GiB of scratch space, so it is
    /// behind an environment variable.
    ///
    /// This used to diff against the JavaScript builder this was ported from, which was
    /// the reference the port was built against. That builder stayed behind; the
    /// individual structures it covered — the answer file, the bootstrap script, both
    /// filesystems, the MBR — are each pinned by committed goldens in their own modules,
    /// so what is left for this test is the part no golden can hold: that the sizing,
    /// the file ordering, the partition offsets and the streamed copy compose the same
    /// way every time and by every route.
    ///
    /// Determinism is the property under test. Three separate bugs of exactly that
    /// shape have been found here — local-time getters in both filesystem builders and
    /// `readdir` ordering of the media list — so any difference between these builds is
    /// a bug, not noise.
    #[test]
    fn builds_the_same_image_by_every_route() {
        let Ok(mount) = std::env::var("OXWIN_TEST_MEDIA") else {
            eprintln!("skipped: set OXWIN_TEST_MEDIA to a mounted Windows ISO");
            return;
        };
        let scratch =
            std::env::temp_dir().join(format!("oxwin-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let rs = scratch.join("rs.img");
        let again = scratch.join("again.img");

        // A factory rather than one value cloned: `Request` is not `Clone`, and widening
        // a public type to suit a test is the wrong way round.
        let request_for = |media: Media, out: &Path| Request {
            media,
            out: out.to_path_buf(),
            config: Config {
                release: WindowsRelease::Server2022,
                edition: "datacenter".into(),
                computer_name: "win-server-01".into(),
                username: "oxide".into(),
                password: "0xide!230xide!23".into(),
                enable_rdp: true,
                inject_drivers: true,
                enable_serial_console: true,
                target_disk: 1,
                locale: "en-US".into(),
                timezone: "UTC".into(),
                product_key: None,
                auto_logon: false,
                image_index: None,
                skip_image_install: false,
                install_from: None,
                install_from_letter: None,
                install_from_label: None,
                ssh_keys: vec![
                    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 dan@example".to_string(),
                ],
                enable_ssh: true,
            },
            edition_hint: None,
            ei_channel: None,
            bare: false,
            assets: crate::assets::Assets::Directory(
                repo_root().join("assets"),
            ),
        };
        let mount_media = || Media::Directory(PathBuf::from(&mount));

        let output = build(
            &request_for(mount_media(), &rs),
            &Reporter::silent(),
            &crate::engine::Cancel::new(),
        )
        .expect("native build");
        assert_eq!(output.image_index, 4, "picked a Server Core image");

        // The same request twice. Nothing in the builder may depend on wall-clock time,
        // hash iteration order, or the order the filesystem hands back a directory.
        build(
            &request_for(mount_media(), &again),
            &Reporter::silent(),
            &crate::engine::Cancel::new(),
        )
        .expect("second native build");
        assert_identical(&rs, &again);
        let _ = std::fs::remove_file(&again);

        // And from the ISO itself, which is the path that needs no mount at all. Two
        // completely different readers — UDF parsing versus the kernel's mount — have to
        // agree on the file list, its order, and every byte of content.
        if let Ok(iso) = std::env::var("OXWIN_TEST_ISO") {
            let from_iso = scratch.join("iso.img");
            build(
                &request_for(Media::Iso(PathBuf::from(iso)), &from_iso),
                &Reporter::silent(),
                &crate::engine::Cancel::new(),
            )
            .expect("native build from the ISO");
            assert_identical(&rs, &from_iso);
            let _ = std::fs::remove_file(&from_iso);
        }

        // And once more through the engine, which is the path the GUI takes. It
        // populates `Config` from `Settings` rather than from CLI flags, so it is a
        // genuinely different route to the same bytes — and it is the route whatever
        // gets tested on a rack will have come through.
        {
            use crate::engine::Engine;
            use crate::settings::{Credentials, Deployment, Settings};
            let via_engine = scratch.join("engine.img");
            let settings = Settings {
                iso: PathBuf::from(&mount),
                deployment: Deployment::Named {
                    hostname: "win-server-01".to_string(),
                },
                credentials: Credentials {
                    username: "oxide".to_string(),
                    password: "0xide!230xide!23".to_string(),
                    keys: vec![
                        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 dan@example"
                            .to_string(),
                    ],
                },
                ..Settings::default()
            };
            // A directory rather than the embedded payload, so this test compares like
            // with like against the requests above it.
            Engine::new(crate::assets::Assets::Directory(
                repo_root().join("assets"),
            ))
            .build(
                &settings,
                &via_engine,
                &Reporter::silent(),
                &crate::engine::Cancel::new(),
            )
            .expect("native build through the engine");
            assert_identical(&rs, &via_engine);
            let _ = std::fs::remove_file(&via_engine);
        }

        let _ = std::fs::remove_file(&rs);
        let _ = std::fs::remove_dir(&scratch);
    }

    /// Compare in chunks, reporting the first differing byte. `cmp` would do, but this
    /// keeps the failure inside the test output.
    fn assert_identical(a: &Path, b: &Path) {
        let (mut fa, mut fb) = (
            File::open(a).expect("js image"),
            File::open(b).expect("native image"),
        );
        let (la, lb) =
            (fa.metadata().unwrap().len(), fb.metadata().unwrap().len());
        assert_eq!(la, lb, "different sizes: {la} vs {lb}");
        let mut buf_a = vec![0u8; 8 * 1024 * 1024];
        let mut buf_b = vec![0u8; 8 * 1024 * 1024];
        let mut at = 0u64;
        loop {
            let n = fa.read(&mut buf_a).unwrap();
            if n == 0 {
                break;
            }
            fb.read_exact(&mut buf_b[..n]).unwrap();
            if buf_a[..n] != buf_b[..n] {
                let offset = buf_a[..n]
                    .iter()
                    .zip(&buf_b[..n])
                    .position(|(x, y)| x != y)
                    .expect("differ");
                let where_ = at + offset as u64;
                panic!(
                    "images differ at byte {where_} (0x{where_:x}): \
                     expected 0x{:02x}, got 0x{:02x}",
                    buf_a[offset], buf_b[offset]
                );
            }
            at += n as u64;
        }
    }
}
