// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Where the third-party payload comes from.
//!
//! The media carries virtio drivers, Win32-OpenSSH and three EFI binaries. None of it is
//! ours; all of it is fetched by `tools/fetch-payload.sh` against pinned SHA-256
//! checksums and never committed.
//!
//! Normally it is compiled in, so a release binary is one file with nothing to locate at
//! runtime. A directory can be substituted, which is how a driver version is tried
//! without a rebuild.

use anyhow::{Context, Result, bail};
use std::borrow::Cow;
use std::path::PathBuf;

mod payload {
    include!(concat!(env!("OUT_DIR"), "/payload.rs"));
}

/// The payload, as embedded at build time. Empty when the tree was built without one.
pub use payload::{EMBEDDED, FINGERPRINT};

#[derive(Debug, Clone)]
pub enum Assets {
    /// Compiled in by `build.rs`.
    Embedded,
    /// A directory laid out like `assets/`: `efi/`, `drivers/`, `openssh/` and
    /// `payload-manifest.json` directly inside it.
    Directory(PathBuf),
}

impl Assets {
    /// What to use unless told otherwise: the embedded payload, or a directory named by
    /// `OXWIN_ASSETS`.
    ///
    /// Deliberately infallible. A build with no payload is still worth starting — the UI
    /// reports it through [`Assets::problem`] on the first screen — and failing here
    /// would mean the app could not open at all.
    pub fn discover() -> Self {
        match std::env::var_os("OXWIN_ASSETS") {
            Some(dir) => Self::Directory(PathBuf::from(dir)),
            None => Self::Embedded,
        }
    }

    /// Why this payload cannot build an image, if it cannot.
    ///
    /// Checked up front rather than at the point of use, because the alternative is
    /// discovering it several minutes into a 4 GiB copy.
    pub fn problem(&self) -> Option<String> {
        match self {
            Self::Embedded if EMBEDDED.is_empty() => Some(
                "this build carries no driver payload. It was built without \
                 assets/ present — run ./tools/fetch-payload.sh and rebuild, or \
                 set OXWIN_ASSETS to a directory holding one."
                    .to_string(),
            ),
            Self::Embedded => None,
            Self::Directory(dir) if !dir.join("payload-manifest.json").is_file() => {
                Some(format!(
                    "{} has no payload-manifest.json. Run \
                     ./tools/fetch-payload.sh, or unset OXWIN_ASSETS to use the \
                     payload compiled into this binary.",
                    dir.display()
                ))
            }
            Self::Directory(_) => None,
        }
    }

