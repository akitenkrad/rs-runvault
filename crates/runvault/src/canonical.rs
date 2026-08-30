//! Canonical JSON, length-prefixed concatenation and BLAKE3.
//!
//! These three pieces decide whether two implementations agree on a hash, so the
//! rules live in one place and are pinned by `schema/v1/testvectors/`
//! (see the design note §3.3).

use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::error::{Error, Result};

/// Canonicalizes a JSON value into the byte string that gets hashed.
///
/// 1. object keys are NFC-normalized and sorted by code point
/// 2. strings are NFC-normalized and escaped minimally (`"`, `\` and control characters)
/// 3. floats use the shortest round-tripping decimal, keep a `.0`, fold `-0.0` to `0.0`
///    and never use exponent notation
/// 4. `NaN` / `Inf` are rejected
/// 5. a missing key and a `null` stay different
/// 6. array order is preserved
pub fn canonicalize(value: &Value) -> Result<String> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&write_number(n)?),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<(String, &Value)> = map.iter().map(|(k, v)| (nfc(k), v)).collect();
            keys.sort_by(|a, b| a.0.cmp(&b.0));
            for pair in keys.windows(2) {
                if pair[0].0 == pair[1].0 {
                    return Err(Error::Canonical(format!(
                        "NFC 正規化するとキー `{}` が重複します",
                        pair[0].0
                    )));
                }
            }
            out.push('{');
            for (i, (key, item)) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string_raw(key, out);
                out.push(':');
                write_value(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_number(n: &serde_json::Number) -> Result<String> {
    if let Some(u) = n.as_u64() {
        return Ok(u.to_string());
    }
    if let Some(i) = n.as_i64() {
        return Ok(i.to_string());
    }
    let f = n
        .as_f64()
        .ok_or_else(|| Error::Canonical(format!("数値 `{n}` を f64 として読めません")))?;
    format_f64(f)
}

/// Formats a float as the shortest decimal that round-trips, without an exponent.
///
/// Rust's `Display` for `f64` already gives the shortest round-tripping form and
/// never switches to exponent notation; the only additions are folding `-0.0`
/// and keeping a fractional part so that a float never looks like an integer.
pub fn format_f64(f: f64) -> Result<String> {
    if !f.is_finite() {
        return Err(Error::Canonical(format!("NaN / Inf は書けません ({f})")));
    }
    let f = if f == 0.0 { 0.0 } else { f };
    let s = format!("{f}");
    Ok(if s.contains('.') { s } else { format!("{s}.0") })
}

fn write_string(s: &str, out: &mut String) {
    write_string_raw(&nfc(s), out);
}

fn write_string_raw(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Appends one input as `<byte length in decimal> ":" <input>`.
///
/// A separator character would let a value containing that character produce the
/// same byte string as a different list of inputs; a length prefix cannot.
pub fn push_lp(out: &mut Vec<u8>, input: &[u8]) {
    out.extend_from_slice(input.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(input);
}

/// Concatenates inputs with a length prefix each.
pub fn join_lp<'a>(inputs: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for input in inputs {
        push_lp(&mut out, input);
    }
    out
}

/// BLAKE3 of the bytes, as 64 lowercase hex characters.
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_by_code_point() {
        let v = json!({"b": 1, "a": 2, "A": 3, "\u{3042}": 4});
        assert_eq!(
            canonicalize(&v).unwrap(),
            "{\"A\":3,\"a\":2,\"b\":1,\"\u{3042}\":4}"
        );
    }

    #[test]
    fn floats_keep_a_fractional_part_and_fold_negative_zero() {
        let v = json!({"a": 1.0, "b": -0.0, "c": 0.5, "d": 1});
        assert_eq!(
            canonicalize(&v).unwrap(),
            r#"{"a":1.0,"b":0.0,"c":0.5,"d":1}"#
        );
    }

    #[test]
    fn floats_never_use_exponent_notation() {
        let big = format_f64(1e21).unwrap();
        assert!(!big.contains('e'), "{big}");
        assert_eq!(big, "1000000000000000000000.0");
        let small = format_f64(1e-7).unwrap();
        assert!(!small.contains('e'), "{small}");
        assert_eq!(small, "0.0000001");
    }

    #[test]
    fn non_finite_is_rejected() {
        assert!(format_f64(f64::NAN).is_err());
        assert!(format_f64(f64::INFINITY).is_err());
    }

    #[test]
    fn missing_and_null_stay_different() {
        let with_null = canonicalize(&json!({"a": 1, "b": null})).unwrap();
        let without = canonicalize(&json!({"a": 1})).unwrap();
        assert_ne!(with_null, without);
    }

    #[test]
    fn strings_are_nfc_normalized_and_minimally_escaped() {
        // "が" written as か + combining dakuten normalizes to the precomposed form.
        let decomposed = json!({"k": "\u{304b}\u{3099}"});
        assert_eq!(canonicalize(&decomposed).unwrap(), "{\"k\":\"\u{304c}\"}");

        let escapes = json!({"k": "a\"b\\c\nd\u{1}e"});
        assert_eq!(
            canonicalize(&escapes).unwrap(),
            "{\"k\":\"a\\\"b\\\\c\\nd\\u0001e\"}"
        );
    }

    #[test]
    fn non_ascii_is_not_escaped() {
        assert_eq!(
            canonicalize(&json!("\u{7814}\u{7a76}")).unwrap(),
            "\"\u{7814}\u{7a76}\""
        );
    }

    #[test]
    fn array_order_is_preserved() {
        assert_eq!(canonicalize(&json!([3, 1, 2])).unwrap(), "[3,1,2]");
    }

    #[test]
    fn keys_colliding_after_nfc_are_rejected() {
        let mut map = serde_json::Map::new();
        map.insert("\u{304c}".into(), json!(1));
        map.insert("\u{304b}\u{3099}".into(), json!(2));
        assert!(canonicalize(&Value::Object(map)).is_err());
    }

    #[test]
    fn length_prefix_separates_inputs_a_separator_would_not() {
        let a = join_lp([b"a:b".as_slice(), b"c".as_slice()]);
        let b = join_lp([b"a".as_slice(), b"b:c".as_slice()]);
        assert_ne!(a, b);
        assert_eq!(a, b"3:a:b1:c".to_vec());
    }

    #[test]
    fn empty_input_still_occupies_a_slot() {
        assert_eq!(
            join_lp([b"".as_slice(), b"x".as_slice()]),
            b"0:1:x".to_vec()
        );
    }
}
