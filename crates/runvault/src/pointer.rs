//! JSON Pointers into `/parameters`.
//!
//! Exclusions are declared, never guessed from a key's name: only the pointers
//! written in `config.json` are removed (design note §3.3). A pointer that does
//! not resolve is an error, so an exclusion cannot silently fail to apply.

use std::collections::HashSet;

use serde_json::Value;

use crate::error::{Error, Result};

/// A parsed RFC 6901 pointer, relative to `/parameters`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pointer {
    raw: String,
    tokens: Vec<String>,
}

impl Pointer {
    /// Parses `/a/b~0c`. The empty pointer (the whole document) is rejected:
    /// excluding everything is never what the caller meant.
    pub fn parse(raw: &str) -> Result<Self> {
        let syntax = || Error::PointerSyntax {
            pointer: raw.to_string(),
        };
        if !raw.starts_with('/') {
            return Err(syntax());
        }
        let mut tokens = Vec::new();
        for segment in raw[1..].split('/') {
            let mut token = String::new();
            let mut chars = segment.chars();
            while let Some(c) = chars.next() {
                if c != '~' {
                    token.push(c);
                    continue;
                }
                match chars.next() {
                    Some('0') => token.push('~'),
                    Some('1') => token.push('/'),
                    _ => return Err(syntax()),
                }
            }
            tokens.push(token);
        }
        Ok(Self {
            raw: raw.to_string(),
            tokens,
        })
    }

    /// The pointer exactly as written. Ordering of seeds uses this string.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Follows the pointer, or `None` when it does not resolve.
    pub fn resolve<'a>(&self, root: &'a Value) -> Option<&'a Value> {
        let mut cursor = root;
        for token in &self.tokens {
            cursor = match cursor {
                Value::Object(map) => map.get(token)?,
                Value::Array(items) => items.get(array_index(token)?)?,
                _ => return None,
            };
        }
        Some(cursor)
    }

    /// Fails unless the pointer resolves inside `root`.
    pub fn require(&self, root: &Value) -> Result<()> {
        if self.resolve(root).is_some() {
            Ok(())
        } else {
            Err(Error::PointerNotFound {
                pointer: self.raw.clone(),
            })
        }
    }
}

fn array_index(token: &str) -> Option<usize> {
    if token != "0" && token.starts_with('0') {
        return None;
    }
    token.parse().ok()
}

/// Rebuilds `root` without the nodes the pointers name.
///
/// Rebuilding rather than deleting in place keeps array removal well defined:
/// the surviving elements keep their order and no other pointer shifts under it.
pub fn prune(root: &Value, remove: &[Pointer]) -> Value {
    let paths: HashSet<&[String]> = remove.iter().map(|p| p.tokens.as_slice()).collect();
    let mut path = Vec::new();
    rebuild(root, &paths, &mut path)
}

fn rebuild(value: &Value, remove: &HashSet<&[String]>, path: &mut Vec<String>) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                path.push(key.clone());
                if !remove.contains(path.as_slice()) {
                    out.insert(key.clone(), rebuild(item, remove, path));
                }
                path.pop();
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                path.push(i.to_string());
                if !remove.contains(path.as_slice()) {
                    out.push(rebuild(item, remove, path));
                }
                path.pop();
            }
            Value::Array(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ptr(s: &str) -> Pointer {
        Pointer::parse(s).unwrap()
    }

    #[test]
    fn resolves_nested_keys_and_array_elements() {
        let v = json!({"a": {"b": [10, 20]}});
        assert_eq!(ptr("/a/b/1").resolve(&v), Some(&json!(20)));
        assert_eq!(ptr("/a/b").resolve(&v), Some(&json!([10, 20])));
        assert_eq!(ptr("/a/c").resolve(&v), None);
    }

    #[test]
    fn decodes_escapes() {
        let v = json!({"a/b": 1, "c~d": 2});
        assert_eq!(ptr("/a~1b").resolve(&v), Some(&json!(1)));
        assert_eq!(ptr("/c~0d").resolve(&v), Some(&json!(2)));
    }

    #[test]
    fn rejects_bad_syntax() {
        assert!(Pointer::parse("").is_err());
        assert!(Pointer::parse("a/b").is_err());
        assert!(Pointer::parse("/a~2b").is_err());
    }

    #[test]
    fn prune_removes_only_the_named_nodes() {
        let v = json!({"keep": 1, "drop": 2, "nest": {"keep": 3, "drop": 4}});
        let pruned = prune(&v, &[ptr("/drop"), ptr("/nest/drop")]);
        assert_eq!(pruned, json!({"keep": 1, "nest": {"keep": 3}}));
    }

    #[test]
    fn prune_keeps_the_order_of_surviving_array_elements() {
        let v = json!({"xs": [1, 2, 3, 4]});
        assert_eq!(prune(&v, &[ptr("/xs/1")]), json!({"xs": [1, 3, 4]}));
    }

    #[test]
    fn require_reports_the_pointer_that_does_not_resolve() {
        let v = json!({"a": 1});
        let err = ptr("/b").require(&v).unwrap_err();
        assert!(err.to_string().contains("/b"), "{err}");
    }

    #[test]
    fn array_indices_with_a_leading_zero_do_not_resolve() {
        let v = json!({"xs": [1, 2]});
        assert_eq!(ptr("/xs/01").resolve(&v), None);
    }
}
