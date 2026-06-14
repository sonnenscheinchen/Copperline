// SPDX-License-Identifier: GPL-3.0-or-later

//! 68020/68030 on-chip cache model (functional + bus-traffic accurate).
//!
//! Both caches are modeled as 64 direct-mapped longword entries (256
//! bytes), which is the exact 68020 instruction cache geometry; the
//! 68030's 16x4-longword line organisation collapses to the same thing
//! once burst fills are ignored (we do not model burst timing; IBE/DBE
//! are stored but inert).
//!
//! A hit serves the access with no bus cycle, which is the real effect
//! that matters on an Amiga: cached fetches stop competing with DMA for
//! the chip bus. Like the real silicon, the instruction cache does NOT
//! snoop writes - self-modifying code must clear it through CACR, and
//! DMA (blitter/copper/disk) never invalidates it. The data cache is
//! write-through; a write that hits invalidates the entry (the next
//! read refills it), which is always coherent and only marginally
//! pessimistic versus update-on-hit.
//!
//! Cache emulation is opt-in (`[cpu] icache` / `dcache`): the default
//! 68020 timing calibration (vendor scale_cycles_for_cpu_type plus the
//! AGA fetch latch) was made without it, so enabling the caches makes
//! chip-RAM code faster than that baseline.

const LINES: usize = 64;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LongwordCache {
    #[serde(with = "serde_big_array::BigArray")]
    tags: [u32; LINES],
    #[serde(with = "serde_big_array::BigArray")]
    valid: [bool; LINES],
    #[serde(with = "serde_big_array::BigArray")]
    data: [u32; LINES],
}

impl Default for LongwordCache {
    fn default() -> Self {
        Self {
            tags: [0; LINES],
            valid: [false; LINES],
            data: [0; LINES],
        }
    }
}

impl LongwordCache {
    #[inline]
    fn index(addr: u32) -> usize {
        ((addr >> 2) as usize) & (LINES - 1)
    }

    #[inline]
    fn tag(addr: u32) -> u32 {
        addr >> 8
    }

    /// Look up the aligned longword containing `addr`.
    #[inline]
    pub fn lookup(&self, addr: u32) -> Option<u32> {
        let idx = Self::index(addr);
        if self.valid[idx] && self.tags[idx] == Self::tag(addr) {
            Some(self.data[idx])
        } else {
            None
        }
    }

    /// Fill the entry for the aligned longword containing `addr`.
    #[inline]
    pub fn fill(&mut self, addr: u32, longword: u32) {
        let idx = Self::index(addr);
        self.tags[idx] = Self::tag(addr);
        self.valid[idx] = true;
        self.data[idx] = longword;
    }

    /// Invalidate the entry holding `addr`, if any (CACR clear-entry, and
    /// data-cache write-through hits).
    #[inline]
    pub fn invalidate_entry(&mut self, addr: u32) {
        let idx = Self::index(addr);
        if self.tags[idx] == Self::tag(addr) {
            self.valid[idx] = false;
        }
    }

    /// Invalidate everything (CACR clear-cache, reset).
    pub fn invalidate_all(&mut self) {
        self.valid = [false; LINES];
    }
}

/// One cache (instruction or data) plus its CACR-controlled state.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CpuCache {
    cache: LongwordCache,
    /// CACR enable bit (EI/ED).
    pub enabled: bool,
    /// CACR freeze bit (FI/FD): hits are served, misses do not allocate.
    pub frozen: bool,
}

