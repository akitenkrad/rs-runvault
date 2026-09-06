//! The two identifiers.
//!
//! `run_uid` is the key everything joins on; `run_slug` is the directory name,
//! which is for people and is not unique (design note §3.1).

use chrono::{DateTime, Local};
use ulid::Ulid;

use crate::error::{Error, Result};

/// Fails unless the value matches the slug grammar of `schema/v1/common.json`.
///
/// The same grammar guards a path element and an index key at once, so `/`,
/// `..` and whitespace cannot reach either.
pub fn validate_slug(field: &'static str, value: &str) -> Result<()> {
    let bad = || Error::Slug {
        field,
        value: value.to_string(),
    };
    if value.is_empty() || value.len() > 64 {
        return Err(bad());
    }
    let mut chars = value.chars();
    let first = chars.next().ok_or_else(bad)?;
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(bad());
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-') {
            return Err(bad());
        }
    }
    Ok(())
}

/// A fresh ULID for the given instant. Sorts by time and is unique across machines.
pub fn new_run_uid(now: DateTime<Local>) -> String {
    Ulid::from_datetime(now.into()).to_string()
}

/// Whether the value is a `run_uid` — the ULID grammar of `schema/v1/common.json`.
///
/// Crockford base32 without `I`, `L`, `O` and `U`, and a first character in
/// `0-7` so that the 48-bit time part fits in 26 characters. `verify` asks this
/// of a `status.json` read back from disk, where nothing guarantees the file
/// was written by `new_run_uid`.
pub fn is_run_uid(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !('0'..='7').contains(&first) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    rest.len() == 25
        && rest.iter().all(|c| {
            c.is_ascii_digit() || (c.is_ascii_uppercase() && !matches!(c, 'I' | 'L' | 'O' | 'U'))
        })
}

/// The timestamp part of `run_slug`, in local time.
pub fn timestamp_part(now: DateTime<Local>) -> String {
    now.format("%Y%m%d_%H%M%S").to_string()
}

/// Builds `<subcommand>_<ts>_<cfg8>_<exec4>`, with the collision suffix when needed.
///
/// The condition prefix puts runs of the same condition next to each other, and
/// the execution suffix keeps a replicate apart from a run of edited code.
pub fn run_slug(
    subcommand: &str,
    timestamp: &str,
    config_hash: &str,
    execution_hash: &str,
    collision_index: Option<u64>,
) -> String {
    let base = format!(
        "{subcommand}_{timestamp}_{}_{}",
        &config_hash[..8],
        &execution_hash[..4]
    );
    match collision_index {
        Some(n) => format!("{base}-{n}"),
        None => base,
    }
}

/// Splits a `run_slug` back into `(cfg8, exec4)`, or `None` when it is not one.
pub fn slug_hash_prefixes(slug: &str) -> Option<(&str, &str)> {
    // Only a trailing `-<digits>` is the collision suffix; a subcommand may itself
    // contain a hyphen, so splitting on the first one would cut the name apart.
    let stem = match slug.rsplit_once('-') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => slug,
    };
    let mut parts = stem.rsplitn(3, '_');
    let exec4 = parts.next()?;
    let cfg8 = parts.next()?;
    if cfg8.len() != 8 || exec4.len() != 4 {
        return None;
    }
    let hex = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    };
    (hex(cfg8) && hex(exec4)).then_some((cfg8, exec4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_reject_path_traversal_and_uppercase() {
        assert!(validate_slug("experiment", "schelling").is_ok());
        assert!(validate_slug("experiment", "multiturn-jailbreak.v2_1").is_ok());
        assert!(validate_slug("experiment", "..").is_err());
        assert!(validate_slug("experiment", "a/b").is_err());
        assert!(validate_slug("experiment", "Schelling").is_err());
        assert!(validate_slug("experiment", "a b").is_err());
        assert!(validate_slug("experiment", "").is_err());
        assert!(validate_slug("experiment", "-leading").is_err());
        assert!(validate_slug("experiment", &"a".repeat(65)).is_err());
        assert!(validate_slug("experiment", &"a".repeat(64)).is_ok());
    }

    #[test]
    fn a_run_uid_starts_in_the_range_the_schema_allows() {
        let uid = new_run_uid(Local::now());
        assert_eq!(uid.len(), 26);
        assert!(('0'..='7').contains(&uid.chars().next().unwrap()), "{uid}");
    }

    #[test]
    fn the_uid_grammar_accepts_what_new_run_uid_makes_and_little_else() {
        assert!(is_run_uid(&new_run_uid(Local::now())));
        assert!(is_run_uid("01K3QZ8F7H9M2N4P6R8T0V2X4Z"));
        assert!(!is_run_uid(""));
        assert!(
            !is_run_uid("81K3QZ8F7H9M2N4P6R8T0V2X4Z"),
            "first char above 7"
        );
        assert!(!is_run_uid("01K3QZ8F7H9M2N4P6R8T0V2X4"), "25 characters");
        assert!(!is_run_uid("01K3QZ8F7H9M2N4P6R8T0V2X4ZZ"), "27 characters");
        assert!(
            !is_run_uid("01K3QZ8F7H9M2N4P6R8T0V2X4I"),
            "I is not in the alphabet"
        );
        assert!(!is_run_uid("01k3qz8f7h9m2n4p6r8t0v2x4z"), "lower case");
    }

    #[test]
    fn run_uids_sort_by_time() {
        let early = new_run_uid(
            DateTime::parse_from_rfc3339("2026-08-30T10:00:00+09:00")
                .unwrap()
                .into(),
        );
        let late = new_run_uid(
            DateTime::parse_from_rfc3339("2026-08-30T11:00:00+09:00")
                .unwrap()
                .into(),
        );
        assert!(early < late, "{early} < {late}");
    }

    #[test]
    fn a_slug_carries_both_hash_prefixes() {
        let slug = run_slug(
            "main",
            "20260830_101500",
            &"9f2c41ab".repeat(8),
            &"3b1d".repeat(16),
            None,
        );
        assert_eq!(slug, "main_20260830_101500_9f2c41ab_3b1d");
        assert_eq!(slug_hash_prefixes(&slug), Some(("9f2c41ab", "3b1d")));
    }

    #[test]
    fn a_collision_suffix_survives_the_round_trip() {
        let slug = run_slug(
            "sweep",
            "20260830_101500",
            &"0".repeat(64),
            &"1".repeat(64),
            Some(2),
        );
        assert_eq!(slug, "sweep_20260830_101500_00000000_1111-2");
        assert_eq!(slug_hash_prefixes(&slug), Some(("00000000", "1111")));
    }

    #[test]
    fn a_legacy_directory_name_is_not_a_slug() {
        assert_eq!(slug_hash_prefixes("20260620_162729_sweep"), None);
    }

    #[test]
    fn a_hyphen_in_the_subcommand_is_not_a_collision_suffix() {
        let slug = run_slug(
            "multi-turn",
            "20260830_101500",
            &"a".repeat(64),
            &"b".repeat(64),
            None,
        );
        assert_eq!(slug_hash_prefixes(&slug), Some(("aaaaaaaa", "bbbb")));
    }
}
