// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! What the user chose, independent of how it is rendered or how it is built.
//!
//! This is the whole reason the core crate exists: the GUI, the eventual CLI, and
//! the byte-compare test harness all need to agree on what a build *is*, and none
//! of them should own that definition.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Is this image for one specific machine, or a template many machines clone?
///
/// This is not just a hostname switch. A golden image is copied, so anything
/// unique baked into it stops being unique the moment it is cloned — which is why
/// [`Credentials`] is constrained by this choice rather than sitting beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Deployment {
    /// Sysprep-style template. Windows generates a random name on each clone.
    GoldenImage,
    /// One machine that keeps the name it is given.
    Named { hostname: String },
}

impl Deployment {
    /// What to hand the builder as `--name`. `*` is Windows' own "pick a random
    /// name" token, which is exactly the golden-image requirement.
    pub fn computer_name(&self) -> &str {
        match self {
            Deployment::GoldenImage => "*",
            Deployment::Named { hostname } => hostname,
        }
    }

    pub fn is_golden(&self) -> bool {
        matches!(self, Deployment::GoldenImage)
    }
}

/// How the first administrator signs in.
///
/// A password is not optional on Windows, however much we might prefer keys. The
/// serial console (SAC) and Remote Desktop both authenticate with a password and
/// have no notion of an SSH key, so a key-only account produces a machine reachable
/// over SSH and nowhere else — including from the serial console, which is the one
/// way in when something has gone wrong. SSH keys are therefore additive: they make
/// SSH passwordless, they do not replace the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    /// Optional. Authorised for SSH in addition to the password.
    pub keys: Vec<String>,
}

impl Default for Credentials {
    fn default() -> Self {
        Self {
            username: "oxide".into(),
            // Deliberately empty: there is no default password anywhere in this
            // crate, so an image can never be built with a shared secret nobody
            // chose. The UI offers to generate one.
            password: String::new(),
            keys: Vec::new(),
        }
    }
}

/// A random password strong enough for a Windows administrator account, satisfying
/// the default complexity policy (upper, lower, digit, symbol).
///
/// Symbols are restricted to characters that survive being written into
/// `autounattend.xml` and typed at a SAC prompt without quoting surprises.
pub fn generate_password() -> String {
    use rand::seq::{IndexedRandom, SliceRandom};
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnpqrstuvwxyz";
    const DIGIT: &[u8] = b"23456789";
    const SYMBOL: &[u8] = b"!@#$%^*-_=+?";

    let mut rng = rand::rng();
    let mut chars: Vec<u8> = Vec::with_capacity(20);
    // One from each class first, so complexity is guaranteed rather than likely.
    for class in [UPPER, LOWER, DIGIT, SYMBOL] {
        chars.push(*class.choose(&mut rng).expect("non-empty"));
    }
    let all: Vec<u8> = [UPPER, LOWER, DIGIT, SYMBOL].concat();
    while chars.len() < 20 {
        chars.push(*all.choose(&mut rng).expect("non-empty"));
    }
    chars.shuffle(&mut rng);
    String::from_utf8(chars).expect("ascii")
}

/// Server media ships each edition twice: with a desktop and without one.
///
/// This is a real choice, not a detail, and it must be explicit. The image names
/// mark only the Core variants (`SERVERDATACENTERCORE`) and leave the Desktop
/// Experience ones unmarked (`SERVERDATACENTER`), so any loose match on
/// "datacenter" finds Core first and installs a server with no desktop at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Experience {
    /// The full GUI. What almost everyone means by "Windows Server", and what you
    /// need if you intend to use Remote Desktop for anything.
    Desktop,
    /// No GUI: command line and remote management only.
    Core,
}

impl Experience {
    pub fn label(self) -> &'static str {
        match self {
            Experience::Desktop => "Desktop Experience",
            Experience::Core => "Server Core",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Experience::Desktop => {
                "Full graphical desktop. Needed for Remote Desktop to be useful."
            }
            Experience::Core => {
                "No desktop. Smaller and patched less often, but managed only from a command line."
            }
        }
    }

    pub const ALL: &'static [Experience] =
        &[Experience::Desktop, Experience::Core];
}

/// Which Windows release. Only Server 2022 is verified on real hardware today;
/// the others are listed so the shape of the enum does not have to change when
/// they are tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowsRelease {
    Server2022,
    Server2025,
    Windows11,
}