impl CpuCache {
    /// Serve a `size`-byte read at `addr` from the cache, if every
    /// longword it touches is resident. `size` must be 1, 2, or 4.
    pub fn read(&self, addr: u32, size: usize) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        let first = self.cache.lookup(addr)?;
        let off = (addr & 3) as usize;
        if off + size <= 4 {
            let shift = (4 - off - size) * 8;
            let mask = if size == 4 {
                0xFFFF_FFFF
            } else {
                (1u32 << (size * 8)) - 1
            };
            return Some((first >> shift) & mask);
        }
        // The access straddles two longwords (misaligned 68020+ access).
        let second = self.cache.lookup(addr.wrapping_add(4) & !3)?;
        let head = off + size - 4; // bytes from the second longword
        let value = ((u64::from(first) << 32) | u64::from(second)) >> ((4 - head) * 8);
        Some(
            (value as u32)
                & if size == 4 {
                    0xFFFF_FFFF
                } else {
                    (1u32 << (size * 8)) - 1
                },
        )
    }

    /// Allocate the longword(s) covering a missed `size`-byte access at
    /// `addr`, honouring the freeze bit. `fetch` peeks the aligned
    /// longword from backing memory without billing bus time (the miss
    /// itself was already billed by the normal access path).
    pub fn fill_after_miss(&mut self, addr: u32, size: usize, mut fetch: impl FnMut(u32) -> u32) {
        if !self.enabled || self.frozen {
            return;
        }
        let first = addr & !3;
        let last = addr.wrapping_add(size as u32 - 1) & !3;
        let mut long = first;
        loop {
            if self.cache.lookup(long).is_none() {
                let value = fetch(long);
                self.cache.fill(long, value);
            }
            if long == last {
                break;
            }
            long = long.wrapping_add(4);
        }
    }

    /// A write went past the cache (write-through): drop any entry it
    /// touches so later reads refill from memory.
    pub fn invalidate_write(&mut self, addr: u32, size: usize) {
        let first = addr & !3;
        let last = addr.wrapping_add(size.max(1) as u32 - 1) & !3;
        self.cache.invalidate_entry(first);
        if last != first {
            self.cache.invalidate_entry(last);
        }
    }

    /// CACR clear-entry strobe: drop the line indexed by CAAR.
    pub fn clear_entry_by_index(&mut self, caar: u32) {
        let idx = LongwordCache::index(caar);
        self.cache.valid[idx] = false;
    }

    pub fn clear_all(&mut self) {
        self.cache.invalidate_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_cache() -> CpuCache {
        CpuCache {
            enabled: true,
            ..CpuCache::default()
        }
    }

    #[test]
    fn miss_then_fill_then_hit() {
        let mut c = enabled_cache();
        assert_eq!(c.read(0x1000, 2), None);
        c.fill_after_miss(0x1000, 2, |addr| {
            assert_eq!(addr, 0x1000);
            0x1234_5678
        });
        assert_eq!(c.read(0x1000, 2), Some(0x1234));
        assert_eq!(c.read(0x1002, 2), Some(0x5678));
        assert_eq!(c.read(0x1000, 4), Some(0x1234_5678));
        assert_eq!(c.read(0x1001, 1), Some(0x34));
    }

    #[test]
    fn straddling_read_needs_both_longwords() {
        let mut c = enabled_cache();
        c.fill_after_miss(0x1000, 4, |_| 0x1122_3344);
        assert_eq!(c.read(0x1002, 4), None);
        c.fill_after_miss(0x1004, 4, |_| 0x5566_7788);
        assert_eq!(c.read(0x1002, 4), Some(0x3344_5566));
        assert_eq!(c.read(0x1003, 2), Some(0x4455));
    }

    #[test]
    fn disabled_cache_serves_nothing_and_frozen_does_not_allocate() {
        let mut c = CpuCache::default();
        c.fill_after_miss(0x1000, 4, |_| 0xAAAA_AAAA);
        c.enabled = true;
        assert_eq!(c.read(0x1000, 4), None);

        c.frozen = true;
        c.fill_after_miss(0x1000, 4, |_| 0xAAAA_AAAA);
        assert_eq!(c.read(0x1000, 4), None);

        // Unfrozen it allocates; refrozen it still serves hits.
        c.frozen = false;
        c.fill_after_miss(0x1000, 4, |_| 0xAAAA_AAAA);
        c.frozen = true;
        assert_eq!(c.read(0x1000, 4), Some(0xAAAA_AAAA));
    }

    #[test]
    fn same_index_different_tag_evicts() {
        let mut c = enabled_cache();
        c.fill_after_miss(0x1000, 4, |_| 1);
        // 0x1100 maps to the same line index (bits 7..2 equal), new tag.
        c.fill_after_miss(0x1100, 4, |_| 2);
        assert_eq!(c.read(0x1100, 4), Some(2));
        assert_eq!(c.read(0x1000, 4), None);
    }

    #[test]
    fn write_invalidates_touched_longwords() {
        let mut c = enabled_cache();
        c.fill_after_miss(0x1000, 4, |_| 1);
        c.fill_after_miss(0x1004, 4, |_| 2);
        c.invalidate_write(0x1002, 4); // straddles both
        assert_eq!(c.read(0x1000, 4), None);
        assert_eq!(c.read(0x1004, 4), None);
    }

    #[test]
    fn clear_entry_by_caar_index_drops_only_that_line() {
        let mut c = enabled_cache();
        c.fill_after_miss(0x1000, 4, |_| 1);
        c.fill_after_miss(0x1004, 4, |_| 2);
        c.clear_entry_by_index(0x1000);
        assert_eq!(c.read(0x1000, 4), None);
        assert_eq!(c.read(0x1004, 4), Some(2));
    }
}
