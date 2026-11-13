//! Oodle Mermaid decompressor (the only codec Fortnite replays use).
//!
//! Port of OozSharp/Kraken.cs from ChestCounter, itself a C# port of ooz by Powzix.
//! Copyright (C) 2016, Powzix. Licensed under the GNU General Public License v3 or later.
//! The C# version uses raw pointers; this one uses indexes and bounds checks instead.

type R<T> = Result<T, String>;

const MERMAID: u8 = 10;
/// Room for the decoder's 8/16/32-byte block copies past the logical end.
const SLACK: usize = 64;

struct Header {
    decoder_type: u8,
    uncompressed: bool,
    use_checksums: bool,
}

fn byte(src: &[u8], i: usize) -> R<u8> {
    src.get(i).copied().ok_or_else(|| "oodle: read past end of input".to_string())
}

fn u16_at(src: &[u8], i: usize) -> R<u16> {
    Ok(u16::from_le_bytes([byte(src, i)?, byte(src, i + 1)?]))
}

pub fn decompress(src: &[u8], out_size: usize) -> R<Vec<u8>> {
    let mut dst = vec![0u8; out_size + SLACK];
    let mut header: Option<Header> = None;
    let mut sp = 0usize;
    let mut dp = 0usize;
    while dp < out_size {
        let (src_used, dst_used) = decode_step(&mut header, &mut dst, dp, out_size - dp, src, sp)?;
        if src_used == 0 && dst_used == 0 {
            // The C# loops forever here; a truncated quantum is an error instead.
            return Err("oodle: not enough input for quantum".into());
        }
        sp += src_used;
        dp += dst_used;
    }
    dst.truncate(out_size);
    Ok(dst)
}

fn decode_step(header: &mut Option<Header>, dst: &mut [u8], off: usize, remaining: usize, src: &[u8], start: usize) -> R<(usize, usize)> {
    let mut s = start;
    let end = src.len();
    if off & 0x3FFFF == 0 {
        let (b0, b1) = (byte(src, s)?, byte(src, s + 1)?);
        if b0 & 0xF != 0xC || (b0 >> 4) & 3 != 0 {
            return Err("oodle: bad block header".into());
        }
        *header = Some(Header { decoder_type: b1 & 0x7F, uncompressed: (b0 >> 6) & 1 == 1, use_checksums: (b1 >> 7) & 1 == 1 });
        s += 2;
    }
    let h = header.as_ref().ok_or("oodle: missing header")?;
    let is_mermaid = h.decoder_type == MERMAID;
    let dst_left = remaining.min(if is_mermaid { 0x40000 } else { 0x4000 });

    if h.uncompressed {
        if end - s < dst_left {
            return Err("oodle: uncompressed block too short".into());
        }
        dst[off..off + dst_left].copy_from_slice(&src[s..s + dst_left]);
        return Ok((s - start + dst_left, dst_left));
    }
    if !is_mermaid {
        return Err(format!("oodle: decoder type {} not supported", h.decoder_type));
    }

    // Quantum header
    let v = (byte(src, s)? as u32) << 16 | (byte(src, s + 1)? as u32) << 8 | byte(src, s + 2)? as u32;
    let size = v & 0x3FFFF;
    let (compressed_size, checksum) = if size != 0x3FFFF {
        let cs = if h.use_checksums { (byte(src, s + 3)? as u32) << 16 | (byte(src, s + 4)? as u32) << 8 | byte(src, s + 5)? as u32 } else { 0 };
        s += if h.use_checksums { 6 } else { 3 };
        (size as usize + 1, cs)
    } else if v >> 18 == 1 {
        let cs = byte(src, s + 3)? as u32;
        s += 4;
        (0, cs)
    } else {
        return Err("oodle: bad quantum header".into());
    };

    if s > end {
        return Err("oodle: index out of range".into());
    }
    if end - s < compressed_size {
        return Ok((0, 0));
    }
    if compressed_size > remaining {
        return Err("oodle: compressed size larger than output".into());
    }
    if compressed_size == 0 {
        // Whole quantum is one repeated byte (ooz memset).
        dst[off..off + dst_left].fill(checksum as u8);
        return Ok((s - start, dst_left));
    }
    if h.use_checksums {
        return Err("oodle: checksums not supported".into());
    }
    if compressed_size == dst_left {
        // Stored quantum. The C# throws NotImplemented; ooz copies it.
        dst[off..off + dst_left].copy_from_slice(&src[s..s + dst_left]);
        return Ok((s - start + dst_left, dst_left));
    }
    let n = mermaid_decode_quantum(dst, off, off + dst_left, src, s, s + compressed_size)?;
    if n != compressed_size {
        return Err("oodle: wrong quantum size".into());
    }
    Ok((s - start + n, dst_left))
}

