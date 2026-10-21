//! Bounded, tracked native allocation and externally supplied allocator tables.
//!
//! Keep the allocator alive until every native session or HPACK context using it
//! has been destroyed. Borrowing the table does not transfer ownership. Built-in
//! callbacks use a fixed alignment suitable for the native library's scalar
//! allocations, keep metadata before each allocation, and never invoke user code.

use std::{
    alloc::{Layout, alloc, dealloc},
    cell::Cell,
    ffi::c_void,
    marker::PhantomData,
    ptr::{self, NonNull},
    rc::Rc,
};

pub use nghttp2_sys::{nghttp2_calloc, nghttp2_free, nghttp2_malloc, nghttp2_mem, nghttp2_realloc};

/// Native allocator table.
pub type RawAllocator = nghttp2_mem;
/// Native `malloc` replacement.
pub type RawMalloc = nghttp2_malloc;
/// Native `free` replacement.
pub type RawFree = nghttp2_free;
/// Native `calloc` replacement.
pub type RawCalloc = nghttp2_calloc;
/// Native `realloc` replacement.
pub type RawRealloc = nghttp2_realloc;

/// Requested payload accounting; metadata and allocator overhead are excluded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationStats {
    /// Bytes currently owned by native objects.
    pub live_bytes: usize,
    /// Largest logical live payload total (realloc counts its replacement once).
    pub peak_bytes: usize,
    /// Number of live blocks, including successful zero-byte requests.
    pub live_blocks: usize,
    /// Requests rejected by the budget, size overflow, or system allocator.
    pub failed_allocations: usize,
}

#[derive(Default)]
struct State {
    limit: Cell<usize>,
    stats: Cell<AllocationStats>,
}

impl State {
    fn failed(&self) {
        let mut stats = self.stats.get();
        stats.failed_allocations = stats.failed_allocations.saturating_add(1);
        self.stats.set(stats);
    }
}

/// Native allocator with stable userdata, optional payload budget, and counters.
///
/// This value is single-threaded, as are the native objects borrowing its table.
/// Moving it preserves both the table and userdata addresses. Budget changes
/// affect subsequent allocations only; lowering a budget never frees existing native allocations.
/// Reallocation budgets count the final live payload, although replacement storage
/// temporarily coexists with the old block until its contents have been copied.
/// HPACK's upstream `nghttp2_hd.c` retains `context->mem` rather than copying
/// the table. Both the table and tracking state therefore have heap-stable
/// addresses until this owner is dropped. Native users must be destroyed first.
/// Externally supplied tables retain their own ownership and accounting contract.
pub struct CustomAllocator {
    raw: Box<RawAllocator>,
    state: Option<NonNull<State>>,
    _single_thread: PhantomData<Rc<()>>,
}

impl Default for CustomAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomAllocator {
    /// Creates a tracked allocator without a payload budget.
    pub fn new() -> Self {
        Self::with_limit(usize::MAX)
    }

    /// Creates an allocator that rejects requests exceeding `max_live_bytes`.
    pub fn with_limit(max_live_bytes: usize) -> Self {
        let state = Box::new(State::default());
        state.limit.set(max_live_bytes);
        let state = Box::into_non_null(state);
        let raw = RawAllocator {
            mem_user_data: state.as_ptr().cast(),
            malloc: Some(tracked_malloc),
            free: Some(tracked_free),
            calloc: Some(tracked_calloc),
            realloc: Some(tracked_realloc),
        };
        Self {
            raw: Box::new(raw),
            state: Some(state),
            _single_thread: PhantomData,
        }
    }

    /// Wraps custom callbacks without taking ownership of their userdata.
    ///
    /// # Safety
    /// All callbacks must be present, obey the C allocation contracts, provide
    /// sufficient alignment, and never unwind. Userdata and callback resources
    /// must remain valid until this wrapper and all native users are destroyed.
    /// Calls must be serialized, and pointers must only be freed or reallocated
    /// by their originating allocator. The wrapper does not free external state.
    pub unsafe fn from_raw(raw: RawAllocator) -> Self {
        Self {
            raw: Box::new(raw),
            state: None,
            _single_thread: PhantomData,
        }
    }

