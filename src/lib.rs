// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![no_std]

extern crate ptr as ptr_;
use ptr_::Unique;

use core::{cmp, fmt, mem, usize, ops::DerefMut, num::NonZeroUsize,
           ptr::{self, NonNull}};

/// Represents the combination of a starting address and
/// a total capacity of the returned block.
#[derive(Debug)]
pub struct Excess(pub NonNull<u8>, pub usize);

/// Layout of a block of memory.
///
/// An instance of `Layout` describes a particular layout of memory.
/// You build a `Layout` up as an input to give to an allocator.
///
/// All layouts have an associated non-negative size and a
/// power-of-two alignment.
///
/// (Note however that layouts are *not* required to have positive
/// size, even though many allocators require that all memory
/// requests have positive size. A caller to the `Alloc::alloc`
/// method must either ensure that conditions like this are met, or
/// use specific allocators with looser requirements.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    // size of the requested block of memory, measured in bytes.
    size: usize,

    // alignment of the requested block of memory, measured in bytes.
    // we ensure that this is always a power-of-two, because API's
    // like `posix_memalign` require it and it is a reasonable
    // constraint to impose on Layout constructors.
    //
    // (However, we do not analogously require `align >= sizeof(void*)`,
    //  even though that is *also* a requirement of `posix_memalign`.)
    align: NonZeroUsize,
}


// FIXME: audit default implementations for overflow errors,
// (potentially switching to overflowing_add and
//  overflowing_mul as necessary).

impl Layout {
    /// Constructs a `Layout` from a given `size` and `align`,
    /// or returns `None` if any of the following conditions
    /// are not met:
    ///
    /// * `align` must be a power of two,
    ///
    /// * `align` must not exceed 2^31 (i.e. `1 << 31`),
    ///
    /// * `size`, when rounded up to the nearest multiple of `align`,
    ///    must not overflow (i.e. the rounded value must be less than
    ///    `usize::MAX`).
    #[inline]
    pub fn from_size_align(size: usize, align: usize) -> Option<Layout> {
        if !align.is_power_of_two() {
            return None;
        }

        if align > (1 << 31) {
            return None;
        }

        // (power-of-two implies align != 0.)

        // Rounded up size is:
        //   size_rounded_up = (size + align - 1) & !(align - 1);
        //
        // We know from above that align != 0. If adding (align - 1)
        // does not overflow, then rounding up will be fine.
        //
        // Conversely, &-masking with !(align - 1) will subtract off
        // only low-order-bits. Thus if overflow occurs with the sum,
        // the &-mask cannot subtract enough to undo that overflow.
        //
        // Above implies that checking for summation overflow is both
        // necessary and sufficient.
        if size > usize::MAX - (align - 1) {
            return None;
        }

        unsafe { Some(Layout::from_size_align_unchecked(size, align)) }
    }

    /// Creates a layout, bypassing all checks.
    ///
    /// # Safety
    ///
    /// This function is unsafe as it does not verify that `align` is
    /// a power-of-two that is also less than or equal to 2^31, nor
    /// that `size` aligned to `align` fits within the address space
    /// (i.e. the `Layout::from_size_align` preconditions).
    #[inline]
    pub const unsafe fn from_size_align_unchecked(size: usize, align: usize) -> Layout {
        Layout { size, align: NonZeroUsize::new_unchecked(align) }
    }

    /// The minimum size in bytes for a memory block of this layout.
    #[inline]
    pub fn size(&self) -> usize { self.size }

    /// The minimum byte alignment for a memory block of this layout.
    #[inline]
    pub fn align(&self) -> NonZeroUsize { self.align }

    /// Constructs a `Layout` suitable for holding a value of type `T`.
    #[inline]
    pub fn new<T>() -> Self {
        Layout { size: mem::size_of::<T>(),
                 align: unsafe { NonZeroUsize::new_unchecked(mem::align_of::<T>()) } }
    }

    /// Produces layout describing a record that could be used to
    /// allocate backing structure for `T` (which could be a trait
    /// or other unsized type like a slice).
    #[inline]
    pub fn for_value<T: ?Sized>(t: &T) -> Self {
        let (size, align) = (mem::size_of_val(t), mem::align_of_val(t));
        Layout::from_size_align(size, align).unwrap()
    }

