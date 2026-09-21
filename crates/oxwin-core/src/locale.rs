// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Region and time zone tables.
//!
//! "Locale" is not one axis, and treating it as one is why these live apart:
//!
//! * The **display language** is constrained by the media — it needs a language pack
//!   present in the image — so it is detected, not chosen from a table here. See
//!   [`crate::media::MediaInfo::languages`].
//! * The **region** drives `UserLocale`, `SystemLocale` and `InputLocale`, none of
//!   which depend on the media. Any tag works on any ISO.
//! * The **time zone** is a separate axis again, and takes a Windows time zone *ID*
//!   (`W. Europe Standard Time`), not an IANA name (`Europe/Berlin`). Windows
//!   ignores an unknown ID silently and leaves the guest on UTC, so this table is
//!   the only validation there is.
//!
//! Curated rather than exhaustive. The full BCP-47 space is a scrolling list nobody
//! can use; anyone who needs a tag that is not here supplies their own answer file,
//! which is what that escape hatch is for.

/// A region: one choice driving `UserLocale`, `SystemLocale` and `InputLocale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// The BCP-47 tag written into the answer file verbatim.
    pub tag: &'static str,
    /// What the picker shows.
    pub label: &'static str,
}

/// A Windows time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeZone {
    /// The Windows time zone ID, written into `<TimeZone>` verbatim.
    pub id: &'static str,
    pub label: &'static str,
}

/// What every committed golden was built with. Changing it moves all of them.
pub const DEFAULT_REGION: &str = "en-US";
/// UTC, and deliberately so: a fleet whose machines disagree about the clock is a
/// log-correlation problem nobody enjoys.
pub const DEFAULT_TIME_ZONE: &str = "UTC";

macro_rules! regions {
    ($($tag:literal => $label:literal),* $(,)?) => {
        pub const REGIONS: &[Region] =
            &[$(Region { tag: $tag, label: $label }),*];
    };
}

regions! {
    "en-US" => "English (United States)",
    "en-GB" => "English (United Kingdom)",
    "en-AU" => "English (Australia)",
    "en-CA" => "English (Canada)",
    "de-DE" => "German (Germany)",
    "fr-FR" => "French (France)",
    "fr-CA" => "French (Canada)",
    "es-ES" => "Spanish (Spain)",
    "es-MX" => "Spanish (Mexico)",
    "it-IT" => "Italian (Italy)",
    "pt-BR" => "Portuguese (Brazil)",
    "nl-NL" => "Dutch (Netherlands)",
    "sv-SE" => "Swedish (Sweden)",
    "nb-NO" => "Norwegian Bokmal (Norway)",
    "da-DK" => "Danish (Denmark)",
    "fi-FI" => "Finnish (Finland)",
    "pl-PL" => "Polish (Poland)",
    "cs-CZ" => "Czech (Czechia)",
    "tr-TR" => "Turkish (Turkiye)",
    "ru-RU" => "Russian (Russia)",
    "ja-JP" => "Japanese (Japan)",
    "ko-KR" => "Korean (Korea)",
    "zh-CN" => "Chinese (Simplified, China)",
    "zh-TW" => "Chinese (Traditional, Taiwan)",
}

macro_rules! zones {
    ($($id:literal => $label:literal),* $(,)?) => {
        pub const TIME_ZONES: &[TimeZone] =
            &[$(TimeZone { id: $id, label: $label }),*];
    };
}

zones! {
    "UTC" => "UTC",
    "Pacific Standard Time" => "Pacific (US & Canada)",
    "Mountain Standard Time" => "Mountain (US & Canada)",
    "Central Standard Time" => "Central (US & Canada)",
    "Eastern Standard Time" => "Eastern (US & Canada)",
    "Atlantic Standard Time" => "Atlantic (Canada)",
    "GMT Standard Time" => "United Kingdom, Ireland, Portugal",
    "W. Europe Standard Time" => "Western Europe",
    "Central Europe Standard Time" => "Central Europe",
    "Romance Standard Time" => "France, Spain, Belgium",
    "E. Europe Standard Time" => "Eastern Europe",
    "Russian Standard Time" => "Moscow",
    "India Standard Time" => "India",
    "China Standard Time" => "China, Hong Kong, Singapore",
    "Tokyo Standard Time" => "Japan",
    "Korea Standard Time" => "Korea",
    "AUS Eastern Standard Time" => "Eastern Australia",
    "New Zealand Standard Time" => "New Zealand",
}

/// Exact match only. A near miss here would write a tag Windows ignores silently,
/// which is the failure mode this table exists to prevent.
pub fn region(tag: &str) -> Option<&'static Region> {
    REGIONS.iter().find(|r| r.tag == tag)
}

pub fn time_zone(id: &str) -> Option<&'static TimeZone> {
    TIME_ZONES.iter().find(|z| z.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are what every committed golden was built with. If either of
    /// these moves, every golden moves with it.
    #[test]
    fn the_defaults_are_what_the_goldens_carry() {
        assert_eq!(DEFAULT_REGION, "en-US");
        assert_eq!(DEFAULT_TIME_ZONE, "UTC");
        assert!(region(DEFAULT_REGION).is_some());
        assert!(time_zone(DEFAULT_TIME_ZONE).is_some());
    }

    /// A malformed tag reaches the answer file untouched and Windows ignores it
    /// silently, so the table is the only validation there is.
    #[test]
    fn every_region_tag_is_well_formed() {
        for r in REGIONS {
            let parts: Vec<&str> = r.tag.split('-').collect();
            assert_eq!(parts.len(), 2, "{}: expected language-REGION", r.tag);
            assert!(
                parts[0].len() == 2
                    && parts[0].chars().all(|c| c.is_ascii_lowercase()),
                "{}: language subtag should be two lowercase letters",
                r.tag
            );
            assert!(
                parts[1].len() == 2
                    && parts[1].chars().all(|c| c.is_ascii_uppercase()),
                "{}: region subtag should be two uppercase letters",
                r.tag
            );
            assert!(!r.label.is_empty(), "{} has no label", r.tag);
        }
    }

    /// A duplicate would render as two identical rows in the picker, and whichever
    /// one the user chose the second would be unreachable.
    #[test]
    fn regions_and_zones_are_unique() {
        for (i, r) in REGIONS.iter().enumerate() {
            assert!(
                !REGIONS[i + 1..].iter().any(|o| o.tag == r.tag),
                "duplicate region {}",
                r.tag
            );
        }
        for (i, z) in TIME_ZONES.iter().enumerate() {
            assert!(
                !TIME_ZONES[i + 1..].iter().any(|o| o.id == z.id),
                "duplicate time zone {}",
                z.id
            );
        }
    }

    /// Windows time zone IDs, not IANA names. `Europe/Berlin` is silently ignored
    /// and the guest stays on UTC, which is the kind of wrong nothing reports.
    #[test]
    fn time_zones_are_windows_ids_not_iana() {
        for z in TIME_ZONES {
            assert!(!z.id.contains('/'), "{} looks like an IANA name", z.id);
            assert!(!z.label.is_empty(), "{} has no label", z.id);
        }
    }

    #[test]
    fn lookup_is_exact_and_rejects_the_unknown() {
        assert_eq!(region("de-DE").map(|r| r.tag), Some("de-DE"));
        assert!(region("de").is_none());
        assert!(region("xx-XX").is_none());
        assert_eq!(
            time_zone("W. Europe Standard Time").map(|z| z.id),
            Some("W. Europe Standard Time")
        );
        assert!(time_zone("Europe/Berlin").is_none());
    }
}