    /// Borrows the table. Native users must retain a borrow of this owner until
    /// their destruction; copying the raw table does not extend its lifetime.
    pub fn as_raw(&self) -> &RawAllocator {
        &self.raw
    }

    /// Returns counters for built-in tracking, or `None` for an external table.
    pub fn stats(&self) -> Option<AllocationStats> {
        self.state.map(|state| {
            // SAFETY: this owner retains the raw allocation until Drop.
            unsafe { state.as_ref() }.stats.get()
        })
    }

    /// Changes the built-in payload budget; returns false for external tables.
    pub fn set_limit(&self, max_live_bytes: usize) -> bool {
        if let Some(state) = self.state {
            // SAFETY: state remains allocated; Cell serializes caller-thread updates.
            unsafe { state.as_ref() }.limit.set(max_live_bytes);
            true
        } else {
            false
        }
    }
}

impl Drop for CustomAllocator {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // SAFETY: this is the unique owner of the raw Box allocation;
            // native users must have been destroyed before the allocator owner.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
        }
    }
}

// The header size is a multiple of the alignment; the returned payload therefore
// has the same alignment. Native nghttp2 allocations require no over-aligned type.
#[repr(C, align(16))]
struct Header {
    requested: usize,
    layout_size: usize,
}

fn block_layout(requested: usize) -> Option<Layout> {
    let total = size_of::<Header>().checked_add(requested.max(1))?;
    Layout::from_size_align(total, align_of::<Header>()).ok()
}

// All helper calls are confined to callbacks with the same live boxed State.
unsafe fn state<'a>(userdata: *mut c_void) -> &'a State {
    // SAFETY: native users preserve the CustomAllocator owner and its boxed state.
    unsafe { &*userdata.cast::<State>() }
}

unsafe fn allocate(size: usize, replacing: Option<usize>, state: &State) -> *mut c_void {
    let stats = state.stats.get();
    let Some(next_bytes) = stats
        .live_bytes
        .checked_sub(replacing.unwrap_or(0))
        .and_then(|n| n.checked_add(size))
    else {
        state.failed();
        return ptr::null_mut();
    };
    let Some(layout) = block_layout(size).filter(|_| next_bytes <= state.limit.get()) else {
        state.failed();
        return ptr::null_mut();
    };
    // SAFETY: layout is nonzero, valid, and aligned for Header.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        state.failed();
        return ptr::null_mut();
    }
    // SAFETY: block has room and alignment for the header followed by payload.
    unsafe {
        block.cast::<Header>().write(Header {
            requested: size,
            layout_size: layout.size(),
        })
    };
    state.stats.set(AllocationStats {
        live_bytes: next_bytes,
        peak_bytes: stats.peak_bytes.max(next_bytes),
        live_blocks: stats
            .live_blocks
            .saturating_add(usize::from(replacing.is_none())),
        ..stats
    });
    // SAFETY: layout contains Header plus at least one payload byte.
    unsafe { block.add(size_of::<Header>()).cast() }
}

unsafe extern "C" fn tracked_malloc(size: usize, userdata: *mut c_void) -> *mut c_void {
    // SAFETY: callbacks receive the boxed state installed in their native table.
    unsafe { allocate(size, None, state(userdata)) }
}

unsafe extern "C" fn tracked_free(pointer: *mut c_void, userdata: *mut c_void) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: pointer originated from these callbacks and has a live header.
    let (header, state) = unsafe {
        (
            pointer
                .cast::<u8>()
                .sub(size_of::<Header>())
                .cast::<Header>(),
            state(userdata),
        )
    };
    // SAFETY: the live header is initialized and uniquely accessed for free.
    let (requested, layout_size) = unsafe { ((*header).requested, (*header).layout_size) };
    let mut stats = state.stats.get();
    stats.live_bytes -= requested;
    stats.live_blocks -= 1;
    state.stats.set(stats);
    // SAFETY: the size and alignment exactly reconstruct the successful allocation.
    unsafe {
        dealloc(
            header.cast(),
            Layout::from_size_align_unchecked(layout_size, align_of::<Header>()),
        )
    };
}