    /// Creates a layout describing the record that can hold a value
    /// of the same layout as `self`, but that also is aligned to
    /// alignment `align` (measured in bytes).
    ///
    /// If `self` already meets the prescribed alignment, then returns
    /// `self`.
    ///
    /// Note that this method does not add any padding to the overall
    /// size, regardless of whether the returned layout has a different
    /// alignment. In other words, if `K` has size 16, `K.align_to(32)`
    /// will *still* have size 16.
    ///
    /// # Panics
    ///
    /// Panics if the combination of `self.size` and the given `align`
    /// violates the conditions listed in `from_size_align`.
    #[inline]
    pub fn align_to(&self, align: usize) -> Self {
        Layout::from_size_align(self.size, cmp::max(self.align.get(), align)).unwrap()
    }

    /// Returns the amount of padding we must insert after `self`
    /// to ensure that the following address will satisfy `align`
    /// (measured in bytes).
    ///
    /// E.g. if `self.size` is 9, then `self.padding_needed_for(4)`
    /// returns 3, because that is the minimum number of bytes of
    /// padding required to get a 4-aligned address (assuming that the
    /// corresponding memory block starts at a 4-aligned address).
    ///
    /// The return value of this function has no meaning if `align` is
    /// not a power-of-two.
    ///
    /// Note that the utility of the returned value requires `align`
    /// to be less than or equal to the alignment of the starting
    /// address for the whole allocated block of memory. One way to
    /// satisfy this constraint is to ensure `align <= self.align`.
    #[inline]
    pub fn padding_needed_for(&self, align: NonZeroUsize) -> usize {
        let len = self.size();
        let align = align.get();

        // Rounded up value is:
        //   len_rounded_up = (len + align - 1) & !(align - 1);
        // and then we return the padding difference: `len_rounded_up - len`.
        //
        // We use modular arithmetic throughout:
        //
        // 1. align is guaranteed to be > 0, so align - 1 is always
        //    valid.
        //
        // 2. `len + align - 1` can overflow by at most `align - 1`,
        //    so the &-mask wth `!(align - 1)` will ensure that in the
        //    case of overflow, `len_rounded_up` will itself be 0.
        //    Thus the returned padding, when added to `len`, yields 0,
        //    which trivially satisfies the alignment `align`.
        //
        // (Of course, attempts to allocate blocks of memory whose
        // size and padding overflow in the above manner should cause
        // the allocator to yield an error anyway.)

        (len.wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1))
            .wrapping_sub(len)
    }

    /// Returns a layout padded so the following address be aligned to
    /// `align` (measured in bytes), assuming the memory block is also
    /// aligned. It is equivalent to appending an array of
    /// `self.padding_needed_for(align)` bytes.
    ///
    /// The return value of this function has no meaning if `align` is
    /// not a power-of-two.
    ///
    /// Note that the utility of the returned value requires `align`
    /// to be less than or equal to the alignment of the starting
    /// address for the whole allocated block of memory. One way to
    /// satisfy this constraint is to ensure `align <= self.align`.
    #[inline]
    pub fn pad_to(&self, align: NonZeroUsize) -> Self {
        Layout {
            size: {
                let align = align.get();
                self.size().wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1)
            },
            align: self.align(),
        }
    }

    /// Creates a layout describing the record for `n` instances of
    /// `self`, with a suitable amount of padding between each to
    /// ensure that each instance is given its requested size and
    /// alignment. On success, returns `(k, offs)` where `k` is the
    /// layout of the array and `offs` is the distance between the start
    /// of each element in the array.
    ///
    /// On arithmetic overflow, returns `None`.
    #[inline]
    pub fn repeat(&self, n: usize) -> Option<(Self, usize)> {
        let padded_size = self.size.checked_add(self.padding_needed_for(self.align))?;
        let alloc_size = padded_size.checked_mul(n)?;

        // We can assume that `self.align` is a power-of-two that does
        // not exceed 2^31. Furthermore, `alloc_size` has already been
        // rounded up to a multiple of `self.align`; therefore, the
        // call to `Layout::from_size_align` below should never panic.
        Some((Layout::from_size_align(alloc_size, self.align.get()).unwrap(), padded_size))
    }

    /// Creates a layout describing the record for `self` followed by
    /// `next`, including any necessary padding to ensure that `next`
    /// will be properly aligned. Note that the result layout will
    /// satisfy the alignment properties of both `self` and `next`.
    ///
    /// Returns `Some((k, offset))`, where `k` is layout of the concatenated
    /// record and `offset` is the relative location, in bytes, of the
    /// start of the `next` embedded within the concatenated record
    /// (assuming that the record itself starts at offset 0).
    ///
    /// On arithmetic overflow, returns `None`.
    #[inline]
    pub fn extend(&self, next: Self) -> Option<(Self, usize)> {
        let new_align = cmp::max(self.align, next.align);
        let realigned = Layout::from_size_align(self.size, new_align.get())?;

        let pad = realigned.padding_needed_for(next.align);

        let offset = self.size.checked_add(pad)?;
        let new_size = offset.checked_add(next.size)?;
        Some((Layout::from_size_align(new_size, new_align.get())?, offset))
    }

    /// Creates a layout describing the record for `n` instances of
    /// `self`, with no padding between each instance.
    ///
    /// Note that, unlike `repeat`, `repeat_packed` does not guarantee
    /// that the repeated instances of `self` will be properly
    /// aligned, even if a given instance of `self` is properly
    /// aligned. In other words, if the layout returned by
    /// `repeat_packed` is used to allocate an array, it is not
    /// guaranteed that all elements in the array will be properly
    /// aligned.
    ///
    /// On arithmetic overflow, returns `None`.
    #[inline]
    pub fn repeat_packed(&self, n: usize) -> Option<Self> {
        Layout::from_size_align(self.size().checked_mul(n)?, self.align.get())
    }

    /// Creates a layout describing the record for `self` followed by
    /// `next` with no additional padding between the two. Since no
    /// padding is inserted, the alignment of `next` is irrelevant,
    /// and is not incorporated *at all* into the resulting layout.
    ///
    /// Returns `(k, offset)`, where `k` is layout of the concatenated
    /// record and `offset` is the relative location, in bytes, of the
    /// start of the `next` embedded within the concatenated record
    /// (assuming that the record itself starts at offset 0).
    ///
    /// (The `offset` is always the same as `self.size()`; we use this
    ///  signature out of convenience in matching the signature of
    ///  `extend`.)
    ///
    /// On arithmetic overflow, returns `None`.
    #[inline]
    pub fn extend_packed(&self, next: Self) -> Option<(Self, usize)> {
        let new_size = self.size().checked_add(next.size())?;
        Some((Layout::from_size_align(new_size, self.align.get())?, self.size()))
    }

    /// Creates a layout describing the record for a `[T; n]`.
    ///
    /// On arithmetic overflow, returns `None`.
    #[inline]
    pub fn array<T>(n: usize) -> Option<Self> {
        Layout::new::<T>().repeat(n).map(|(k, offs)| {
            debug_assert!(offs == mem::size_of::<T>());
            k
        })
    }
}

