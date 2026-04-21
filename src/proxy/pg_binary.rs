//! PostgreSQL binary wire-format decoders.
//!
//! When a client uses the extended query protocol with binary format codes
//! (pgx, most modern drivers), bind parameters arrive as raw bytes laid out
//! according to PG's per-type binary representation — NOT as UTF-8 text.
//!
//! The proxy's capture path substitutes bind params into the captured SQL
//! template as text literals (to make replay work through simple-protocol
//! text-substitution). Without a binary-to-text decoder, non-UTF-8 param
//! bytes get stringified to `'<binary N bytes>'` and every replay query
//! fails with `ERROR: invalid input syntax for type X` (SC-014).
//!
//! This module decodes the common built-in types. Unknown OIDs fall back to
//! the old `'<binary N bytes>'` placeholder so behavior only improves, never
//! regresses.
//!
//! Decoders match the layout documented in the PostgreSQL source
//! (`src/backend/utils/adt/*.c` send/recv functions) and in the protocol
//! docs (<https://www.postgresql.org/docs/current/protocol.html>).

/// OIDs for extension types that aren't fixed in PG's built-in type oid
/// range. pgvector (and anything like PostGIS, citext, hstore) assigns OIDs
/// at `CREATE EXTENSION` time, so the proxy has to discover them at startup
/// and pass them in alongside each decode call.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtensionOids {
    /// OID for `pgvector.vector` (dim*4 bytes of float4).
    pub vector: Option<u32>,
    /// OID for `pgvector.halfvec` (dim*2 bytes of IEEE-754 binary16).
    pub halfvec: Option<u32>,
    /// OID for `pgvector.sparsevec` (i32 dim + i32 nnz + indices + values).
    pub sparsevec: Option<u32>,
}

/// Format a PostgreSQL binary-format value as a SQL text literal, given its
/// type OID. Returns the string already SQL-escaped and quoted (e.g.
/// `'42'`, `'hello'`, `'f47ac10b-58cc-4372-a567-0e02b2c3d479'`).
///
/// Unknown OIDs or malformed bytes return `None` — the caller can fall back
/// to the legacy placeholder.
///
/// This is the builtin-only entry point. Call
/// [`binary_to_sql_literal_ext`] to also decode dynamically-assigned
/// extension OIDs (pgvector, etc.).
pub fn binary_to_sql_literal(oid: u32, bytes: &[u8]) -> Option<String> {
    binary_to_sql_literal_ext(oid, bytes, &ExtensionOids::default())
}

/// Like [`binary_to_sql_literal`] but also decodes extension types whose
/// OIDs were discovered at proxy startup (see `ExtensionOids`).
pub fn binary_to_sql_literal_ext(oid: u32, bytes: &[u8], ext: &ExtensionOids) -> Option<String> {
    // Check extension OIDs first — they're dynamic, so they can collide with
    // values above pg's built-in range but we still want to match them.
    if Some(oid) == ext.vector {
        return decode_vector(bytes).map(|s| format!("'{s}'::vector"));
    }
    if Some(oid) == ext.halfvec {
        return decode_halfvec(bytes).map(|s| format!("'{s}'::halfvec"));
    }
    if Some(oid) == ext.sparsevec {
        return decode_sparsevec(bytes).map(|s| format!("'{s}'::sparsevec"));
    }
    match oid {
        // bool
        16 => {
            if bytes.len() != 1 {
                return None;
            }
            Some(if bytes[0] == 0 {
                "'f'".into()
            } else {
                "'t'".into()
            })
        }
        // int8
        20 => {
            if bytes.len() != 8 {
                return None;
            }
            let v = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{v}'"))
        }
        // int2
        21 => {
            if bytes.len() != 2 {
                return None;
            }
            let v = i16::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{v}'"))
        }
        // int4
        23 => {
            if bytes.len() != 4 {
                return None;
            }
            let v = i32::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{v}'"))
        }
        // text, name, varchar, bpchar, char (single)
        25 | 19 | 1042 | 1043 | 18 => {
            let s = std::str::from_utf8(bytes).ok()?;
            Some(sql_quote_text(s))
        }
        // oid, xid, cid, regproc, regprocedure, regoper, regoperator,
        // regclass, regtype (all unsigned 32-bit)
        26 | 28 | 29 | 24 | 2202 | 2203 | 2204 | 2205 | 2206 => {
            if bytes.len() != 4 {
                return None;
            }
            let v = u32::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{v}'"))
        }
        // json (sent as raw UTF-8, same as text binary format)
        114 => {
            let s = std::str::from_utf8(bytes).ok()?;
            Some(sql_quote_text(s))
        }
        // float4
        700 => {
            if bytes.len() != 4 {
                return None;
            }
            let v = f32::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{v}'"))
        }
        // float8
        701 => {
            if bytes.len() != 8 {
                return None;
            }
            let v = f64::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{v}'"))
        }
        // bytea — encode as escape-string bytea literal: E'\\x<hex>'
        17 => Some(format!("E'\\\\x{}'", hex_encode(bytes))),
        // date — 4 bytes: days since 2000-01-01
        1082 => {
            if bytes.len() != 4 {
                return None;
            }
            let days = i32::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{}'::date", format_pg_date(days)?))
        }
        // time (without tz) — 8 bytes: microseconds since midnight
        1083 => {
            if bytes.len() != 8 {
                return None;
            }
            let micros = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(format!("'{}'::time", format_pg_time(micros)?))
        }
        // timestamp (without tz) — 8 bytes: microseconds since 2000-01-01 00:00:00
        1114 => {
            if bytes.len() != 8 {
                return None;
            }
            let micros = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(format!(
                "'{}'::timestamp",
                format_pg_timestamp(micros, false)?
            ))
        }
        // timestamptz — same binary layout as timestamp, interpreted as UTC
        1184 => {
            if bytes.len() != 8 {
                return None;
            }
            let micros = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(format!(
                "'{}'::timestamptz",
                format_pg_timestamp(micros, true)?
            ))
        }
        // numeric / decimal — header (4 i16: ndigits, weight, sign, dscale)
        // followed by ndigits * i16 of base-10000 digits
        1700 => decode_numeric(bytes).map(|s| format!("'{s}'")),
        // uuid
        2950 => {
            if bytes.len() != 16 {
                return None;
            }
            Some(format!("'{}'", format_uuid(bytes)))
        }
        // jsonb — 1 version byte (must be 1) + JSON UTF-8 bytes
        3802 => {
            if bytes.is_empty() || bytes[0] != 1 {
                return None;
            }
            let s = std::str::from_utf8(&bytes[1..]).ok()?;
            Some(format!("{}::jsonb", sql_quote_text(s)))
        }
        // xml — UTF-8 bytes, same on-the-wire as text
        142 => {
            let s = std::str::from_utf8(bytes).ok()?;
            Some(format!("{}::xml", sql_quote_text(s)))
        }
        // money — 8 bytes, i64 in minor units (cents). Render as decimal
        // string cast to money; PG parses `'1234.56'::money` regardless of
        // lc_monetary because the cast path is locale-tolerant for plain
        // numeric strings.
        790 => {
            if bytes.len() != 8 {
                return None;
            }
            let v = i64::from_be_bytes(bytes.try_into().ok()?);
            let negative = v < 0;
            let abs = v.unsigned_abs();
            let dollars = abs / 100;
            let cents = abs % 100;
            let sign = if negative { "-" } else { "" };
            Some(format!("'{sign}{dollars}.{cents:02}'::money"))
        }
        // interval — 16 bytes: i64 microseconds, i32 days, i32 months
        1186 => decode_interval(bytes).map(|s| format!("'{s}'::interval")),
        // timetz — 12 bytes: i64 microseconds since midnight, i32 offset seconds
        1266 => decode_timetz(bytes).map(|s| format!("'{s}'::timetz")),
        // bit, varbit — i32 bit_count, then ceil(bit_count/8) bytes
        1560 | 1562 => decode_bit_string(bytes).map(|s| format!("B'{s}'")),
        // inet, cidr — family, bits, is_cidr, addr_len, addr bytes
        869 | 650 => {
            let s = decode_inet_cidr(bytes)?;
            let cast = if oid == 650 { "cidr" } else { "inet" };
            Some(format!("'{s}'::{cast}"))
        }
        // macaddr — 6 bytes
        829 => {
            if bytes.len() != 6 {
                return None;
            }
            Some(format!(
                "'{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}'::macaddr",
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
            ))
        }
        // macaddr8 — 8 bytes
        774 => {
            if bytes.len() != 8 {
                return None;
            }
            Some(format!(
                "'{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}'::macaddr8",
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]
            ))
        }
        // Array types — decoder inspects the binary header for element OID
        // and dimensions, then recursively renders each element via
        // `array_element_text`. The outer OID hints at the expected element
        // type for the SQL cast ; the binary header is authoritative for
        // decoding.
        1000 | 1001 | 1005 | 1007 | 1009 | 1014 | 1015 | 1016 | 1021 | 1022 | 1115 | 1182
        | 1183 | 1185 | 1187 | 1231 | 2951 | 3807 => {
            let literal = decode_array(bytes)?;
            Some(format!("'{}'::{}", literal, array_cast_name(oid)))
        }
        _ => None,
    }
}

