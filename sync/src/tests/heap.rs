//! A counting allocator for the sync tests, so a test can measure what a
//! path holds instead of estimating it. Counted per thread: tests running
//! beside each other add nothing to one another's peak, and work the store
//! runs on its own threads is left out.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy)]
struct Heap {
    live: usize,
    peak: usize,
}

thread_local! {
    static HEAP: Cell<Heap> = const { Cell::new(Heap { live: 0, peak: 0 }) };
}

fn heap_add(bytes: usize) {
    let _ = HEAP.try_with(|heap| {
        let mut h = heap.get();
        h.live = h.live.saturating_add(bytes);
        h.peak = h.peak.max(h.live);
        heap.set(h);
    });
}

fn heap_sub(bytes: usize) {
    let _ = HEAP.try_with(|heap| {
        let mut h = heap.get();
        h.live = h.live.saturating_sub(bytes);
        heap.set(h);
    });
}

struct Counting;

// SAFETY: every call goes to the system allocator with the same layout;
// the counting touches only a thread-local cell.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            heap_add(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        heap_sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            heap_sub(layout.size());
            heap_add(new_size);
        }
        new
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// A point from which the thread's heap growth is measured.
pub(crate) struct HeapMark {
    base: usize,
}

impl HeapMark {
    /// Starts measuring: the peak from here on counts bytes above what the
    /// thread holds now.
    pub(crate) fn start() -> Self {
        let base = HEAP.with(|heap| {
            let mut h = heap.get();
            h.peak = h.live;
            heap.set(h);
            h.live
        });
        HeapMark { base }
    }

    /// The most bytes the thread has held above the mark's start.
    pub(crate) fn peak(&self) -> usize {
        HEAP.with(|heap| heap.get().peak.saturating_sub(self.base))
    }
}
