// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Every resource a golden run touches, named from one run name.
//!
//! This is what makes the cycle resumable without a journal. A journal is an
//! assertion nothing checks — delete a disk by hand and it still claims the disk
//! exists — so instead the names are a function of `--run`, every step asks the
//! rack what is already there, and resuming is re-running the identical command.
//!
//! Validation happens here, before anything is created, and it validates the
//! *derived* names rather than the one that was typed. A long run name is legal
//! where `<run>-installer` is not, and finding that out after a forty-minute
//! upload is the failure this prevents.

use anyhow::{Context, Result};

/// The names of everything one run owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    run: String,
}

impl Names {
    /// Derive and validate. Fails if any derived name is one the rack would
    /// reject.
    pub fn new(run: &str) -> Result<Self> {
        let names = Self { run: run.to_string() };
        // The run name itself first, so the error names what the user typed rather
        // than a suffix they never wrote.
        check(&names.run)?;
        for derived in names.all() {
            check(&derived)?;
        }
        Ok(names)
    }

    pub fn run(&self) -> &str {
        &self.run
    }

    /// The uploaded install media. Pinned as the instance's boot disk.
    pub fn installer_disk(&self) -> String {
        format!("{}-installer", self.run)
    }

    /// The blank disk Windows installs onto, and the one that gets snapshotted.
    pub fn system_disk(&self) -> String {
        format!("{}-system", self.run)
    }

    /// The temporary machine that installs and then generalizes itself.
    pub fn instance(&self) -> String {
        self.run.clone()
    }

    pub fn snapshot(&self) -> String {
        format!("{}-snap", self.run)
    }

    /// The product. An image and an instance are different resource types, so the
    /// plain run name is free for the thing someone will actually use later.
    pub fn image(&self) -> String {
        self.run.clone()
    }

    /// `--verify-clone` only: a disk made from the finished image.
    pub fn clone_disk(&self) -> String {
        format!("{}-clone", self.run)
    }

    pub fn clone_instance(&self) -> String {
        format!("{}-clone", self.run)
    }

    fn all(&self) -> Vec<String> {
        vec![
            self.installer_disk(),
            self.system_disk(),
            self.instance(),
            self.snapshot(),
            self.image(),
            self.clone_disk(),
            self.clone_instance(),
        ]
    }
}

/// Legal to the control plane, judged by the control plane's own parser.
///
/// Not a regex of ours: a second opinion about what a name may contain is a second
/// thing to keep in step with the API, and it would be wrong in exactly the cases
/// nobody tests.
fn check(name: &str) -> Result<()> {
    name.parse::<oxide::types::Name>()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("{name:?} is not a usable name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resume is re-running the same command, which only works if every name is a
    /// function of the run name rather than something remembered.
    #[test]
    fn every_name_derives_from_the_run_name() {
        let n = Names::new("ws2022-golden").unwrap();
        assert_eq!(n.installer_disk(), "ws2022-golden-installer");
        assert_eq!(n.system_disk(), "ws2022-golden-system");
        assert_eq!(n.instance(), "ws2022-golden");
        assert_eq!(n.snapshot(), "ws2022-golden-snap");
        // The image is the product of the run, so it gets the plain name. An
        // instance and an image are different resource types and may share one.
        assert_eq!(n.image(), "ws2022-golden");
        assert_eq!(n.clone_disk(), "ws2022-golden-clone");
        assert_eq!(n.clone_instance(), "ws2022-golden-clone");
    }

    /// Every derived name must be a legal control-plane name, not just the one the
    /// user typed. A long run name produces a longer disk name, and the rejection
    /// would otherwise arrive forty minutes in, after the upload.
    #[test]
    fn a_run_name_whose_derivatives_are_illegal_is_refused_up_front() {
        let long = "a".repeat(60);
        // The run name itself is legal.
        assert!(long.parse::<oxide::types::Name>().is_ok());
        let error = Names::new(&long).unwrap_err().to_string();
        assert!(
            error.contains("-installer"),
            "the error must name the derived name that is too long: {error}"
        );
    }

    #[test]
    fn an_illegal_run_name_is_refused() {
        for bad in ["", "Has-Capitals", "ends-with-", "has spaces", "1-leading"]
        {
            assert!(
                Names::new(bad).is_err(),
                "{bad:?} is not a legal name and must be refused"
            );
        }
    }

    /// A legal name is one the SDK will accept, so the check has to be the SDK's
    /// own parse rather than a regex of ours that drifts from it.
    #[test]
    fn a_legal_run_name_is_accepted() {
        for good in ["ws2022", "ws2022-golden", "a", "a-1-b"] {
            assert!(Names::new(good).is_ok(), "{good:?} should be legal");
        }
    }
}
