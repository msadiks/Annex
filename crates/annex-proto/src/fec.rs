//! Reed-Solomon forward error correction per frame (spec section 8.5).
//!
//! A frame is split into `k` equally sized data shards; `m` parity shards are
//! added. Any `k` of the `k + m` shards rebuild the frame. Encoders and decoders
//! keep their working memory between frames, so after warm-up the hot path does
//! not allocate.

use reed_solomon_simd::{ReedSolomonDecoder, ReedSolomonEncoder};

/// Keyframes always get at least this much parity.
pub const MIN_IDR_FEC_PERCENT: u8 = 20;
/// Upper bound for adaptive FEC.
pub const MAX_FEC_PERCENT: u8 = 50;

/// Number of parity shards for `k` data shards: `ceil(k * percent / 100)`,
/// with the keyframe minimum applied and clamped to what the codec supports.
pub fn parity_count(k: usize, fec_percent: u8, idr: bool) -> usize {
    let mut pct = fec_percent.min(MAX_FEC_PERCENT) as usize;
    if idr {
        pct = pct.max(MIN_IDR_FEC_PERCENT as usize);
    }
    if pct == 0 || k == 0 {
        return 0;
    }
    let mut m = (k * pct).div_ceil(100);
    while m > 0 && !ReedSolomonEncoder::supports(k, m) {
        m -= 1;
    }
    m
}

#[derive(Debug, thiserror::Error)]
pub enum FecError {
    #[error("reed-solomon: {0}")]
    Rs(#[from] reed_solomon_simd::Error),
    #[error("not enough shards to recover the frame")]
    NotEnoughShards,
}

/// Reusable parity generator.
#[derive(Default)]
pub struct FecEncoder {
    inner: Option<ReedSolomonEncoder>,
}

impl FecEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Computes `m` parity shards over `k` data shards stored back to back in
    /// `data` (`k * shard_size` bytes). Parity is written back to back into
    /// `parity` (`m * shard_size` bytes).
    pub fn encode(
        &mut self,
        data: &[u8],
        k: usize,
        m: usize,
        shard_size: usize,
        parity: &mut [u8],
    ) -> Result<(), FecError> {
        debug_assert_eq!(data.len(), k * shard_size);
        debug_assert_eq!(parity.len(), m * shard_size);
        if m == 0 {
            return Ok(());
        }
        let enc = match &mut self.inner {
            Some(e) => {
                e.reset(k, m, shard_size)?;
                e
            }
            None => self.inner.insert(ReedSolomonEncoder::new(k, m, shard_size)?),
        };
        for shard in data.chunks_exact(shard_size) {
            enc.add_original_shard(shard)?;
        }
        let result = enc.encode()?;
        for (i, rec) in result.recovery_iter().enumerate() {
            parity[i * shard_size..(i + 1) * shard_size].copy_from_slice(rec);
        }
        Ok(())
    }
}

/// Reusable frame recovery.
#[derive(Default)]
pub struct FecDecoder {
    inner: Option<ReedSolomonDecoder>,
}

