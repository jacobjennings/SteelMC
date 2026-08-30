//! Per-chunk visitation mask used by portable carvers.

/// A `16 × height × 16` bitset of local block positions in a chunk.
#[derive(Debug, Clone)]
pub struct CarvingMask {
    min_y: i32,
    bits: Vec<u64>,
}

impl CarvingMask {
    /// Creates an empty mask covering `[min_y, min_y + height)`.
    #[must_use]
    pub fn new(height: i32, min_y: i32) -> Self {
        let bits = (256_i32 * height).unsigned_abs().div_ceil(64) as usize;
        Self {
            min_y,
            bits: vec![0; bits],
        }
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        let local_x = (x & 15) as u32;
        let local_z = ((z & 15) as u32) << 4;
        let relative_y = ((y - self.min_y) as u32) << 8;
        (local_x | local_z | relative_y) as usize
    }

    /// Marks the local position when it has not already been visited.
    #[inline]
    pub fn set_if_unset(&mut self, x: i32, y: i32, z: i32) -> bool {
        let index = self.index(x, y, z);
        let lane = index / 64;
        let bit = 1_u64 << (index % 64);
        if self.bits[lane] & bit != 0 {
            return false;
        }
        self.bits[lane] |= bit;
        true
    }
}
