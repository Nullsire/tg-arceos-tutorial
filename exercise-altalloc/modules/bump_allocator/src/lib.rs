#![no_std]

use axallocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
use core::alloc::Layout;
use core::ptr::NonNull;

/// Early memory allocator
/// Use it before formal bytes-allocator and pages-allocator can work!
/// This is a double-end memory range:
/// - Alloc bytes forward
/// - Alloc pages backward
///
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start       b_pos        p_pos       end
///
/// For bytes area, 'count' records number of allocations.
/// When it goes down to ZERO, free bytes-used area.
/// For pages area, it will never be freed!
///
pub struct EarlyAllocator<const PAGE_SIZE: usize> {
    start: usize,
    size: usize,
    b_pos: usize,
    p_pos: usize,
    count: usize,
}

impl<const PAGE_SIZE: usize> EarlyAllocator<PAGE_SIZE> {
    pub const fn new() -> Self {
        Self {
            start: 0,
            size: 0,
            b_pos: 0,
            p_pos: 0,
            count: 0,
        }
    }
}

impl<const PAGE_SIZE: usize> BaseAllocator for EarlyAllocator<PAGE_SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        self.start = start;
        self.size = size;
        self.b_pos = start;
        self.p_pos = start + size;
        self.count = 0;
    }

    fn add_memory(&mut self, _start: usize, size: usize) -> AllocResult {
        self.p_pos += size;
        self.size += size;
        Ok(())
    }
}

impl<const PAGE_SIZE: usize> ByteAllocator for EarlyAllocator<PAGE_SIZE> {
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        // Align b_pos up to layout.align()
        self.b_pos = (self.b_pos + layout.align() - 1) & !(layout.align() - 1);
        // Check if enough space
        if self.b_pos + layout.size() > self.p_pos {
            return Err(AllocError::NoMemory);
        }
        let ptr = self.b_pos;
        self.b_pos += layout.size();
        self.count += 1;
        Ok(NonNull::new(ptr as *mut u8).unwrap())
    }

    fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
        self.count -= 1;
        if self.count == 0 {
            self.b_pos = self.start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.size
    }

    fn used_bytes(&self) -> usize {
        self.b_pos - self.start
    }

    fn available_bytes(&self) -> usize {
        self.p_pos - self.b_pos
    }
}

impl<const PAGE_SIZE: usize> PageAllocator for EarlyAllocator<PAGE_SIZE> {
    const PAGE_SIZE: usize = PAGE_SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        let total = num_pages * PAGE_SIZE;
        // Align p_pos down to align_pow2
        self.p_pos = self.p_pos & !(align_pow2 - 1);
        let new_p_pos = self.p_pos - total;
        if new_p_pos < self.b_pos {
            return Err(AllocError::NoMemory);
        }
        self.p_pos = new_p_pos;
        Ok(self.p_pos)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // No-op: bump allocator never frees pages
    }

    fn alloc_pages_at(
        &mut self,
        _base: usize,
        _num_pages: usize,
        _align_pow2: usize,
    ) -> AllocResult<usize> {
        Err(AllocError::NoMemory)
    }

    fn total_pages(&self) -> usize {
        self.size / PAGE_SIZE
    }

    fn used_pages(&self) -> usize {
        (self.start + self.size - self.p_pos) / PAGE_SIZE
    }

    fn available_pages(&self) -> usize {
        (self.p_pos - self.start) / PAGE_SIZE
    }
}