/// Map an array OID to the PostgreSQL text-cast name (e.g. `int[]`, `uuid[]`).
/// Used to append an explicit cast to the decoded array literal so the replay
/// parser picks the right element type even in contexts where inference would
/// go wrong.
fn array_cast_name(oid: u32) -> &'static str {
    match oid {
        1000 => "boolean[]",
        1001 => "bytea[]",
        1005 => "int2[]",
        1007 => "int4[]",
        1009 => "text[]",
        1014 => "bpchar[]",
        1015 => "varchar[]",
        1016 => "int8[]",
        1021 => "float4[]",
        1022 => "float8[]",
        1115 => "timestamp[]",
        1182 => "date[]",
        1183 => "time[]",
        1185 => "timestamptz[]",
        1187 => "interval[]",
        1231 => "numeric[]",
        2951 => "uuid[]",
        3807 => "jsonb[]",
        _ => "anyarray",
    }
}

fn sql_quote_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn format_uuid(bytes: &[u8]) -> String {
    // 8-4-4-4-12 hex chars, lowercase
    let hex = hex_encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Days since 2000-01-01 → "YYYY-MM-DD" via proleptic Gregorian arithmetic.
///
/// We avoid pulling in `chrono` for one function; the math is:
/// convert to days since 0000-03-01 (civil-from-days), then split into year
/// / month / day. Algorithm adapted from Howard Hinnant's "chrono date"
/// reference implementation (public domain).
fn format_pg_date(days_since_pg_epoch: i32) -> Option<String> {
    // PG epoch = 2000-01-01. Hinnant's civil_from_days uses days-since-
    // 0000-03-01. Days from 0000-03-01 to 2000-01-01 = 730425.
    let z = (days_since_pg_epoch as i64) + 730_425;
    let (y, m, d) = civil_from_days(z);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm — accepts z = days from 0000-03-01.
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn format_pg_time(micros: i64) -> Option<String> {
    if !(0..86_400_000_000).contains(&micros) {
        return None;
    }
    let us = micros % 1_000_000;
    let total_s = micros / 1_000_000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    if us == 0 {
        Some(format!("{h:02}:{m:02}:{s:02}"))
    } else {
        Some(format!("{h:02}:{m:02}:{s:02}.{us:06}"))
    }
}

fn format_pg_timestamp(micros: i64, utc: bool) -> Option<String> {
    // Split into days and time-of-day.
    let mut days = micros.div_euclid(86_400_000_000);
    let mut tod = micros.rem_euclid(86_400_000_000);
    // rem_euclid can return 0..86_400_000_000; both already bounded.
    if tod < 0 {
        tod += 86_400_000_000;
        days -= 1;
    }
    let date = format_pg_date(days as i32)?;
    let time = format_pg_time(tod)?;
    if utc {
        Some(format!("{date} {time}+00"))
    } else {
        Some(format!("{date} {time}"))
    }
}

/// Decode PostgreSQL numeric binary format into a decimal string.
///
/// Layout: 4 i16 header fields then ndigits * i16 of base-10000 digits:
///   ndigits  — number of base-10000 digits
///   weight   — weight of the first digit (power of 10000)
///   sign     — 0x0000 positive, 0x4000 negative, 0xC000 NaN
///   dscale   — number of digits after the decimal point
///
/// Special: 0xD000 = +Infinity, 0xF000 = -Infinity (PG 14+).
fn decode_numeric(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 8 {
        return None;
    }
    let ndigits = i16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let weight = i16::from_be_bytes([bytes[2], bytes[3]]);
    let sign = u16::from_be_bytes([bytes[4], bytes[5]]);
    let dscale = i16::from_be_bytes([bytes[6], bytes[7]]).max(0) as usize;

    match sign {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }

    if bytes.len() < 8 + ndigits * 2 {
        return None;
    }

    let negative = sign == 0x4000;

    if ndigits == 0 {
        // Zero value — still honor dscale for "0.00" style.
        if dscale == 0 {
            return Some("0".into());
        }
        return Some(format!("0.{}", "0".repeat(dscale)));
    }

    let digits: Vec<u16> = (0..ndigits)
        .map(|i| {
            let off = 8 + i * 2;
            u16::from_be_bytes([bytes[off], bytes[off + 1]])
        })
        .collect();

    // Expand into a single decimal string, aligning around the decimal point.
    // Each base-10000 digit is 4 decimal digits. `weight` is the power of 10000
    // of the *first* digit: e.g. weight=0 means the first digit is the units
    // group (1-9999), weight=1 means 10000-99_999_999, weight=-1 means the
    // first digit is in the tenths-through-thousandths positions.

    let mut int_part = String::new();
    let mut frac_part = String::new();

    for i in 0..=weight.max(0) {
        let idx = i as usize;
        let group = digits.get(idx).copied().unwrap_or(0);
        if int_part.is_empty() {
            int_part.push_str(&group.to_string());
        } else {
            int_part.push_str(&format!("{group:04}"));
        }
    }
    if int_part.is_empty() {
        int_part.push('0');
    }

    // Fractional digits start at index (weight+1) relative to the first digit,
    // but in base-10000 groups. Negative weights contribute leading zero groups.
    let mut frac_group_idx = weight + 1;
    while frac_group_idx < 0 {
        frac_part.push_str("0000");
        frac_group_idx += 1;
    }
    let mut di = (weight + 1).max(0) as usize;
    while di < ndigits {
        frac_part.push_str(&format!("{:04}", digits[di]));
        di += 1;
    }

    // Trim or pad frac_part to dscale.
    if frac_part.len() > dscale {
        frac_part.truncate(dscale);
    } else if frac_part.len() < dscale {
        frac_part.push_str(&"0".repeat(dscale - frac_part.len()));
    }

    let sign_str = if negative { "-" } else { "" };
    if dscale == 0 {
        Some(format!("{sign_str}{int_part}"))
    } else {
        Some(format!("{sign_str}{int_part}.{frac_part}"))
    }
}

/// Decode PG interval binary (16 bytes: i64 microseconds, i32 days, i32 months)
/// into a string PG will parse back via `::interval`. Format: `N mons N days HH:MM:SS[.ffffff]`.
fn decode_interval(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 {
        return None;
    }
    let micros = i64::from_be_bytes(bytes[0..8].try_into().ok()?);
    let days = i32::from_be_bytes(bytes[8..12].try_into().ok()?);
    let months = i32::from_be_bytes(bytes[12..16].try_into().ok()?);

    let mut parts = Vec::new();
    if months != 0 {
        parts.push(format!("{months} mons"));
    }
    if days != 0 {
        parts.push(format!("{days} days"));
    }
    // Always include a time component even when zero, so `'0'::interval`
    // doesn't land as an empty string literal that PG would reject.
    let negative = micros < 0;
    let abs_micros = micros.unsigned_abs();
    let total_s = abs_micros / 1_000_000;
    let us = abs_micros % 1_000_000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    let sign = if negative { "-" } else { "" };
    let time = if us == 0 {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}.{us:06}")
    };
    if parts.is_empty() && micros == 0 {
        return Some("0".to_string());
    }
    parts.push(time);
    Some(parts.join(" "))
}

/// Decode timetz binary (12 bytes: i64 microseconds since midnight, i32
/// offset seconds) into "HH:MM:SS[.ffffff]+HH" form.
///
/// PG stores the offset as seconds *west* of UTC per POSIX convention
/// (EST → +18000). Text output inverts the sign so the on-screen form reads
/// "-05" for EST. Our output matches.
fn decode_timetz(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 12 {
        return None;
    }
    let micros = i64::from_be_bytes(bytes[0..8].try_into().ok()?);
    let offset_secs_west = i32::from_be_bytes(bytes[8..12].try_into().ok()?);
    let time = format_pg_time(micros)?;
    // Display sign: invert the stored "seconds west" so EST (+18000 stored)
    // becomes "-05:00" on screen.
    let display_offset = -offset_secs_west;
    let sign = if display_offset < 0 { '-' } else { '+' };
    let abs = display_offset.unsigned_abs();
    let oh = abs / 3600;
    let om = (abs % 3600) / 60;
    if om == 0 {
        Some(format!("{time}{sign}{oh:02}"))
    } else {
        Some(format!("{time}{sign}{oh:02}:{om:02}"))
    }
}

/// Decode PG bit / varbit binary (4 bytes bit_count, then ceil(bit_count/8)
/// bytes packed MSB-first) into a bit-string literal body like `010101`.
fn decode_bit_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 {
        return None;
    }
    let bit_count = i32::from_be_bytes(bytes[0..4].try_into().ok()?);
    if bit_count < 0 {
        return None;
    }
    let bit_count = bit_count as usize;
    let byte_count = bit_count.div_ceil(8);
    if bytes.len() < 4 + byte_count {
        return None;
    }
    let packed = &bytes[4..4 + byte_count];
    let mut out = String::with_capacity(bit_count);
    for i in 0..bit_count {
        let byte = packed[i / 8];
        let bit = (byte >> (7 - (i % 8))) & 1;
        out.push(if bit == 0 { '0' } else { '1' });
    }
    Some(out)
}