unsafe extern "C" fn tracked_calloc(
    count: usize,
    size: usize,
    userdata: *mut c_void,
) -> *mut c_void {
    let Some(total) = count.checked_mul(size) else {
        // SAFETY: userdata is the table's live boxed state.
        unsafe { state(userdata) }.failed();
        return ptr::null_mut();
    };
    // SAFETY: callback userdata remains valid throughout this call.
    let pointer = unsafe { tracked_malloc(total, userdata) };
    if !pointer.is_null() {
        // SAFETY: successful allocation reserves at least total writable bytes.
        unsafe { ptr::write_bytes(pointer.cast::<u8>(), 0, total) };
    }
    pointer
}

unsafe extern "C" fn tracked_realloc(
    pointer: *mut c_void,
    size: usize,
    userdata: *mut c_void,
) -> *mut c_void {
    if pointer.is_null() {
        // SAFETY: same callback state, realloc(NULL,n) is malloc(n).
        return unsafe { tracked_malloc(size, userdata) };
    }
    if size == 0 {
        // SAFETY: pointer originated from this allocator; zero-size frees it.
        unsafe { tracked_free(pointer, userdata) };
        return ptr::null_mut();
    }
    // SAFETY: live pointers from this allocator have an initialized header.
    let old_size = unsafe {
        (*pointer
            .cast::<u8>()
            .sub(size_of::<Header>())
            .cast::<Header>())
        .requested
    };
    // SAFETY: userdata belongs to this allocation's originating allocator.
    let state = unsafe { state(userdata) };
    // SAFETY: the old allocation remains live until a replacement succeeds.
    let replacement = unsafe { allocate(size, Some(old_size), state) };
    if !replacement.is_null() {
        // SAFETY: allocations are disjoint, and both hold at least min(old,new) bytes.
        unsafe {
            ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                replacement.cast::<u8>(),
                old_size.min(size),
            )
        };
        // SAFETY: the replaced block is still live; accounting already reflects
        // its replacement, so release only its storage rather than counting twice.
        unsafe {
            let header = pointer
                .cast::<u8>()
                .sub(size_of::<Header>())
                .cast::<Header>();
            let layout =
                Layout::from_size_align_unchecked((*header).layout_size, align_of::<Header>());
            dealloc(header.cast(), layout);
        }
    }
    replacement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_scenario_budget_overflow_zero_and_realloc_preserve_payload() {
        let owner = CustomAllocator::with_limit(32);
        let raw = owner.as_raw();
        // SAFETY: all pointers are operated on through the originating live table.
        unsafe {
            let data = raw.mem_user_data;
            let pointer = raw.calloc.unwrap()(4, 4, data).cast::<u8>();
            assert!(!pointer.is_null());
            assert_eq!(std::slice::from_raw_parts(pointer, 16), &[0; 16]);
            pointer.write(71);
            assert!(raw.realloc.unwrap()(pointer.cast(), 33, data).is_null());
            assert_eq!(*pointer, 71);
            let grown = raw.realloc.unwrap()(pointer.cast(), 32, data).cast::<u8>();
            assert!(!grown.is_null());
            assert_eq!(*grown, 71);
            let shrunk = raw.realloc.unwrap()(grown.cast(), 8, data);
            assert!(!shrunk.is_null());
            assert!(raw.calloc.unwrap()(usize::MAX, 2, data).is_null());
            assert!(raw.malloc.unwrap()(usize::MAX, data).is_null());
            assert!(raw.realloc.unwrap()(shrunk, 0, data).is_null());
            raw.free.unwrap()(ptr::null_mut(), data);
            let zero = raw.malloc.unwrap()(0, data);
            assert!(!zero.is_null());
            let nonzero = raw.realloc.unwrap()(zero, 1, data);
            assert!(!nonzero.is_null());
            raw.free.unwrap()(nonzero, data);
            let null_alloc = raw.realloc.unwrap()(ptr::null_mut(), 1, data);
            raw.free.unwrap()(null_alloc, data);
        }
        assert_eq!(
            owner.stats(),
            Some(AllocationStats {
                live_bytes: 0,
                peak_bytes: 32,
                live_blocks: 0,
                failed_allocations: 3,
            })
        );
    }

    #[test]
    fn unit_scenario_lowered_budget_preserves_live_payload() {
        let owner = CustomAllocator::with_limit(32);
        let raw = owner.as_raw();
        // SAFETY: live pointers always use their originating allocator, and all
        // successful allocations are freed before dropping its owner.
        unsafe {
            let data = raw.mem_user_data;
            let pointer = raw.malloc.unwrap()(16, data).cast::<u8>();
            assert!(!pointer.is_null());
            pointer.write(71);
            assert!(owner.set_limit(8));
            assert!(raw.realloc.unwrap()(pointer.cast(), 12, data).is_null());
            assert_eq!(*pointer, 71);
            assert_eq!(owner.stats().unwrap().live_bytes, 16);
            let shrunk = raw.realloc.unwrap()(pointer.cast(), 8, data).cast::<u8>();
            assert!(!shrunk.is_null());
            assert_eq!(*shrunk, 71);
            raw.free.unwrap()(shrunk.cast(), data);
            let zero = raw.calloc.unwrap()(0, usize::MAX, data);
            assert!(!zero.is_null());
            raw.free.unwrap()(zero, data);
        }
        assert_eq!(owner.stats().unwrap().live_bytes, 0);
        assert_eq!(owner.stats().unwrap().live_blocks, 0);
        assert_eq!(owner.stats().unwrap().failed_allocations, 1);
    }

    #[test]
    fn integration_lifecycle_native_hpack_uses_stable_userdata_and_releases_all_blocks() {
        let owner = CustomAllocator::new();
        let mut deflater = ptr::null_mut();
        let mut inflater = ptr::null_mut();
        let table = ptr::from_ref(owner.as_raw()).cast_mut();
        // SAFETY: HPACK retains but never mutates the heap table. Moving its owner
        // preserves the table and userdata until both native objects are deleted.
        unsafe {
            assert_eq!(
                nghttp2_sys::nghttp2_hd_deflate_new2(&mut deflater, 4096, table),
                0
            );
            assert_eq!(
                nghttp2_sys::nghttp2_hd_inflate_new2(&mut inflater, table),
                0
            );
            let owner = Box::new(owner);
            assert_eq!(table.cast_const(), ptr::from_ref(owner.as_raw()));
            assert!(owner.stats().unwrap().live_bytes > 0);
            assert!(owner.stats().unwrap().live_blocks >= 2);
            nghttp2_sys::nghttp2_hd_deflate_del(deflater);
            nghttp2_sys::nghttp2_hd_inflate_del(inflater);
            assert_eq!(owner.stats().unwrap().live_bytes, 0);
            assert_eq!(owner.stats().unwrap().live_blocks, 0);
        }
    }

    #[test]
    fn integration_scenario_native_failure_and_budget_change() {
        let owner = CustomAllocator::with_limit(0);
        let mut inflater = ptr::null_mut();
        let table = ptr::from_ref(owner.as_raw()).cast_mut();
        // SAFETY: live allocator and valid output slot; allocation is rejected deterministically.
        unsafe {
            assert_ne!(
                nghttp2_sys::nghttp2_hd_inflate_new2(&mut inflater, table),
                0
            );
        }
        assert!(owner.stats().unwrap().failed_allocations > 0);
        assert_eq!(owner.stats().unwrap().live_blocks, 0);
        assert!(owner.set_limit(usize::MAX));
        // SAFETY: owner outlives the successfully created inflater and its deletion.
        unsafe {
            assert_eq!(
                nghttp2_sys::nghttp2_hd_inflate_new2(&mut inflater, table),
                0
            );
            nghttp2_sys::nghttp2_hd_inflate_del(inflater);
        }
        assert_eq!(owner.stats().unwrap().live_blocks, 0);
    }

    #[test]
    fn integration_lifecycle_external_table_preserves_callback_customization() {
        let tracked = CustomAllocator::with_limit(usize::MAX);
        // SAFETY: tracked owns the callbacks' userdata and outlives external and
        // the native inflater. Its callbacks implement the required C contracts.
        let external = unsafe { CustomAllocator::from_raw(*tracked.as_raw()) };
        assert_eq!(external.stats(), None);
        assert!(!external.set_limit(0));
        let table = ptr::from_ref(external.as_raw()).cast_mut();
        let mut inflater = ptr::null_mut();
        // SAFETY: both owners outlive the context. HPACK retains the heap table
        // pointer without mutating the table through its C non-const signature.
        unsafe {
            assert_eq!(
                nghttp2_sys::nghttp2_hd_inflate_new2(&mut inflater, table),
                0
            );
            assert!(tracked.stats().unwrap().live_bytes > 0);
            nghttp2_sys::nghttp2_hd_inflate_del(inflater);
        }
        assert_eq!(tracked.stats().unwrap().live_blocks, 0);
    }

    #[test]
    fn integration_scenario_partial_native_construction_failure_releases_allocations() {
        for limit in [0, 64, 512, 4096, 16384] {
            let owner = CustomAllocator::with_limit(limit);
            let table = ptr::from_ref(owner.as_raw()).cast_mut();
            let mut deflater = ptr::null_mut();
            // SAFETY: native construction receives valid slots and an allocator
            // that stays live through failure cleanup or explicit destruction.
            unsafe {
                let result = nghttp2_sys::nghttp2_hd_deflate_new2(&mut deflater, 4096, table);
                if result == 0 {
                    nghttp2_sys::nghttp2_hd_deflate_del(deflater);
                }
            }
            let stats = owner.stats().unwrap();
            assert_eq!(stats.live_blocks, 0, "deflater budget {limit}");
            assert_eq!(stats.live_bytes, 0, "deflater budget {limit}");
            let mut inflater = ptr::null_mut();
            // SAFETY: the same live allocator is reused and each successful
            // native context is destroyed exactly once.
            unsafe {
                let result = nghttp2_sys::nghttp2_hd_inflate_new2(&mut inflater, table);
                if result == 0 {
                    nghttp2_sys::nghttp2_hd_inflate_del(inflater);
                }
            }
            let stats = owner.stats().unwrap();
            assert_eq!(stats.live_blocks, 0, "inflater budget {limit}");
            assert_eq!(stats.live_bytes, 0, "inflater budget {limit}");
        }
    }
    #[test]
    fn integration_lifecycle_state_survives_owner_moves_and_observation() {
        let allocator = CustomAllocator::with_limit(1024 * 1024);
        let table = ptr::from_ref(allocator.as_raw()).cast_mut();
        let userdata = allocator.as_raw().mem_user_data;
        let mut inflater = ptr::null_mut();
        // SAFETY: the native inflater borrows the retained heap-stable table and state.
        unsafe {
            assert_eq!(
                nghttp2_sys::nghttp2_hd_inflate_new2(&mut inflater, table),
                0
            );
        }
        let mut owners = vec![allocator];
        let moved = owners.pop().unwrap();
        assert_eq!(moved.as_raw().mem_user_data, userdata);
        assert!(moved.stats().unwrap().live_blocks > 0);
        assert!(moved.set_limit(2 * 1024 * 1024));
        // SAFETY: the same live owner/table is used after moves and field observations.
        unsafe {
            let raw = moved.as_raw();
            let bytes = raw.malloc.unwrap()(16, userdata);
            assert!(!bytes.is_null());
            raw.free.unwrap()(bytes, userdata);
            nghttp2_sys::nghttp2_hd_inflate_del(inflater);
        }
        assert_eq!(moved.stats().unwrap().live_blocks, 0);
        assert_eq!(moved.stats().unwrap().live_bytes, 0);
    }
}
