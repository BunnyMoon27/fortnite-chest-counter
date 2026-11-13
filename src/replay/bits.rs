//! Bit-level reader for Unreal network data (port of Unreal.Core BitReader / NetBitReader).

use super::names::UNREAL_NAMES;
use super::versions::*;

/// Indexes for the temporary end markers (FBitArchiveEndIndex).
pub const END_CONTENT_BLOCK: usize = 1;
pub const END_FIELD_HEADER: usize = 2;

#[derive(Clone, Default)]
pub struct BitReader {
    buf: Vec<u8>,
    pub pos: usize,
    pub last_bit: usize,
    pub error: bool,
    temp_last_bit: [usize; 4],
    pub engine_version: u32,
}

impl BitReader {
    pub fn new(buf: Vec<u8>, bits: usize, engine_version: u32) -> Self {
        BitReader { buf, pos: 0, last_bit: bits, error: false, temp_last_bit: [0; 4], engine_version }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.last_bit
    }

    pub fn can_read(&self, n: usize) -> bool {
        self.pos + n <= self.last_bit
    }

    pub fn bits_left(&self) -> usize {
        self.last_bit.saturating_sub(self.pos)
    }

    #[inline]
    fn bit_at(&self, p: usize) -> bool {
        self.buf.get(p >> 3).is_some_and(|b| b & (1 << (p & 7)) != 0)
    }

    pub fn read_bit(&mut self) -> bool {
        if self.pos >= self.last_bit || self.error {
            self.error = true;
            return false;
        }
        let b = self.bit_at(self.pos);
        self.pos += 1;
        b
    }

    pub fn read_bits_u64(&mut self, n: usize) -> u64 {
        if !self.can_read(n) {
            self.error = true;
            return 0;
        }
        let mut v = 0u64;
        for i in 0..n {
            if self.bit_at(self.pos) {
                v |= 1 << i;
            }
            self.pos += 1;
        }
        v
    }

    /// Reads `n` bits into a new byte buffer (LSB first, like UE's SerializeBits).
    pub fn read_bits(&mut self, n: usize) -> Vec<u8> {
        if !self.can_read(n) {
            self.error = true;
            return Vec::new();
        }
        let mut out = vec![0u8; n.div_ceil(8)];
        if self.pos & 7 == 0 {
            let start = self.pos >> 3;
            let full = n / 8;
            out[..full].copy_from_slice(&self.buf[start..start + full]);
            self.pos += full * 8;
            for i in 0..n % 8 {
                if self.bit_at(self.pos) {
                    out[full] |= 1 << i;
                }
                self.pos += 1;
            }
            return out;
        }
        for i in 0..n {
            if self.bit_at(self.pos) {
                out[i >> 3] |= 1 << (i & 7);
            }
            self.pos += 1;
        }
        out
    }

    pub fn read_byte(&mut self) -> u8 {
        // UE BitReader.ReadByte does not bounds-check; keep error semantics on overflow.
        if !self.can_read(8) {
            self.pos += 8;
            self.error = true;
            return 0;
        }
        self.read_bits_u64(8) as u8
    }