/// Decode inet or cidr binary: 1 byte family, 1 byte netmask bits, 1 byte
/// is_cidr, 1 byte addr_len, addr_len bytes network-order address.
fn decode_inet_cidr(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 {
        return None;
    }
    let family = bytes[0];
    let bits = bytes[1];
    let _is_cidr = bytes[2];
    let addr_len = bytes[3] as usize;
    if bytes.len() < 4 + addr_len {
        return None;
    }
    let addr = &bytes[4..4 + addr_len];
    let (addr_str, default_bits) = match (family, addr_len) {
        // AF_INET (PG's family byte is 2 on the wire, PGSQL_AF_INET)
        (2, 4) => (
            format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]),
            32u8,
        ),
        // AF_INET6 (PG's family byte is 3 on the wire, PGSQL_AF_INET6)
        (3, 16) => {
            // Format as 8 groups of 16-bit hex, with :: compression for the
            // longest run of zero groups (standard RFC 5952 rendering).
            let groups: Vec<u16> = (0..8)
                .map(|i| u16::from_be_bytes([addr[i * 2], addr[i * 2 + 1]]))
                .collect();
            (compress_ipv6(&groups), 128u8)
        }
        _ => return None,
    };
    if bits == default_bits {
        Some(addr_str)
    } else {
        Some(format!("{addr_str}/{bits}"))
    }
}

