// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Reading the source media, and asking it what it is.
//!
//! Two things live here. [`Source`] is the reader every other module goes through. Read an
//! ISO without mounting it, or use a directory someone already mounted.
//!
//! The media already knows. Every `<IMAGE>` in the WIM's XML resource carries `ARCH`,
//! `PRODUCTTYPE`, `BUILD` and `INSTALLATIONTYPE`, and we already read that resource to
//! choose an edition. So this is a UDF walk and two seeks, sub-second on every ISO
//! tested, and it runs when the media is picked, not when the build starts, because
//! stage 2 can only offer real choices if the media has already been read.

use crate::settings::{Problem, WindowsRelease};
use crate::udf::{Entry, UdfImage};
use crate::wim;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Where the media comes from: an ISO read directly, or a directory it is mounted at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Iso(PathBuf),
    Directory(PathBuf),
}

impl Media {
    /// Classify a path the user handed us. A directory is a mount; anything else is
    /// treated as an ISO and fails with a UDF error if it is not one.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if path.is_dir() { Self::Directory(path) } else { Self::Iso(path) }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Iso(p) | Self::Directory(p) => p,
        }
    }
}

/// What the media says it is.
///
/// Every field here was read off the media; nothing is asserted by a user. The four
/// identity fields are per-image on the wire and hold the first image's values, with
/// [`MediaInfo::problems`] reporting any image that disagrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    pub media: Media,
    /// Size of `sources/install.wim`. The cheapest identity field there is.
    pub wim_size: u64,
    /// Every installable image, in the media's own order.
    pub images: Vec<wim::Image>,
    /// Path of `ei.cfg` on the media, when it has one. Evaluation media ships one and
    /// non-evaluation media does not — verified across three server evaluations against
    /// Windows 11 retail and the Server 2022 volume-licensing DVD. We write our own
    /// either way, so this describes what Setup would have done unaided.
    pub ei_cfg: Option<String>,
    /// Oxide Rack is currently only x86_64, but Windows now ships ARM images and we
    /// need to check.
    pub arch: Option<u32>,
    /// Useful for ID, but also not a source of truth, Windows Desktop and server share
    /// build numbers.
    pub build: Option<u32>,
    pub product_type: String,
    /// The release the media reports, or the nearest one we know when the build is
    /// unfamiliar. `None` when the media said too little to tell.
    pub release: Option<WindowsRelease>,
    /// Whether `build` matched a base build in the release table. `false` alongside a
    /// `Some(release)` means that release is a guess, and the caller has to say so.
    pub build_recognised: bool,
}

/// Read the media and report what it is.
pub fn inspect(media: &Media) -> Result<MediaInfo> {
    let mut source = Source::open(media)?;

    let wim_file = source.find("/sources/install.wim").with_context(|| {
        format!(
            "{} has no sources/install.wim, so there is nothing to identify it by. \
             A Windows ISO or a complete mount of one is needed; a partial copy is not.",
            media.path().display()
        )
    })?;
    let wim_size = wim_file.size;

    let images = {
        let mut read =
            |offset: u64, len: usize| source.read_range(&wim_file, offset, len);
        wim::read_images(&mut read).context("reading the WIM image list")?
    };

    // The whole media walk, only to answer whether ei.cfg is there. Cheap next to the
    // WIM read, and it is a fact worth carrying: it is the one structural difference
    // between evaluation and volume-licensing media.
    let ei_cfg = source.list()?.into_iter().find_map(|f| {
        f.volume_path
            .to_lowercase()
            .ends_with("/ei.cfg")
            .then_some(f.volume_path)
    });

    let first = images.first();
    let (release, build_recognised) = release_of(&images);
    Ok(MediaInfo {
        media: media.clone(),
        wim_size,
        ei_cfg,
        arch: first.and_then(|i| i.arch),
        build: first.and_then(|i| i.build),
        product_type: first.map(|i| i.product_type.clone()).unwrap_or_default(),
        release,
        build_recognised,
        images,
    })
}