impl WindowsRelease {
    /// The builder's `--windows=` token.
    pub fn token(self) -> &'static str {
        match self {
            WindowsRelease::Server2022 => "ws2022",
            WindowsRelease::Server2025 => "ws2025",
            WindowsRelease::Windows11 => "win11",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WindowsRelease::Server2022 => "Windows Server 2022",
            WindowsRelease::Server2025 => "Windows Server 2025",
            WindowsRelease::Windows11 => "Windows 11",
        }
    }

    /// Whether we have actually booted this on an Oxide rack. The UI should say
    /// so rather than implying equal support.
    pub fn verified_on_hardware(self) -> bool {
        matches!(self, WindowsRelease::Server2022)
    }

    /// What the UI offers. Only Server 2022 for now — the other variants stay
    /// defined so the plumbing is ready, but offering an untested release as an
    /// equal choice invites someone to pick it and hit problems nobody has seen.
    /// Add them here once each has installed on a rack.
    pub const ALL: &'static [WindowsRelease] = &[WindowsRelease::Server2022];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The `.iso` the user picked. Mounting it is the engine's problem.
    pub iso: PathBuf,
    pub release: WindowsRelease,
    /// Edition selector passed through to the WIM image chooser, e.g. `datacenter`.
    pub edition: String,
    /// With or without a desktop. Combined with `edition` to pick the WIM image.
    pub experience: Experience,
    pub deployment: Deployment,
    pub credentials: Credentials,
    pub enable_ssh: bool,
    pub enable_rdp: bool,
    pub inject_drivers: bool,
    pub enable_serial_console: bool,
    /// A retail/volume key, or `None` for evaluation media (which rejects keys).
    pub product_key: Option<String>,
    /// Disk index Setup installs onto. 1 = the second disk, because disk 0 is our
    /// own install media.
    pub target_disk: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            iso: PathBuf::new(),
            release: WindowsRelease::Server2022,
            edition: "datacenter".into(),
            experience: Experience::Desktop,
            deployment: Deployment::GoldenImage,
            credentials: Credentials::default(),
            enable_ssh: true,
            enable_rdp: true,
            inject_drivers: true,
            enable_serial_console: true,
            product_key: None,
            target_disk: 1,
        }
    }
}

/// A reason the current settings cannot be built, or a caution about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub field: &'static str,
    pub message: String,
    pub blocking: bool,
}

impl Problem {
    fn block(field: &'static str, message: impl Into<String>) -> Self {
        Self { field, message: message.into(), blocking: true }
    }
    fn warn(field: &'static str, message: impl Into<String>) -> Self {
        Self { field, message: message.into(), blocking: false }
    }
}

impl Settings {
    /// The hint the image chooser gets: the edition, with `core` appended when the
    /// Core variant is wanted. Built here rather than in the engine so the GUI, the
    /// CLI and the tests cannot disagree about it.
    pub fn edition_hint(&self) -> String {
        match self.experience {
            Experience::Desktop => self.edition.trim().to_lowercase(),
            Experience::Core => {
                format!("{}core", self.edition.trim().to_lowercase())
            }
        }
    }

    /// Everything wrong with these settings. Blocking problems stop the build;
    /// non-blocking ones are shown but do not gate it.
    pub fn problems(&self) -> Vec<Problem> {
        let mut v = Vec::new();

        if self.iso.as_os_str().is_empty() {
            v.push(Problem::block("iso", "Choose a Windows ISO."));
        }

        if let Deployment::Named { hostname } = &self.deployment {
            v.extend(hostname_problem(hostname));
        }

        let Credentials { username, password, keys } = &self.credentials;

        if username.trim().is_empty() {
            v.push(Problem::block("username", "Enter a username."));
        } else if is_reserved_username(username) {
            v.push(Problem::block(
                "username",
                format!(
                    "{username:?} is reserved by Windows; pick another name."
                ),
            ));
        }
        if password.is_empty() {
            v.push(Problem::block("password", "Set a password."));
        } else if password.len() < 8 {
            v.push(Problem::block(
                "password",
                "Windows' default policy needs at least 8 characters.",
            ));
        }
        // The password lands in autounattend.xml as cleartext on media that stays
        // attached to the instance, so this is worth saying regardless; on a golden
        // image it is worse, because every clone inherits the same one.
        if self.deployment.is_golden() && !password.is_empty() {
            v.push(Problem::warn(
                "credentials",
                "Every machine cloned from this image will share this password, and it \
                 is stored in cleartext on the install media. Change it after the first \
                 boot, or build a separate image per machine.",
            ));
        }
        for k in keys {
            if let Some(p) = key_problem(k) {
                v.push(p);
            }
        }

        if !self.enable_ssh && !self.enable_rdp && !self.enable_serial_console {
            v.push(Problem::block(
                "access",
                "With SSH, RDP and the serial console all off there is no way to reach the guest.",
            ));
        }

        if self.enable_rdp {
            v.push(Problem::warn(
                "enable_rdp",
                "RDP also needs a VPC firewall rule for tcp/3389 — the default VPC allows \
                 only SSH and ICMP, so enabling it here is necessary but not sufficient.",
            ));
        }

        if !self.inject_drivers {
            v.push(Problem::warn(
                "inject_drivers",
                "Without the virtio drivers the guest will install but have no network.",
            ));
        }

        if !self.release.verified_on_hardware() {
            v.push(Problem::warn(
                "release",
                format!(
                    "{} has not been verified on an Oxide rack yet.",
                    self.release.label()
                ),
            ));
        }

        if let Some(key) = &self.product_key {
            if !looks_like_product_key(key) {
                v.push(Problem::block(
                    "product_key",
                    "A product key is 5 groups of 5 characters, e.g. XXXXX-XXXXX-XXXXX-XXXXX-XXXXX.",
                ));
            }
        }

        v
    }