/// RFC 5952 IPv6 compression: replace the longest run of consecutive zero
/// groups (length ≥ 2) with `::`.
fn compress_ipv6(groups: &[u16]) -> String {
    let mut best_start = 0;
    let mut best_len = 0;
    let mut cur_start = 0;
    let mut cur_len = 0;
    for (i, &g) in groups.iter().enumerate() {
        if g == 0 {
            if cur_len == 0 {
                cur_start = i;
            }
            cur_len += 1;
            if cur_len > best_len {
                best_len = cur_len;
                best_start = cur_start;
            }
        } else {
            cur_len = 0;
        }
    }
    if best_len < 2 {
        return groups
            .iter()
            .map(|g| format!("{g:x}"))
            .collect::<Vec<_>>()
            .join(":");
    }
    let head: Vec<String> = groups[..best_start]
        .iter()
        .map(|g| format!("{g:x}"))
        .collect();
    let tail: Vec<String> = groups[best_start + best_len..]
        .iter()
        .map(|g| format!("{g:x}"))
        .collect();
    format!("{}::{}", head.join(":"), tail.join(":"))
}

/// Decode a 1-dimensional PostgreSQL array in binary format, returning the
/// array contents as a PG text-literal body (without the surrounding single
/// quotes) like `{1,2,3}` or `{"a b","c,d"}`.
///
/// Binary header layout (see `src/backend/utils/adt/array_send.c`):
///   4 bytes ndims, 4 bytes flags (1 iff has nulls), 4 bytes element OID,
///   then ndims × (4 bytes dim_len, 4 bytes lower_bound),
///   then flattened elements: each is (4 bytes len OR -1 for NULL) + data.
///
/// Multi-dimensional arrays and non-1 lower bounds are rejected (return None
/// → caller falls back to `'<binary N bytes>'` placeholder). This covers the
/// vast majority of real-world workloads; lifting either restriction is a
/// mechanical extension.
fn decode_array(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 12 {
        return None;
    }
    let ndims = i32::from_be_bytes(bytes[0..4].try_into().ok()?);
    let _flags = i32::from_be_bytes(bytes[4..8].try_into().ok()?);
    let elem_oid = u32::from_be_bytes(bytes[8..12].try_into().ok()?);

    if ndims == 0 {
        return Some("{}".to_string());
    }
    if ndims != 1 {
        // Multi-dimensional arrays are rare in bind params; bail out and let
        // the caller fall back to the placeholder rather than mis-decode.
        return None;
    }

    let header_len = 12 + (ndims as usize) * 8;
    if bytes.len() < header_len {
        return None;
    }
    let dim_len = i32::from_be_bytes(bytes[12..16].try_into().ok()?) as usize;
    let lower_bound = i32::from_be_bytes(bytes[16..20].try_into().ok()?);
    if lower_bound != 1 {
        // Non-default lower bound would require `[lb:ub]={...}` prefix —
        // skip to stay simple. Real-world apps almost never set lb != 1.
        return None;
    }

    let mut pos = header_len;
    let mut parts = Vec::with_capacity(dim_len);
    for _ in 0..dim_len {
        if pos + 4 > bytes.len() {
            return None;
        }
        let len = i32::from_be_bytes(bytes[pos..pos + 4].try_into().ok()?);
        pos += 4;
        if len == -1 {
            parts.push("NULL".to_string());
            continue;
        }
        let elen = len as usize;
        if pos + elen > bytes.len() {
            return None;
        }
        let elem_bytes = &bytes[pos..pos + elen];
        pos += elen;
        parts.push(array_element_text(elem_oid, elem_bytes)?);
    }

    Some(format!("{{{}}}", parts.join(",")))
}

/// Render a single array element as it should appear inside the `{...}`
/// literal (i.e. quoted-and-escaped for string-like types, raw for numeric
/// and structured types that PG's array parser accepts unquoted).
fn array_element_text(elem_oid: u32, bytes: &[u8]) -> Option<String> {
    match elem_oid {
        // bool
        16 => Some(if bytes.first().copied() == Some(0) {
            "f".into()
        } else {
            "t".into()
        }),
        // int2
        21 => Some(i16::from_be_bytes(bytes.try_into().ok()?).to_string()),
        // int4
        23 => Some(i32::from_be_bytes(bytes.try_into().ok()?).to_string()),
        // int8
        20 => Some(i64::from_be_bytes(bytes.try_into().ok()?).to_string()),
        // float4
        700 => Some(f32::from_be_bytes(bytes.try_into().ok()?).to_string()),
        // float8
        701 => Some(f64::from_be_bytes(bytes.try_into().ok()?).to_string()),
        // numeric
        1700 => decode_numeric(bytes),
        // uuid
        2950 => {
            if bytes.len() != 16 {
                return None;
            }
            Some(format_uuid(bytes))
        }
        // text, varchar, bpchar, name, char
        25 | 19 | 1042 | 1043 | 18 => {
            let s = std::str::from_utf8(bytes).ok()?;
            Some(pg_array_quote(s))
        }
        // bytea — render as the `\x` hex escape and quote because it
        // contains backslashes which PG's array parser treats specially.
        17 => Some(pg_array_quote(&format!("\\x{}", hex_encode(bytes)))),
        // date
        1082 => {
            if bytes.len() != 4 {
                return None;
            }
            let days = i32::from_be_bytes(bytes.try_into().ok()?);
            Some(pg_array_quote(&format_pg_date(days)?))
        }
        // timestamp
        1114 => {
            if bytes.len() != 8 {
                return None;
            }
            let micros = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(pg_array_quote(&format_pg_timestamp(micros, false)?))
        }
        // timestamptz
        1184 => {
            if bytes.len() != 8 {
                return None;
            }
            let micros = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(pg_array_quote(&format_pg_timestamp(micros, true)?))
        }
        // time
        1083 => {
            if bytes.len() != 8 {
                return None;
            }
            let micros = i64::from_be_bytes(bytes.try_into().ok()?);
            Some(pg_array_quote(&format_pg_time(micros)?))
        }
        // interval
        1186 => decode_interval(bytes).map(|s| pg_array_quote(&s)),
        // jsonb — strip the version byte, then quote the JSON text
        3802 => {
            if bytes.is_empty() || bytes[0] != 1 {
                return None;
            }
            let s = std::str::from_utf8(&bytes[1..]).ok()?;
            Some(pg_array_quote(s))
        }
        _ => None,
    }
}

