// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! User selection and image options.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Is this image for one specific machine, or a template many machines clone?
///
/// This is not just a hostname switch. A golden image is sysprepped then copied, so anything
/// unique baked into it stops being unique the moment it is cloned, which is why
/// [`Credentials`] is constrained by this choice rather than sitting beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Deployment {
    /// A template meant to be cloned.
    ///
    /// Sets `<ComputerName>*</ComputerName>`, so Setup picks a random name instead of a
    /// fixed one. **That is all it does, and on its own it is not enough.** `*` is
    /// resolved once, during `specialize`; a clone of the resulting disk keeps the name
    /// that was picked, and the machine SID with it. Windows re-runs `specialize`, and
    /// so re-resolves `*` only after `sysprep /generalize`.
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

/// Creds for how the first administrator signs in.
///
/// A password is not optional on Windows. The serial console (SAC) and Remote Desktop
///  both authenticate with a password and have no notion of an SSH key, so a key-only
/// account produces a machine reachable over SSH and nowhere else, including from the
/// serial console, which is the one way in when something has gone wrong. Perhaps
/// eventually worth allowing that if cloudinit works well, and users request it.
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

/// Which Windows release.
///
/// Not a preference: the media says which of these it is, and [`crate::media::inspect`]
/// reads it. A user-asserted release is unchecked by anything, and picking the wrong one
/// fails the way everything here fails, the build succeeds and the install looks fine,
/// with the hardware-check bypasses in the wrong state and server edition names on client
/// media. Server 2012 R2 and below use a different setup system, and struggle more with
/// NVMe. They are also EOL, currently they are not targetted for support. 2016 goes EOL
/// Jan 2027 anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowsRelease {
    Server2016,
    Server2019,
    Server2022,
    Server2025,
    Windows10,
    Windows11,
}

impl WindowsRelease {
    /// The builder's `--windows=` token.
    pub fn token(self) -> &'static str {
        match self {
            WindowsRelease::Server2016 => "ws2016",
            WindowsRelease::Server2019 => "ws2019",
            WindowsRelease::Server2022 => "ws2022",
            WindowsRelease::Server2025 => "ws2025",
            WindowsRelease::Windows10 => "win10",
            WindowsRelease::Windows11 => "win11",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WindowsRelease::Server2016 => "Windows Server 2016",
            WindowsRelease::Server2019 => "Windows Server 2019",
            WindowsRelease::Server2022 => "Windows Server 2022",
            WindowsRelease::Server2025 => "Windows Server 2025",
            WindowsRelease::Windows10 => "Windows 10",
            WindowsRelease::Windows11 => "Windows 11",
        }
    }

    /// Whether we have actually booted this on an Oxide rack. The UI should say
    /// so rather than implying equal support.
    ///
    /// Kept in step with the hardware table in `TESTED-MEDIA.md`, which is the record.
    /// Server 2025 and Windows 11 are absent deliberately: both build correctly and
    /// neither has installed on a rack, blocked on propolis#1199 and on an NVMe
    /// controller fault on the rack itself. Server 2016 is absent because it has NVMe
    /// controller issues, and currently isnt fully supported.
    pub fn verified_on_hardware(self) -> bool {
        matches!(
            self,
            WindowsRelease::Server2019
                | WindowsRelease::Server2022
                | WindowsRelease::Windows10
        )
    }

    /// Client (Windows 10/11) rather than server.
    ///
    /// The media's own signal for this is `PRODUCTTYPE`:`WinNT` against `ServerNT`,
    /// never a substring of an edition name, which is a marketing string and translated
    /// on localised media.
    pub fn is_client(self) -> bool {
        matches!(self, WindowsRelease::Windows10 | WindowsRelease::Windows11)
    }

    /// virtio-win's directory name for this release, and the key into the payload
    /// manifest's `drivers` map.
    ///
    /// The one place this mapping lives. It used to exist twice: a `match` in
    /// `builder.rs` whose `_` arm handed 2k22 drivers to everything that was not Windows
    /// 11, and a field in `unattend::Target` that nothing read. `tools/fetch-payload.sh`
    /// fetches exactly these names, and a test asserts the embedded payload carries
    /// every one of them.
    ///
    /// In virtio-win 0.1.285 `2k16` is byte-identical to `2k19`, `2k22` and `w10` — 38
    /// files, same names, same hashes.
    pub fn driver_dir(self) -> &'static str {
        match self {
            WindowsRelease::Server2016 => "2k16",
            WindowsRelease::Server2019 => "2k19",
            WindowsRelease::Server2022 => "2k22",
            WindowsRelease::Server2025 => "2k25",
            WindowsRelease::Windows10 => "w10",
            WindowsRelease::Windows11 => "w11",
        }
    }

    /// The `BUILD` values this release's media reports.
    ///
    /// These are *base* builds, not patch levels: the Windows 10 media whose filename
    /// says 19045 reports 19041, so this can never be the number a filename implies.
    ///
    /// Windows 11 has several because each annual release bumps it, and 26100 appears
    /// here *and* under Server 2025: the same build number ships as both, which is why
    /// nothing may key on the build alone. `is_client` is what separates them.
    pub fn base_builds(self) -> &'static [u32] {
        match self {
            WindowsRelease::Server2016 => &[14393],
            WindowsRelease::Server2019 => &[17763],
            WindowsRelease::Server2022 => &[20348],
            WindowsRelease::Server2025 => &[26100],
            WindowsRelease::Windows10 => &[10240, 19041],
            WindowsRelease::Windows11 => &[22000, 22621, 26100, 26200],
        }
    }

    /// Every release this workspace knows how to build. Iterate this, never a literal
    /// list, so adding a variant cannot leave a table half-filled.
    pub const ALL: &'static [WindowsRelease] = &[
        WindowsRelease::Server2016,
        WindowsRelease::Server2019,
        WindowsRelease::Server2022,
        WindowsRelease::Server2025,
        WindowsRelease::Windows10,
        WindowsRelease::Windows11,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The `.iso` the user picked. Mounting it is the engine's problem.
    pub iso: PathBuf,
    pub release: WindowsRelease,
    /// Which image to install, as an index, an `EDITIONID` or part of a name. Resolved
    /// against the media's own image list by [`crate::wim::select_image`]. Empty means
    /// no preference, and [`crate::media::default_image`] chooses, which is the default,
    /// because `datacenter` is not an edition any client media carries.
    pub edition: String,
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
            edition: String::new(),
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
    pub(crate) fn block(
        field: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self { field, message: message.into(), blocking: true }
    }
    pub(crate) fn warn(
        field: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self { field, message: message.into(), blocking: false }
    }
}

