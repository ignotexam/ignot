//! SHA-256, hex, and the one JSON form everything is hashed in.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Error, Result, invalid};

/// SHA-256 of some bytes.
#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// SHA-256 of some bytes as `0x` and 64 hex digits.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&sha256(data))
}

/// Bytes as `0x` and lowercase hex.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

/// Hex, with or without `0x`, as bytes.
pub fn from_hex(text: &str) -> Result<Vec<u8>> {
    let digits = text.strip_prefix("0x").unwrap_or(text).as_bytes();
    let (pairs, rest) = digits.as_chunks::<2>();
    if !rest.is_empty() {
        return invalid("hex: odd number of digits");
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Error::Invalid("hex: a character that is not a hex digit".into())),
    };
    pairs.iter().map(|&[high, low]| Ok((nibble(high)? << 4) | nibble(low)?)).collect()
}

/// Whether two hex strings name the same bytes, whatever their letter case.
#[must_use]
pub fn same_hex(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// JSON with object keys sorted at every depth and no spaces, so equal data always gives equal bytes.
///
/// Only integers are allowed as numbers: a fraction prints differently from one library to the next, and
/// these bytes are hashed.
pub fn canonical<T: Serialize>(value: &T) -> Result<String> {
    let mut out = String::new();
    write(&serde_json::to_value(value)?, &mut out)?;
    Ok(out)
}

fn write(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if !(n.is_u64() || n.is_i64()) {
                return invalid("canonical JSON holds whole numbers only");
            }
            out.push_str(&n.to_string());
        }
        Value::String(s) => out.push_str(&serde_json::to_string(s)?),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Sorted here, not left to the map type: its order depends on a crate feature another crate can turn on.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                write(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sha256_of_abc() {
        assert_eq!(sha256_hex(b"abc"), "0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn hex_round_trips_and_rejects_rubbish() {
        assert_eq!(from_hex("0x00ffA1").ok(), Some(vec![0, 255, 161]));
        assert_eq!(to_hex(&[0, 255, 161]), "0x00ffa1");
        assert!(from_hex("0x0").is_err());
        assert!(from_hex("zz").is_err());
        assert!(same_hex("0xAB", "0xab"));
    }

    #[test]
    fn canonical_sorts_keys_at_every_depth() {
        let a = json!({"b": 1, "a": {"z": [true, null], "y": "x\"\n"}});
        let b = json!({"a": {"y": "x\"\n", "z": [true, null]}, "b": 1});
        assert_eq!(canonical(&a).ok(), canonical(&b).ok());
        assert_eq!(canonical(&a).ok().as_deref(), Some(r#"{"a":{"y":"x\"\n","z":[true,null]},"b":1}"#));
    }

    #[test]
    fn canonical_refuses_fractions() {
        assert!(canonical(&json!({"cost": 0.5})).is_err());
        assert!(canonical(&json!({"cost": 5})).is_ok());
    }
}