/// Which release is this, and did we recognise its build?
///
/// `PRODUCTTYPE` and `BUILD` together, never either alone. **Server 2025 and Windows 11
/// 24H2 are both build 26100**, so a build-only match installs the wrong answer file with
/// total confidence; and `PRODUCTTYPE` alone cannot tell 2019 from 2025.
///
/// An unfamiliar build still yields a release, the nearest known one of the same kind.
/// Server 2016, or whatever ships in 2027. The caller learns it was a guess from the
/// second return value.
fn detect_release(image: &wim::Image) -> (Option<WindowsRelease>, bool) {
    // Without a build there is nothing to match, and without either kind field there is
    // no way to know whether to match it against the server or the client releases.
    let Some(build) = image.build else { return (None, false) };
    if image.product_type.is_empty() && image.installation_type.is_empty() {
        return (None, false);
    }

    let client = wim::is_client_image(image);
    let candidates = || {
        WindowsRelease::ALL.iter().copied().filter(|r| r.is_client() == client)
    };

    if let Some(exact) = candidates().find(|r| r.base_builds().contains(&build))
    {
        return (Some(exact), true);
    }
    // Newer than anything we know: the most recent release at or below this build, which
    // is the one whose drivers and answer file are most likely to fit.
    let below = candidates()
        .filter_map(|r| {
            r.base_builds()
                .iter()
                .copied()
                .filter(|b| *b < build)
                .max()
                .map(|b| (b, r))
        })
        .max_by_key(|(b, _)| *b)
        .map(|(_, r)| r);
    // Older than anything we know: the oldest, for the same reason in reverse.
    let guess = below.or_else(|| {
        candidates()
            .filter_map(|r| {
                r.base_builds().iter().copied().min().map(|b| (b, r))
            })
            .min_by_key(|(b, _)| *b)
            .map(|(_, r)| r)
    });
    (guess, false)
}

impl MediaInfo {
    /// Client media (Windows 10/11) rather than server.
    pub fn is_client(&self) -> bool {
        self.images.first().is_some_and(wim::is_client_image)
    }

    pub fn is_evaluation(&self) -> bool {
        self.images
            .iter()
            .any(|i| i.edition_id.to_lowercase().ends_with("eval"))
    }

    /// Whether this media can be built at all, and what to say about it.
    ///
    /// Same shape as [`crate::settings::Settings::problems`] so the UI renders both the
    /// same way: blocking problems stop the build, warnings are shown and do not.
    pub fn problems(&self) -> Vec<Problem> {
        problems_for(&self.images)
    }

    pub fn is_buildable(&self) -> bool {
        !self.problems().iter().any(|p| p.blocking)
    }
}

/// The same verdict as [`MediaInfo::problems`], from an image list alone.
///
/// Split out because [`crate::builder`] has already read the image list by the time it
/// could refuse, and making it walk the media a second time to be told what it is holding
/// would be silly. This is the function that actually decides; `MediaInfo` delegates.
pub fn problems_for(images: &[wim::Image]) -> Vec<Problem> {
    let mut v = Vec::new();

    if images.is_empty() {
        v.push(Problem::block(
            "media",
            "This media lists no installable images. Setup would show an empty \
             edition list with no explanation.",
        ));
        return v;
    }

    let first = &images[0];
    {
        match first.arch {
            Some(wim::ARCH_AMD64) => {}
            Some(wim::ARCH_ARM64) => v.push(Problem::block(
                "media",
                "This is Arm64 media. The Oxide rack and supporting materials are amd64 \
                 drivers and processorArchitecture in the answer file. Please present \
                 x86_64 media.",
            )),
            Some(other) => v.push(Problem::block(
                "media",
                format!(
                    "This media reports architecture {other}, which is neither amd64 \
                     ({}) nor Arm64 ({}). Nothing here would apply to it.",
                    wim::ARCH_AMD64, wim::ARCH_ARM64
                ),
            )),
            // Every ISO read so far states it. Absent is not a refusal, because refusing
            // media over a missing tag would be worse than proceeding on the odds.
            None => v.push(Problem::warn(
                "media",
                "This media does not say which architecture it is for. The build assumes \
                 amd64, which is all this app supports.",
            )),
        }
    }

    // Nothing in the format promises the images agree, and every release-wide
    // decision below takes the first image's answer.
    {
        for image in &images[1..] {
            if image.arch != first.arch
                || image.build != first.build
                || image.product_type != first.product_type
            {
                v.push(Problem::block(
                    "media",
                    format!(
                        "Image {} describes a different Windows than image {} \
                         (arch/build/product type disagree). This media is not something \
                         this app has seen; nothing here would be trustworthy.",
                        image.index, first.index
                    ),
                ));
                break;
            }
        }
    }

    match detect_release(first) {
        (Some(_), true) => {}
        (Some(release), false) => v.push(Problem::warn(
            "media",
            format!(
                "Build {} is not one this app knows. It will be built as {}, using \
                 the {} drivers, which is a guess — verify the guest has a network \
                 before trusting the image.",
                first.build.map(|b| b.to_string()).unwrap_or("?".into()),
                release.label(),
                release.driver_dir(),
            ),
        )),
        (None, _) => v.push(Problem::warn(
            "media",
            "This media does not say which Windows release it is. Every \
             release-dependent choice: drivers, and the hardware-check bypasses \
             has to be set by hand.",
        )),
    }

    v
}

