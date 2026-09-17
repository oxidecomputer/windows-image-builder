// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Which racks this machine is logged into.
//!
//! Read out of the same `credentials.toml` that `oxide auth login` writes, because
//! authenticating is something we get free with cli: it involves a browser, a device
//! code and a token with a lifetime, and every one of those is a thing the `oxide` CLI
//! already does properly. We read the result.
//!
//! **No token ever leaves this module.** [`Profile`] carries the name, host, user and
//! expiry and nothing else, so a token cannot reach a log line, a progress event, a
//! panic message or a GUI label by accident. Constructing a client is done by handing
//! the SDK a [`Selector`]. A profile name, or nothing at all; and letting it read the
//! credentials itself. There is a test that the token is absent from `Profile`'s own
//! `Debug` output, because `{:?}` on a config struct is exactly how a secret ends up in
//! a bug report.
//!
//! A login can also come from `OXIDE_HOST` and `OXIDE_TOKEN`, which is how the `oxide`
//! CLI is driven in CI. Read [`Selector`] before touching that path: naming a profile
//! disables it.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// How to point the SDK at a login.
///
/// This is an enum rather than a `String` because of a trap in the SDK's own resolution
/// order: `OXIDE_TOKEN` is consulted **only when no profile was named** (`auth.rs`,
/// `if let (None, Ok(env_token)) = (profile, var("OXIDE_TOKEN"))`). So passing a profile
/// name unconditionally, the obvious implementation, silently disables environment
/// credentials, and the CI job that was working stops working with a message about a
/// missing config file. Making "name none at all" a distinct variant means a caller
/// cannot express that mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Name this profile explicitly. Beats anything in the environment.
    Profile(String),
    /// Name nothing and let the SDK resolve `OXIDE_TOKEN` / `OXIDE_HOST`.
    Environment,
}

/// One logged-in rack, minus the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// What to hand the SDK to use this login.
    pub selector: Selector,
    /// Display name. For an environment login there is no `[profile.<name>]` key, so
    /// this is a label and **not** something to pass to the SDK, that is what
    /// [`Profile::selector`] is for.
    pub name: String,
    /// Rack hostname, shown so a user with several can tell them apart.
    pub host: String,
    /// The account the token belongs to, when the file records one. `None` for an
    /// environment login: `OXIDE_TOKEN` carries no identity, and finding out whose it is
    /// would mean an API call this crate has no business making just to draw a list.
    pub user: Option<String>,
    /// Whether `config.toml` names this one as the default.
    pub is_default: bool,
    /// RFC 3339, verbatim from the file. `None` means the file records no expiry, which
    /// is normal for a long-lived token.
    pub expires: Option<String>,
}

impl Profile {
    /// Whether the token had expired as of `now`.
    ///
    /// `now` is a parameter rather than a call to the clock so that this is testable
    /// without waiting a year, and so the caller decides what "now" means. An expired
    /// token does not fail loudly, it produces a 401 on the first real request, which
    /// reads as the rack being broken rather than as a login having lapsed.
    pub fn is_expired(
        &self,
        now: chrono::DateTime<chrono::FixedOffset>,
    ) -> bool {
        let Some(raw) = &self.expires else { return false };
        // An unparseable timestamp is treated as *not* expired: refusing to use a
        // working token because a field this crate barely understands is in an
        // unexpected format would be worse than the 401 it is trying to pre-empt.
        match chrono::DateTime::parse_from_rfc3339(raw) {
            Ok(at) => at <= now,
            Err(_) => false,
        }
    }

    /// [`Profile::is_expired`] against the wall clock.
    ///
    /// Exists so that no caller needs `chrono` in its own dependency tree merely to ask
    /// whether a login has lapsed, the same reason the async runtime stays behind this
    /// crate's boundary.
    pub fn is_expired_now(&self) -> bool {
        self.is_expired(chrono::Utc::now().fixed_offset())
    }
}

/// Where the `oxide` CLI keeps its configuration.
///
/// `$OXIDE_CONFIG_DIR` first, matching the CLI, so a test or a CI job can point this
/// somewhere harmless without touching a real login.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("OXIDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = home_dir().context(
        "no home directory, so the oxide credentials file cannot be found. Set \
         OXIDE_CONFIG_DIR to point at it",
    )?;
    Ok(home.join(".config").join("oxide"))
}

