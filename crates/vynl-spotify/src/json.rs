//! RFC 8259 compliant pure-Rust JSON value model, recursive-descent parser,
//! canonical serializer, and ergonomic typed accessors.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

/// Maximum nesting depth permitted by the recursive-descent JSON parser.
const MAX_PARSE_DEPTH: usize = 128;

/// An RFC 8259 JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// JSON `null`.
    Null,
    /// JSON boolean (`true` or `false`).
    Bool(bool),
    /// JSON IEEE-754 floating-point number.
    Number(f64),
    /// JSON UTF-8 string.
    String(String),
    /// JSON ordered array.
    Array(Vec<Self>),
    /// JSON key-value object with deterministic key ordering.
    Object(BTreeMap<String, Self>),
}

impl JsonValue {
    /// Constructs a JSON object from an iterator of `(key, value)` pairs.
    #[must_use]
    pub fn object<I, K>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, Self)>,
        K: Into<String>,
    {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(k.into(), v);
        }
        Self::Object(map)
    }

    /// Returns `true` if this value is `JsonValue::Null`.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Returns the inner `bool` if this value is `JsonValue::Bool`.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the inner `f64` if this value is `JsonValue::Number`.
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns the inner value as `i64` when it is an exact finite integer in range.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn as_i64(&self) -> Option<i64> {
        let n = self.as_f64()?;
        if !n.is_finite() || n.fract() != 0.0 {
            return None;
        }
        if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n) {
            return None;
        }
        Some(n as i64)
    }

    /// Returns the inner value as `u64` when it is a non-negative finite integer in range.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn as_u64(&self) -> Option<u64> {
        let n = self.as_f64()?;
        if !n.is_finite() || n.fract() != 0.0 || n < 0.0 {
            return None;
        }
        if n >= 18_446_744_073_709_551_616.0 {
            return None;
        }
        Some(n as u64)
    }

    /// Returns the inner value as `u32` when it is a non-negative integer fitting in `u32`.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        u32::try_from(self.as_u64()?).ok()
    }

    /// Returns the inner value as `u8` when it is a non-negative integer fitting in `u8`.
    #[must_use]
    pub fn as_u8(&self) -> Option<u8> {
        u8::try_from(self.as_u64()?).ok()
    }

    /// Returns the inner string slice if this value is `JsonValue::String`.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the inner array slice if this value is `JsonValue::Array`.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    /// Returns the inner object map if this value is `JsonValue::Object`.
    #[must_use]
    pub const fn as_object(&self) -> Option<&BTreeMap<String, Self>> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// Looks up a field by `key` when this value is a JSON object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?.get(key)
    }

    /// Looks up an element by `index` when this value is a JSON array.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<&Self> {
        self.as_array()?.get(index)
    }

    /// Traverses nested JSON objects along `path`.
    #[must_use]
    pub fn get_path(&self, path: &[&str]) -> Option<&Self> {
        let mut current = self;
        for segment in path {
            current = current.get(segment)?;
        }
        Some(current)
    }

    /// Returns a string field from this JSON object.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    /// Returns a boolean field from this JSON object.
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    /// Returns a `u64` field from this JSON object.
    #[must_use]
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get(key)?.as_u64()
    }

    /// Returns a `u32` field from this JSON object.
    #[must_use]
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key)?.as_u32()
    }

    /// Returns a `u8` field from this JSON object.
    #[must_use]
    pub fn get_u8(&self, key: &str) -> Option<u8> {
        self.get(key)?.as_u8()
    }

    /// Returns an `i64` field from this JSON object.
    #[must_use]
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key)?.as_i64()
    }

    /// Returns an `f64` field from this JSON object.
    #[must_use]
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    /// Returns an array field from this JSON object.
    #[must_use]
    pub fn get_array(&self, key: &str) -> Option<&[Self]> {
        self.get(key)?.as_array()
    }

    /// Returns an object map field from this JSON object.
    #[must_use]
    pub fn get_object(&self, key: &str) -> Option<&BTreeMap<String, Self>> {
        self.get(key)?.as_object()
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<f64> for JsonValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<u64> for JsonValue {
    #[allow(clippy::cast_precision_loss)]
    fn from(value: u64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<u32> for JsonValue {
    fn from(value: u32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<u16> for JsonValue {
    fn from(value: u16) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<u8> for JsonValue {
    fn from(value: u8) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<i64> for JsonValue {
    #[allow(clippy::cast_precision_loss)]
    fn from(value: i64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec<Self>> for JsonValue {
    fn from(value: Vec<Self>) -> Self {
        Self::Array(value)
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&to_json_string(self))
    }
}

/// Parses an RFC 8259 JSON document into a [`JsonValue`].
///
/// # Errors
/// Returns a descriptive error string with byte offset when `input` is malformed,
/// exceeds nesting limits, or contains trailing tokens.
pub fn parse_json(input: &str) -> Result<JsonValue, String> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        pos: 0,
        depth: 0,
    };
    parser.skip_ws();
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(format!(
            "unexpected trailing token at byte offset {}",
            parser.pos
        ));
    }
    Ok(value)
}

/// Serializes a [`JsonValue`] into a compact, deterministic RFC 8259 JSON string.
#[must_use]
pub fn to_json_string(value: &JsonValue) -> String {
    let mut out = String::new();
    write_json_value(value, &mut out);
    out
}

fn write_json_value(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Number(n) => write_json_number(*n, out),
        JsonValue::String(s) => write_json_string(s, out),
        JsonValue::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_json_value(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(map) => {
            out.push('{');
            for (idx, (key, val)) in map.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_json_value(val, out);
            }
            out.push('}');
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn write_json_number(n: f64, out: &mut String) {
    if !n.is_finite() {
        out.push_str("null");
        return;
    }
    if n == 0.0 {
        out.push('0');
        return;
    }
    if n.fract() == 0.0 && (-9_007_199_254_740_991.0..=9_007_199_254_740_991.0).contains(&n) {
        let int_val = n as i64;
        let _ = write!(out, "{int_val}");
    } else {
        let _ = write!(out, "{n}");
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                let _ = write!(out, "\\u{code:04x}");
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.pos).copied()?;
        self.pos += 1;
        Some(byte)
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_ws();
        let Some(b) = self.peek() else {
            return Err(format!(
                "unexpected end of input at byte offset {}",
                self.pos
            ));
        };
        match b {
            b'n' => self.parse_literal(b"null", JsonValue::Null),
            b't' => self.parse_literal(b"true", JsonValue::Bool(true)),
            b'f' => self.parse_literal(b"false", JsonValue::Bool(false)),
            b'"' => self.parse_string().map(JsonValue::String),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!(
                "unexpected byte 0x{other:02x} at byte offset {}",
                self.pos
            )),
        }
    }

    fn parse_literal(&mut self, expected: &[u8], value: JsonValue) -> Result<JsonValue, String> {
        let start = self.pos;
        if self.bytes.get(start..start + expected.len()) == Some(expected) {
            self.pos += expected.len();
            Ok(value)
        } else {
            Err(format!("invalid literal at byte offset {start}"))
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(format!(
                "maximum nesting depth ({MAX_PARSE_DEPTH}) exceeded at byte offset {}",
                self.pos
            ));
        }

        // Consume '['
        self.pos += 1;
        self.skip_ws();

        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(JsonValue::Array(items));
        }

        loop {
            let val = self.parse_value()?;
            items.push(val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(JsonValue::Array(items));
                }
                _ => {
                    return Err(format!(
                        "expected ',' or ']' in array at byte offset {}",
                        self.pos
                    ));
                }
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(format!(
                "maximum nesting depth ({MAX_PARSE_DEPTH}) exceeded at byte offset {}",
                self.pos
            ));
        }

        // Consume '{'
        self.pos += 1;
        self.skip_ws();

        let mut map = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(JsonValue::Object(map));
        }

        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!(
                    "expected string key in object at byte offset {}",
                    self.pos
                ));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.next_byte() != Some(b':') {
                return Err(format!(
                    "expected ':' after object key at byte offset {}",
                    self.pos
                ));
            }
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(JsonValue::Object(map));
                }
                _ => {
                    return Err(format!(
                        "expected ',' or '}}' in object at byte offset {}",
                        self.pos
                    ));
                }
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        let start = self.pos;
        if self.next_byte() != Some(b'"') {
            return Err(format!("expected '\"' at byte offset {start}"));
        }

        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    self.parse_escape(&mut out)?;
                }
                0x00..=0x1F => {
                    return Err(format!(
                        "unescaped control character 0x{b:02x} in string at byte offset {}",
                        self.pos
                    ));
                }
                _ => {
                    let rest = std::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| format!("invalid UTF-8 at byte offset {}", self.pos))?;
                    let Some(ch) = rest.chars().next() else {
                        return Err(format!(
                            "unterminated string starting at byte offset {start}"
                        ));
                    };
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(format!(
            "unterminated string starting at byte offset {start}"
        ))
    }

    fn parse_escape(&mut self, out: &mut String) -> Result<(), String> {
        let Some(esc) = self.next_byte() else {
            return Err(format!("unterminated escape at byte offset {}", self.pos));
        };
        match esc {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{08}'),
            b'f' => out.push('\u{0C}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let code = self.parse_hex4()?;
                if (0xD800..=0xDBFF).contains(&code) {
                    if self.bytes.get(self.pos..self.pos + 2) != Some(b"\\u") {
                        return Err(format!(
                            "missing low surrogate escape at byte offset {}",
                            self.pos
                        ));
                    }
                    self.pos += 2;
                    let low = self.parse_hex4()?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(format!(
                            "invalid low surrogate 0x{low:04x} at byte offset {}",
                            self.pos
                        ));
                    }
                    let combined =
                        0x1_0000 + ((u32::from(code) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                    let ch = char::from_u32(combined).ok_or_else(|| {
                        format!(
                            "invalid unicode scalar U+{combined:04X} at byte offset {}",
                            self.pos
                        )
                    })?;
                    out.push(ch);
                } else if (0xDC00..=0xDFFF).contains(&code) {
                    return Err(format!(
                        "unpaired low surrogate 0x{code:04x} at byte offset {}",
                        self.pos
                    ));
                } else {
                    let ch = char::from_u32(u32::from(code)).ok_or_else(|| {
                        format!(
                            "invalid unicode scalar U+{code:04X} at byte offset {}",
                            self.pos
                        )
                    })?;
                    out.push(ch);
                }
            }
            other => {
                return Err(format!(
                    "invalid escape sequence \\0x{other:02x} at byte offset {}",
                    self.pos
                ));
            }
        }
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let Some(b) = self.next_byte() else {
                return Err(format!("truncated \\u escape at byte offset {}", self.pos));
            };
            let digit = match b {
                b'0'..=b'9' => u16::from(b - b'0'),
                b'a'..=b'f' => u16::from(b - b'a') + 10,
                b'A'..=b'F' => u16::from(b - b'A') + 10,
                _ => {
                    return Err(format!(
                        "invalid hex digit in \\u escape at byte offset {}",
                        self.pos
                    ));
                }
            };
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;

        if self.peek() == Some(b'-') {
            self.pos += 1;
        }

        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(format!("leading zero in number at byte offset {start}"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => {
                return Err(format!("invalid number at byte offset {start}"));
            }
        }

        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!(
                    "expected digit after decimal point at byte offset {}",
                    self.pos
                ));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!(
                    "expected digit in exponent at byte offset {}",
                    self.pos
                ));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }

        let slice = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| format!("invalid UTF-8 in number at byte offset {start}"))?;
        let num: f64 = slice
            .parse()
            .map_err(|_| format!("failed to parse number `{slice}` at byte offset {start}"))?;
        if !num.is_finite() {
            return Err(format!(
                "number out of finite f64 range at byte offset {start}"
            ));
        }
        Ok(JsonValue::Number(num))
    }
}

