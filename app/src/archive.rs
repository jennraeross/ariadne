//! Minimal ZIP support shared by DOCX/ePub export and project archives.
//!
//! Ariadne writes stored (uncompressed) entries, but the reader also
//! tolerates deflated archives produced by other tools. Entry names are
//! returned verbatim; callers validate them before touching the disk.

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Minimal stored (uncompressed) ZIP writer — enough for DOCX/ePub.
pub fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    fn w16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn w32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let crc = crc32(data);
        let head = out.len() as u32;
        let len = data.len() as u32;
        w32(&mut out, 0x0403_4B50);
        w16(&mut out, 20);
        w16(&mut out, 0x0800);
        w16(&mut out, 0);
        w16(&mut out, 0);
        w16(&mut out, 0);
        w32(&mut out, crc);
        w32(&mut out, len);
        w32(&mut out, len);
        w16(&mut out, name.len() as u16);
        w16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        w32(&mut central, 0x0201_4B50);
        w16(&mut central, 20);
        w16(&mut central, 20);
        w16(&mut central, 0x0800);
        w16(&mut central, 0);
        w16(&mut central, 0);
        w16(&mut central, 0);
        w32(&mut central, crc);
        w32(&mut central, len);
        w32(&mut central, len);
        w16(&mut central, name.len() as u16);
        w16(&mut central, 0);
        w16(&mut central, 0);
        w16(&mut central, 0);
        w16(&mut central, 0);
        w32(&mut central, 0);
        w32(&mut central, head);
        central.extend_from_slice(name.as_bytes());
    }
    let cstart = out.len() as u32;
    out.extend_from_slice(&central);
    let cend = out.len() as u32;
    w32(&mut out, 0x0605_4B50);
    w16(&mut out, 0);
    w16(&mut out, 0);
    w16(&mut out, files.len() as u16);
    w16(&mut out, files.len() as u16);
    w32(&mut out, cend - cstart);
    w32(&mut out, cstart);
    w16(&mut out, 0);
    out
}

/// Read a ZIP archive written by [`build_zip`] (or any standard
/// ZIP with stored/deflated entries). Used only for project-archive
/// import; entry names are validated by the caller before any file is
/// written. ZIP64 and encrypted entries are rejected.
pub fn read_entries(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    fn le16(b: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
    }
    fn le32(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }
    const EOCD: u32 = 0x0605_4B50;
    const CENTRAL: u32 = 0x0201_4B50;
    const LOCAL: u32 = 0x0403_4B50;
    const MAX_ENTRY: usize = 256 * 1024 * 1024;
    if bytes.len() < 22 {
        return Err("Not a ZIP archive".to_string());
    }
    let eocd = (0..=bytes.len() - 22)
        .rev()
        .find(|&i| le32(bytes, i) == Some(EOCD))
        .ok_or_else(|| "Not a ZIP archive".to_string())?;
    let count = le16(bytes, eocd + 10).ok_or("Corrupt ZIP directory")? as usize;
    let mut p = le32(bytes, eocd + 16).ok_or("Corrupt ZIP directory")? as usize;
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for _ in 0..count {
        if le32(bytes, p) != Some(CENTRAL) {
            return Err("Corrupt ZIP directory".to_string());
        }
        let method = le16(bytes, p + 10).ok_or("Corrupt ZIP entry")?;
        let csize = le32(bytes, p + 20).ok_or("Corrupt ZIP entry")? as usize;
        let name_len = le16(bytes, p + 28).ok_or("Corrupt ZIP entry")? as usize;
        let extra_len = le16(bytes, p + 30).ok_or("Corrupt ZIP entry")? as usize;
        let comment_len = le16(bytes, p + 32).ok_or("Corrupt ZIP entry")? as usize;
        let local_off = le32(bytes, p + 42).ok_or("Corrupt ZIP entry")? as usize;
        let name_at = p + 46;
        let name_bytes = bytes
            .get(name_at..name_at + name_len)
            .ok_or("Corrupt ZIP entry")?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        p = name_at + name_len + extra_len + comment_len;
        if le32(bytes, local_off) != Some(LOCAL) {
            return Err("Corrupt ZIP local header".to_string());
        }
        let l_name = le16(bytes, local_off + 26).ok_or("Corrupt ZIP entry")? as usize;
        let l_extra = le16(bytes, local_off + 28).ok_or("Corrupt ZIP entry")? as usize;
        let data_at = local_off + 30 + l_name + l_extra;
        let data = bytes
            .get(data_at..data_at + csize)
            .ok_or("Truncated ZIP entry")?;
        let content = match method {
            0 => {
                if data.len() > MAX_ENTRY {
                    return Err("ZIP entry is too large".to_string());
                }
                data.to_vec()
            }
            8 => {
                use std::io::Read as _;
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(data)
                    .take(MAX_ENTRY as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("Could not inflate {name}: {e}"))?;
                if out.len() > MAX_ENTRY {
                    return Err("ZIP entry is too large".to_string());
                }
                out
            }
            _ => return Err(format!("Unsupported ZIP compression in {name}")),
        };
        out.push((name, content));
    }
    Ok(out)
}