    /// Read one file, by its path relative to the assets directory.
    ///
    /// Manifest entries carry an `assets/` prefix for historical reasons, so it is
    /// tolerated here rather than requiring every caller to remember to strip it.
    pub fn read(&self, rel: &str) -> Result<Cow<'static, [u8]>> {
        let rel = rel.strip_prefix("assets/").unwrap_or(rel);
        match self {
            Self::Embedded => EMBEDDED
                .iter()
                .find(|(name, _)| *name == rel)
                .map(|(_, data)| Cow::Borrowed(*data))
                .with_context(|| {
                    if EMBEDDED.is_empty() {
                        "this build carries no driver payload — run \
                         ./tools/fetch-payload.sh and rebuild"
                            .to_string()
                    } else {
                        format!("{rel} is not in the embedded payload")
                    }
                }),
            Self::Directory(dir) => {
                let path = dir.join(rel);
                Ok(Cow::Owned(std::fs::read(&path).with_context(|| {
                    format!(
                        "reading {} — run ./tools/fetch-payload.sh",
                        path.display()
                    )
                })?))
            }
        }
    }

    /// The manifest, which says which drivers belong to which Windows release.
    pub fn manifest(&self) -> Result<Vec<u8>> {
        if let Some(problem) = self.problem() {
            bail!("{problem}");
        }
        Ok(self.read("payload-manifest.json")?.into_owned())
    }

    /// One line for `oxwin doctor` and the log, so which payload is in play is recorded
    /// rather than assumed.
    pub fn describe(&self) -> String {
        match self {
            Self::Embedded if EMBEDDED.is_empty() => {
                "none embedded".to_string()
            }
            Self::Embedded => format!(
                "embedded, {} files, fingerprint {FINGERPRINT}",
                EMBEDDED.len()
            ),
            Self::Directory(dir) => format!("directory {}", dir.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `assets/` prefix from the manifest and a bare path have to name the same file,
    /// or half the payload silently fails to resolve.
    #[test]
    fn a_manifest_prefix_is_tolerated() {
        let dir = std::env::temp_dir()
            .join(format!("oxwin-assets-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("efi")).unwrap();
        std::fs::write(dir.join("efi/shellx64.efi"), b"shell").unwrap();
        let assets = Assets::Directory(dir.clone());
        assert_eq!(&*assets.read("efi/shellx64.efi").unwrap(), b"shell");
        assert_eq!(&*assets.read("assets/efi/shellx64.efi").unwrap(), b"shell");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The error a user actually hits has to name the script that fixes it.
    #[test]
    fn a_missing_file_names_the_fetch_script() {
        let assets = Assets::Directory(PathBuf::from("/nonexistent"));
        let err = assets.read("efi/shellx64.efi").unwrap_err().to_string();
        assert!(err.contains("fetch-payload.sh"), "{err}");
    }

    /// A directory with no manifest is reported as a problem before a build starts, not
    /// as a read failure four minutes in.
    #[test]
    fn a_directory_without_a_manifest_is_a_problem() {
        let assets = Assets::Directory(PathBuf::from("/nonexistent"));
        let problem = assets.problem().expect("should be a problem");
        assert!(problem.contains("payload-manifest.json"), "{problem}");
    }

    /// Every file the manifest names is readable from the embedded table, at the size it
    /// claims.
    ///
    /// `build.rs` checks this too, but against the directory it was reading. This checks
    /// the table that actually shipped — which is the only thing a downloaded binary has,
    /// and the one place a mistake would surface as an image missing a driver rather than
    /// as a build failure.
    ///
    /// Does not skip when there is no payload: it asserts the no-payload contract
    /// instead, so this test says something either way.
    #[test]
    fn the_embedded_payload_matches_its_own_manifest() {
        let assets = Assets::Embedded;
        let Ok(raw) = assets.manifest() else {
            assert!(
                EMBEDDED.is_empty(),
                "a payload is embedded but has no manifest"
            );
            assert!(assets.problem().is_some());
            return;
        };
        let manifest: serde_json::Value =
            serde_json::from_slice(&raw).expect("the manifest parses");

        let mut checked = 0;
        for (release, entries) in
            manifest["drivers"].as_object().expect("drivers is an object")
        {
            for entry in entries.as_array().expect("a driver list") {
                let path = entry["path"].as_str().expect("a driver path");
                let want =
                    entry["size"].as_u64().expect("a driver size") as usize;
                let data = assets
                    .read(path)
                    .unwrap_or_else(|e| panic!("{release} {path}: {e}"));
                assert_eq!(
                    data.len(),
                    want,
                    "{release} {path} is the wrong size"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "the manifest lists no drivers at all");

        // The OpenSSH payload and the three EFI binaries, which are named rather than
        // listed — a manifest that had lost a block would otherwise pass.
        let ssh = manifest["openSsh"].as_str().expect("openSsh");
        assert!(!assets.read(ssh).expect("openssh payload").is_empty());
        for name in ["shellx64.efi", "uefintfs.efi", "exfat_x64.efi"] {
            let data = assets
                .read(&format!("efi/{name}"))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!data.is_empty(), "{name} is empty");
        }

        // Every release this workspace can build must have drivers in the payload it
        // shipped with. Without this, a release whose `driver_dir` names a directory
        // `fetch-payload.sh` does not fetch fails at build time on a user's machine —
        // and the NetKVM case fails later still, as a guest with no network on a rack.
        for release in crate::settings::WindowsRelease::ALL {
            let dir = release.driver_dir();
            let drivers =
                manifest["drivers"][dir].as_array().unwrap_or_else(|| {
                    panic!(
                        "the payload has no {dir} drivers, which {} needs. \
                         tools/fetch-payload.sh must fetch every target in \
                         WindowsRelease::ALL.",
                        release.label()
                    )
                });
            // NetKVM specifically: it is the network driver, and its absence is the one
            // failure that looks like a successful install until someone tries to ssh in.
            assert!(
                drivers.iter().any(|d| d["driver"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case("NetKVM"))),
                "{dir} has drivers but no NetKVM, so {} would install with no network",
                release.label()
            );
        }
    }

    /// Whether this build embedded a payload or not, `describe` says which — the log has
    /// to be able to distinguish "wrong drivers" from "no drivers".
    #[test]
    fn describe_distinguishes_an_empty_payload() {
        let text = Assets::Embedded.describe();
        if EMBEDDED.is_empty() {
            assert_eq!(text, "none embedded");
            assert!(Assets::Embedded.problem().is_some());
        } else {
            assert!(text.starts_with("embedded, "), "{text}");
            assert!(Assets::Embedded.problem().is_none());
        }
    }
}
