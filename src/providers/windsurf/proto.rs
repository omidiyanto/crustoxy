//! Protobuf wire format codec — zero-dependency, schema-less.
//!
//! Wire types:
//!   0 = Varint    (int32, uint64, bool, enum)
//!   2 = LenDelim  (string, bytes, embedded messages)
//!
//! Ported from WindsurfAPI/src/proto.js

// ─── Varint encoding ────────────────────────────────────

pub fn encode_varint(value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut v = value;
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v > 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if v == 0 {
            break;
        }
    }
    bytes
}

pub fn decode_varint(buf: &[u8], offset: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    let mut pos = offset;
    while pos < buf.len() {
        let byte = buf[pos];
        pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((result, pos - offset));
        }
        shift += 7;
        if shift >= 64 {
            return None; // overflow
        }
    }
    None // truncated
}

// ─── Field-level writers ────────────────────────────────

fn make_tag(field: u32, wire_type: u32) -> Vec<u8> {
    encode_varint(((field << 3) | wire_type) as u64)
}

/// Write a varint field (wire type 0).
pub fn write_varint_field(field: u32, value: u64) -> Vec<u8> {
    let mut buf = make_tag(field, 0);
    buf.extend(encode_varint(value));
    buf
}

/// Write a length-delimited string field (wire type 2).
pub fn write_string_field(field: u32, s: &str) -> Vec<u8> {
    if s.is_empty() {
        // Still emit the field even for empty strings (needed for cascade_id etc.)
        let mut buf = make_tag(field, 2);
        buf.push(0); // length = 0
        return buf;
    }
    let data = s.as_bytes();
    let mut buf = make_tag(field, 2);
    buf.extend(encode_varint(data.len() as u64));
    buf.extend(data);
    buf
}

/// Write an embedded message field (wire type 2).
/// Always emits the field — even for zero-length messages, which is
/// semantically distinct from "field absent" in protobuf.
pub fn write_message_field(field: u32, msg: &[u8]) -> Vec<u8> {
    let mut buf = make_tag(field, 2);
    buf.extend(encode_varint(msg.len() as u64));
    buf.extend(msg);
    buf
}

/// Write a bool field (wire type 0). Emits both true and false.
pub fn write_bool_field(field: u32, value: bool) -> Vec<u8> {
    write_varint_field(field, if value { 1 } else { 0 })
}

// ─── Parser ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProtoField {
    pub field_num: u32,
    pub wire_type: u32,
    pub varint_value: u64,    // valid for wire_type 0
    pub bytes_value: Vec<u8>, // valid for wire_type 2
}

impl ProtoField {
    pub fn as_string(&self) -> String {
        String::from_utf8_lossy(&self.bytes_value).to_string()
    }
}

/// Parse a protobuf buffer into a list of fields.
pub fn parse_fields(buf: &[u8]) -> Vec<ProtoField> {
    let mut fields = Vec::new();
    let mut pos = 0;

    while pos < buf.len() {
        let (tag, tag_len) = match decode_varint(buf, pos) {
            Some(v) => v,
            None => break,
        };
        pos += tag_len;

        let field_num = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u32;

        match wire_type {
            0 => {
                // varint
                let (value, vlen) = match decode_varint(buf, pos) {
                    Some(v) => v,
                    None => break,
                };
                pos += vlen;
                fields.push(ProtoField {
                    field_num,
                    wire_type,
                    varint_value: value,
                    bytes_value: Vec::new(),
                });
            }
            1 => {
                // fixed64
                if pos + 8 > buf.len() {
                    break;
                }
                pos += 8;
                // skip fixed64 fields — not needed for our use case
            }
            2 => {
                // length-delimited
                let (len, llen) = match decode_varint(buf, pos) {
                    Some(v) => v,
                    None => break,
                };
                pos += llen;
                let sz = len as usize;
                if pos + sz > buf.len() {
                    break;
                }
                let data = buf[pos..pos + sz].to_vec();
                pos += sz;
                fields.push(ProtoField {
                    field_num,
                    wire_type,
                    varint_value: 0,
                    bytes_value: data,
                });
            }
            5 => {
                // fixed32
                if pos + 4 > buf.len() {
                    break;
                }
                pos += 4;
                // skip fixed32 fields — not needed for our use case
            }
            _ => break,
        }
    }

    fields
}

/// Get first field matching a number with specific wire type.
pub fn get_field_typed(fields: &[ProtoField], num: u32, wire_type: u32) -> Option<&ProtoField> {
    fields
        .iter()
        .find(|f| f.field_num == num && f.wire_type == wire_type)
}

/// Get all fields matching a number.
pub fn get_all_fields(fields: &[ProtoField], num: u32) -> Vec<&ProtoField> {
    fields.iter().filter(|f| f.field_num == num).collect()
}