/// Raw byte stream inside the compressed data (only "memcopy" chunks are used by Fortnite).
fn decode_bytes(src: &[u8], s: usize, s_end: usize, out_limit: i64) -> R<(usize, usize, usize)> {
    if s_end - s < 2 {
        return Err("oodle: decode_bytes too short".into());
    }
    let b0 = src[s];
    if (b0 >> 4) & 7 != 0 {
        return Err("oodle: entropy-coded streams not supported".into());
    }
    let (size, data) = if b0 >= 0x80 {
        ((((b0 as usize) << 8) | src[s + 1] as usize) & 0xFFF, s + 2)
    } else {
        if s_end - s < 3 {
            return Err("oodle: decode_bytes too short".into());
        }
        let size = (b0 as usize) << 16 | (src[s + 1] as usize) << 8 | src[s + 2] as usize;
        if size & !0x3ffff != 0 {
            return Err("oodle: reserved bits set".into());
        }
        (size, s + 3)
    };
    if size as i64 > out_limit || s_end - data < size {
        return Err("oodle: decode_bytes size".into());
    }
    Ok((data, size, data + size - s))
}

struct LzTable {
    cmd: (usize, usize), // range in src
    cmd2_offsets: usize,
    cmd2_offsets_end: usize,
    lit: usize, // index in src
    lit_end: usize,
    off16: Vec<u16>,
    off16_pos: usize,
    off32_1: Vec<u32>,
    off32_2: Vec<u32>,
    length: usize, // index in src
}

fn mermaid_decode_quantum(dst: &mut [u8], mut d: usize, d_end: usize, src: &[u8], mut s: usize, s_end: usize) -> R<usize> {
    let s_in = s;
    while d_end - d != 0 {
        let count = (d_end - d).min(0x20000);
        if s_end - s < 4 {
            return Err("oodle: quantum too short".into());
        }
        let hdr = src[s + 2] as usize | (src[s + 1] as usize) << 8 | (src[s] as usize) << 16;
        if hdr & 0x800000 == 0 {
            return Err("oodle: stored-without-matches chunk not supported".into());
        }
        s += 3;
        let used = hdr & 0x7FFFF;
        let mode = (hdr >> 19) & 0xF;
        if s_end - s < used {
            return Err("oodle: chunk larger than input".into());
        }
        if used < count {
            let temp_usage = (2 * count + 32).min(0x40000) as i64;
            let mut lz = read_lz_table(mode, src, s, s + used, dst, d, count, d, temp_usage)?;
            process_lz_runs(mode, src, s + used, dst, d, count, d, &mut lz)?;
        } else if used > count || mode != 0 {
            return Err("oodle: bad raw chunk".into());
        } else {
            dst[d..d + count].copy_from_slice(&src[s..s + count]);
        }
        s += used;
        d += count;
    }
    Ok(s - s_in)
}

