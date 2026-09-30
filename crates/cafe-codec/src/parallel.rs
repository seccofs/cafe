//! Internal helper for fanning independent per-tile work out across
//! threads (spec section 4.4: "Each `IDAT` is independent ... decoded as
//! soon as it arrives" — the same independence property already exploited
//! by [`crate::tile`]'s per-tile predictor reset applies equally well to
//! parallel execution, not just streaming).
//!
//! This module is private (`mod parallel`, not `pub mod`) — it has no
//! public API of its own, only the thread-fan-out/result-collection
//! boilerplate [`decoder`](crate::decoder)/[`encoder`](crate::encoder)
//! would otherwise have to duplicate. Contains no `unsafe` code: every
//! worker thread computes its assigned tiles into freshly-allocated,
//! per-tile buffers of its own ([`map_parallel`]'s `T` return value) —
//! nothing here shares mutable access to one buffer across threads. An
//! earlier version of this module also exposed a `for_each_parallel`
//! entry point plus a `RawSliceMut` raw-pointer primitive so
//! [`crate::decoder::decode_bytes_parallel`] could write each tile
//! directly into a shared pixel buffer from multiple threads; both were
//! removed once [`crate::decoder::decode_bytes_parallel`] was rewritten
//! to collect tiles via [`map_parallel`] and copy them into place
//! single-threaded afterward instead — a fully safe equivalent, since
//! that copy is cheap relative to each tile's own ZSTD
//! decompression/predictor-reversal work (the actual bottleneck, per
//! `AGENTS.md`'s parallel-tiling benchmark).

use crate::error::Result;

/// Number of worker threads to use for a parallel tile pass. Wraps
/// [`std::thread::available_parallelism`], falling back to `1` (meaning
/// "don't bother parallelizing") if the platform can't report a count.
pub(crate) fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Below this many tiles, thread spawn/join overhead isn't worth it even
/// on a many-core machine — confirmed by this project's own PoC benchmark
/// (`AGENTS.md`'s parallel-tiling investigation): per-tile work is cheap
/// enough that a handful of tiles finishes before threads would even be
/// done spawning. Chosen conservatively; tuning this further would need
/// its own benchmark, per `AGENTS.md`'s "every feature proves itself with
/// a benchmark first" principle.
pub(crate) const MIN_TILES_FOR_PARALLEL: usize = 4;

/// Splits `0..item_count` into `n_workers` contiguous, roughly-equal
/// ranges (the last one absorbing any remainder), skipping empty ranges.
/// Shared range-splitting logic for both the decoder's and encoder's
/// parallel tile passes, so the two can't drift on how work is divided.
pub(crate) fn split_ranges(item_count: usize, n_workers: usize) -> Vec<std::ops::Range<usize>> {
    if item_count == 0 || n_workers == 0 {
        return Vec::new();
    }
    let chunk_size = item_count.div_ceil(n_workers).max(1);
    (0..item_count)
        .step_by(chunk_size)
        .map(|start| start..(start + chunk_size).min(item_count))
        .collect()
}

/// Runs `per_item` for every index in `0..item_count`, fanned out across
/// up to [`worker_count`] threads (or run directly on the calling thread
/// if `item_count < MIN_TILES_FOR_PARALLEL` or only one worker is
/// available) via [`std::thread::scope`], collecting every value into a
/// `Vec` in index order (`result[i]` is always `per_item(i)`'s output,
/// regardless of which thread computed it or when it finished).
/// Whichever error is encountered first *in index order* is returned
/// (ties among threads are broken by index, not completion order, so
/// error reporting is deterministic regardless of scheduling).
///
/// `per_item` must be `Sync` (called concurrently from multiple threads);
/// since it only ever returns an owned `T` rather than writing through a
/// shared reference, there is no aliasing hazard for this function to
/// guard against — every worker thread's output lives in that thread's
/// own return value until this function joins them back together.
/// Used by both [`crate::decoder::decode_bytes_parallel`] (per-tile
/// decompression + predictor reversal, collected into a `Vec<DecodedTile>`
/// then copied into the final pixel buffer single-threaded) and
/// [`crate::encoder::encode_bytes_parallel`] (per-tile predictor
/// selection + ZSTD race + chunk framing, producing a variable-length
/// `Vec<u8>` per tile that must still be written to the output in scan
/// order).
pub(crate) fn map_parallel<T, F>(item_count: usize, per_item: F) -> Result<Vec<T>>
where
    T: Send,
    F: Fn(usize) -> Result<T> + Sync,
{
    if item_count == 0 {
        return Ok(Vec::new());
    }
    let n_workers = worker_count();
    if item_count < MIN_TILES_FOR_PARALLEL || n_workers <= 1 {
        return (0..item_count).map(&per_item).collect();
    }

    let ranges = split_ranges(item_count, n_workers);
    let mut results: Vec<(usize, Result<T>)> = std::thread::scope(|s| {
        let handles: Vec<_> = ranges
            .into_iter()
            .map(|range| {
                let per_item = &per_item;
                s.spawn(move || range.clone().map(|i| (i, per_item(i))).collect::<Vec<_>>())
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // Deterministic ordering: index order, not completion order.
    results.sort_by_key(|(i, _)| *i);
    let mut out = Vec::with_capacity(results.len());
    for (_, r) in results {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CodecError;

    #[test]
    fn test_split_ranges_covers_every_index_exactly_once() {
        for item_count in [0usize, 1, 3, 4, 7, 100, 4096] {
            for n_workers in [1usize, 2, 3, 8, 24] {
                let ranges = split_ranges(item_count, n_workers);
                let mut seen = vec![false; item_count];
                for r in &ranges {
                    for i in r.clone() {
                        assert!(!seen[i], "index {i} covered twice");
                        seen[i] = true;
                    }
                }
                assert!(seen.iter().all(|&s| s), "not every index covered");
            }
        }
    }

    #[test]
    fn test_split_ranges_empty_input() {
        assert_eq!(split_ranges(0, 4), Vec::<std::ops::Range<usize>>::new());
    }

    #[test]
    fn test_map_parallel_preserves_index_order() {
        let out = map_parallel(100, |i| Ok(i * 2)).unwrap();
        assert_eq!(out, (0..100).map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn test_map_parallel_below_threshold() {
        let out = map_parallel(2, |i| Ok(i + 1)).unwrap();
        assert_eq!(out, vec![1, 2]);
    }

    #[test]
    fn test_map_parallel_empty_is_ok() {
        let out: Vec<usize> = map_parallel(0, |_| -> Result<usize> {
            panic!("should never be called");
        })
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn test_map_parallel_propagates_first_error_by_index() {
        let result: Result<Vec<usize>> = map_parallel(20, |i| {
            if i == 15 || i == 3 {
                Err(CodecError::InvalidPredictorCode(i as u8))
            } else {
                Ok(i)
            }
        });
        assert!(matches!(result, Err(CodecError::InvalidPredictorCode(3))));
    }

    #[test]
    fn test_map_parallel_runs_every_index_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let seen: Vec<AtomicUsize> = (0..50).map(|_| AtomicUsize::new(0)).collect();
        map_parallel(50, |i| {
            seen[i].fetch_add(1, Ordering::SeqCst);
            Ok::<_, CodecError>(())
        })
        .unwrap();
        assert!(seen.iter().all(|c| c.load(Ordering::SeqCst) == 1));
    }
}