/// The `AllocErr` error specifies whether an allocation failure is
/// specifically due to resource exhaustion or if it is due to
/// something wrong when combining the given input arguments with this
/// allocator.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AllocErr {
    /// Error due to hitting some resource limit or otherwise running
    /// out of memory. This condition strongly implies that *some*
    /// series of deallocations would allow a subsequent reissuing of
    /// the original allocation request to succeed.
    Exhausted { request: Layout },

    /// Error due to allocator being fundamentally incapable of
    /// satisfying the original request. This condition implies that
    /// such an allocation request will never succeed on the given
    /// allocator, regardless of environment, memory pressure, or
    /// other contextual conditions.
    ///
    /// For example, an allocator that does not support requests for
    /// large memory blocks might return this error variant.
    Unsupported { details: &'static str },
}

impl AllocErr {
    #[inline]
    pub fn invalid_input(details: &'static str) -> Self {
        AllocErr::Unsupported { details }
    }
    #[inline]
    pub fn is_memory_exhausted(&self) -> bool {
        if let AllocErr::Exhausted { .. } = *self { true } else { false }
    }
    #[inline]
    pub fn is_request_unsupported(&self) -> bool {
        if let AllocErr::Unsupported { .. } = *self { true } else { false }
    }
    #[inline]
    pub fn description(&self) -> &str {
        match *self {
            AllocErr::Exhausted { .. } => "allocator memory exhausted",
            AllocErr::Unsupported { .. } => "unsupported allocator request",
        }
    }
}

// (we need this for downstream impl of trait Error)
impl fmt::Display for AllocErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "{}", self.description()) }
}

/// The `CannotReallocInPlace` error is used when `grow_in_place` or
/// `shrink_in_place` were unable to reuse the given memory block for
/// a requested layout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CannotReallocInPlace;

impl CannotReallocInPlace {
    pub fn description(&self) -> &str { "cannot reallocate allocator's memory in place" }
}

// (we need this for downstream impl of trait Error)
impl fmt::Display for CannotReallocInPlace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "{}", self.description()) }
}