/// Decode pgvector's `vector` binary format into a text literal body like
/// `[1.5,2.5,3.5]` suitable for `'[...]'::vector` on replay.
///
/// Wire format (pgvector `vector_send`): 2B dim (i16) + 2B reserved + dim*4B
/// big-endian float4.
fn decode_vector(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 {
        return None;
    }
    let dim = i16::from_be_bytes([bytes[0], bytes[1]]);
    if dim < 0 {
        return None;
    }
    let dim = dim as usize;
    if bytes.len() != 4 + dim * 4 {
        return None;
    }
    let mut parts = Vec::with_capacity(dim);
    for i in 0..dim {
        let off = 4 + i * 4;
        let v = f32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        parts.push(v.to_string());
    }
    Some(format!("[{}]", parts.join(",")))
}

/// Decode pgvector's `halfvec` binary format. 2B dim + 2B reserved + dim*2B
/// IEEE-754 binary16 (half-precision) floats, big-endian. We expand each
/// half-float to f32 for the text representation.
fn decode_halfvec(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 {
        return None;
    }
    let dim = i16::from_be_bytes([bytes[0], bytes[1]]);
    if dim < 0 {
        return None;
    }
    let dim = dim as usize;
    if bytes.len() != 4 + dim * 2 {
        return None;
    }
    let mut parts = Vec::with_capacity(dim);
    for i in 0..dim {
        let off = 4 + i * 2;
        let raw = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
        parts.push(f16_to_f32(raw).to_string());
    }
    Some(format!("[{}]", parts.join(",")))
}

/// Decode pgvector's `sparsevec` binary format. Layout (pgvector
/// `sparsevec_send`): 4B dim (i32) + 4B nnz (i32) + 4B reserved + nnz*4B
/// 1-based indices (i32) + nnz*4B values (float4). Text form:
/// `{idx:val,idx:val}/dim` — PG's sparsevec parser accepts this exactly.
fn decode_sparsevec(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 12 {
        return None;
    }
    let dim = i32::from_be_bytes(bytes[0..4].try_into().ok()?);
    let nnz = i32::from_be_bytes(bytes[4..8].try_into().ok()?);
    if dim < 0 || nnz < 0 {
        return None;
    }
    let nnz = nnz as usize;
    let expected = 12 + nnz * 8;
    if bytes.len() != expected {
        return None;
    }
    let mut parts = Vec::with_capacity(nnz);
    for i in 0..nnz {
        let idx_off = 12 + i * 4;
        let val_off = 12 + nnz * 4 + i * 4;
        let idx = i32::from_be_bytes(bytes[idx_off..idx_off + 4].try_into().ok()?);
        let val = f32::from_be_bytes(bytes[val_off..val_off + 4].try_into().ok()?);
        parts.push(format!("{idx}:{val}"));
    }
    Some(format!("{{{}}}/{dim}", parts.join(",")))
}

/// Convert IEEE-754 binary16 bits to f32.
///
/// Layout: sign(1) | exp(5, bias 15) | mantissa(10). Zero exp with nonzero
/// mantissa is a subnormal; exp 31 is Inf / NaN. Reference:
/// <https://en.wikipedia.org/wiki/Half-precision_floating-point_format>.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 0x1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let mant = (bits & 0x3ff) as u32;

    let f32_bits = if exp == 0 {
        if mant == 0 {
            // Signed zero.
            sign << 31
        } else {
            // Subnormal → normalized f32.
            let mut m = mant;
            let mut e: i32 = 1;
            while (m & 0x400) == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3ff;
            let f32_exp = (127i32 - 15 + e) as u32;
            (sign << 31) | (f32_exp << 23) | (m << 13)
        }
    } else if exp == 31 {
        // Inf / NaN — propagate with mantissa shifted into f32 width.
        (sign << 31) | (0xffu32 << 23) | (mant << 13)
    } else {
        // Normal → rebias and widen mantissa.
        let f32_exp = exp + (127 - 15);
        (sign << 31) | (f32_exp << 23) | (mant << 13)
    };
    f32::from_bits(f32_bits)
}