/// Which image to install when the caller expressed no preference.
///
/// Datacenter with a desktop on server media, Pro on client media: what someone would
/// pick if asked. Deliberately never Home, which has no Remote Desktop host, and never a
/// Core image, which is a real choice rather than a default.
///
/// This exists because "no hint" used to mean "the first image", and the first image on
/// the Windows 11 retail ISO is Home. Home doesnt have RDP server.
pub fn default_image(images: &[wim::Image]) -> Option<&wim::Image> {
    for hint in ["datacenter", "pro"] {
        if let Some(image) = wim::select_image(images, hint) {
            return Some(image);
        }
    }
    images.iter().find(|i| !wim::is_core_image(i)).or_else(|| images.first())
}

/// Everything about the image the user actually chose.
///
/// Separate from [`problems_for`] because it depends on a choice rather than on the
/// media: the same ISO is fine or not depending on which of its eleven images is picked.
pub fn problems_for_image(
    image: &wim::Image,
    enable_rdp: bool,
) -> Vec<Problem> {
    let mut v = Vec::new();

    // The exact inverse of the trap in `is_core_image`. There, `FLAGS=Core` on client
    // media means Home and *not* Server Core; here that same value is the thing being
    // matched, and it is meaningful only because client-ness has already been
    // established. `Core`, `CoreN`, `CoreSingleLanguage` and `CoreCountrySpecific` are
    // the Home family.
    let home = wim::is_client_image(image)
        && image.edition_id.to_lowercase().starts_with("core");
    if home && enable_rdp {
        v.push(Problem::warn(
            "enable_rdp",
            format!(
                "{} has no Remote Desktop host — Home editions can accept no incoming \
                 RDP connection and cannot domain-join. SSH and the serial console will \
                 work; Remote Desktop will not, however it is configured.",
                image.name
            ),
        ));
    }
    v
}

/// Which release an image list describes, and whether its build was recognised.
///
/// The list's own answer, taken from the first image; [`problems_for`] is what reports a
/// list whose images disagree.
pub fn release_of(images: &[wim::Image]) -> (Option<WindowsRelease>, bool) {
    images.first().map(detect_release).unwrap_or((None, false))
}

/// One file to copy onto the media volume.
pub(crate) struct MediaFile {
    /// Path as it will appear on the volume, with a leading slash.
    pub(crate) volume_path: String,
    pub(crate) size: u64,
    origin: Origin,
}

enum Origin {
    Host(PathBuf),
    Udf(Entry),
}

/// The media, opened.
pub(crate) enum Source {
    Iso(Box<UdfImage>),
    Directory(PathBuf),
}

