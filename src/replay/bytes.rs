//! Byte-aligned reader for the replay file and its chunks (port of Unreal.Core BinaryReader).

use super::bits::hardcoded_name;
use super::versions::ENGINE_CHANNEL_NAMES;

pub type R<T> = Result<T, String>;

pub struct Bytes {
    pub data: Vec<u8>,
    pub pos: usize,
}

impl Bytes {
    pub fn new(data: Vec<u8>) -> Self {
        Bytes { data, pos: 0 }
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }
    pub fn take(&mut self, n: usize) -> R<&[u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(|| format!("read of {n} bytes past end at {}", self.pos))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub fn take_vec(&mut self, n: usize) -> R<Vec<u8>> {
        self.take(n).map(<[u8]>::to_vec)
    }
    pub fn skip(&mut self, n: usize) -> R<()> {
        self.take(n).map(|_| ())
    }
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }
    pub fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> R<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> R<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> R<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> R<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u32_bool(&mut self) -> R<bool> {
        Ok(self.u32()? >= 1)
    }
    pub fn int_packed(&mut self) -> R<u32> {
        let mut value: u32 = 0;
        let mut count = 0u32;
        loop {
            let b = self.u8()?;
            value = value.wrapping_add(((b >> 1) as u32).wrapping_shl(7 * count));
            count += 1;
            if b & 1 == 0 {
                return Ok(value);
            }
        }
    }
    pub fn fstring(&mut self) -> R<String> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len < 0 {
            let n = usize::try_from(-(len as i64) * 2).map_err(|e| e.to_string())?;
            let mut b = self.take_vec(n)?;
            if b.len() >= 2 && b[b.len() - 1] == 0 && b[b.len() - 2] == 0 {
                b.truncate(b.len() - 2);
            }
            let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(String::from_utf16_lossy(&units).trim_matches(' ').to_string())
        } else {
            let mut b = self.take_vec(len as usize)?;
            if b.last() == Some(&0) {
                b.pop();
            }
            Ok(String::from_utf8_lossy(&b).trim_matches(' ').to_string())
        }
    }
    pub fn fname(&mut self, engine_version: u32) -> R<String> {
        if self.bool()? {
            let idx = if engine_version < ENGINE_CHANNEL_NAMES { self.u32()? } else { self.int_packed()? };
            return Ok(hardcoded_name(idx));
        }
        let s = self.fstring()?;
        self.i32()?;
        Ok(s)
    }
}