    pub fn is_buildable(&self) -> bool {
        !self.problems().iter().any(|p| p.blocking)
    }
}

/// NetBIOS rules, which are stricter than DNS: 15 characters, no dots, and a
/// short list of forbidden punctuation.
fn hostname_problem(hostname: &str) -> Vec<Problem> {
    let mut v = Vec::new();
    if hostname.trim().is_empty() {
        v.push(Problem::block("hostname", "Enter a hostname."));
        return v;
    }
    if hostname.len() > 15 {
        v.push(Problem::block(
            "hostname",
            format!(
                "{} characters; Windows truncates names over 15.",
                hostname.len()
            ),
        ));
    }
    if hostname.contains('.') {
        v.push(Problem::block(
            "hostname",
            "A computer name cannot contain dots.",
        ));
    }
    if hostname.chars().any(|c| r#"\/:*?"<>|,~!@#$%^&'(){}_ "#.contains(c)) {
        v.push(Problem::block(
            "hostname",
            "Use only letters, digits and hyphens.",
        ));
    }
    if hostname.chars().all(|c| c.is_ascii_digit()) {
        v.push(Problem::block(
            "hostname",
            "A computer name cannot be all digits.",
        ));
    }
    v
}

fn is_reserved_username(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "administrator",
        "guest",
        "system",
        "network service",
        "local service",
        "defaultaccount",
        "wdagutilityaccount",
        "public",
    ];
    RESERVED.contains(&name.trim().to_ascii_lowercase().as_str())
}

fn key_problem(key: &str) -> Option<Problem> {
    let key = key.trim();
    // Anything else is a private key, a filename, or a paste accident — all of
    // which would otherwise fail silently inside the guest.
    const PREFIXES: &[&str] = &[
        "ssh-rsa ",
        "ssh-ed25519 ",
        "ecdsa-sha2-nistp256 ",
        "ecdsa-sha2-nistp384 ",
        "ecdsa-sha2-nistp521 ",
        "sk-ssh-ed25519@openssh.com ",
        "sk-ecdsa-sha2-nistp256@openssh.com ",
    ];
    if PREFIXES.iter().any(|p| key.starts_with(p)) {
        return None;
    }
    if key.contains("PRIVATE KEY") {
        return Some(Problem::block(
            "credentials",
            "That is a private key. Use the .pub file.",
        ));
    }
    Some(Problem::block(
        "credentials",
        "Not an SSH public key — it should start with ssh-ed25519 or ssh-rsa.",
    ))
}

