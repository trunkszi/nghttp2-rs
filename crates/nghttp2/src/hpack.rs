//! Standalone HPACK compression, streaming decompression, and table inspection.
//!
//! Contexts are single-threaded and retain their dynamic tables across header
//! blocks. Emitted headers are owned so later decoding cannot invalidate them.
use std::{fmt, marker::PhantomData, ptr::NonNull, rc::Rc};

use nghttp2_sys as sys;
pub use nghttp2_sys::{nghttp2_hd_deflater, nghttp2_hd_inflater};

use crate::{
    error::RawErrorCode,
    mem::RawAllocator,
    nv::{NGHTTP2_NV_FLAG_NO_INDEX, NO_COPY_FLAGS, RawHeaderField},
};
/// Opaque native HPACK deflater.
pub type RawHdDeflater = nghttp2_hd_deflater;
/// Opaque native HPACK inflater.
pub type RawHdInflater = nghttp2_hd_inflater;
/// Default maximum size of the HPACK dynamic table.
pub const DEFAULT_DYNAMIC_TABLE_SIZE: u32 = sys::NGHTTP2_DEFAULT_HEADER_TABLE_SIZE;

/// Native HPACK failure code, preserved without losing its numeric value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HpackError(pub RawErrorCode);
impl fmt::Display for HpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HPACK error {}", self.0)
    }
}
impl std::error::Error for HpackError {}
/// Result of a standalone HPACK operation.
pub type HpackResult<T> = Result<T, HpackError>;

/// Borrowed binary header bytes, with optional never-index encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HpackHeader<'a> {
    pub name: &'a [u8],
    pub value: &'a [u8],
    pub no_index: bool,
}
impl<'a> HpackHeader<'a> {
    /// Borrows name and value for the duration of an encoding call.
    #[must_use]
    pub const fn new(name: &'a [u8], value: &'a [u8]) -> Self {
        Self {
            name,
            value,
            no_index: false,
        }
    }
    /// Controls the HPACK never-index flag for sensitive header values.
    #[must_use]
    pub const fn with_no_index(mut self, no_index: bool) -> Self {
        self.no_index = no_index;
        self
    }
}
/// An emitted header independent of both the input chunk and native context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InflatedHeader {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
    pub no_index: bool,
}
/// One native decoder step; consuming all input does not imply `finished`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InflateStep {
    pub consumed: usize,
    pub header: Option<InflatedHeader>,
    pub finished: bool,
}