/// An implementation of `Alloc` can allocate, reallocate, and
/// deallocate arbitrary blocks of data described via `Layout`.
///
/// Some of the methods require that a memory block be *currently
/// allocated* via an allocator. This means that:
///
/// * the starting address for that memory block was previously
///   returned by a previous call to an allocation method (`alloc`,
///   `alloc_zeroed`, `alloc_excess`, `alloc_one`, `alloc_array`) or
///   reallocation method (`realloc`, `realloc_excess`, or
///   `realloc_array`), and
///
/// * the memory block has not been subsequently deallocated, where
///   blocks are deallocated either by being passed to a deallocation
///   method (`dealloc`, `dealloc_one`, `dealloc_array`) or by being
///   passed to a reallocation method (see above) that returns `Ok`.
///
/// A note regarding zero-sized types and zero-sized layouts: many
/// methods in the `Alloc` trait state that allocation requests
/// must be non-zero size, or else undefined behavior can result.
///
/// * However, some higher-level allocation methods (`alloc_one`,
///   `alloc_array`) are well-defined on zero-sized types and can
///   optionally support them: it is left up to the implementor
///   whether to return `Err`, or to return `Ok` with some pointer.
///
/// * If an `Alloc` implementation chooses to return `Ok` in this
///   case (i.e. the pointer denotes a zero-sized inaccessible block)
///   then that returned pointer must be considered "currently
///   allocated". On such an allocator, *all* methods that take
///   currently-allocated pointers as inputs must accept these
///   zero-sized pointers, *without* causing undefined behavior.
///
/// * In other words, if a zero-sized pointer can flow out of an
///   allocator, then that allocator must likewise accept that pointer
///   flowing back into its deallocation and reallocation methods.
///
/// Some of the methods require that a layout *fit* a memory block.
/// What it means for a layout to "fit" a memory block means (or
/// equivalently, for a memory block to "fit" a layout) is that the
/// following two conditions must hold:
///
/// 1. The block's starting address must be aligned to `layout.align()`.
///
/// 2. The block's size must fall in the range `[use_min, use_max]`, where:
///
///    * `use_min` is `self.usable_size(layout).0`, and
///
///    * `use_max` is the capacity that was (or would have been)
///      returned when (if) the block was allocated via a call to
///      `alloc_excess` or `realloc_excess`.
///
/// Note that:
///
///  * the size of the layout most recently used to allocate the block
///    is guaranteed to be in the range `[use_min, use_max]`, and
///
///  * a lower-bound on `use_max` can be safely approximated by a call to
///    `usable_size`.
///
///  * if a layout `k` fits a memory block (denoted by `ptr`)
///    currently allocated via an allocator `a`, then it is legal to
///    use that layout to deallocate it, i.e. `a.dealloc(ptr, k);`.
///
/// # Unsafety
///
/// The `Alloc` trait is an `unsafe` trait for a number of reasons, and
/// implementors must ensure that they adhere to these contracts:
///
/// * Pointers returned from allocation functions must point to valid memory and
///   retain their validity until at least the instance of `Alloc` is dropped
///   itself.
///
/// * It's undefined behavior if global allocators unwind.  This restriction may
///   be lifted in the future, but currently a panic from any of these
///   functions may lead to memory unsafety. Note that as of the time of this
///   writing allocators *not* intending to be global allocators can still panic
///   in their implementation without violating memory safety.
///
/// * `Layout` queries and calculations in general must be correct. Callers of
///   this trait are allowed to rely on the contracts defined on each method,
///   and implementors must ensure such contracts remain true.
///
/// Note that this list may get tweaked over time as clarifications are made in
/// the future. Additionally global allocators may gain unique requirements for
/// how to safely implement one in the future as well.
pub unsafe trait Alloc {
    // (Note: existing allocators have unspecified but well-defined
    // behavior in response to a zero size allocation request ;
    // e.g. in C, `malloc` of 0 will either return a null pointer or a
    // unique pointer, but will not have arbitrary undefined
    // behavior. Rust should consider revising the alloc::heap crate
    // to reflect this reality.)

    /// Returns a pointer meeting the size and alignment guarantees of
    /// `layout`.
    ///
    /// If this method returns an `Ok(addr)`, then the `addr` returned
    /// will be non-null address pointing to a block of storage
    /// suitable for holding an instance of `layout`.
    ///
    /// The returned block of storage may or may not have its contents
    /// initialized. (Extension subtraits might restrict this
    /// behavior, e.g. to ensure initialization to particular sets of
    /// bit patterns.)
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure that `layout` has non-zero size.
    ///
    /// (Extension subtraits might provide more specific bounds on
    /// behavior, e.g. guarantee a sentinel address or a null pointer
    /// in response to a zero-size allocation request.)
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints.
    ///
    /// Implementations are encouraged to return `Err` on memory
    /// exhaustion rather than panicking or aborting, but this is not
    /// a strict requirement. (Specifically: it is *legal* to
    /// implement this trait atop an underlying native allocation
    /// library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn alloc(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocErr>;