fn looks_like_product_key(key: &str) -> bool {
    let groups: Vec<&str> = key.trim().split('-').collect();
    groups.len() == 5
        && groups.iter().all(|g| {
            g.len() == 5 && g.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Settings {
        Settings {
            iso: PathBuf::from("/tmp/x.iso"),
            credentials: Credentials {
                username: "oxide".into(),
                password: "0xide!230xide!23".into(),
                keys: vec!["ssh-ed25519 AAAA test".into()],
            },
            ..Default::default()
        }
    }

    #[test]
    fn golden_image_gets_a_random_computer_name() {
        assert_eq!(Deployment::GoldenImage.computer_name(), "*");
    }

    #[test]
    fn defaults_carry_no_password() {
        assert!(Settings::default().credentials.password.is_empty());
        assert!(!Settings::default().is_buildable());
    }

    #[test]
    fn a_complete_setup_is_buildable() {
        assert!(base().is_buildable(), "{:?}", base().problems());
    }

    #[test]
    fn no_iso_blocks() {
        let s = Settings { iso: PathBuf::new(), ..base() };
        assert!(!s.is_buildable());
    }

    /// Keys are additive on Windows, never a substitute: SAC and RDP authenticate
    /// with a password and know nothing about SSH keys, so an account with keys and
    /// no password can only be reached over SSH.
    #[test]
    fn keys_without_a_password_still_blocks() {
        let s = Settings {
            credentials: Credentials {
                password: String::new(),
                ..base().credentials
            },
            ..base()
        };
        assert!(!s.is_buildable());
        assert!(
            s.problems().iter().any(|p| p.field == "password" && p.blocking)
        );
    }

    #[test]
    fn no_keys_at_all_is_fine() {
        let s = Settings {
            credentials: Credentials { keys: vec![], ..base().credentials },
            ..base()
        };
        assert!(s.is_buildable(), "{:?}", s.problems());
    }

    #[test]
    fn private_key_paste_is_caught() {
        let s = Settings {
            credentials: Credentials {
                keys: vec!["-----BEGIN OPENSSH PRIVATE KEY-----".into()],
                ..base().credentials
            },
            ..base()
        };
        assert!(s.problems().iter().any(|p| p.message.contains("private key")));
    }

    #[test]
    fn a_golden_image_warns_that_clones_share_the_password() {
        let s = Settings { deployment: Deployment::GoldenImage, ..base() };
        assert!(s.is_buildable());
        assert!(
            s.problems()
                .iter()
                .any(|p| !p.blocking && p.field == "credentials")
        );
    }

    #[test]
    fn short_password_blocks() {
        let s = Settings {
            credentials: Credentials {
                password: "short".into(),
                ..base().credentials
            },
            ..base()
        };
        assert!(!s.is_buildable());
    }

    #[test]
    fn reserved_username_blocks() {
        let s = Settings {
            credentials: Credentials {
                username: "Administrator".into(),
                ..base().credentials
            },
            ..base()
        };
        assert!(!s.is_buildable());
    }

    #[test]
    fn overlong_hostname_blocks() {
        let s = Settings {
            deployment: Deployment::Named {
                hostname: "this-name-is-far-too-long".into(),
            },
            ..base()
        };
        assert!(!s.is_buildable());
    }

    #[test]
    fn all_access_off_blocks() {
        let s = Settings {
            enable_ssh: false,
            enable_rdp: false,
            enable_serial_console: false,
            ..base()
        };
        assert!(!s.is_buildable());
    }

    #[test]
    fn generated_passwords_meet_windows_complexity() {
        for _ in 0..50 {
            let p = generate_password();
            assert_eq!(p.len(), 20);
            assert!(p.chars().any(|c| c.is_ascii_uppercase()), "{p}");
            assert!(p.chars().any(|c| c.is_ascii_lowercase()), "{p}");
            assert!(p.chars().any(|c| c.is_ascii_digit()), "{p}");
            assert!(p.chars().any(|c| !c.is_ascii_alphanumeric()), "{p}");
            // Nothing that would need escaping in XML or confuse a SAC prompt.
            assert!(!p.contains(['<', '>', '&', '"', '\'', ' ']), "{p}");
        }
    }

    /// The bug this guards: "datacenter" is a substring of "SERVERDATACENTERCORE",
    /// so a loose match finds Core first. One build shipped Server Core to a rack
    /// because of it. Desktop is the default and must stay unmarked.
    #[test]
    fn desktop_is_the_default_and_core_is_explicit() {
        let s = base();
        assert_eq!(s.experience, Experience::Desktop);
        assert_eq!(s.edition_hint(), "datacenter");

        let core = Settings { experience: Experience::Core, ..base() };
        assert_eq!(core.edition_hint(), "datacentercore");

        let standard = Settings { edition: "Standard".into(), ..base() };
        assert_eq!(standard.edition_hint(), "standard");
    }

    #[test]
    fn generated_passwords_differ() {
        assert_ne!(generate_password(), generate_password());
    }

    #[test]
    fn malformed_product_key_blocks() {
        let s = Settings { product_key: Some("ABC-DEF".into()), ..base() };
        assert!(!s.is_buildable());
    }
}