fn home_dir() -> Option<PathBuf> {
    // `USERPROFILE` on Windows, `HOME` elsewhere. Deliberately not a dependency: this is
    // the whole of what the `dirs` crate would be used for here.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Every profile `oxide auth login` has written, default first and then alphabetical.
///
/// An absent credentials file is an empty list rather than an error.
pub fn profiles() -> Result<Vec<Profile>> {
    let mut all: Vec<Profile> = environment_profile().into_iter().collect();
    all.extend(profiles_in(&config_dir()?)?);
    Ok(all)
}

/// A login supplied by `OXIDE_HOST` and `OXIDE_TOKEN` rather than by a file.
///
/// This is how the `oxide` CLI is driven in CI, and without it a machine that is
/// perfectly able to reach a rack would be told it is not logged in, the file is
/// absent, because on such a machine nobody ever ran `oxide auth login`.
///
/// Listed first, because it is what the SDK will actually use when no profile is named.
/// The token is checked for presence and never read into the returned value.
pub fn environment_profile() -> Option<Profile> {
    environment_profile_from(|name| {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    })
}

/// [`environment_profile`], against an arbitrary lookup.
///
/// The environment is process-global and Rust runs tests in threads, so a test that set
/// `OXIDE_TOKEN` would be visible to every other test in the binary. Injecting the
// lookup keeps this testable and clean, the same way [`Profile::is_expired`] takes `now`.
pub fn environment_profile_from(
    get: impl Fn(&str) -> Option<String>,
) -> Option<Profile> {
    let host = get("OXIDE_HOST")?;
    // Both are required together. A token with no host cannot be used, and the SDK
    // reports that as `MissingHost` rather than as "you set half of it".
    get("OXIDE_TOKEN")?;
    Some(Profile {
        selector: Selector::Environment,
        name: "OXIDE_TOKEN".into(),
        host,
        user: None,
        // Nothing else can be default while these are set: the SDK reaches them first.
        is_default: true,
        // The environment carries no expiry. An expired token here fails at the first
        // request, and there is nothing we could have read to predict it.
        expires: None,
    })
}

/// [`profiles`], against an explicit directory. Public so the CLI can be pointed at one
/// and so the tests need no real login.
pub fn profiles_in(dir: &Path) -> Result<Vec<Profile>> {
    let path = dir.join("credentials.toml");
    let Some(raw) = read_if_present(&path)? else {
        return Ok(Vec::new());
    };
    let credentials: oxide::CredentialsFile = toml::from_str(&raw)
        .with_context(|| format!("{} is not valid TOML", path.display()))?;

    let default = default_profile(dir)?;
    let mut profiles: Vec<Profile> = credentials
        .profile
        .into_iter()
        .map(|(name, creds)| Profile {
            is_default: default.as_deref() == Some(name.as_str()),
            selector: Selector::Profile(name.clone()),
            name,
            host: creds.host,
            user: Some(creds.user),
            expires: creds.time_expires,
        })
        .collect();

    // The default first, because it is the one that will be used if nobody chooses, and
    // the rest alphabetically so the order does not depend on a hash.
    profiles.sort_by(|a, b| {
        b.is_default.cmp(&a.is_default).then_with(|| a.name.cmp(&b.name))
    });
    Ok(profiles)
}

/// The profile the CLI would use if none is named.
fn default_profile(dir: &Path) -> Result<Option<String>> {
    let path = dir.join("config.toml");
    let Some(raw) = read_if_present(&path)? else {
        return Ok(None);
    };
    // A malformed config.toml costs us only the "which one is default" marker, so it is
    // not worth failing the whole listing over, the credentials are what matter.
    Ok(toml::from_str::<oxide::BasicConfigFile>(&raw)
        .ok()
        .and_then(|config| config.default_profile))
}

/// `None` for a missing file, an error for one that exists and cannot be read.
///
/// Collapsing those two into `None` would turn a permissions problem into a silent
/// "you are not logged in", and the user would go round the `oxide auth login` loop
/// forever without being told why it did not take.
fn read_if_present(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory laid out the way `oxide auth login` leaves one.
    fn write_config(credentials: &str, config: Option<&str>) -> tempdir::Dir {
        let dir = tempdir::Dir::new();
        std::fs::write(dir.path().join("credentials.toml"), credentials)
            .unwrap();
        if let Some(config) = config {
            std::fs::write(dir.path().join("config.toml"), config).unwrap();
        }
        dir
    }

    const TWO_PROFILES: &str = r#"
[profile.staging]
host = "https://staging.example.com"
token = "oxide-token-SECRET-staging"
token_id = "1de6e8f6-0000-0000-0000-000000000000"
user = "someone@example.com"

[profile.rack2]
host = "https://rack2.example.com"
token = "oxide-token-SECRET-rack2"
token_id = "2de6e8f6-0000-0000-0000-000000000000"
user = "someone@example.com"
"#;

    #[test]
    fn reads_the_profiles_the_cli_wrote() {
        let dir = write_config(TWO_PROFILES, None);
        let profiles = profiles_in(dir.path()).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].name, "rack2");
        assert_eq!(profiles[0].host, "https://rack2.example.com");
        assert_eq!(profiles[0].user.as_deref(), Some("someone@example.com"));
        assert!(profiles.iter().all(|p| !p.is_default));
    }

    #[test]
    fn the_default_profile_sorts_first() {
        let dir =
            write_config(TWO_PROFILES, Some("default-profile = \"staging\"\n"));
        let profiles = profiles_in(dir.path()).unwrap();
        assert_eq!(profiles[0].name, "staging");
        assert!(profiles[0].is_default);
        assert!(!profiles[1].is_default);
    }

    /// `{:?}` on a config struct is how secrets reach bug reports. `Profile` has no
    /// token field at all, which is the only version of this that cannot regress into
    /// someone adding one "just for convenience".
    #[test]
    fn no_token_survives_into_a_profile() {
        let dir = write_config(TWO_PROFILES, None);
        let rendered = format!("{:?}", profiles_in(dir.path()).unwrap());
        assert!(
            !rendered.contains("SECRET"),
            "a token reached Profile's Debug output: {rendered}"
        );
        assert!(!rendered.contains("token"), "{rendered}");
    }

    /// Not being logged in is an ordinary state, not a failure. The app still saves
    /// images to a file.
    #[test]
    fn a_missing_credentials_file_is_an_empty_list() {
        let dir = tempdir::Dir::new();
        assert_eq!(profiles_in(dir.path()).unwrap(), Vec::new());
    }

    #[test]
    fn a_corrupt_credentials_file_is_an_error() {
        let dir = write_config("this is not toml {{{", None);
        assert!(profiles_in(dir.path()).is_err());
    }

    /// A broken `config.toml` costs the default marker and nothing else.
    #[test]
    fn a_corrupt_config_file_still_lists_profiles() {
        let dir = write_config(TWO_PROFILES, Some("}}} not toml"));
        let profiles = profiles_in(dir.path()).unwrap();
        assert_eq!(profiles.len(), 2);
        assert!(profiles.iter().all(|p| !p.is_default));
    }

    fn at(rfc3339: &str) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap()
    }

    #[test]
    fn expiry_is_compared_against_the_supplied_instant() {
        let profile = |expires: Option<&str>| Profile {
            selector: Selector::Profile("p".into()),
            name: "p".into(),
            host: "h".into(),
            user: Some("u".into()),
            is_default: false,
            expires: expires.map(str::to_string),
        };
        let now = at("2026-08-27T12:00:00Z");

        assert!(profile(Some("2026-08-27T11:59:59Z")).is_expired(now));
        assert!(!profile(Some("2026-08-27T12:00:01Z")).is_expired(now));
        // No expiry recorded is a long-lived token, not an expired one.
        assert!(!profile(None).is_expired(now));
        // Expiring exactly now counts as expired.
        assert!(profile(Some("2026-08-27T13:00:00+01:00")).is_expired(now));
        // Offsets are respected rather than string-compared. `11:30-01:00` is 12:30 UTC,
        // half an hour *after* now, so the token is still good,  while comparing the
        // strings would see "11:30" against "12:00" and call it expired.
        assert!(!profile(Some("2026-08-27T11:30:00-01:00")).is_expired(now));
        // And an unparseable value must not lock the user out of a working token.
        assert!(!profile(Some("whenever")).is_expired(now));
    }

    /// A CI box has no credentials file, so without this it would be told it is not
    /// logged in while being perfectly able to reach the rack.
    #[test]
    fn the_environment_is_a_login() {
        let env = |host: Option<&str>, token: Option<&str>| {
            environment_profile_from(move |name| match name {
                "OXIDE_HOST" => host.map(str::to_string),
                "OXIDE_TOKEN" => token.map(str::to_string),
                _ => None,
            })
        };

        let p = env(Some("https://rack.example.com"), Some("SECRET")).unwrap();
        assert_eq!(p.host, "https://rack.example.com");
        assert!(p.is_default, "the SDK reaches these before any file profile");
        assert_eq!(p.user, None);
        // And crucially: it must not name a profile, or the SDK stops consulting
        // OXIDE_TOKEN at all.
        assert_eq!(p.selector, Selector::Environment);
        assert!(
            !format!("{p:?}").contains("SECRET"),
            "the token reached the profile"
        );

        // Half a login is not a login. Either alone leaves the SDK unable to proceed,
        // and claiming otherwise would produce a picker entry that always fails.
        assert_eq!(env(Some("https://rack.example.com"), None), None);
        assert_eq!(env(None, Some("SECRET")), None);
        assert_eq!(env(None, None), None);
    }

    /// A scratch directory that removes itself. Small enough not to justify a
    /// dependency, and the tests above need nothing more.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                // The process id and a counter, rather than a random number: this needs
                // to be unique among concurrently running tests in one binary, which is
                // exactly what those two give.
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "oxwin-rack-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::SeqCst)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