    /// Deallocate the memory referenced by `ptr`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must denote a block of memory currently allocated via
    ///   this allocator,
    ///
    /// * `layout` must *fit* that block of memory,
    ///
    /// * In addition to fitting the block of memory `layout`, the
    ///   alignment of the `layout` must match the alignment used
    ///   to allocate that block of memory.
    unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout);

    // == ALLOCATOR-SPECIFIC QUANTITIES AND LIMITS ==
    // usable_size

    /// Returns bounds on the guaranteed usable size of a successful
    /// allocation created with the specified `layout`.
    ///
    /// In particular, if one has a memory block allocated via a given
    /// allocator `a` and layout `k` where `a.usable_size(k)` returns
    /// `(l, u)`, then one can pass that block to `a.dealloc()` with a
    /// layout in the size range [l, u].
    ///
    /// (All implementors of `usable_size` must ensure that
    /// `l <= k.size() <= u`)
    ///
    /// Both the lower- and upper-bounds (`l` and `u` respectively)
    /// are provided, because an allocator based on size classes could
    /// misbehave if one attempts to deallocate a block without
    /// providing a correct value for its size (i.e., one within the
    /// range `[l, u]`).
    ///
    /// Clients who wish to make use of excess capacity are encouraged
    /// to use the `alloc_excess` and `realloc_excess` instead, as
    /// this method is constrained to report conservative values that
    /// serve as valid bounds for *all possible* allocation method
    /// calls.
    ///
    /// However, for clients that do not wish to track the capacity
    /// returned by `alloc_excess` locally, this method is likely to
    /// produce useful results.
    #[inline]
    fn usable_size(&self, layout: Layout) -> (usize, usize) { (layout.size(), layout.size()) }

    // == METHODS FOR MEMORY REUSE ==
    // realloc. alloc_excess, realloc_excess

    /// Returns a pointer suitable for holding data described by
    /// `new_layout`, meeting its size and alignment guarantees. To
    /// accomplish this, this may extend or shrink the allocation
    /// referenced by `ptr` to fit `new_layout`.
    ///
    /// If this returns `Ok`, then ownership of the memory block
    /// referenced by `ptr` has been transferred to this
    /// allocator. The memory may or may not have been freed, and
    /// should be considered unusable (unless of course it was
    /// transferred back to the caller again via the return value of
    /// this method).
    ///
    /// If this method returns `Err`, then ownership of the memory
    /// block has not been transferred to this allocator, and the
    /// contents of the memory block are unaltered.
    ///
    /// For best results, `new_layout` should not impose a different
    /// alignment constraint than `layout`. (In other words,
    /// `new_layout.align()` should equal `layout.align()`.) However,
    /// behavior is well-defined (though underspecified) when this
    /// constraint is violated; further discussion below.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * `layout` must *fit* the `ptr` (see above). (The `new_layout`
    ///   argument need not fit it.)
    ///
    /// * `new_layout` must have size greater than zero.
    ///
    /// * the alignment of `new_layout` is non-zero.
    ///
    /// (Extension subtraits might provide more specific bounds on
    /// behavior, e.g. guarantee a sentinel address or a null pointer
    /// in response to a zero-size allocation request.)
    ///
    /// # Errors
    ///
    /// Returns `Err` only if `new_layout` does not match the
    /// alignment of `layout`, or does not meet the allocator's size
    /// and alignment constraints of the allocator, or if reallocation
    /// otherwise fails.
    ///
    /// (Note the previous sentence did not say "if and only if" -- in
    /// particular, an implementation of this method *can* return `Ok`
    /// if `new_layout.align() != old_layout.align()`; or it can
    /// return `Err` in that scenario, depending on whether this
    /// allocator can dynamically adjust the alignment constraint for
    /// the block.)
    ///
    /// Implementations are encouraged to return `Err` on memory
    /// exhaustion rather than panicking or aborting, but this is not
    /// a strict requirement. (Specifically: it is *legal* to
    /// implement this trait atop an underlying native allocation
    /// library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an
    /// reallocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    unsafe fn realloc(&mut self,
                      ptr: NonNull<u8>,
                      layout: Layout,
                      new_size: usize) -> Result<NonNull<u8>, AllocErr> {
        let old_size = layout.size();

        if let Ok(()) = self.resize_in_place(ptr, layout, new_size) {
            return Ok(ptr);
        }

        // otherwise, fall back on alloc + copy + dealloc.
        let result = self.alloc(Layout { size: new_size, align: layout.align() });
        if let Ok(new_ptr) = result {
            ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(),
                                     cmp::min(old_size, new_size));
            self.dealloc(ptr, layout);
        }
        result
    }

    /// Behaves like `alloc`, but also ensures that the contents
    /// are set to zero before being returned.
    ///
    /// # Safety
    ///
    /// This function is unsafe for the same reasons that `alloc` is.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints, just as in `alloc`.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    #[inline]
    unsafe fn alloc_zeroed(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocErr> {
        let size = layout.size();
        let r = self.alloc(layout);
        if let Ok(p) = r {
            ptr::write_bytes(p.as_ptr(), 0, size);
        }
        r
    }

    /// Behaves like `alloc`, but also returns the whole size of
    /// the returned block. For some `layout` inputs, like arrays, this
    /// may include extra storage usable for additional data.
    ///
    /// # Safety
    ///
    /// This function is unsafe for the same reasons that `alloc` is.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints, just as in `alloc`.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    #[inline]
    unsafe fn alloc_excess(&mut self, layout: Layout) -> Result<Excess, AllocErr> {
        let usable_size = self.usable_size(layout);
        self.alloc(layout).map(|p| Excess(p, usable_size.1))
    }

    /// Behaves like `realloc`, but also returns the whole size of
    /// the returned block. For some `layout` inputs, like arrays, this
    /// may include extra storage usable for additional data.
    ///
    /// # Safety
    ///
    /// This function is unsafe for the same reasons that `realloc` is.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `layout` does not meet allocator's size or alignment
    /// constraints, just as in `realloc`.
    ///
    /// Clients wishing to abort computation in response to an
    /// reallocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    #[inline]
    unsafe fn realloc_excess(&mut self,
                             ptr: NonNull<u8>,
                             layout: Layout,
                             new_size: usize) -> Result<Excess, AllocErr> {
        let new_layout = Layout { size: new_size, align: layout.align() };
        let usable_size = self.usable_size(new_layout);
        self.realloc(ptr, layout, new_size).map(|p| Excess(p, usable_size.1))
    }

    /// Attempts to resize the allocation referenced by `ptr` to fit `new_layout`.
    ///
    /// If this returns `Ok`, then the allocator has asserted that the
    /// memory block referenced by `ptr` now fits `new_layout`, and thus can
    /// be used to carry data of that layout. (The allocator is allowed to
    /// expend effort to accomplish this, such as extending the memory block to
    /// include successor blocks, or virtual memory tricks.)
    ///
    /// Regardless of what this method returns, ownership of the
    /// memory block referenced by `ptr` has not been transferred, and
    /// the contents of the memory block up to the lesser of the old and new
    /// sizes are unaltered.
    ///
    /// If this returns `Err`, then the memory block is considered to
    /// still represent the original `layout`. None of the
    /// block has been carved off for reuse elsewhere, ownership of
    /// the memory block has not been transferred, and the contents of
    /// the memory block are unaltered.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * `layout` must *fit* the `ptr` (see above); note the
    ///   `new_layout` argument need not fit it,
    ///
    /// * `new_layout.align()` must equal `layout.align()`.
    ///
    /// # Errors
    ///
    /// Returns `Err(CannotReallocInPlace)` when the allocator is
    /// unable to assert that the memory block referenced by `ptr`
    /// could fit `layout`.
    ///
    /// Note that one cannot pass `CannotReallocInPlace` to the `oom`
    /// method; clients are expected either to be able to recover from
    /// `resize_in_place` failures without aborting, or to fall back on
    /// another reallocation method before resorting to an abort.
    #[inline]
    unsafe fn resize_in_place(&mut self,
                              ptr: NonNull<u8>,
                              layout: Layout,
                              new_size: usize) -> Result<(), CannotReallocInPlace> {
        let _ = ptr; // this default implementation doesn't care about the actual address.
        let (l, u) = self.usable_size(layout);
        // l ≤ layout.size() ≤ u [guaranteed by usable_size()]
        if u >= new_size && l <= new_size { Ok(()) }
        else { Err(CannotReallocInPlace) }
    }


    // == COMMON USAGE PATTERNS ==
    // alloc_one, dealloc_one, alloc_array, realloc_array. dealloc_array

    /// Allocates a block suitable for holding an instance of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// The returned block is suitable for passing to the
    /// `alloc`/`realloc` methods of this allocator.
    ///
    /// Note to implementors: If this returns `Ok(ptr)`, then `ptr`
    /// must be considered "currently allocated" and must be
    /// acceptable input to methods such as `realloc` or `dealloc`,
    /// *even if* `T` is a zero-sized type. In other words, if your
    /// `Alloc` implementation overrides this method in a manner
    /// that can return a zero-sized `ptr`, then all reallocation and
    /// deallocation methods need to be similarly overridden to accept
    /// such values as input.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `T` does not meet allocator's size or alignment constraints.
    ///
    /// For zero-sized `T`, may return either of `Ok` or `Err`, but
    /// will *not* yield undefined behavior.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    #[inline]
    fn alloc_one<T>(&mut self) -> Result<Unique<T>, AllocErr> {
        let k = Layout::new::<T>();
        if k.size() > 0 {
            unsafe { self.alloc(k).map(|p| p.cast().into()) }
        } else {
            Err(AllocErr::invalid_input("zero-sized type invalid for alloc_one"))
        }
    }

    /// Deallocates a block suitable for holding an instance of `T`.
    ///
    /// The given block must have been produced by this allocator,
    /// and must be suitable for storing a `T` (in terms of alignment
    /// as well as minimum and maximum size); otherwise yields
    /// undefined behavior.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure both:
    ///
    /// * `ptr` must denote a block of memory currently allocated via this allocator
    ///
    /// * the layout of `T` must *fit* that block of memory.
    #[inline]
    unsafe fn dealloc_one<T>(&mut self, ptr: Unique<T>) {
        let k = Layout::new::<T>();
        if k.size() > 0 { self.dealloc(ptr.as_ptr().cast(), k); }
    }

    /// Allocates a block suitable for holding `n` instances of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// Returns actual size of array allocated.
    ///
    /// The returned block is suitable for passing to the
    /// `alloc`/`realloc` methods of this allocator.
    ///
    /// Note to implementors: If this returns `Ok(ptr)`, then `ptr`
    /// must be considered "currently allocated" and must be
    /// acceptable input to methods such as `realloc` or `dealloc`,
    /// *even if* `T` is a zero-sized type. In other words, if your
    /// `Alloc` implementation overrides this method in a manner
    /// that can return a zero-sized `ptr`, then all reallocation and
    /// deallocation methods need to be similarly overridden to accept
    /// such values as input.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `[T; n]` does not meet allocator's size or alignment
    /// constraints.
    ///
    /// For zero-sized `T` or `n == 0`, may return either of `Ok` or
    /// `Err`, but will *not* yield undefined behavior.
    ///
    /// Always returns `Err` on arithmetic overflow.
    ///
    /// Clients wishing to abort computation in response to an
    /// allocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    #[inline]
    fn alloc_array<T>(&mut self, n: usize) -> Result<(Unique<T>, usize), AllocErr> {
        match Layout::array::<T>(n) {
            Some(ref layout) if layout.size() > 0 => { unsafe {
                self.alloc_excess(layout.clone())
                    .map(|Excess(p, n)| (p.cast().into(), n / mem::size_of::<T>()))
            } },
            _ => Err(AllocErr::invalid_input("invalid layout for alloc_array")),
        }
    }

    /// Reallocates a block previously suitable for holding `n_old`
    /// instances of `T`, returning a block suitable for holding
    /// `n_new` instances of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// Returns actual size of array allocated.
    ///
    /// The returned block is suitable for passing to the
    /// `alloc`/`realloc` methods of this allocator.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure all of the following:
    ///
    /// * `ptr` must be currently allocated via this allocator,
    ///
    /// * the layout of `[T; n_old]` must *fit* that block of memory.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or
    /// `[T; n_new]` does not meet allocator's size or alignment
    /// constraints.
    ///
    /// For zero-sized `T` or `n_new == 0`, may return either of `Ok` or
    /// `Err`, but will *not* yield undefined behavior.
    ///
    /// Always returns `Err` on arithmetic overflow.
    ///
    /// Clients wishing to abort computation in response to an
    /// reallocation error are encouraged to call the allocator's `oom`
    /// method, rather than directly invoking `panic!` or similar.
    #[inline]
    unsafe fn realloc_array<T>(&mut self,
                               ptr: Unique<T>,
                               n_old: usize,
                               n_new: usize) -> Result<(Unique<T>, usize), AllocErr> {
        match (Layout::array::<T>(n_old), Layout::array::<T>(n_new), ptr.as_ptr()) {
            (Some(ref k_old), Some(ref k_new), ptr) if k_old.size() > 0 && k_new.size() > 0 => {
                self.realloc_excess(ptr.cast(), k_old.clone(), k_new.size())
                    .map(|Excess(p, n)| (p.cast().into(),
                                         n / mem::size_of::<T>()))
            }
            _ => {
                Err(AllocErr::invalid_input("invalid layout for realloc_array"))
            },
        }
    }

    /// Deallocates a block suitable for holding `n` instances of `T`.
    ///
    /// Captures a common usage pattern for allocators.
    ///
    /// # Safety
    ///
    /// This function is unsafe because undefined behavior can result
    /// if the caller does not ensure both:
    ///
    /// * `ptr` must denote a block of memory currently allocated via this allocator
    ///
    /// * the layout of `[T; n]` must *fit* that block of memory.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either `[T; n]` or the given
    /// memory block does not meet allocator's size or alignment
    /// constraints.
    ///
    /// Always returns `Err` on arithmetic overflow.
    #[inline]
    unsafe fn dealloc_array<T>(&mut self, ptr: Unique<T>, n: usize) -> Result<(), AllocErr> {
        match Layout::array::<T>(n) {
            Some(ref k) if k.size() > 0 => {
                Ok(self.dealloc(ptr.as_ptr().cast(), k.clone()))
            },
            _ => {
                Err(AllocErr::invalid_input("invalid layout for dealloc_array"))
            },
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct NullAllocator(());

unsafe impl Alloc for NullAllocator {
    #[inline] unsafe fn alloc(&mut self, _: Layout) -> Result<NonNull<u8>, AllocErr> { Err(AllocErr::Unsupported { details: "" }) }
    #[inline] unsafe fn dealloc(&mut self, _: NonNull<u8>, _: Layout) {}
}

unsafe impl<A: Alloc + ?Sized, P: DerefMut<Target = A>> Alloc for P {
    #[inline] unsafe fn alloc(&mut self, l: Layout) -> Result<NonNull<u8>, AllocErr> { self.deref_mut().alloc(l) }
    #[inline] unsafe fn dealloc(&mut self, ptr: NonNull<u8>, l: Layout) { self.deref_mut().dealloc(ptr, l) }
    #[inline] unsafe fn realloc(&mut self, ptr: NonNull<u8>, old_l: Layout, new_size: usize) -> Result<NonNull<u8>, AllocErr> { self.deref_mut().realloc(ptr, old_l, new_size) }
    #[inline] unsafe fn alloc_zeroed(&mut self, l: Layout) -> Result<NonNull<u8>, AllocErr> { self.deref_mut().alloc_zeroed(l) }
    #[inline] unsafe fn alloc_excess(&mut self, l: Layout) -> Result<Excess, AllocErr> { self.deref_mut().alloc_excess(l) }
    #[inline] unsafe fn realloc_excess(&mut self, ptr: NonNull<u8>, old_l: Layout, new_size: usize) -> Result<Excess, AllocErr> { self.deref_mut().realloc_excess(ptr, old_l, new_size) }
    #[inline] unsafe fn resize_in_place(&mut self, ptr: NonNull<u8>, old_l: Layout, new_size: usize) -> Result<(), CannotReallocInPlace> { self.deref_mut().resize_in_place(ptr, old_l, new_size) }

    #[inline] fn usable_size(&self, l: Layout) -> (usize, usize) { self.deref().usable_size(l) }

    #[inline] fn alloc_one<T>(&mut self) -> Result<Unique<T>, AllocErr> { self.deref_mut().alloc_one() }
    #[inline] unsafe fn dealloc_one<T>(&mut self, ptr: Unique<T>) { self.deref_mut().dealloc_one(ptr) }
    #[inline] fn alloc_array<T>(&mut self, n: usize) -> Result<(Unique<T>, usize), AllocErr> { self.deref_mut().alloc_array(n) }
    #[inline] unsafe fn realloc_array<T>(&mut self, ptr: Unique<T>, old_n: usize, new_n: usize) -> Result<(Unique<T>, usize), AllocErr> { self.deref_mut().realloc_array(ptr, old_n, new_n) }
    #[inline] unsafe fn dealloc_array<T>(&mut self, ptr: Unique<T>, n: usize) -> Result<(), AllocErr> { self.deref_mut().dealloc_array(ptr, n) }
}