    pub fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        if !self.can_read(n * 8) {
            self.error = true;
            return Vec::new();
        }
        self.read_bits(n * 8)
    }

    fn read_le<const N: usize>(&mut self) -> Option<[u8; N]> {
        if !self.can_read(N * 8) {
            self.error = true;
            return None;
        }
        let v = self.read_bits(N * 8);
        v.try_into().ok()
    }

    pub fn read_u16(&mut self) -> u16 {
        self.read_le::<2>().map(u16::from_le_bytes).unwrap_or(0)
    }
    pub fn read_i16(&mut self) -> i16 {
        self.read_le::<2>().map(i16::from_le_bytes).unwrap_or(0)
    }
    pub fn read_u32(&mut self) -> u32 {
        self.read_le::<4>().map(u32::from_le_bytes).unwrap_or(0)
    }
    pub fn read_i32(&mut self) -> i32 {
        self.read_le::<4>().map(i32::from_le_bytes).unwrap_or(0)
    }
    pub fn read_u64(&mut self) -> u64 {
        self.read_le::<8>().map(u64::from_le_bytes).unwrap_or(0)
    }
    pub fn read_f32(&mut self) -> f32 {
        self.read_le::<4>().map(f32::from_le_bytes).unwrap_or(0.0)
    }
    pub fn read_f64(&mut self) -> f64 {
        self.read_le::<8>().map(f64::from_le_bytes).unwrap_or(0.0)
    }

    /// FArchive::SerializeIntPacked: 7 bits per byte, low bit = "more follows".
    pub fn read_int_packed(&mut self) -> u32 {
        let mut value: u32 = 0;
        let mut shift = 0u32;
        for _ in 0..5 {
            if !self.can_read(8) {
                self.error = true;
                break;
            }
            let b = self.read_bits_u64(8) as u32;
            value |= (b >> 1).wrapping_shl(shift);
            shift += 7;
            if b & 1 == 0 {
                break;
            }
        }
        value
    }

    /// FBitReader::SerializeInt: value strictly below `max`.
    pub fn read_serialized_int(&mut self, max: u32) -> u32 {
        let mut value: u32 = 0;
        let mut mask: u32 = 1;
        while (value as u64 + mask as u64) < max as u64 {
            if self.read_bit() {
                value |= mask;
            }
            mask = mask.wrapping_mul(2);
            if mask == 0 {
                break;
            }
        }
        value
    }

    pub fn read_fstring(&mut self) -> String {
        let len = self.read_i32();
        if len == 0 {
            return String::new();
        }
        if len < 0 {
            let Some(n) = (len as i64).checked_mul(-2).and_then(|n| usize::try_from(n).ok()) else {
                self.error = true;
                return String::new();
            };
            let mut bytes = self.read_bytes(n);
            if bytes.len() >= 2 && bytes[bytes.len() - 1] == 0 && bytes[bytes.len() - 2] == 0 {
                bytes.truncate(bytes.len() - 2);
            }
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units).trim_matches(' ').to_string()
        } else {
            let mut bytes = self.read_bytes(len as usize);
            if bytes.last() == Some(&0) {
                bytes.pop();
            }
            String::from_utf8_lossy(&bytes).trim_matches(' ').to_string()
        }
    }

    pub fn read_fname(&mut self) -> String {
        if self.read_bit() {
            let idx = if self.engine_version < ENGINE_CHANNEL_NAMES { self.read_u32() } else { self.read_int_packed() };
            return hardcoded_name(idx);
        }
        let s = self.read_fstring();
        let _number = self.read_i32();
        s
    }

    pub fn skip_bits(&mut self, n: usize) {
        if self.pos + n > self.buf.len() * 8 {
            self.error = true;
            return;
        }
        self.pos += n;
    }

    /// Appends bytes after the current buffer (partial bunches are byte aligned).
    pub fn append(&mut self, data: &[u8], bits: usize) {
        self.last_bit += bits;
        self.buf.extend_from_slice(data);
    }

    /// ChestCounter patch kept: always remember the previous end, even on overflow.
    pub fn set_temp_end(&mut self, size: usize, index: usize) {
        self.temp_last_bit[index] = self.last_bit;
        let end = self.pos + size;
        if end > self.last_bit {
            self.error = true;
            return;
        }
        self.last_bit = end;
    }

    pub fn restore_temp_end(&mut self, index: usize) {
        self.pos = self.last_bit;
        self.last_bit = self.temp_last_bit[index];
        self.error = false;
    }

    /// Copies the next `bits` bits into a fresh reader (the `_cmdReader.FillBitsFrom` pattern).
    /// Like the C#, an overflow leaves this reader untouched and returns an empty reader.
    pub fn sub_reader(&mut self, bits: usize) -> BitReader {
        if bits == 0 || !self.can_read(bits) {
            let mut r = BitReader::new(Vec::new(), 0, self.engine_version);
            r.error = self.error;
            return r;
        }
        let data = self.read_bits(bits);
        let mut r = BitReader::new(data, bits, self.engine_version);
        r.error = self.error;
        r
    }

    // --- property serializers (NetBitReader) ---

    /// FUniqueNetIdRepl::NetSerialize.
    pub fn read_net_id(&mut self) -> String {
        const TYPE_HASH_OTHER: u8 = 31;
        let flags = self.read_byte();
        let encoded = flags & 1 != 0;
        if encoded && flags & 2 != 0 {
            return String::new();
        }
        let type_hash = (flags & 0xF8) >> 3;
        if type_hash == 0 {
            return "NULL".into();
        }
        let mut valid = true;
        if type_hash == TYPE_HASH_OTHER && self.read_fstring() == "None" {
            valid = false;
        }
        if !valid {
            return String::new();
        }
        if encoded {
            let size = self.read_byte() as usize;
            hex_upper(&self.read_bytes(size))
        } else {
            self.read_fstring()
        }
    }
}

pub fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Name for a hardcoded FName index; unknown indexes print as their number (like C# enum ToString).
pub fn hardcoded_name(idx: u32) -> String {
    // Later entries win for duplicate values, as in the C# cache.
    UNREAL_NAMES.iter().rev().find(|(v, _)| *v == idx).map(|(_, n)| n.to_string()).unwrap_or_else(|| idx.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_packed_and_serialized_int() {
        // 300 = 0b1_0010_1100 -> bytes: ((300 & 0x7F) << 1) | 1 = 0x59, (300 >> 7) << 1 = 0x04
        let mut r = BitReader::new(vec![0x59, 0x04], 16, 30);
        assert_eq!(r.read_int_packed(), 300);
        let mut r = BitReader::new(vec![0b101], 8, 30);
        assert_eq!(r.read_serialized_int(8), 5);
        assert_eq!(r.pos, 3);
    }

    #[test]
    fn unaligned_reads() {
        let mut r = BitReader::new(vec![0xFF, 0x01, 0x00], 24, 30);
        assert!(r.read_bit());
        assert_eq!(r.read_byte(), 0xFF);
        assert_eq!(r.pos, 9);
        let mut r = BitReader::new(vec![0b1010_1010, 0b0000_0001], 16, 30);
        r.pos = 1;
        assert_eq!(r.read_bits(8), vec![0b1101_0101]);
    }
}