#[cfg(test)]
mod tests {
    use super::{JsonValue, parse_json, to_json_string};

    #[test]
    fn parses_and_serializes_complex_document() -> Result<(), String> {
        let raw = r#"{
            "name": "Daft Punk",
            "active": false,
            "tracks": 13,
            "rating": 9.5,
            "empty": null,
            "tags": ["electronic", "french house", "line\nbreak", "quote\"ok"],
            "unicode": "caf\u00e9 \uD83C\uDFB5"
        }"#;
        let parsed = parse_json(raw)?;
        assert_eq!(parsed.get_str("name"), Some("Daft Punk"));
        assert_eq!(parsed.get_bool("active"), Some(false));
        assert_eq!(parsed.get_u64("tracks"), Some(13));
        assert_eq!(parsed.get_f64("rating"), Some(9.5));
        assert_eq!(parsed.get("empty").map(JsonValue::is_null), Some(true));
        assert_eq!(parsed.get_str("unicode"), Some("café 🎵"));

        let serialized = to_json_string(&parsed);
        let reparsed = parse_json(&serialized)?;
        assert_eq!(reparsed, parsed);
        Ok(())
    }

    #[test]
    fn rejects_invalid_rfc8259_inputs() {
        assert!(parse_json("").is_err());
        assert!(parse_json("01").is_err());
        assert!(parse_json("[1, 2,]").is_err());
        assert!(parse_json("{\"a\": 1} trailing").is_err());
        assert!(parse_json("\"raw\nnewline\"").is_err());
        assert!(parse_json("\"\\uD83C\"").is_err());
        assert!(parse_json("1e").is_err());
    }

    #[test]
    fn supports_nested_path_helpers() -> Result<(), String> {
        let doc =
            parse_json(r#"{"album":{"images":[{"url":"https://i.scdn.co/a","width":640}]}}"#)?;
        let img = doc
            .get_path(&["album", "images"])
            .and_then(|v| v.get_index(0))
            .ok_or_else(|| String::from("missing image"))?;
        assert_eq!(img.get_str("url"), Some("https://i.scdn.co/a"));
        assert_eq!(img.get_u32("width"), Some(640));
        Ok(())
    }
}