/// Quote-and-escape a value for inclusion inside a PG array text literal.
///
/// PG array syntax needs double quotes around any element containing a
/// comma, brace, double-quote, backslash, whitespace, or the literal word
/// `NULL`. Inside the quotes, `\` and `"` must be backslash-escaped. Empty
/// strings must also be quoted to distinguish them from missing elements.
fn pg_array_quote(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.eq_ignore_ascii_case("NULL")
        || s.chars()
            .any(|c| matches!(c, ',' | '{' | '}' | '"' | '\\') || c.is_whitespace());
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_int4() {
        let bytes = 42_i32.to_be_bytes();
        assert_eq!(binary_to_sql_literal(23, &bytes), Some("'42'".into()));
        let bytes = (-1_i32).to_be_bytes();
        assert_eq!(binary_to_sql_literal(23, &bytes), Some("'-1'".into()));
    }

    #[test]
    fn test_int8() {
        let bytes = 1_234_567_890_123_i64.to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(20, &bytes),
            Some("'1234567890123'".into())
        );
    }

    #[test]
    fn test_int2() {
        let bytes = 12345_i16.to_be_bytes();
        assert_eq!(binary_to_sql_literal(21, &bytes), Some("'12345'".into()));
    }

    #[test]
    fn test_bool() {
        assert_eq!(binary_to_sql_literal(16, &[0]), Some("'f'".into()));
        assert_eq!(binary_to_sql_literal(16, &[1]), Some("'t'".into()));
    }

    #[test]
    fn test_text() {
        assert_eq!(binary_to_sql_literal(25, b"hello"), Some("'hello'".into()));
        assert_eq!(binary_to_sql_literal(25, b"it's"), Some("'it''s'".into()));
    }

    #[test]
    fn test_uuid() {
        // f47ac10b-58cc-4372-a567-0e02b2c3d479
        let bytes = [
            0xf4, 0x7a, 0xc1, 0x0b, 0x58, 0xcc, 0x43, 0x72, 0xa5, 0x67, 0x0e, 0x02, 0xb2, 0xc3,
            0xd4, 0x79,
        ];
        assert_eq!(
            binary_to_sql_literal(2950, &bytes),
            Some("'f47ac10b-58cc-4372-a567-0e02b2c3d479'".into())
        );
    }

    #[test]
    fn test_float4() {
        let bytes = 1.5_f32.to_be_bytes();
        let out = binary_to_sql_literal(700, &bytes).unwrap();
        assert_eq!(out, "'1.5'");
    }

    #[test]
    fn test_float8() {
        let bytes = 1.25_f64.to_be_bytes();
        let out = binary_to_sql_literal(701, &bytes).unwrap();
        assert_eq!(out, "'1.25'");
    }

    #[test]
    fn test_bytea() {
        assert_eq!(
            binary_to_sql_literal(17, &[0xde, 0xad, 0xbe, 0xef]),
            Some("E'\\\\xdeadbeef'".into())
        );
    }

    #[test]
    fn test_date() {
        // 0 days since PG epoch = 2000-01-01
        let bytes = 0_i32.to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(1082, &bytes),
            Some("'2000-01-01'::date".into())
        );
        // 366 days = 2001-01-01 (2000 is a leap year)
        let bytes = 366_i32.to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(1082, &bytes),
            Some("'2001-01-01'::date".into())
        );
    }

    #[test]
    fn test_timestamp() {
        // 0 micros = 2000-01-01 00:00:00
        let bytes = 0_i64.to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(1114, &bytes),
            Some("'2000-01-01 00:00:00'::timestamp".into())
        );
    }

    #[test]
    fn test_timestamptz() {
        let bytes = 0_i64.to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(1184, &bytes),
            Some("'2000-01-01 00:00:00+00'::timestamptz".into())
        );
    }

    #[test]
    fn test_numeric_zero() {
        // ndigits=0, weight=0, sign=0, dscale=0
        let bytes = [0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(binary_to_sql_literal(1700, &bytes), Some("'0'".into()));
    }

    #[test]
    fn test_numeric_integer() {
        // Value: 42 → ndigits=1, weight=0, sign=0, dscale=0, digits=[42]
        let bytes = [0, 1, 0, 0, 0, 0, 0, 0, 0, 42];
        assert_eq!(binary_to_sql_literal(1700, &bytes), Some("'42'".into()));
    }

    #[test]
    fn test_numeric_negative() {
        // Value: -12345 → ndigits=2, weight=1, sign=0x4000, dscale=0, digits=[1, 2345]
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_i16.to_be_bytes()); // ndigits
        bytes.extend_from_slice(&1_i16.to_be_bytes()); // weight
        bytes.extend_from_slice(&0x4000_u16.to_be_bytes()); // sign (negative)
        bytes.extend_from_slice(&0_i16.to_be_bytes()); // dscale
        bytes.extend_from_slice(&1_u16.to_be_bytes()); // digit[0]
        bytes.extend_from_slice(&2345_u16.to_be_bytes()); // digit[1]
        assert_eq!(binary_to_sql_literal(1700, &bytes), Some("'-12345'".into()));
    }

    #[test]
    fn test_numeric_with_fraction() {
        // Value: 3.14 → ndigits=2, weight=0, sign=0, dscale=2, digits=[3, 1400]
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_i16.to_be_bytes());
        bytes.extend_from_slice(&0_i16.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&2_i16.to_be_bytes());
        bytes.extend_from_slice(&3_u16.to_be_bytes());
        bytes.extend_from_slice(&1400_u16.to_be_bytes());
        assert_eq!(binary_to_sql_literal(1700, &bytes), Some("'3.14'".into()));
    }

    #[test]
    fn test_numeric_nan() {
        // sign=0xC000
        let bytes = [0, 0, 0, 0, 0xC0, 0, 0, 0];
        assert_eq!(binary_to_sql_literal(1700, &bytes), Some("'NaN'".into()));
    }

    #[test]
    fn test_jsonb() {
        let mut bytes = vec![1]; // version
        bytes.extend_from_slice(b"{\"k\":1}");
        assert_eq!(
            binary_to_sql_literal(3802, &bytes),
            Some("'{\"k\":1}'::jsonb".into())
        );
    }

    #[test]
    fn test_unknown_oid_returns_none() {
        assert_eq!(binary_to_sql_literal(9999, b"whatever"), None);
    }

    #[test]
    fn test_wrong_length_returns_none() {
        // int4 with 3 bytes
        assert_eq!(binary_to_sql_literal(23, &[0, 0, 0]), None);
    }

    // ── New types added in the SC-014 follow-up ─────────────────────────

    #[test]
    fn test_xml() {
        assert_eq!(
            binary_to_sql_literal(142, b"<root>hi</root>"),
            Some("'<root>hi</root>'::xml".into())
        );
    }

    #[test]
    fn test_money() {
        // $12.34 = 1234 cents
        let bytes = 1234_i64.to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(790, &bytes),
            Some("'12.34'::money".into())
        );
        // negative: -$0.05 = -5 cents
        let bytes = (-5_i64).to_be_bytes();
        assert_eq!(
            binary_to_sql_literal(790, &bytes),
            Some("'-0.05'::money".into())
        );
    }

    #[test]
    fn test_interval_zero() {
        let bytes = [0u8; 16];
        assert_eq!(
            binary_to_sql_literal(1186, &bytes),
            Some("'0'::interval".into())
        );
    }

    #[test]
    fn test_interval_full() {
        // 2 months, 5 days, 1h 2m 3.456789s
        let mut bytes = Vec::new();
        let micros: i64 = (3600 + 2 * 60 + 3) * 1_000_000 + 456_789;
        bytes.extend_from_slice(&micros.to_be_bytes());
        bytes.extend_from_slice(&5_i32.to_be_bytes()); // days
        bytes.extend_from_slice(&2_i32.to_be_bytes()); // months
        assert_eq!(
            binary_to_sql_literal(1186, &bytes),
            Some("'2 mons 5 days 01:02:03.456789'::interval".into())
        );
    }

    #[test]
    fn test_timetz_utc() {
        // 12:00:00 at UTC
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((12_i64 * 3600) * 1_000_000).to_be_bytes());
        bytes.extend_from_slice(&0_i32.to_be_bytes()); // offset 0
        assert_eq!(
            binary_to_sql_literal(1266, &bytes),
            Some("'12:00:00+00'::timetz".into())
        );
    }

    #[test]
    fn test_timetz_est() {
        // 09:30:00 EST — PG stores offset as seconds WEST of UTC, so EST = +18000
        let mut bytes = Vec::new();
        let micros: i64 = ((9_i64 * 3600) + (30 * 60)) * 1_000_000;
        bytes.extend_from_slice(&micros.to_be_bytes());
        bytes.extend_from_slice(&18000_i32.to_be_bytes());
        assert_eq!(
            binary_to_sql_literal(1266, &bytes),
            Some("'09:30:00-05'::timetz".into())
        );
    }

    #[test]
    fn test_bit_string() {
        // 6-bit string 010101
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&6_i32.to_be_bytes()); // bit_count
        bytes.push(0b0101_0100); // 6 bits packed MSB-first into a byte (top 6 bits valid)
        assert_eq!(
            binary_to_sql_literal(1560, &bytes),
            Some("B'010101'".into())
        );
    }

    #[test]
    fn test_inet_v4() {
        // 192.168.1.5/24
        let bytes = vec![2, 24, 0, 4, 192, 168, 1, 5];
        assert_eq!(
            binary_to_sql_literal(869, &bytes),
            Some("'192.168.1.5/24'::inet".into())
        );
    }

    #[test]
    fn test_inet_v4_host() {
        // 10.0.0.1 with default /32 → bare host, no /bits
        let bytes = vec![2, 32, 0, 4, 10, 0, 0, 1];
        assert_eq!(
            binary_to_sql_literal(869, &bytes),
            Some("'10.0.0.1'::inet".into())
        );
    }

    #[test]
    fn test_cidr_v4() {
        // 10.0.0.0/8
        let bytes = vec![2, 8, 1, 4, 10, 0, 0, 0];
        assert_eq!(
            binary_to_sql_literal(650, &bytes),
            Some("'10.0.0.0/8'::cidr".into())
        );
    }

    #[test]
    fn test_inet_v6() {
        // 2001:db8::1 (bits 128 → no /bits)
        let mut bytes = vec![3, 128, 0, 16];
        bytes.extend_from_slice(&[
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
        ]);
        assert_eq!(
            binary_to_sql_literal(869, &bytes),
            Some("'2001:db8::1'::inet".into())
        );
    }

    #[test]
    fn test_macaddr() {
        let bytes = vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(
            binary_to_sql_literal(829, &bytes),
            Some("'aa:bb:cc:dd:ee:ff'::macaddr".into())
        );
    }

    #[test]
    fn test_macaddr8() {
        let bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(
            binary_to_sql_literal(774, &bytes),
            Some("'01:02:03:04:05:06:07:08'::macaddr8".into())
        );
    }

    // ── Array tests ─────────────────────────────────────────────────────

    fn array_header(elem_oid: u32, dim_len: i32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&1_i32.to_be_bytes()); // ndims
        h.extend_from_slice(&0_i32.to_be_bytes()); // flags (no nulls)
        h.extend_from_slice(&elem_oid.to_be_bytes());
        h.extend_from_slice(&dim_len.to_be_bytes()); // dim_len
        h.extend_from_slice(&1_i32.to_be_bytes()); // lower_bound
        h
    }

    fn push_elem(out: &mut Vec<u8>, data: &[u8]) {
        out.extend_from_slice(&(data.len() as i32).to_be_bytes());
        out.extend_from_slice(data);
    }

    #[test]
    fn test_array_int4() {
        let mut bytes = array_header(23, 3);
        push_elem(&mut bytes, &1_i32.to_be_bytes());
        push_elem(&mut bytes, &2_i32.to_be_bytes());
        push_elem(&mut bytes, &3_i32.to_be_bytes());
        assert_eq!(
            binary_to_sql_literal(1007, &bytes),
            Some("'{1,2,3}'::int4[]".into())
        );
    }

    #[test]
    fn test_array_int4_with_null() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes()); // flags: has_nulls
        bytes.extend_from_slice(&23_u32.to_be_bytes());
        bytes.extend_from_slice(&3_i32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        push_elem(&mut bytes, &1_i32.to_be_bytes());
        bytes.extend_from_slice(&(-1_i32).to_be_bytes()); // NULL element
        push_elem(&mut bytes, &3_i32.to_be_bytes());
        assert_eq!(
            binary_to_sql_literal(1007, &bytes),
            Some("'{1,NULL,3}'::int4[]".into())
        );
    }

    #[test]
    fn test_array_text_with_quoting() {
        let mut bytes = array_header(25, 3);
        push_elem(&mut bytes, b"simple");
        push_elem(&mut bytes, b"has space");
        push_elem(&mut bytes, b"has,comma");
        assert_eq!(
            binary_to_sql_literal(1009, &bytes),
            Some(r#"'{simple,"has space","has,comma"}'::text[]"#.into())
        );
    }

    #[test]
    fn test_array_text_with_backslash_and_quote() {
        let mut bytes = array_header(25, 1);
        push_elem(&mut bytes, b"a\"b\\c");
        // Both " and \ are escaped with a backslash inside the double quotes.
        assert_eq!(
            binary_to_sql_literal(1009, &bytes),
            Some(r#"'{"a\"b\\c"}'::text[]"#.into())
        );
    }

    #[test]
    fn test_array_uuid() {
        let u = [
            0xf4, 0x7a, 0xc1, 0x0b, 0x58, 0xcc, 0x43, 0x72, 0xa5, 0x67, 0x0e, 0x02, 0xb2, 0xc3,
            0xd4, 0x79,
        ];
        let mut bytes = array_header(2950, 1);
        push_elem(&mut bytes, &u);
        assert_eq!(
            binary_to_sql_literal(2951, &bytes),
            Some("'{f47ac10b-58cc-4372-a567-0e02b2c3d479}'::uuid[]".into())
        );
    }

    #[test]
    fn test_array_empty() {
        // ndims=0 → empty array regardless of element type
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0_i32.to_be_bytes()); // ndims
        bytes.extend_from_slice(&0_i32.to_be_bytes()); // flags
        bytes.extend_from_slice(&23_u32.to_be_bytes()); // elem_oid
        assert_eq!(
            binary_to_sql_literal(1007, &bytes),
            Some("'{}'::int4[]".into())
        );
    }

    #[test]
    fn test_array_multidim_rejected() {
        // 2-D arrays return None → caller falls back to placeholder
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_i32.to_be_bytes()); // ndims
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.extend_from_slice(&23_u32.to_be_bytes());
        // rest doesn't matter
        bytes.extend_from_slice(&[0; 32]);
        assert_eq!(binary_to_sql_literal(1007, &bytes), None);
    }

    #[test]
    fn test_array_non_one_lower_bound_rejected() {
        // Arrays with lower_bound != 1 bail to placeholder
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.extend_from_slice(&0_i32.to_be_bytes());
        bytes.extend_from_slice(&23_u32.to_be_bytes());
        bytes.extend_from_slice(&2_i32.to_be_bytes()); // dim_len
        bytes.extend_from_slice(&0_i32.to_be_bytes()); // lower_bound = 0
        assert_eq!(binary_to_sql_literal(1007, &bytes), None);
    }

    #[test]
    fn test_pg_array_quote() {
        assert_eq!(pg_array_quote("simple"), "simple");
        assert_eq!(pg_array_quote("has space"), "\"has space\"");
        assert_eq!(pg_array_quote(""), "\"\"");
        assert_eq!(pg_array_quote("null"), "\"null\""); // case-insensitive NULL guard
        assert_eq!(pg_array_quote("a\\b"), "\"a\\\\b\"");
    }

    // ── pgvector extension types (dynamic OIDs) ─────────────────────────

    #[test]
    fn test_vector_3d() {
        // dim=3, [1.5, 2.5, -3.5]
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3_i16.to_be_bytes());
        bytes.extend_from_slice(&0_i16.to_be_bytes()); // reserved
        bytes.extend_from_slice(&1.5_f32.to_be_bytes());
        bytes.extend_from_slice(&2.5_f32.to_be_bytes());
        bytes.extend_from_slice(&(-3.5_f32).to_be_bytes());
        // Extension OIDs map the pgvector type to some dynamic value (we pick 99999)
        let ext = ExtensionOids {
            vector: Some(99999),
            ..Default::default()
        };
        assert_eq!(
            binary_to_sql_literal_ext(99999, &bytes, &ext),
            Some("'[1.5,2.5,-3.5]'::vector".into())
        );
    }

    #[test]
    fn test_vector_ignored_without_ext_oids() {
        // Same bytes, but without the OID registered → falls back to None
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i16.to_be_bytes());
        bytes.extend_from_slice(&0_i16.to_be_bytes());
        bytes.extend_from_slice(&1.0_f32.to_be_bytes());
        assert_eq!(binary_to_sql_literal(99999, &bytes), None);
    }

    #[test]
    fn test_halfvec() {
        // dim=2, values 1.0 and 2.0 as IEEE-754 binary16
        // f16 bits: 1.0 = 0x3c00, 2.0 = 0x4000
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_i16.to_be_bytes());
        bytes.extend_from_slice(&0_i16.to_be_bytes());
        bytes.extend_from_slice(&0x3c00_u16.to_be_bytes());
        bytes.extend_from_slice(&0x4000_u16.to_be_bytes());
        let ext = ExtensionOids {
            halfvec: Some(88888),
            ..Default::default()
        };
        assert_eq!(
            binary_to_sql_literal_ext(88888, &bytes, &ext),
            Some("'[1,2]'::halfvec".into())
        );
    }

    #[test]
    fn test_f16_to_f32_zero_and_special() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x8000), -0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xbc00), -1.0);
        // Inf
        assert!(f16_to_f32(0x7c00).is_infinite() && f16_to_f32(0x7c00) > 0.0);
        // -Inf
        assert!(f16_to_f32(0xfc00).is_infinite() && f16_to_f32(0xfc00) < 0.0);
        // NaN
        assert!(f16_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn test_sparsevec() {
        // dim=10, nnz=2, indices=[3,7], values=[0.5, 0.25]
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&10_i32.to_be_bytes());
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        bytes.extend_from_slice(&0_i32.to_be_bytes()); // reserved
        bytes.extend_from_slice(&3_i32.to_be_bytes());
        bytes.extend_from_slice(&7_i32.to_be_bytes());
        bytes.extend_from_slice(&0.5_f32.to_be_bytes());
        bytes.extend_from_slice(&0.25_f32.to_be_bytes());
        let ext = ExtensionOids {
            sparsevec: Some(77777),
            ..Default::default()
        };
        assert_eq!(
            binary_to_sql_literal_ext(77777, &bytes, &ext),
            Some("'{3:0.5,7:0.25}/10'::sparsevec".into())
        );
    }

    #[test]
    fn test_compress_ipv6() {
        // All zeros → ::
        assert_eq!(compress_ipv6(&[0; 8]), "::");
        // Single zero run
        assert_eq!(
            compress_ipv6(&[0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]),
            "2001:db8::1"
        );
        // No zeros
        assert_eq!(compress_ipv6(&[1, 2, 3, 4, 5, 6, 7, 8]), "1:2:3:4:5:6:7:8");
    }
}
