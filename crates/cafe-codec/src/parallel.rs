//! Internal helper for fanning independent per-tile work out across
//! threads (spec section 4.4: "Each `IDAT` is independent ... decoded as
//! soon as it arrives" — the same independence property already exploited
//! by [`crate::tile`]'s per-tile predictor reset applies equally well to
//! parallel execution, not just streaming).
//!
//! This module is private (`mod parallel`, not `pub mod`) — it has no
//! public API of its own, only the small, tightly-scoped unsafe primitive
//! [`decoder`](crate::decoder)/[`encoder`](crate::encoder) need to let
//! multiple threads write into disjoint byte ranges of one shared buffer
//! without a lock, plus the thread-fan-out/error-collection boilerplate
//! both call sites would otherwise have to duplicate.

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

/// A raw, unsynchronized view of a `&mut [u8]` that can be shared (by
/// shared reference) across multiple scoped threads, each of which is
/// trusted by its *caller* to only ever touch a byte range disjoint from
/// every other thread's range.
///
/// This is the standard "known-disjoint-writes" pattern used by, e.g.,
/// `[T]::chunks_mut`/rayon's parallel iterators internally — the safety
/// obligation is pushed onto the caller of [`RawSliceMut::as_mut_slice`],
/// not onto this type itself, since Rust's aliasing rules have no way to
/// express "these two threads never touch the same bytes" through the
/// type system alone here (the tile rectangles' non-overlap is a property
/// of [`crate::tiling::TileLayout`]'s geometry, not something encodable in
/// the slice's type).
pub(crate) struct RawSliceMut {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: `RawSliceMut` is just a pointer + length with no interior
// mutability of its own; sending it across threads is safe because it
// grants no access by itself — all actual access happens through the
// unsafe `as_mut_slice` method below, whose own safety contract is what
// prevents data races, not `Send`/`Sync`.
unsafe impl Send for RawSliceMut {}
unsafe impl Sync for RawSliceMut {}

impl RawSliceMut {
    pub(crate) fn new(slice: &mut [u8]) -> Self {
        Self {
            ptr: slice.as_mut_ptr(),
            len: slice.len(),
        }
    }

    /// Reconstructs the full `&mut [u8]` this was built from.
    ///
    /// # Safety
    ///
    /// The caller must ensure that, across all concurrent calls to this
    /// method (from any thread) on the same `RawSliceMut`, no two calls
    /// ever read or write overlapping byte ranges of the returned slice.
    /// Every call site in this crate satisfies this by construction:
    /// [`crate::tiling::TileLayout::tile_rect`] partitions the image into
    /// non-overlapping rectangles, and each worker thread only ever writes
    /// the rows belonging to the tile indices it was assigned.
    // clippy's `mut_from_ref` lint exists to catch accidental unsound
    // `&self -> &mut T` APIs, but that's exactly this type's intentional,
    // documented purpose (see the safety contract above) — every caller
    // in this crate is reviewed for disjoint access, so this is allowed
    // rather than restructured around the lint.
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn as_mut_slice(&self) -> &mut [u8] {
        std::slice::from_raw_parts_mut(self.ptr, self.len)
    }
}

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
/// available) via [`std::thread::scope`]. `per_item` receives its own
/// tile index and must return `Ok(())` or the first error it hits;
/// whichever error is encountered first *in index order* is returned
/// (ties among threads are broken by index, not completion order, so
/// error reporting is deterministic regardless of scheduling).
///
/// `per_item` must be `Sync` (called concurrently from multiple threads)
/// and touch only data reachable through indices in its own assigned
/// range — this function has no way to enforce that itself, which is why
/// it's `pub(crate)` rather than a public API: every call site in this
/// crate is reviewed for that property (see [`RawSliceMut`]'s doc comment
/// for the specific technique used to share a mutable pixel buffer this
/// way).
pub(crate) fn for_each_parallel<F>(item_count: usize, per_item: F) -> Result<()>
where
    F: Fn(usize) -> Result<()> + Sync,
{
    if item_count == 0 {
        return Ok(());
    }
    let n_workers = worker_count();
    if item_count < MIN_TILES_FOR_PARALLEL || n_workers <= 1 {
        for i in 0..item_count {
            per_item(i)?;
        }
        return Ok(());
    }

    let ranges = split_ranges(item_count, n_workers);
    let results: Vec<(usize, Result<()>)> = std::thread::scope(|s| {
        let handles: Vec<_> = ranges
            .into_iter()
            .map(|range| {
                let per_item = &per_item;
                s.spawn(move || {
                    for i in range.clone() {
                        if let Err(e) = per_item(i) {
                            return vec![(i, Err(e))];
                        }
                    }
                    range.map(|i| (i, Ok(()))).collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|_| vec![]))
            .collect()
    });

    // Deterministic error selection: the lowest-index error wins,
    // regardless of which thread happened to finish first.
    let mut results = results;
    results.sort_by_key(|(i, _)| *i);
    for (_, r) in results {
        r?;
    }
    Ok(())
}

/// Like [`for_each_parallel`], but `per_item` returns a value instead of
/// just success/failure, and this collects every value into a `Vec` in
/// index order (`result[i]` is always `per_item(i)`'s output, regardless
/// of which thread computed it or when it finished) — used by
/// [`crate::encoder::encode_bytes_parallel`], whose per-tile work
/// (predictor selection + ZSTD race + chunk framing) produces a
/// variable-length `Vec<u8>` per tile that must still be written to the
/// output in scan order.
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    fn test_raw_slice_mut_disjoint_writes_are_safe() {
        let mut buf = vec![0u8; 100];
        let raw = RawSliceMut::new(&mut buf);
        std::thread::scope(|s| {
            for t in 0..4 {
                let raw = &raw;
                s.spawn(move || {
                    let slice = unsafe { raw.as_mut_slice() };
                    for byte in slice.iter_mut().skip(t * 25).take(25) {
                        *byte = t as u8;
                    }
                });
            }
        });
        for t in 0..4u8 {
            for &byte in buf.iter().skip(t as usize * 25).take(25) {
                assert_eq!(byte, t);
            }
        }
    }

    #[test]
    fn test_for_each_parallel_runs_every_index() {
        let counter = AtomicUsize::new(0);
        let seen: Vec<AtomicUsize> = (0..50).map(|_| AtomicUsize::new(0)).collect();
        for_each_parallel(50, |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            seen[i].fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 50);
        assert!(seen.iter().all(|c| c.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn test_for_each_parallel_below_threshold_runs_sequentially() {
        // Not asserting single-threadedness directly (hard to observe),
        // just that small counts still produce correct results.
        let seen: Vec<AtomicUsize> = (0..2).map(|_| AtomicUsize::new(0)).collect();
        for_each_parallel(2, |i| {
            seen[i].fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert!(seen.iter().all(|c| c.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn test_for_each_parallel_propagates_first_error_by_index() {
        let result = for_each_parallel(20, |i| {
            if i == 15 || i == 3 {
                Err(CodecError::InvalidPredictorCode(i as u8))
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(CodecError::InvalidPredictorCode(3))));
    }

    #[test]
    fn test_for_each_parallel_empty_is_ok() {
        for_each_parallel(0, |_| -> Result<()> {
            panic!("should never be called");
        })
        .unwrap();
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
}