impl FecDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores missing data shards in place.
    ///
    /// `data` holds `k` shard slots back to back, `have_data[i]` says whether
    /// slot `i` was received. `parity`/`have_parity` likewise for `m` parity
    /// shards. On success every data slot is filled.
    #[allow(clippy::too_many_arguments)]
    pub fn recover(
        &mut self,
        data: &mut [u8],
        have_data: &[bool],
        parity: &[u8],
        have_parity: &[bool],
        k: usize,
        m: usize,
        shard_size: usize,
    ) -> Result<(), FecError> {
        let present = have_data.iter().filter(|b| **b).count();
        if present == k {
            return Ok(());
        }
        let parity_present = have_parity.iter().filter(|b| **b).count();
        if present + parity_present < k || m == 0 {
            return Err(FecError::NotEnoughShards);
        }
        let dec = match &mut self.inner {
            Some(d) => {
                d.reset(k, m, shard_size)?;
                d
            }
            None => self.inner.insert(ReedSolomonDecoder::new(k, m, shard_size)?),
        };
        for i in 0..k {
            if have_data[i] {
                dec.add_original_shard(i, &data[i * shard_size..(i + 1) * shard_size])?;
            }
        }
        let mut added = present;
        for j in 0..m {
            if added >= k {
                break;
            }
            if have_parity[j] {
                dec.add_recovery_shard(j, &parity[j * shard_size..(j + 1) * shard_size])?;
                added += 1;
            }
        }
        let result = dec.decode()?;
        for (idx, shard) in result.restored_original_iter() {
            data[idx * shard_size..(idx + 1) * shard_size].copy_from_slice(shard);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_counts() {
        assert_eq!(parity_count(10, 10, false), 1);
        assert_eq!(parity_count(11, 10, false), 2);
        assert_eq!(parity_count(1, 10, false), 1);
        assert_eq!(parity_count(10, 10, true), 2);
        assert_eq!(parity_count(10, 30, true), 3);
        assert_eq!(parity_count(10, 0, false), 0);
        assert_eq!(parity_count(10, 0, true), 2);
        assert_eq!(parity_count(100, 255, false), 50);
    }

    #[test]
    fn recovers_any_k_of_k_plus_m() {
        let k = 12;
        let m = 4;
        let ss = 64;
        let data: Vec<u8> = (0..k * ss).map(|i| (i * 7 + 3) as u8).collect();
        let mut parity = vec![0u8; m * ss];
        FecEncoder::new().encode(&data, k, m, ss, &mut parity).unwrap();

        let mut dec = FecDecoder::new();
        // Drop up to m data shards in a few patterns.
        for lost in [vec![0], vec![3, 4], vec![0, 5, 11], vec![1, 2, 3, 4]] {
            let mut d = data.clone();
            let mut have = vec![true; k];
            for &l in &lost {
                have[l] = false;
                d[l * ss..(l + 1) * ss].fill(0xEE);
            }
            dec.recover(&mut d, &have, &parity, &[true; 4], k, m, ss).unwrap();
            assert_eq!(d, data, "lost {lost:?}");
        }
    }

    #[test]
    fn recovers_with_partial_parity() {
        let k = 5;
        let m = 3;
        let ss = 32;
        let data: Vec<u8> = (0..k * ss).map(|i| i as u8).collect();
        let mut parity = vec![0u8; m * ss];
        FecEncoder::new().encode(&data, k, m, ss, &mut parity).unwrap();
        let mut d = data.clone();
        d[0..ss].fill(0);
        d[2 * ss..3 * ss].fill(0);
        let have = [false, true, false, true, true];
        FecDecoder::new()
            .recover(&mut d, &have, &parity, &[false, true, true], k, m, ss)
            .unwrap();
        assert_eq!(d, data);
    }

    #[test]
    fn too_few_shards_fails() {
        let k = 4;
        let m = 1;
        let ss = 16;
        let data = vec![9u8; k * ss];
        let mut parity = vec![0u8; m * ss];
        FecEncoder::new().encode(&data, k, m, ss, &mut parity).unwrap();
        let mut d = data.clone();
        let have = [false, false, true, true];
        assert!(matches!(
            FecDecoder::new().recover(&mut d, &have, &parity, &[true], k, m, ss),
            Err(FecError::NotEnoughShards)
        ));
    }

    #[test]
    fn encoder_reuse_across_sizes() {
        let mut enc = FecEncoder::new();
        let mut dec = FecDecoder::new();
        for (k, m, ss) in [(3, 1, 8), (40, 8, 1184), (1, 1, 2), (7, 2, 100)] {
            let data: Vec<u8> = (0..k * ss).map(|i| (i % 251) as u8).collect();
            let mut parity = vec![0u8; m * ss];
            enc.encode(&data, k, m, ss, &mut parity).unwrap();
            let mut d = data.clone();
            d[0..ss].fill(0);
            let mut have = vec![true; k];
            have[0] = false;
            dec.recover(&mut d, &have, &parity, &vec![true; m], k, m, ss).unwrap();
            assert_eq!(d, data);
        }
    }
}