fn check_table_size(size: usize) -> HpackResult<()> {
    u32::try_from(size)
        .map(|_| ())
        .map_err(|_| HpackError(sys::NGHTTP2_ERR_INVALID_ARGUMENT))
}
fn check(code: RawErrorCode) -> HpackResult<()> {
    if code < 0 {
        Err(HpackError(code))
    } else {
        Ok(())
    }
}
/// # Safety
/// The descriptor and nonempty buffers must remain valid for the returned lifetime.
unsafe fn header_view<'a>(raw: *const RawHeaderField) -> Option<HpackHeader<'a>> {
    // SAFETY: the caller supplies a live native descriptor or a null table miss.
    let raw = unsafe { raw.as_ref() }?;
    // SAFETY: nghttp2 owns these buffers; zero-length pointers may be null.
    let name = if raw.namelen == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees the nonempty name buffer remains live.
        unsafe { std::slice::from_raw_parts(raw.name, raw.namelen) }
    };
    // SAFETY: the same native ownership and length contract applies to values.
    let value = if raw.valuelen == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees the nonempty value buffer remains live.
        unsafe { std::slice::from_raw_parts(raw.value, raw.valuelen) }
    };
    Some(HpackHeader {
        name,
        value,
        no_index: raw.flags & NGHTTP2_NV_FLAG_NO_INDEX as u8 != 0,
    })
}
macro_rules! table_accessors {
    ($count:ident, $entry:ident, $size:ident, $max:ident) => {
        /// Includes all 61 static entries and current dynamic entries.
        #[must_use]
        pub fn num_table_entries(&self) -> usize {
            // SAFETY: this read-only getter receives the live owned context.
            unsafe { sys::$count(self.ptr.as_ptr()) }
        }
        /// Borrows a 1-based entry; zero and out-of-range indices return None.
        #[must_use]
        pub fn table_entry(&self, index: usize) -> Option<HpackHeader<'_>> {
            // SAFETY: &self prevents table mutation during the returned borrow.
            unsafe { header_view(sys::$entry(self.ptr.as_ptr(), index)) }
        }
        /// Used dynamic bytes, including the 32-byte per-entry overhead.
        #[must_use]
        pub fn dynamic_table_size(&self) -> usize {
            // SAFETY: this read-only getter receives the live owned context.
            unsafe { sys::$size(self.ptr.as_ptr()) }
        }
        /// Current maximum dynamic table size in bytes.
        #[must_use]
        pub fn max_dynamic_table_size(&self) -> usize {
            // SAFETY: this read-only getter receives the live owned context.
            unsafe { sys::$max(self.ptr.as_ptr()) }
        }
    };
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum InflateState {
    Ready,
    Active,
    Finished,
    Failed,
}
/// Stateful streaming HPACK decoder. A decode error makes the context unusable.
pub struct HpInflater {
    ptr: NonNull<RawHdInflater>,
    state: InflateState,
    _single_thread: PhantomData<Rc<()>>,
}
impl HpInflater {
    /// Creates an independent decoder with the default dynamic table limit.
    /// # Errors
    /// Returns the native allocation error if construction fails.
    pub fn new() -> HpackResult<Self> {
        // SAFETY: a null allocator selects the library's default allocator.
        unsafe { Self::create(std::ptr::null_mut()) }
    }
    /// Creates a decoder with a custom allocator table.
    /// # Safety
    /// All allocator callbacks must be present, mutually compatible C allocators,
    /// and must not unwind. The table must stay at the same address, and its
    /// user-data/resources must remain valid through decoder destruction.
    /// # Errors
    /// Returns the native allocation error if construction fails.
    pub unsafe fn new_with_mem(mem: &RawAllocator) -> HpackResult<Self> {
        // SAFETY: the caller guarantees the allocator's callbacks and lifetime.
        unsafe { Self::create(std::ptr::from_ref(mem).cast_mut()) }
    }
    unsafe fn create(mem: *mut RawAllocator) -> HpackResult<Self> {
        let mut ptr = std::ptr::null_mut();
        // SAFETY: ptr is a writable output and mem satisfies the constructor contract.
        check(unsafe { sys::nghttp2_hd_inflate_new2(&mut ptr, mem) })?;
        Ok(Self {
            ptr: NonNull::new(ptr).ok_or(HpackError(sys::NGHTTP2_ERR_NOMEM))?,
            state: InflateState::Ready,
            _single_thread: PhantomData,
        })
    }
    /// Updates the SETTINGS_HEADER_TABLE_SIZE limit between completed blocks.
    /// # Errors
    /// Returns INVALID_ARGUMENT above u32::MAX, INVALID_STATE during a block, or NOMEM.
    pub fn change_table_size(&mut self, size: usize) -> HpackResult<()> {
        check_table_size(size)?;
        if self.state != InflateState::Ready {
            return Err(HpackError(sys::NGHTTP2_ERR_INVALID_STATE));
        }
        // SAFETY: the owned decoder is live and exclusively borrowed.
        check(unsafe { sys::nghttp2_hd_inflate_change_table_size(self.ptr.as_ptr(), size) })
    }
    /// Processes part of a block, copying any emitted header before returning.
    /// Pass `final_chunk = true` on the last chunk and on subsequent calls with
    /// its remaining suffix (including an empty suffix). Continue until finished,
    /// then call `end_headers`. A header can be emitted without consuming input.
    /// # Errors
    /// Returns the native decoder error, or INVALID_STATE after completion until
    /// `end_headers` is called. A native decoder error is permanent.
    pub fn inflate_step(&mut self, input: &[u8], final_chunk: bool) -> HpackResult<InflateStep> {
        if self.state == InflateState::Failed {
            return Err(HpackError(sys::NGHTTP2_ERR_HEADER_COMP));
        }
        if self.state == InflateState::Finished {
            return Err(HpackError(sys::NGHTTP2_ERR_INVALID_STATE));
        }
        self.state = InflateState::Active;
        // SAFETY: all-zero fields form a valid output-only descriptor.
        let mut raw: RawHeaderField = unsafe { std::mem::zeroed() };
        let mut flags = 0;
        // SAFETY: the decoder is exclusively borrowed and input/output pointers are live.
        let result = unsafe {
            sys::nghttp2_hd_inflate_hd3(
                self.ptr.as_ptr(),
                &mut raw,
                &mut flags,
                input.as_ptr(),
                input.len(),
                i32::from(final_chunk),
            )
        };
        if result < 0 {
            self.state = InflateState::Failed;
            return Err(HpackError(
                i32::try_from(result).unwrap_or(sys::NGHTTP2_ERR_HEADER_COMP),
            ));
        }
        let header = if flags & sys::NGHTTP2_HD_INFLATE_EMIT as i32 != 0 {
            // SAFETY: EMIT initializes raw; copy the view before any further decoder mutation.
            let view =
                unsafe { header_view(&raw) }.ok_or(HpackError(sys::NGHTTP2_ERR_HEADER_COMP))?;
            Some(InflatedHeader {
                name: view.name.to_vec(),
                value: view.value.to_vec(),
                no_index: view.no_index,
            })
        } else {
            None
        };
        let finished = flags & sys::NGHTTP2_HD_INFLATE_FINAL as i32 != 0;
        if finished {
            self.state = InflateState::Finished;
        }
        Ok(InflateStep {
            consumed: result as usize,
            header,
            finished,
        })
    }
    /// Decodes one complete block and prepares the table for the next block.
    /// # Errors
    /// Returns INVALID_STATE if a streaming block is active, or the native error
    /// for truncated, malformed, oversized, or unallocatable header data.
    pub fn inflate_block(&mut self, input: &[u8]) -> HpackResult<Vec<InflatedHeader>> {
        if self.state == InflateState::Failed {
            return Err(HpackError(sys::NGHTTP2_ERR_HEADER_COMP));
        }
        if self.state != InflateState::Ready {
            return Err(HpackError(sys::NGHTTP2_ERR_INVALID_STATE));
        }
        let mut headers = Vec::new();
        let mut offset = 0;
        loop {
            let step = self.inflate_step(&input[offset..], true)?;
            offset += step.consumed;
            let emitted = step.header.is_some();
            if let Some(header) = step.header {
                headers.push(header);
            }
            if step.finished {
                self.end_headers()?;
                return Ok(headers);
            }
            if step.consumed == 0 && !emitted {
                self.state = InflateState::Failed;
                return Err(HpackError(sys::NGHTTP2_ERR_HEADER_COMP));
            }
        }
    }
    /// Releases per-block resources after a step reports `finished`.
    /// # Errors
    /// Returns INVALID_STATE for incomplete, idle, or failed blocks.
    pub fn end_headers(&mut self) -> HpackResult<()> {
        if self.state != InflateState::Finished {
            return Err(HpackError(sys::NGHTTP2_ERR_INVALID_STATE));
        }
        // SAFETY: FINAL was observed, and the native context is live and exclusively borrowed.
        check(unsafe { sys::nghttp2_hd_inflate_end_headers(self.ptr.as_ptr()) })?;
        self.state = InflateState::Ready;
        Ok(())
    }
    table_accessors!(
        nghttp2_hd_inflate_get_num_table_entries,
        nghttp2_hd_inflate_get_table_entry,
        nghttp2_hd_inflate_get_dynamic_table_size,
        nghttp2_hd_inflate_get_max_dynamic_table_size
    );
}
impl Drop for HpInflater {
    fn drop(&mut self) {
        // SAFETY: this unique context was created successfully and is destroyed exactly once.
        unsafe { sys::nghttp2_hd_inflate_del(self.ptr.as_ptr()) }
    }
}
/// Stateful HPACK encoder; native encoding failures require a new encoder.
pub struct HpDeflater {
    ptr: NonNull<RawHdDeflater>,
    _single_thread: PhantomData<Rc<()>>,
}
impl HpDeflater {
    /// Creates an encoder with a hard upper bound on dynamic-table memory.
    /// # Errors
    /// Returns INVALID_ARGUMENT above u32::MAX, or the native allocation error.
    pub fn new(max_dynamic_table_size: usize) -> HpackResult<Self> {
        // SAFETY: null selects the default allocator.
        unsafe { Self::create(max_dynamic_table_size, std::ptr::null_mut()) }
    }
    /// Creates an encoder with a custom allocator.
    /// # Safety
    /// Callbacks must be present, compatible C allocators and must not unwind.
    /// The table must stay at the same address and its user-data/resources must
    /// remain valid through encoder destruction.
    /// # Errors
    /// Returns INVALID_ARGUMENT above u32::MAX, or the native allocation error.
    pub unsafe fn new_with_mem(
        max_dynamic_table_size: usize,
        mem: &RawAllocator,
    ) -> HpackResult<Self> {
        // SAFETY: allocator validity and lifetime are guaranteed by the caller.
        unsafe { Self::create(max_dynamic_table_size, std::ptr::from_ref(mem).cast_mut()) }
    }
    unsafe fn create(size: usize, mem: *mut RawAllocator) -> HpackResult<Self> {
        check_table_size(size)?;
        let mut ptr = std::ptr::null_mut();
        // SAFETY: ptr is writable; mem satisfies the constructor's allocator contract.
        check(unsafe { sys::nghttp2_hd_deflate_new2(&mut ptr, size, mem) })?;
        Ok(Self {
            ptr: NonNull::new(ptr).ok_or(HpackError(sys::NGHTTP2_ERR_NOMEM))?,
            _single_thread: PhantomData,
        })
    }
    /// Applies the peer's table limit, capped by the constructor's memory limit.
    /// # Errors
    /// Returns INVALID_ARGUMENT above u32::MAX, or the native allocation error.
    pub fn change_table_size(&mut self, size: usize) -> HpackResult<()> {
        check_table_size(size)?;
        // SAFETY: this exclusively borrowed context remains live.
        check(unsafe { sys::nghttp2_hd_deflate_change_table_size(self.ptr.as_ptr(), size) })
    }
    /// Returns the native upper bound on encoded bytes.
    /// # Errors
    /// Returns BUFFER_ERROR on arithmetic overflow or NOMEM on descriptor allocation failure.
    pub fn deflate_bound(&self, headers: &[HpackHeader<'_>]) -> HpackResult<usize> {
        checked_bound(headers)?;
        let raw = raw_headers(headers)?;
        // SAFETY: the read-only bound query receives valid borrowed descriptors.
        Ok(unsafe { sys::nghttp2_hd_deflate_bound(self.ptr.as_ptr(), raw.as_ptr(), raw.len()) })
    }
    /// Encodes a complete block into caller-provided storage.
    /// Size storage using `deflate_bound`. A native encoding failure (including
    /// insufficient output space) permanently poisons the native encoder.
    /// # Errors
    /// Returns the native compression/allocation/buffer error or BUFFER_ERROR on overflow.
    pub fn deflate_into(
        &mut self,
        headers: &[HpackHeader<'_>],
        output: &mut [u8],
    ) -> HpackResult<usize> {
        checked_bound(headers)?;
        let raw = raw_headers(headers)?;
        // SAFETY: descriptors and output remain live for the synchronous encoding call.
        let result = unsafe {
            sys::nghttp2_hd_deflate_hd2(
                self.ptr.as_ptr(),
                output.as_mut_ptr(),
                output.len(),
                raw.as_ptr(),
                raw.len(),
            )
        };
        if result < 0 {
            Err(HpackError(
                i32::try_from(result).unwrap_or(sys::NGHTTP2_ERR_HEADER_COMP),
            ))
        } else {
            Ok(result as usize)
        }
    }
    /// Encodes one complete block into an owned buffer.
    /// # Errors
    /// Returns allocation, bound-overflow, or native compression errors.
    pub fn deflate_vec(&mut self, headers: &[HpackHeader<'_>]) -> HpackResult<Vec<u8>> {
        let capacity = self.deflate_bound(headers)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| HpackError(sys::NGHTTP2_ERR_NOMEM))?;
        output.resize(capacity, 0);
        let written = self.deflate_into(headers, &mut output)?;
        output.truncate(written);
        Ok(output)
    }
    table_accessors!(
        nghttp2_hd_deflate_get_num_table_entries,
        nghttp2_hd_deflate_get_table_entry,
        nghttp2_hd_deflate_get_dynamic_table_size,
        nghttp2_hd_deflate_get_max_dynamic_table_size
    );
}
impl Drop for HpDeflater {
    fn drop(&mut self) {
        // SAFETY: this is the unique owner, and its allocator remains valid.
        unsafe { sys::nghttp2_hd_deflate_del(self.ptr.as_ptr()) }
    }
}
fn checked_bound(headers: &[HpackHeader<'_>]) -> HpackResult<usize> {
    let bound = headers
        .len()
        .checked_mul(12)
        .and_then(|n| n.checked_add(12));
    headers.iter().try_fold(
        bound.ok_or(HpackError(sys::NGHTTP2_ERR_BUFFER_ERROR))?,
        |size, h| {
            size.checked_add(h.name.len())
                .and_then(|n| n.checked_add(h.value.len()))
                .ok_or(HpackError(sys::NGHTTP2_ERR_BUFFER_ERROR))
        },
    )
}
fn raw_headers(headers: &[HpackHeader<'_>]) -> HpackResult<Vec<RawHeaderField>> {
    let mut raw = Vec::new();
    raw.try_reserve_exact(headers.len())
        .map_err(|_| HpackError(sys::NGHTTP2_ERR_NOMEM))?;
    raw.extend(headers.iter().map(|h| RawHeaderField {
        name: h.name.as_ptr().cast_mut(),
        value: h.value.as_ptr().cast_mut(),
        namelen: h.name.len(),
        valuelen: h.value.len(),
        flags: (NO_COPY_FLAGS
            | if h.no_index {
                NGHTTP2_NV_FLAG_NO_INDEX
            } else {
                0
            }) as u8,
    }));
    Ok(raw)
}
#[cfg(test)]
mod tests {
    use super::{HpackHeader as Header, *};
    const ARGUMENT: HpackError = HpackError(sys::NGHTTP2_ERR_INVALID_ARGUMENT);
    const INVALID: HpackError = HpackError(sys::NGHTTP2_ERR_INVALID_STATE);
    const COMPRESSION: HpackError = HpackError(sys::NGHTTP2_ERR_HEADER_COMP);
    fn inflate_chunks<'a>(
        dec: &mut HpInflater,
        chunks: impl IntoIterator<Item = (&'a [u8], bool)>,
    ) -> Vec<InflatedHeader> {
        let mut output = Vec::new();
        for (mut rest, last) in chunks {
            loop {
                let step = dec.inflate_step(rest, last).unwrap();
                rest = &rest[step.consumed..];
                let emitted = step.header.is_some();
                output.extend(step.header);
                if step.finished {
                    assert!(last && rest.is_empty());
                    dec.end_headers().unwrap();
                    break;
                }
                if rest.is_empty() && !emitted {
                    break;
                }
                assert!(step.consumed > 0 || emitted);
            }
        }
        assert!(matches!(dec.state, InflateState::Ready));
        output
    }
    fn roundtrip(chunk: usize) {
        let headers = [
            Header::new(b":method", b"POST"),
            Header::new(b":path", b"/pkg.Service/Call"),
            Header::new(b"content-type", b"application/grpc"),
            Header::new(b"te", b"trailers"),
            Header::new(b"opaque-bin", b"\0\xff\x80ab\0"),
            Header::new(b"x-repeat", b"one"),
            Header::new(b"x-repeat", b"two"),
            Header::new(b"x-empty", b""),
            Header::new(b"authorization", b"secret").with_no_index(true),
        ];
        let mut enc = HpDeflater::new(4096).unwrap();
        let mut dec = HpInflater::new().unwrap();
        for _ in 0..3 {
            let bound = enc.deflate_bound(&headers).unwrap();
            let mut block = vec![0; bound];
            let len = enc.deflate_into(&headers, &mut block).unwrap();
            block.truncate(len);
            let decoded = if chunk == 0 {
                dec.inflate_block(&block).unwrap()
            } else {
                let chunks = block.chunks(chunk);
                let count = chunks.len();
                let chunks = chunks.enumerate().map(|(n, p)| (p, n + 1 == count));
                inflate_chunks(&mut dec, chunks)
            };
            assert_eq!(decoded.len(), headers.len());
            for (got, expected) in decoded.iter().zip(headers) {
                assert_eq!(got.name, expected.name);
                assert_eq!(got.value, expected.value);
                assert_eq!(got.no_index, expected.no_index);
            }
            for index in 62..=enc.num_table_entries() {
                assert_ne!(enc.table_entry(index).unwrap().name, b"authorization");
                assert_ne!(dec.table_entry(index).unwrap().name, b"authorization");
            }
            assert_eq!(enc.dynamic_table_size(), dec.dynamic_table_size());
            assert_eq!(enc.num_table_entries(), dec.num_table_entries());
        }
    }
    #[test]
    fn binary_grpc_roundtrip_and_chunking() {
        for chunk in [0, 1, 2, 5, 17] {
            roundtrip(chunk);
        }
    }
    #[test]
    fn final_drain_and_block_lifecycle() {
        let mut dec = HpInflater::new().unwrap();
        for _ in 0..3 {
            assert!(dec.inflate_block(&[]).unwrap().is_empty());
        }
        assert_eq!(dec.end_headers(), Err(INVALID));
        let step = dec.inflate_step(&[0x82], true).unwrap();
        assert_eq!(step.consumed, 1);
        assert!(!step.finished);
        let owned = step.header.unwrap();
        assert_eq!(owned.value, b"GET");
        assert_eq!(dec.change_table_size(0), Err(INVALID));
        assert!(dec.end_headers().is_err());
        assert!(dec.inflate_block(&[0x83]).is_err());
        let step = dec.inflate_step(&[], true).unwrap();
        assert_eq!(step.consumed, 0);
        assert!(step.finished && step.header.is_none());
        assert!(dec.inflate_step(&[], true).is_err());
        dec.end_headers().unwrap();
        assert_eq!(dec.inflate_block(&[0x83]).unwrap()[0].value, b"POST");
        drop(dec);
        assert_eq!(owned.name, b":method");
        assert_eq!(owned.value, b"GET");
    }
    #[test]
    fn rfc_huffman_vector_and_every_split() {
        let encoded = [
            0x82, 0x86, 0x84, 0x41, 0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab,
            0x90, 0xf4, 0xff,
        ];
        for split in 0..=encoded.len() {
            let mut dec = HpInflater::new().unwrap();
            let output = inflate_chunks(
                &mut dec,
                [(&encoded[..split], false), (&encoded[split..], true)],
            );
            let fields: Vec<_> = output.iter().map(|h| (&h.name[..], &h.value[..])).collect();
            assert_eq!(
                fields,
                [
                    (b":method".as_slice(), b"GET".as_slice()),
                    (b":scheme", b"http"),
                    (b":path", b"/"),
                    (b":authority", b"www.example.com")
                ]
            );
        }
    }
    #[test]
    fn tables_and_resize_preserve_peer_state() {
        let mut enc = HpDeflater::new(4096).unwrap();
        let mut dec = HpInflater::new().unwrap();
        if let Some(oversize) = (u32::MAX as usize).checked_add(1) {
            assert_eq!(HpDeflater::new(oversize).err(), Some(ARGUMENT));
            let invalid = Err(ARGUMENT);
            assert_eq!(enc.change_table_size(oversize), invalid);
            assert_eq!(dec.change_table_size(oversize), invalid);
            assert_eq!(
                (enc.max_dynamic_table_size(), dec.max_dynamic_table_size()),
                (4096, 4096)
            );
        }
        assert_eq!(enc.num_table_entries(), 61);
        for index in 1..=61 {
            assert_eq!(enc.table_entry(index), dec.table_entry(index));
        }
        assert!(enc.table_entry(0).is_none() && dec.table_entry(0).is_none());
        assert!(enc.table_entry(62).is_none() && dec.table_entry(62).is_none());
        assert_eq!(enc.table_entry(2).unwrap().value, b"GET");
        for (name, value) in [(b"x".as_slice(), b"a".as_slice()), (b"y", b"b")] {
            let block = enc.deflate_vec(&[Header::new(name, value)]).unwrap();
            assert_eq!(dec.inflate_block(&block).unwrap()[0].name, name);
            assert_eq!(enc.table_entry(62), dec.table_entry(62));
            assert_eq!(dec.table_entry(62).unwrap().value, value);
        }
        assert_eq!(dec.num_table_entries(), 63);
        assert_eq!(dec.dynamic_table_size(), 68);
        enc.change_table_size(34).unwrap();
        dec.change_table_size(34).unwrap();
        let block = enc.deflate_vec(&[Header::new(b"y", b"b")]).unwrap();
        dec.inflate_block(&block).unwrap();
        assert_eq!(enc.max_dynamic_table_size(), 34);
        assert_eq!(dec.max_dynamic_table_size(), 34);
        assert_eq!(dec.num_table_entries(), 62);
        assert_eq!(dec.dynamic_table_size(), 34);
        enc.change_table_size(0).unwrap();
        dec.change_table_size(0).unwrap();
        let method = [Header::new(b":method", b"GET")];
        let block = enc.deflate_vec(&method).unwrap();
        assert_eq!(dec.inflate_block(&block).unwrap()[0].value, b"GET");
        assert_eq!(dec.num_table_entries(), 61);
        assert_eq!(enc.dynamic_table_size(), 0);
        let mut limited = HpDeflater::new(64).unwrap();
        limited.change_table_size(4096).unwrap();
        assert_eq!(limited.max_dynamic_table_size(), 64);
        let mut largest = HpDeflater::new(u32::MAX as usize).unwrap();
        largest.change_table_size(u32::MAX as usize).unwrap();
        let bound = largest.deflate_bound(&[]).unwrap();
        let mut output = vec![0; bound];
        assert!(largest.deflate_into(&[], &mut output).unwrap() <= bound);
        let mut enc = HpDeflater::new(0).unwrap();
        let mut dec = HpInflater::new().unwrap();
        dec.change_table_size(0).unwrap();
        let headers = [Header::new(b"x", b"value")];
        for _ in 0..2 {
            let block = enc.deflate_vec(&headers).unwrap();
            assert_eq!(dec.inflate_block(&block).unwrap()[0].value, b"value");
            assert_eq!((enc.num_table_entries(), dec.num_table_entries()), (61, 61));
            assert_eq!((enc.dynamic_table_size(), dec.dynamic_table_size()), (0, 0));
        }
    }
    #[test]
    fn native_errors_are_permanent() {
        for block in [b"\x80".as_slice(), b"\xbe", b"\x00", b"\xff", b"\x82\x20"] {
            let mut dec = HpInflater::new().unwrap();
            for input in [block, &[0x82]] {
                assert_eq!(dec.inflate_block(input), Err(COMPRESSION));
            }
        }
        let mut enc = HpDeflater::new(4096).unwrap();
        let headers = [Header::new(b"custom", b"a value")];
        assert_eq!(
            enc.deflate_into(&headers, &mut []),
            Err(HpackError(sys::NGHTTP2_ERR_INSUFF_BUFSIZE))
        );
        assert_eq!(enc.deflate_vec(&headers), Err(COMPRESSION));
    }
    #[test]
    fn resizing_rejects_missing_or_oversized_peer_update() {
        for block in [b"\x82".as_slice(), b"\x3f\x22\x82"] {
            let mut dec = HpInflater::new().unwrap();
            dec.change_table_size(64).unwrap();
            assert_eq!(dec.inflate_block(block).unwrap_err().0, COMPRESSION.0);
        }
        let mut dec = HpInflater::new().unwrap();
        dec.change_table_size(64).unwrap();
        assert_eq!(dec.inflate_block(b"\x3f\x21\x82").unwrap()[0].value, b"GET");
        let step = dec.inflate_step(b"\x40", false).unwrap();
        assert_eq!((step.consumed, step.finished), (1, false));
        assert!(step.header.is_none());
        assert_eq!(dec.change_table_size(0).unwrap_err().0, INVALID.0);
        assert_eq!(dec.end_headers().unwrap_err().0, INVALID.0);
        assert_eq!(dec.inflate_block(&[]).unwrap_err().0, INVALID.0);
        let headers = inflate_chunks(&mut dec, [(b"\x01x\x01a".as_slice(), true)]);
        assert_eq!(headers[0].value, b"a");
        assert_eq!(dec.table_entry(62).unwrap().name, b"x");
    }
    use std::{cell::Cell, ffi::c_void};
    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
        fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
    }
    struct Counts {
        live: Cell<usize>,
        calls: Cell<usize>,
        limit: usize,
    }
    impl Counts {
        fn permit(&self) -> bool {
            let n = self.calls.replace(self.calls.get() + 1);
            n < self.limit
        }
    }
    unsafe extern "C" fn tracked_malloc(size: usize, data: *mut c_void) -> *mut c_void {
        // SAFETY: the constructor retains this test's live Counts pointer.
        let counts = unsafe { &*data.cast::<Counts>() };
        if !counts.permit() {
            return std::ptr::null_mut();
        }
        // SAFETY: C malloc accepts any requested size.
        let ptr = unsafe { malloc(size) };
        if !ptr.is_null() {
            counts.live.set(counts.live.get() + 1);
        }
        ptr
    }
    unsafe extern "C" fn tracked_free(ptr: *mut c_void, data: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        // SAFETY: this allocator's nonnull pointers came from C malloc/realloc.
        unsafe {
            free(ptr);
        }
        // SAFETY: Counts outlives every custom allocator callback.
        let counts = unsafe { &*data.cast::<Counts>() };
        counts.live.set(counts.live.get() - 1);
    }
    unsafe extern "C" fn tracked_calloc(n: usize, size: usize, data: *mut c_void) -> *mut c_void {
        let Some(bytes) = n.checked_mul(size) else {
            return std::ptr::null_mut();
        };
        // SAFETY: this callback forwards the same valid allocator state.
        let ptr = unsafe { tracked_malloc(bytes, data) };
        if !ptr.is_null() {
            // SAFETY: the successful allocation has at least bytes writable bytes.
            unsafe {
                ptr.cast::<u8>().write_bytes(0, bytes);
            }
        }
        ptr
    }
    unsafe extern "C" fn tracked_realloc(
        ptr: *mut c_void,
        size: usize,
        data: *mut c_void,
    ) -> *mut c_void {
        if ptr.is_null() {
            // SAFETY: null realloc follows this allocator's malloc contract.
            return unsafe { tracked_malloc(size, data) };
        }
        if size == 0 {
            // SAFETY: ptr is a live allocation belonging to this allocator.
            unsafe {
                tracked_free(ptr, data);
            }
            return std::ptr::null_mut();
        }
        // SAFETY: Counts is the same live state passed during construction.
        let counts = unsafe { &*data.cast::<Counts>() };
        if !counts.permit() {
            return std::ptr::null_mut();
        }
        // SAFETY: a nonzero-size realloc preserves the live allocation count, even on failure.
        unsafe { realloc(ptr, size) }
    }
    #[test]
    fn custom_allocator_cleanup_and_failures() {
        for limit in [0, 1, 2, 3, 4, 5, usize::MAX] {
            let counts = Counts {
                live: Cell::new(0),
                calls: Cell::new(0),
                limit,
            };
            let mem = RawAllocator {
                mem_user_data: std::ptr::from_ref(&counts).cast_mut().cast(),
                malloc: Some(tracked_malloc),
                free: Some(tracked_free),
                calloc: Some(tracked_calloc),
                realloc: Some(tracked_realloc),
            };
            if let Some(oversize) = (u32::MAX as usize).checked_add(1) {
                // SAFETY: callbacks and state remain live throughout this rejected construction.
                let error = unsafe { HpDeflater::new_with_mem(oversize, &mem) }.err();
                assert_eq!(error, Some(ARGUMENT));
                assert_eq!(counts.calls.get(), 0);
            }
            // SAFETY: callbacks are compatible C allocators; mem and counts outlive both contexts.
            let enc = unsafe { HpDeflater::new_with_mem(4096, &mem) };
            // SAFETY: the same allocator lifetime and callback guarantees apply to the decoder.
            let dec = unsafe { HpInflater::new_with_mem(&mem) };
            if limit == usize::MAX {
                let mut enc = enc.unwrap();
                let mut dec = dec.unwrap();
                let headers = [Header::new(b"x-custom", b"owned")];
                let block = enc.deflate_vec(&headers).unwrap();
                assert_eq!(dec.inflate_block(&block).unwrap()[0].value, b"owned");
                drop(enc);
                assert!(counts.live.get() > 0);
                drop(dec);
            } else {
                let errors = [enc.as_ref().err(), dec.as_ref().err()];
                for error in errors.into_iter().flatten() {
                    assert_eq!(error.0, sys::NGHTTP2_ERR_NOMEM);
                }
                drop(enc);
                drop(dec);
            }
            assert_eq!(counts.live.get(), 0, "limit={limit}");
            assert!(counts.calls.get() > 0);
        }
    }
}