#[allow(clippy::too_many_arguments)]
fn read_lz_table(mode: usize, src: &[u8], mut s: usize, s_end: usize, dst: &mut [u8], d: usize, dst_size: usize, offset: usize, scratch_end: i64) -> R<LzTable> {
    if mode > 1 || s_end - s < 10 {
        return Err("oodle: bad lz table".into());
    }
    if offset == 0 {
        dst[d..d + 8].copy_from_slice(&src[s..s + 8]);
        s += 8;
    }
    // Scratch space is only tracked for its size checks (base address assumed 8-aligned).
    let mut scratch: i64 = 104;
    let limit = |scratch: i64, cap: usize| (scratch_end - scratch).min(cap as i64);

    let (lit, lit_count, n) = decode_bytes(src, s, s_end, limit(scratch, dst_size))?;
    s += n;
    scratch += lit_count as i64;
    let (cmd, cmd_count, n) = decode_bytes(src, s, s_end, limit(scratch, dst_size))?;
    s += n;
    scratch += cmd_count as i64;

    let cmd2_offsets_end = cmd_count;
    let cmd2_offsets = if dst_size <= 0x10000 {
        cmd_count
    } else {
        if s_end - s < 2 {
            return Err("oodle: lz table too short".into());
        }
        let v = u16_at(src, s)? as usize;
        s += 2;
        if v > cmd2_offsets_end {
            return Err("oodle: bad cmd2 offset".into());
        }
        v
    };

    if s_end - s < 2 {
        return Err("oodle: lz table too short".into());
    }
    let off16_count = u16_at(src, s)? as usize;
    let off16: Vec<u16> = if off16_count == 0xFFFF {
        s += 2;
        let (hi, hi_count, n) = decode_bytes(src, s, s_end, limit(scratch, dst_size >> 1))?;
        s += n;
        scratch += hi_count as i64;
        let (lo, lo_count, n) = decode_bytes(src, s, s_end, limit(scratch, dst_size >> 1))?;
        s += n;
        scratch += lo_count as i64;
        if lo_count != hi_count {
            return Err("oodle: off16 halves differ".into());
        }
        scratch = (scratch + 1) & !1;
        if scratch + lo_count as i64 * 2 > scratch_end {
            return Err("oodle: scratch overflow".into());
        }
        scratch += lo_count as i64 * 2;
        (0..lo_count).map(|i| src[lo + i] as u16 + src[hi + i] as u16 * 256).collect()
    } else {
        let start = s + 2;
        s += 2 + off16_count * 2;
        if s > s_end {
            return Err("oodle: off16 stream past end".into());
        }
        (0..off16_count).map(|i| u16::from_le_bytes([src[start + 2 * i], src[start + 2 * i + 1]])).collect()
    };

    if s_end - s < 3 {
        return Err("oodle: lz table too short".into());
    }
    let temp = src[s] as u32 | (src[s + 1] as u32) << 8 | (src[s + 2] as u32) << 16;
    s += 3;
    let (mut off32_1, mut off32_2) = (Vec::new(), Vec::new());
    if temp != 0 {
        let mut size1 = (temp >> 12) as usize;
        let mut size2 = (temp & 0xFFF) as usize;
        if size1 == 4095 {
            if s_end - s < 2 {
                return Err("oodle: lz table too short".into());
            }
            size1 = u16_at(src, s)? as usize;
            s += 2;
        }
        if size2 == 4095 {
            if s_end - s < 2 {
                return Err("oodle: lz table too short".into());
            }
            size2 = u16_at(src, s)? as usize;
            s += 2;
        }
        if scratch + 4 * (size1 + size2) as i64 + 64 > scratch_end {
            return Err("oodle: scratch overflow".into());
        }
        let n;
        (off32_1, n) = decode_far_offsets(src, s, s_end, size1, offset as i64)?;
        s += n;
        let n;
        (off32_2, n) = decode_far_offsets(src, s, s_end, size2, offset as i64 + 0x10000)?;
        s += n;
    } else if scratch_end - scratch < 32 {
        return Err("oodle: scratch overflow".into());
    }

    Ok(LzTable {
        cmd: (cmd, cmd + cmd_count),
        cmd2_offsets,
        cmd2_offsets_end,
        lit,
        lit_end: lit + lit_count,
        off16,
        off16_pos: 0,
        off32_1,
        off32_2,
        length: s,
    })
}

fn decode_far_offsets(src: &[u8], s: usize, s_end: usize, count: usize, offset: i64) -> R<(Vec<u32>, usize)> {
    let mut out = Vec::with_capacity(count);
    let mut p = s;
    for _ in 0..count {
        if s_end - p < 3 {
            return Err("oodle: far offsets too short".into());
        }
        let mut off = src[p] as u32 | (src[p + 1] as u32) << 8 | (src[p + 2] as u32) << 16;
        p += 3;
        if offset >= 0xC00000 - 1 && off >= 0xc00000 {
            if p == s_end {
                return Err("oodle: far offsets too short".into());
            }
            off += (src[p] as u32) << 22;
            p += 1;
        }
        if off as i64 > offset {
            return Err("oodle: far offset beyond output".into());
        }
        out.push(off);
    }
    Ok((out, p - s))
}

fn process_lz_runs(mode: usize, src: &[u8], s_end: usize, dst: &mut [u8], mut d: usize, mut dst_size: usize, offset: usize, lz: &mut LzTable) -> R<()> {
    if mode == 0 {
        return Err("oodle: mode 0 not supported".into());
    }
    let mut saved_dist: i64 = -8;
    let mut src_cur = usize::MAX;
    for iteration in 0..2 {
        let cur = dst_size.min(0x10000);
        let (cmd, off32) = if iteration == 0 {
            ((lz.cmd.0, lz.cmd.0 + lz.cmd2_offsets), std::mem::take(&mut lz.off32_1))
        } else {
            ((lz.cmd.0 + lz.cmd2_offsets, lz.cmd.0 + lz.cmd2_offsets_end), std::mem::take(&mut lz.off32_2))
        };
        let start_off = if offset == 0 && iteration == 0 { 8 } else { 0 };
        src_cur = mermaid_mode1(dst, d, cur, src, s_end, lz, cmd, &off32, &mut saved_dist, start_off)?;
        d += cur;
        dst_size -= cur;
        if dst_size == 0 {
            break;
        }
    }
    if src_cur != s_end {
        return Err("oodle: lz runs did not consume input".into());
    }
    Ok(())
}

