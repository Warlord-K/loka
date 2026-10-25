use serde_json::{Map, Number, Value};

pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

fn write_len(out: &mut Vec<u8>, len: usize, fix: u8, fix_max: usize, codes: [u8; 3]) {
    if len <= fix_max {
        out.push(fix | len as u8);
    } else if len <= u8::MAX as usize && codes[0] != 0 {
        out.push(codes[0]);
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(codes[1]);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(codes[2]);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.push(0xc0),
        Value::Bool(b) => out.push(if *b { 0xc3 } else { 0xc2 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if (0..=127).contains(&i) {
                    out.push(i as u8);
                } else if (-32..0).contains(&i) {
                    out.push(i as i8 as u8);
                } else {
                    out.push(0xd3);
                    out.extend_from_slice(&i.to_be_bytes());
                }
            } else if let Some(u) = n.as_u64() {
                out.push(0xcf);
                out.extend_from_slice(&u.to_be_bytes());
            } else {
                out.push(0xcb);
                out.extend_from_slice(&n.as_f64().unwrap_or(0.0).to_be_bytes());
            }
        }
        Value::String(s) => {
            write_len(out, s.len(), 0xa0, 31, [0xd9, 0xda, 0xdb]);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(items) => {
            write_len(out, items.len(), 0x90, 15, [0, 0xdc, 0xdd]);
            for item in items {
                write(item, out);
            }
        }
        Value::Object(map) => {
            write_len(out, map.len(), 0x80, 15, [0, 0xde, 0xdf]);
            for (k, v) in map {
                write(&Value::String(k.clone()), out);
                write(v, out);
            }
        }
    }
}

pub fn decode(bytes: &[u8]) -> Option<Value> {
    let mut pos = 0;
    read(bytes, &mut pos, 0)
}

fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(n)?;
    let slice = bytes.get(*pos..end)?;
    *pos = end;
    Some(slice)
}

fn uint(bytes: &[u8], pos: &mut usize, n: usize) -> Option<u64> {
    Some(take(bytes, pos, n)?.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64))
}

fn int(bytes: &[u8], pos: &mut usize, n: usize) -> Option<i64> {
    let raw = uint(bytes, pos, n)?;
    let shift = 64 - n * 8;
    Some(((raw << shift) as i64) >> shift)
}

fn text(bytes: &[u8], pos: &mut usize, n: usize) -> Option<Value> {
    Some(Value::String(String::from_utf8_lossy(take(bytes, pos, n)?).into_owned()))
}

fn array(bytes: &[u8], pos: &mut usize, n: usize, depth: usize) -> Option<Value> {
    let mut items = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        items.push(read(bytes, pos, depth + 1)?);
    }
    Some(Value::Array(items))
}

fn map(bytes: &[u8], pos: &mut usize, n: usize, depth: usize) -> Option<Value> {
    let mut out = Map::new();
    for _ in 0..n {
        let key = match read(bytes, pos, depth + 1)? {
            Value::String(s) => s,
            other => other.to_string(),
        };
        out.insert(key, read(bytes, pos, depth + 1)?);
    }
    Some(Value::Object(out))
}

fn float(value: f64) -> Value {
    Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
}

fn read(bytes: &[u8], pos: &mut usize, depth: usize) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    let tag = *take(bytes, pos, 1)?.first()?;
    match tag {
        0x00..=0x7f => Some(Value::from(tag as u64)),
        0x80..=0x8f => map(bytes, pos, (tag & 0x0f) as usize, depth),
        0x90..=0x9f => array(bytes, pos, (tag & 0x0f) as usize, depth),
        0xa0..=0xbf => text(bytes, pos, (tag & 0x1f) as usize),
        0xc0 => Some(Value::Null),
        0xc2 => Some(Value::Bool(false)),
        0xc3 => Some(Value::Bool(true)),
        0xc4..=0xc6 => {
            let n = uint(bytes, pos, 1 << (tag - 0xc4))? as usize;
            text(bytes, pos, n)
        }
        0xca => Some(float(f32::from_bits(uint(bytes, pos, 4)? as u32) as f64)),
        0xcb => Some(float(f64::from_bits(uint(bytes, pos, 8)?))),
        0xcc..=0xcf => Some(Value::from(uint(bytes, pos, 1 << (tag - 0xcc))?)),
        0xd0..=0xd3 => Some(Value::from(int(bytes, pos, 1 << (tag - 0xd0))?)),
        0xd9..=0xdb => {
            let n = uint(bytes, pos, 1 << (tag - 0xd9))? as usize;
            text(bytes, pos, n)
        }
        0xdc | 0xdd => {
            let n = uint(bytes, pos, if tag == 0xdc { 2 } else { 4 })? as usize;
            array(bytes, pos, n, depth)
        }
        0xde | 0xdf => {
            let n = uint(bytes, pos, if tag == 0xde { 2 } else { 4 })? as usize;
            map(bytes, pos, n, depth)
        }
        0xe0..=0xff => Some(Value::from(tag as i8 as i64)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_realtime_events() {
        let event = json!({
            "type": "session.update",
            "session": { "voice": "eve", "rate": 24000, "neg": -5, "big": -40000, "on": true, "none": null, "f": 0.5, "list": [1, "two", [3]] },
            "long": "x".repeat(300),
        });
        assert_eq!(decode(&encode(&event)), Some(event));
    }
}