impl Source {
    pub(crate) fn open(media: &Media) -> Result<Self> {
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
    pub(crate) fn list(&mut self) -> Result<Vec<MediaFile>> {
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

    pub(crate) fn find(&mut self, volume_path: &str) -> Result<MediaFile> {
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
                let path = resolve_ignoring_case(root, volume_path)
                    .with_context(|| {
                        format!(
                            "{} is not on this media",
                            root.join(volume_path.trim_start_matches('/'))
                                .display()
                        )
                    })?;
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

    pub(crate) fn read(&mut self, file: &MediaFile) -> Result<Vec<u8>> {
        match (&mut *self, &file.origin) {
            (Self::Iso(udf), Origin::Udf(entry)) => udf.read_file(entry),
            (_, Origin::Host(path)) => std::fs::read(path)
                .with_context(|| format!("reading {}", path.display())),
            _ => bail!("{} does not belong to this media", file.volume_path),
        }
    }

    pub(crate) fn read_range(
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
    pub(crate) fn stream_into(
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

/// Resolve a volume path under `root`, matching each component case-insensitively.
///
/// Windows media mixes casing within one directory — `sources/install.wim` sits beside
/// `sources/EI.CFG` on every ISO examined — and **macOS mounts UDF case-sensitively**, so
/// a plain `join` resolves whichever spelling the caller happened to guess. The ISO route
/// has always compared with `eq_ignore_ascii_case`; this is the mount route catching up,
/// so the two cannot disagree about what the media contains.
///
/// Nothing is known to be broken by this today: `install.wim` is lowercase on all six
/// ISOs on hand.
fn resolve_ignoring_case(root: &Path, volume_path: &str) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for want in volume_path.trim_matches('/').split('/') {
        if want.is_empty() {
            continue;
        }
        // The spelling the caller asked for, first: one `stat` rather than listing a
        // directory that may hold a thousand files.
        let exact = at.join(want);
        at =
            if exact.exists() { exact } else { find_ignoring_case(&at, want)? };
    }
    Some(at)
}

/// The one entry of `dir` whose name matches `want` apart from case.
///
/// Compares the names this side rather than leaning on the filesystem, so it behaves the
/// same on a case-sensitive UDF mount and a case-insensitive APFS copy.
fn find_ignoring_case(dir: &Path, want: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(want))
        .map(|e| e.path())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One image, described the way real media describes it.
    fn image(product_type: &str, build: u32, installation: &str) -> wim::Image {
        wim::Image {
            index: 1,
            name: "test".into(),
            description: String::new(),
            edition_id: "ServerDatacenterEval".into(),
            flags: "ServerDataCenterEval".into(),
            arch: Some(wim::ARCH_AMD64),
            build: Some(build),
            product_type: product_type.into(),
            installation_type: installation.into(),
        }
    }

    fn detect(product_type: &str, build: u32) -> Option<WindowsRelease> {
        let kind = if product_type == "WinNT" { "Client" } else { "Server" };
        detect_release(&image(product_type, build, kind)).0
    }

    /// Every release must detect back to itself from every build it claims. An exhaustive
    /// round trip, so adding a variant with a build another variant already claims fails
    /// here rather than at an install.
    #[test]
    fn every_release_round_trips_through_its_own_base_builds() {
        for release in WindowsRelease::ALL {
            let product_type =
                if release.is_client() { "WinNT" } else { "ServerNT" };
            for build in release.base_builds() {
                let (got, exact) = detect_release(&image(
                    product_type,
                    *build,
                    if release.is_client() { "Client" } else { "Server" },
                ));
                assert!(
                    exact,
                    "{} build {build} not recognised",
                    release.label()
                );
                assert_eq!(
                    got,
                    Some(*release),
                    "{} build {build} detected as {:?}",
                    release.label(),
                    got.map(|r| r.label())
                );
            }
        }
    }

    /// The collision that would have cost a day: Server 2025 and Windows 11 24H2 are both
    /// build 26100, and only `PRODUCTTYPE` separates them. Getting it wrong means the
    /// hardware-check bypasses in the wrong state and the wrong drivers.
    #[test]
    fn build_26100_is_server_2025_or_windows_11_depending_on_product_type() {
        assert_eq!(detect("ServerNT", 26100), Some(WindowsRelease::Server2025));
        assert_eq!(detect("WinNT", 26100), Some(WindowsRelease::Windows11));
        // And they do not share a driver directory, which is what the mix-up would cost.
        assert_ne!(
            WindowsRelease::Server2025.driver_dir(),
            WindowsRelease::Windows11.driver_dir()
        );
    }

    /// `BUILD` is the base build. The Windows 10 media whose filename says 19045 reports
    /// 19041, so the table holds base builds and the filename is never consulted.
    #[test]
    fn the_base_build_is_what_matches_not_the_patch_level() {
        assert_eq!(detect("WinNT", 19041), Some(WindowsRelease::Windows10));
        // 19045 is what the filename claims. It is not a base build, so it is a guess —
        // a correct one, but the caller is told.
        let (release, exact) = detect_release(&image("WinNT", 19045, "Client"));
        assert_eq!(release, Some(WindowsRelease::Windows10));
        assert!(!exact, "19045 must not be treated as a known build");
    }

    /// Newer than the table: buildable, as the nearest release we know, and flagged.
    #[test]
    fn an_unknown_newer_build_guesses_the_nearest_release() {
        let (release, exact) =
            detect_release(&image("ServerNT", 30000, "Server"));
        assert_eq!(release, Some(WindowsRelease::Server2025));
        assert!(!exact);

        let (release, exact) = detect_release(&image("WinNT", 30000, "Client"));
        assert_eq!(release, Some(WindowsRelease::Windows11));
        assert!(!exact);
    }

    /// Older than the table: Server 2012 R2 is build 9600, below every entry. It still
    /// resolves, to the oldest release we know, rather than being walled off.
    ///
    /// 2012 R2 is out of scope deliberately: it went out of support in October 2023 and
    /// predates components the answer file relies on. One day someone may ask for it
    /// for some legacy support, but if that day comes, then we can cross that road.
    #[test]
    fn an_unknown_older_build_falls_back_to_the_oldest_release() {
        let (release, exact) =
            detect_release(&image("ServerNT", 9600, "Server"));
        assert_eq!(release, Some(WindowsRelease::Server2016));
        assert!(!exact);
    }

    /// Server 2016 is build 14393 and is now a release in its own right, not the
    /// nearest-neighbour guess to Server 2019 it used to resolve to.
    #[test]
    fn server_2016_is_an_exact_match_rather_than_a_guess() {
        let (release, exact) =
            detect_release(&image("ServerNT", 14393, "Server"));
        assert_eq!(release, Some(WindowsRelease::Server2016));
        assert!(exact);
    }

    /// A server build must never resolve to a client release, or the answer file carries
    /// client edition names and the wrong bypasses.
    #[test]
    fn detection_never_crosses_the_client_server_line() {
        for build in [14393, 17763, 19041, 20348, 22621, 26100, 30000] {
            let server = detect("ServerNT", build).expect("a server release");
            assert!(!server.is_client(), "{build} detected as {server:?}");
            let client = detect("WinNT", build).expect("a client release");
            assert!(client.is_client(), "{build} detected as {client:?}");
        }
    }

    /// Nothing to go on is `None`, not a plausible-looking default. A default here would
    /// be an assertion, which is the input detection exists to remove.
    #[test]
    fn media_that_says_too_little_detects_nothing() {
        let mut bare = image("", 20348, "");
        assert_eq!(detect_release(&bare), (None, false));
        bare.build = None;
        bare.product_type = "ServerNT".into();
        assert_eq!(detect_release(&bare), (None, false));
    }

    /// `INSTALLATIONTYPE` alone is enough when `PRODUCTTYPE` is missing.
    #[test]
    fn installation_type_substitutes_for_a_missing_product_type() {
        let (release, exact) = detect_release(&image("", 22621, "Client"));
        assert_eq!(release, Some(WindowsRelease::Windows11));
        assert!(exact);
    }

    fn info(images: Vec<wim::Image>) -> MediaInfo {
        let first = images.first().cloned();
        let (release, build_recognised) =
            first.as_ref().map(detect_release).unwrap_or((None, false));
        MediaInfo {
            media: Media::Iso("/tmp/test.iso".into()),
            wim_size: 4_340_202_461,
            ei_cfg: Some("/sources/ei.cfg".into()),
            arch: first.as_ref().and_then(|i| i.arch),
            build: first.as_ref().and_then(|i| i.build),
            product_type: first.map(|i| i.product_type).unwrap_or_default(),
            release,
            build_recognised,
            images,
        }
    }

    #[test]
    fn server_2022_media_is_buildable_with_nothing_to_say() {
        let info = info(vec![image("ServerNT", 20348, "Server")]);
        assert_eq!(info.release, Some(WindowsRelease::Server2022));
        assert!(info.is_buildable());
        assert!(info.problems().is_empty(), "{:?}", info.problems());
        assert!(!info.is_client());
        assert!(info.is_evaluation());
    }

    /// Arm64 media builds an unbootable image in silence today. It has to be refused, and
    /// the message has to name the architecture rather than say "unsupported media".
    #[test]
    fn arm64_media_is_refused() {
        let mut images = vec![image("WinNT", 26200, "Client")];
        images[0].arch = Some(wim::ARCH_ARM64);
        let info = info(images);
        assert!(!info.is_buildable());
        let message = &info.problems()[0].message;
        assert!(message.contains("Arm64"), "{message}");
    }

    #[test]
    fn media_with_no_images_is_refused() {
        let info = info(Vec::new());
        assert!(!info.is_buildable());
        assert!(info.problems()[0].message.contains("no installable images"));
    }

    /// Images that describe different Windowses mean every release-wide answer here was
    /// taken from an arbitrary one of them.
    #[test]
    fn images_that_disagree_about_the_release_are_refused() {
        let info = info(vec![
            image("ServerNT", 20348, "Server"),
            wim::Image { index: 2, ..image("WinNT", 26100, "Client") },
        ]);
        assert!(!info.is_buildable());
        assert!(
            info.problems().iter().any(|p| p.message.contains("image 1")),
            "{:?}",
            info.problems()
        );
    }

    /// An unfamiliar build is a warning, never a block, and the warning has to name both
    /// the build and the driver directory it guessed — those are what someone checking
    /// the guess needs.
    #[test]
    fn an_unrecognised_build_warns_and_still_builds() {
        let info = info(vec![image("ServerNT", 30000, "Server")]);
        assert!(info.is_buildable());
        let warning = info
            .problems()
            .into_iter()
            .find(|p| !p.blocking)
            .expect("a warning");
        assert!(warning.message.contains("30000"), "{}", warning.message);
        assert!(warning.message.contains("2k25"), "{}", warning.message);
    }

    fn client(index: u32, name: &str, edition_id: &str) -> wim::Image {
        wim::Image {
            index,
            name: name.into(),
            description: name.into(),
            edition_id: edition_id.into(),
            flags: edition_id.into(),
            arch: Some(wim::ARCH_AMD64),
            build: Some(22621),
            product_type: "WinNT".into(),
            installation_type: "Client".into(),
        }
    }

    /// Home has no RDP server, and RDP is one of only three ways into a guest. Picking it
    /// with Remote Desktop switched on has to say so here rather than on a rack. Client
    /// editions ALSO dont have serial output, which makes it an issue.
    ///
    /// The whole Home family is `EDITIONID` beginning `Core`. The same value that fooled
    /// the old Core-image test. Here it is load-bearing, but only because client-ness has
    /// already been established.
    #[test]
    fn home_with_rdp_enabled_warns() {
        for (index, name, edition) in [
            (1, "Windows 11 Home", "Core"),
            (2, "Windows 11 Home N", "CoreN"),
            (3, "Windows 11 Home Single Language", "CoreSingleLanguage"),
        ] {
            let image = client(index, name, edition);
            let problems = problems_for_image(&image, true);
            assert_eq!(problems.len(), 1, "{name}: {problems:?}");
            assert!(!problems[0].blocking, "{name} must not block");
            assert!(problems[0].message.contains(name), "{:?}", problems[0]);
            // Nothing to warn about when RDP was never asked for.
            assert!(problems_for_image(&image, false).is_empty(), "{name}");
        }
    }

    /// Every other client edition has a Remote Desktop host, and no server image is Home
    /// however much `Core` appears in its name.
    #[test]
    fn editions_that_do_have_rdp_say_nothing() {
        for image in [
            client(6, "Windows 11 Pro", "Professional"),
            client(4, "Windows 11 Education", "Education"),
            client(1, "Windows 10 Enterprise Evaluation", "EnterpriseEval"),
        ] {
            assert!(
                problems_for_image(&image, true).is_empty(),
                "{}",
                image.name
            );
        }
        // Server Core: `FLAGS` ends in Core, `EDITIONID` does not begin with it, and it
        // is not a client image either way.
        let core = image("ServerNT", 20348, "Server Core");
        assert!(problems_for_image(&core, true).is_empty());
    }

    #[test]
    fn retail_media_is_not_evaluation_media() {
        let mut images = vec![image("ServerNT", 20348, "Server")];
        images[0].edition_id = "ServerDatacenter".into();
        assert!(!info(images).is_evaluation());
    }

    /// A mounted ISO must resolve the same paths the ISO reader does.
    ///
    /// `find_ignoring_case` is tested rather than the whole lookup, deliberately: it does
    /// the comparison itself instead of asking the filesystem, so this proves the same
    /// thing on a case-insensitive APFS volume as on a case-sensitive UDF mount. A test
    /// that leaned on `Path::exists` would pass on macOS whether the code was right or
    /// not.
    #[test]
    fn a_mounted_path_resolves_whatever_its_casing() {
        let dir = std::env::temp_dir()
            .join(format!("oxwin-case-{}", std::process::id()));
        let sources = dir.join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        // Exactly the mixed casing real media uses in one directory.
        std::fs::write(sources.join("install.wim"), b"wim").unwrap();
        std::fs::write(sources.join("EI.CFG"), b"cfg").unwrap();

        for (want, expect) in [
            ("install.wim", "install.wim"),
            ("INSTALL.WIM", "install.wim"),
            ("ei.cfg", "EI.CFG"),
            ("EI.CFG", "EI.CFG"),
        ] {
            let got = find_ignoring_case(&sources, want)
                .unwrap_or_else(|| panic!("{want} not found"));
            assert_eq!(
                got.file_name().unwrap().to_string_lossy(),
                expect,
                "{want}"
            );
        }
        assert!(find_ignoring_case(&sources, "boot.wim").is_none());

        // And the whole path, which is what callers use.
        let found =
            resolve_ignoring_case(&dir, "/SOURCES/install.wim").unwrap();
        assert!(found.is_file(), "{}", found.display());
        assert!(resolve_ignoring_case(&dir, "/sources/nope.wim").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real thing. Reads whichever ISO is to hand and checks that detection produced
    /// a release, that it agrees with the media's own product type, and that the
    /// architecture verdict matches what the media reports.
    #[test]
    fn inspects_real_media() {
        let Ok(iso) = std::env::var("OXWIN_TEST_ISO") else {
            eprintln!("skipped: set OXWIN_TEST_ISO to a Windows ISO");
            return;
        };
        let media = Media::at(&iso);
        let info = inspect(&media).expect("inspecting the media");

        assert!(!info.images.is_empty(), "no images found");
        assert!(info.wim_size > 1 << 30, "install.wim is {}", info.wim_size);

        let release = info.release.expect("a release was detected");
        assert_eq!(
            release.is_client(),
            info.is_client(),
            "detected {} for {} media",
            release.label(),
            info.product_type
        );
        assert!(
            info.build_recognised,
            "build {:?} is not in the release table; if this ISO is legitimate, add it",
            info.build
        );

        // Arm64 media is the one ISO on hand that must be refused, and it is refused for
        // its architecture rather than for anything incidental.
        match info.arch {
            Some(wim::ARCH_AMD64) => assert!(
                info.is_buildable(),
                "amd64 media refused: {:?}",
                info.problems()
            ),
            Some(wim::ARCH_ARM64) => assert!(
                !info.is_buildable(),
                "Arm64 media accepted, which would build an image that cannot boot"
            ),
            other => panic!("unexpected architecture {other:?}"),
        }

        // Evaluation media ships an ei.cfg and non-evaluation media does not. Both are
        // fine — we write our own — so this is only checked as a fact about the media.
        eprintln!(
            "{}: {} build {:?}, {} images, ei.cfg {}, {}",
            std::path::Path::new(&iso)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            release.label(),
            info.build,
            info.images.len(),
            if info.ei_cfg.is_some() { "present" } else { "absent" },
            if info.is_evaluation() { "evaluation" } else { "retail/VL" },
        );
    }
}