/// 8-byte block copy with load-then-store semantics (`*(ulong*)dst = *(ulong*)src`).
#[inline]
fn copy64(dst: &mut [u8], to: usize, from_buf: Option<&[u8]>, from: i64) -> R<()> {
    let mut tmp = [0u8; 8];
    if from < 0 {
        return Err("oodle: match before start of output".into());
    }
    {
        let source: &[u8] = from_buf.unwrap_or(&*dst);
        for (k, t) in tmp.iter_mut().enumerate() {
            *t = source.get(from as usize + k).copied().unwrap_or(0);
        }
    }
    dst.get_mut(to..to + 8).ok_or("oodle: write past output")?.copy_from_slice(&tmp);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn mermaid_mode1(dst: &mut [u8], d_start: usize, size: usize, src: &[u8], s_end: usize, lz: &mut LzTable, cmd: (usize, usize), off32: &[u32], saved_dist: &mut i64, start_off: usize) -> R<usize> {
    let d_end = (d_start + size) as i64;
    let d_begin = d_start as i64;
    let mut d = (d_start + start_off) as i64;
    let mut recent = *saved_dist;
    let mut ls = lz.length;
    let mut lit = lz.lit;
    let lit_end = lz.lit_end;
    let mut o16 = lz.off16_pos;
    let mut o32 = 0usize;

    let read_len = |ls: &mut usize, add: i64| -> R<i64> {
        if s_end - *ls == 0 {
            return Err("oodle: length stream empty".into());
        }
        let mut len = src[*ls] as i64;
        if len > 251 {
            if s_end - *ls < 3 {
                return Err("oodle: length stream too short".into());
            }
            len += u16::from_le_bytes([src[*ls + 1], src[*ls + 2]]) as i64 * 4;
            *ls += 2;
        }
        *ls += 1;
        Ok(len + add)
    };

    for c in cmd.0..cmd.1 {
        let flag = src[c] as u32;
        if flag >= 24 {
            let new_dist = lz.off16.get(o16).copied().unwrap_or(0) as i64;
            // Bit 7 clear: take a new distance from the off16 stream; set: reuse the recent one.
            let take_new = (flag >> 7) == 0;
            let lit_len = (flag & 7) as i64;
            copy64(dst, d as usize, Some(src), lit as i64)?;
            d += lit_len;
            lit += lit_len as usize;
            if take_new {
                recent = -new_dist;
                o16 += 1;
            }
            let m = d + recent;
            copy64(dst, d as usize, None, m)?;
            copy64(dst, d as usize + 8, None, m + 8)?;
            d += ((flag >> 3) & 0xF) as i64;
        } else if flag > 2 {
            let len = flag as i64 + 5;
            let off = *off32.get(o32).ok_or("oodle: off32 stream exhausted")? as i64;
            o32 += 1;
            let m = d_begin - off;
            recent = m - d;
            if d_end - d < len {
                return Err("oodle: match past block end".into());
            }
            for k in 0..4 {
                copy64(dst, (d + 8 * k) as usize, None, m + 8 * k)?;
            }
            d += len;
        } else if flag == 0 {
            let mut len = read_len(&mut ls, 64)?;
            if d_end - d < len || (lit_end as i64 - lit as i64) < len {
                return Err("oodle: literal run too long".into());
            }
            loop {
                copy64(dst, d as usize, Some(src), lit as i64)?;
                copy64(dst, d as usize + 8, Some(src), lit as i64 + 8)?;
                d += 16;
                lit += 16;
                len -= 16;
                if len <= 0 {
                    break;
                }
            }
            d += len;
            lit = (lit as i64 + len) as usize;
        } else {
            let (add, from16) = if flag == 1 { (91, true) } else { (29, false) };
            let mut len = read_len(&mut ls, add)?;
            let mut m = if from16 {
                let off = *lz.off16.get(o16).ok_or("oodle: off16 stream exhausted")? as i64;
                o16 += 1;
                d - off
            } else {
                let off = *off32.get(o32).ok_or("oodle: off32 stream exhausted")? as i64;
                o32 += 1;
                d_begin - off
            };
            recent = m - d;
            loop {
                copy64(dst, d as usize, None, m)?;
                copy64(dst, d as usize + 8, None, m + 8)?;
                d += 16;
                m += 16;
                len -= 16;
                if len <= 0 {
                    break;
                }
            }
            d += len;
        }
    }

    let rest = d_end - d;
    if rest < 0 || lit + rest as usize > src.len() {
        return Err("oodle: trailing literals past end".into());
    }
    let (d, rest) = (d as usize, rest as usize);
    dst[d..d + rest].copy_from_slice(&src[lit..lit + rest]);
    lit += rest;

    *saved_dist = recent;
    lz.length = ls;
    lz.off16_pos = o16;
    lz.lit = lit;
    Ok(ls)
}