impl Settings {
    /// The hint the image chooser gets.
    ///
    /// An image index, an `EDITIONID`, or any substring of a name, whatever the caller
    /// has. The GUI fills it with the index of the row picked out of the media's own
    /// image list, which is the only unambiguous selector: two images on server media
    /// share `ServerDatacenterEval`, differing only in Core-ness.
    pub fn edition_hint(&self) -> String {
        self.edition.trim().to_lowercase()
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

/// Dont want to make the system mad.
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

    /// A warning shown against a release somebody has actually installed is noise, and
    /// noise is how a real warning gets ignored. This exists so that moving a release
    /// between the two lists is a deliberate edit rather than something nobody notices.
    #[test]
    fn the_verified_list_matches_the_hardware_record() {
        use WindowsRelease::*;
        for release in [Server2019, Server2022, Windows10] {
            assert!(release.verified_on_hardware(), "{release:?}");
        }
        // Blocked on propolis#1199 and a rack NVMe fault — see TESTED-MEDIA.md.
        for release in [Server2025, Windows11] {
            assert!(!release.verified_on_hardware(), "{release:?}");
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

    /// The hint is passed through untouched apart from case and whitespace.
    ///
    /// It used to have `core` appended here when a separate Desktop/Core switch was set,
    /// while `Config::from_settings` appended `-core` for the same fact, two spellings
    /// of one thing. Core-ness now comes from the image chosen out of the media's own
    /// list, so there is nothing left to append and nothing left to disagree about.
    #[test]
    fn the_edition_hint_is_the_edition_verbatim() {
        // Empty by default: no preference, resolved against the media rather than
        // asserted against a table.
        assert_eq!(base().edition_hint(), "");

        let standard = Settings { edition: "  Standard ".into(), ..base() };
        assert_eq!(standard.edition_hint(), "standard");

        // An index is the selector the GUI actually sends, and it must survive intact:
        // `wim::select_image` treats an all-digits hint as an image index.
        let by_index = Settings { edition: "3".into(), ..base() };
        assert_eq!(by_index.edition_hint(), "3");
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
