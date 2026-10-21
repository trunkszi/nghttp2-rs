//! Header name/value descriptors.
//!
//! An [`nghttp2_nv`] is a borrowed view. With the `NO_COPY` flags set the library
//! queues the caller's pointers and reads them when the frame is packed, so the
//! buffers must stay valid and unmodified until `nghttp2_on_frame_send_callback`
//! or `nghttp2_on_frame_not_send_callback` fires. With those flags clear it
//! copies the bytes into one allocation owned by the frame. `NO_COPY_NAME` also
//! skips the library's lowercasing, so the name must already be lowercase.
//!
//! [`HeaderField`] is the borrowed form, [`OwnedHeader`] the owning form, and
//! [`NvFlags`] the typed flag set shared by both. The `nv!` macro embeds literal
//! bytes in the binary, which makes them suitable for the zero-copy path.
//!
//! The `check_*` and `validate*` routines cover the character-level rules those C
//! helpers enforce; the session layer applies the remaining messaging checks.

pub use nghttp2_macros::nv;
pub use nghttp2_sys::{
    NGHTTP2_NV_FLAG_NO_COPY_NAME, NGHTTP2_NV_FLAG_NO_COPY_VALUE, NGHTTP2_NV_FLAG_NO_INDEX,
    NGHTTP2_NV_FLAG_NONE, nghttp2_nv, nghttp2_nv_flag,
};

pub(crate) use crate as nghttp2;

/// Native header name/value pair.
pub type RawHeaderField = nghttp2_nv;

/// Native header flags.
pub type RawHeaderFlags = nghttp2_nv_flag;

/// Flags that keep the name and value buffers under caller ownership.
pub const NO_COPY_FLAGS: RawHeaderFlags =
    NGHTTP2_NV_FLAG_NO_COPY_NAME | NGHTTP2_NV_FLAG_NO_COPY_VALUE;

/// Typed form of the [`nghttp2_nv`] flag field.
///
/// The C struct stores the flags as `u8` while the C constants are declared as
/// `c_uint`; this type keeps the narrow representation and exposes the constants
/// under their native names.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct NvFlags(u8);

impl NvFlags {
    /// No flags set.
    pub const NONE: Self = Self(NGHTTP2_NV_FLAG_NONE as u8);

    /// Do not add the field to the HPACK dynamic table.
    pub const NO_INDEX: Self = Self(NGHTTP2_NV_FLAG_NO_INDEX as u8);

    /// The caller keeps ownership of the name buffer and must pass it in lowercase.
    pub const NO_COPY_NAME: Self = Self(NGHTTP2_NV_FLAG_NO_COPY_NAME as u8);

    /// The caller keeps ownership of the value buffer.
    pub const NO_COPY_VALUE: Self = Self(NGHTTP2_NV_FLAG_NO_COPY_VALUE as u8);

    /// Both buffers stay caller-owned; the name must already be lowercase.
    pub const NO_COPY: Self = Self::NO_COPY_NAME.union(Self::NO_COPY_VALUE);

    /// Both buffers stay under caller ownership and the field is not indexed.
    pub const NO_COPY_NO_INDEX: Self = Self::NO_COPY.union(Self::NO_INDEX);

    /// Bits this wrapper models; anything else is discarded on construction.
    const KNOWN_BITS: u8 = Self::NO_COPY_NO_INDEX.0;

    /// Builds a flag set, dropping bits that [`NvFlags`] does not model.
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & Self::KNOWN_BITS)
    }

    /// Returns the raw bits, exactly as stored in [`nghttp2_nv::flags`].
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether every flag set in `other` is also set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns whether no flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the flags set in either `self` or `other`.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for NvFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for NvFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// Borrowed header name/value pair with typed flags.
///
/// Construction does not validate because it has to call into C, which would rule
/// out constants such as [`METHOD_POST`]. Call [`HeaderField::validate`] or
/// [`HeaderField::validate_rfc9113`] before submitting.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HeaderField<'a> {
    name: &'a [u8],
    value: &'a [u8],
    flags: NvFlags,
}

impl<'a> HeaderField<'a> {
    /// Builds a field that borrows both byte slices and carries `flags`.
    ///
    /// `'static` slices are the zero-copy case, but any borrow that outlives the
    /// descriptor works.
    pub const fn from_static(name: &'a [u8], value: &'a [u8], flags: NvFlags) -> Self {
        Self { name, value, flags }
    }

    /// Builds a field that keeps both buffers under caller ownership.
    pub const fn new(name: &'a [u8], value: &'a [u8]) -> Self {
        Self::from_static(name, value, NvFlags::NO_COPY)
    }

    /// Returns the header name bytes.
    pub const fn name(&self) -> &'a [u8] {
        self.name
    }

    /// Returns the header value bytes.
    pub const fn value(&self) -> &'a [u8] {
        self.value
    }

    /// Returns the flags carried by this field.
    pub const fn flags(&self) -> NvFlags {
        self.flags
    }

    /// Builds the raw descriptor that `nghttp2` submission calls expect.
    ///
    /// The `name` and `value` fields are `*mut` only because the C header spells
    /// them that way. The bytes behind them are never written through, and the
    /// library does not free them while the `NO_COPY` flags are set.
    ///
    /// The descriptor borrows the field's bytes. [`METHOD_POST`] and every `nv!`
    /// product point into the binary; a caller-owned buffer must stay alive and
    /// unmodified until the frame is sent or abandoned, because submission only
    /// queues the pointers.
    pub const fn as_raw(&self) -> RawHeaderField {
        RawHeaderField {
            name: self.name.as_ptr().cast_mut(),
            value: self.value.as_ptr().cast_mut(),
            namelen: self.name.len(),
            valuelen: self.value.len(),
            flags: self.flags.bits(),
        }
    }

    /// Validates the field with the character-level HTTP/2 rules for inbound headers.
    ///
    /// The name must be non-empty, may carry one leading `:`, and every remaining
    /// byte must be an RFC 9113 token character, so uppercase fails. The value must
    /// not contain CR, LF, NUL, DEL, or another control byte; HTAB, SP, and bytes
    /// from 0x21 through 0xFF other than DEL are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHeader::Name`] when the name fails its check, or
    /// [`InvalidHeader::Value`] when the value does.
    pub fn validate(&self) -> Result<(), InvalidHeader> {
        if !check_header_name(self.name) {
            return Err(InvalidHeader::Name);
        }
        if !check_header_value(self.value) {
            return Err(InvalidHeader::Value);
        }
        Ok(())
    }

    /// Validates the field with the stricter RFC 9113 character rules before submission.
    ///
    /// The name rules are the same as [`HeaderField::validate`]. A `:authority` or
    /// `host` value must consist only of authority characters and must not carry
    /// userinfo (`@`); every other value must have no leading or trailing SP or
    /// HTAB.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHeader::Name`] when the name fails its check,
    /// [`InvalidHeader::Authority`] when an authority value fails, or
    /// [`InvalidHeader::Value`] when another value fails the strict check.
    pub fn validate_rfc9113(&self) -> Result<(), InvalidHeader> {
        if !check_header_name(self.name) {
            return Err(InvalidHeader::Name);
        }
        if self.name == b":authority" || self.name == b"host" {
            // The public helper accepts '@' for URI userinfo; RFC 3986 section
            // 3.2.2 excludes userinfo from :authority and host, and nghttp2's
            // receive path rejects it.
            return if check_authority(self.value) && !self.value.contains(&b'@') {
                Ok(())
            } else {
                Err(InvalidHeader::Authority)
            };
        }
        if !check_header_value_rfc9113(self.value) {
            return Err(InvalidHeader::Value);
        }
        Ok(())
    }
}

/// Whether a header name is valid.
///
/// The name must be non-empty and may start with one `:`. Every remaining byte
/// must be an RFC 9113 token character; uppercase letters are token characters
/// but are rejected here because HTTP/2 requires lowercase names.
#[must_use]
pub fn check_header_name(name: &[u8]) -> bool {
    // SAFETY: `name` is a live slice, so the pointer and length describe
    // initialized memory that the C function may read. It does not retain the
    // pointer and returns before dereferencing when the length is zero.
    unsafe { nghttp2_sys::nghttp2_check_header_name(name.as_ptr(), name.len()) != 0 }
}

/// Whether a header value is valid.
///
/// HTAB, SP, and bytes 0x21..=0xFF except DEL (0x7F) are accepted; every other
/// control byte is rejected, matching the rules for received fields.
#[must_use]
pub fn check_header_value(value: &[u8]) -> bool {
    // SAFETY: `value` is a live slice, so the pointer and length describe
    // initialized memory that the C function may read. It does not retain the
    // pointer and the loop body never runs when the length is zero.
    unsafe { nghttp2_sys::nghttp2_check_header_value(value.as_ptr(), value.len()) != 0 }
}

/// Whether a header value is valid under the stricter RFC 9113 rules.
///
/// The empty value is valid. Otherwise leading and trailing SP or HTAB are
/// rejected, and the remaining bytes must pass [`check_header_value`].
#[must_use]
pub fn check_header_value_rfc9113(value: &[u8]) -> bool {
    // SAFETY: `value` is a live slice, so the pointer and length describe
    // initialized memory that the C function may read. The function returns
    // early for an empty slice and does not retain the pointer.
    unsafe { nghttp2_sys::nghttp2_check_header_value_rfc9113(value.as_ptr(), value.len()) != 0 }
}

/// Whether an authority value is valid.
///
/// Empty is valid. Otherwise every byte must be allowed in a URI authority:
/// alphanumerics and `!$%&'()*+,-.:;=@[]_~`; space, `/`, `?`, and `#` are rejected.
#[must_use]
pub fn check_authority(authority: &[u8]) -> bool {
    // SAFETY: `authority` is a live slice, so the pointer and length describe
    // initialized memory that the C function may read. It does not retain the
    // pointer and the loop body never runs when the length is zero.
    unsafe { nghttp2_sys::nghttp2_check_authority(authority.as_ptr(), authority.len()) != 0 }
}

/// Header whose name and value bytes are owned by the wrapper.
///
/// Converting a [`HeaderField`] copies both byte strings, so the owned header
/// remains valid after the source buffers change or are dropped. The flags are
/// preserved unchanged, including `NO_COPY`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OwnedHeader {
    name: Vec<u8>,
    value: Vec<u8>,
    flags: NvFlags,
}

impl OwnedHeader {
    /// Copies `name` and `value` into a header that keeps both buffers under
    /// caller ownership.
    pub fn new(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self::with_flags(name, value, NvFlags::NO_COPY)
    }

    /// Copies `name` and `value` into a header carrying `flags`.
    pub fn with_flags(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>, flags: NvFlags) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            flags,
        }
    }

    /// Returns the header name bytes.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Returns the header value bytes.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// Returns the flags carried by this header.
    #[must_use]
    pub fn flags(&self) -> NvFlags {
        self.flags
    }

    /// Borrows the header as a [`HeaderField`].
    #[must_use]
    pub fn as_field(&self) -> HeaderField<'_> {
        HeaderField::from_static(&self.name, &self.value, self.flags)
    }

    /// Builds the raw descriptor that `nghttp2` submission calls expect.
    ///
    /// The descriptor points into this header's buffers, so `self` must outlive
    /// the queued frame: dropping it or moving a vector after submission leaves
    /// nghttp2 with dangling pointers. The `*mut` fields are never written
    /// through, and the library does not free the buffers under `NO_COPY`.
    pub fn as_raw(&self) -> RawHeaderField {
        self.as_field().as_raw()
    }

    /// Validates the field with the character-level HTTP/2 rules for inbound headers.
    ///
    /// See [`HeaderField::validate`] for the exact name and value rules.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHeader::Name`] when the name fails its check, or
    /// [`InvalidHeader::Value`] when the value does.
    pub fn validate(&self) -> Result<(), InvalidHeader> {
        self.as_field().validate()
    }

    /// Validates the field with the stricter RFC 9113 character rules before submission.
    ///
    /// See [`HeaderField::validate_rfc9113`] for the exact rules.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHeader::Name`] when the name fails its check,
    /// [`InvalidHeader::Authority`] when a `:authority` value fails the authority
    /// check, or [`InvalidHeader::Value`] when any other value fails the strict
    /// check.
    pub fn validate_rfc9113(&self) -> Result<(), InvalidHeader> {
        self.as_field().validate_rfc9113()
    }
}

impl<'a> From<HeaderField<'a>> for OwnedHeader {
    fn from(field: HeaderField<'a>) -> Self {
        Self {
            name: field.name().to_vec(),
            value: field.value().to_vec(),
            flags: field.flags(),
        }
    }
}

/// Reason a header field failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidHeader {
    /// The name is empty, contains a non-token byte, or contains uppercase.
    Name,
    /// The value contains a byte the library rejects, or, under RFC 9113 rules,
    /// begins or ends with SP or HTAB.
    Value,
    /// A `:authority` or `host` value contains a byte outside the authority
    /// character set or carries userinfo (`@`).
    Authority,
}

impl core::fmt::Display for InvalidHeader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::Name => "invalid header name",
            Self::Value => "invalid header value",
            Self::Authority => "invalid :authority value",
        };
        f.write_str(message)
    }
}

impl std::error::Error for InvalidHeader {}

/// The `:method: POST` pseudo-header as a compile-time constant.
///
/// Both byte strings live in the binary, so the descriptor never needs cleanup
/// and can be reused across submissions without copying.
pub const METHOD_POST: HeaderField<'static> = nv!(":method", "POST");

/// Builds the raw `:method: POST` pseudo-header without copying either string.
///
/// The fields are `*mut` only because `nghttp2_nv` declares them that way; the
/// buffers must never be written through, and the library never frees them under
/// `NO_COPY`. The bytes are the static data behind [`METHOD_POST`].
#[must_use]
pub fn method_post() -> RawHeaderField {
    METHOD_POST.as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_static(_: HeaderField<'static>) {}

    #[test]
    fn nv_macro_two_argument_form_sets_no_copy() {
        let field = nv!("content-type", "text/html");
        assert_eq!(field.name(), b"content-type");
        assert_eq!(field.value(), b"text/html");
        assert_eq!(
            field.flags().bits(),
            (NGHTTP2_NV_FLAG_NO_COPY_NAME | NGHTTP2_NV_FLAG_NO_COPY_VALUE) as u8
        );
        assert_eq!(field.flags(), NvFlags::NO_COPY);
    }

    #[test]
    fn nv_macro_no_index_form_adds_no_index() {
        let field = nv!("x-test", "value", no_index);
        assert_eq!(field.name(), b"x-test");
        assert_eq!(field.value(), b"value");
        assert_eq!(
            field.flags().bits(),
            (NGHTTP2_NV_FLAG_NO_COPY_NAME
                | NGHTTP2_NV_FLAG_NO_COPY_VALUE
                | NGHTTP2_NV_FLAG_NO_INDEX) as u8
        );
        assert_eq!(field.flags(), NvFlags::NO_COPY_NO_INDEX);
    }

    #[test]
    fn method_post_is_static_and_zero_copy() {
        assert_static(METHOD_POST);
        let raw = METHOD_POST.as_raw();
        assert_eq!(raw.namelen, 7);
        assert_eq!(raw.valuelen, 4);
        assert_eq!(raw.flags, NO_COPY_FLAGS as u8);
        // SAFETY: METHOD_POST borrows static byte strings of the stated lengths,
        // and the slices are read while those constants are alive.
        let (name, value) = unsafe {
            (
                core::slice::from_raw_parts(raw.name, raw.namelen),
                core::slice::from_raw_parts(raw.value, raw.valuelen),
            )
        };
        assert_eq!(name, b":method");
        assert_eq!(value, b"POST");
        assert_eq!(method_post().flags, raw.flags);
    }

    #[test]
    fn header_field_new_keeps_buffers_under_caller_ownership() {
        let field = HeaderField::new(b"x-name", b"value");
        assert_eq!(field.name(), b"x-name");
        assert_eq!(field.value(), b"value");
        assert_eq!(field.flags(), NvFlags::NO_COPY);
        assert_eq!(
            field.flags().bits(),
            (NGHTTP2_NV_FLAG_NO_COPY_NAME | NGHTTP2_NV_FLAG_NO_COPY_VALUE) as u8
        );
    }

    #[test]
    fn nv_flags_match_native_constants() {
        assert_eq!(NvFlags::NONE.bits(), NGHTTP2_NV_FLAG_NONE as u8);
        assert_eq!(NvFlags::NO_INDEX.bits(), NGHTTP2_NV_FLAG_NO_INDEX as u8);
        assert_eq!(
            NvFlags::NO_COPY_NAME.bits(),
            NGHTTP2_NV_FLAG_NO_COPY_NAME as u8
        );
        assert_eq!(
            NvFlags::NO_COPY_VALUE.bits(),
            NGHTTP2_NV_FLAG_NO_COPY_VALUE as u8
        );
        assert_eq!(
            NvFlags::NO_COPY.bits(),
            (NGHTTP2_NV_FLAG_NO_COPY_NAME | NGHTTP2_NV_FLAG_NO_COPY_VALUE) as u8
        );
        assert_eq!(
            NvFlags::NO_COPY_NO_INDEX.bits(),
            (NGHTTP2_NV_FLAG_NO_INDEX
                | NGHTTP2_NV_FLAG_NO_COPY_NAME
                | NGHTTP2_NV_FLAG_NO_COPY_VALUE) as u8
        );
        assert_eq!(NO_COPY_FLAGS, NvFlags::NO_COPY.bits() as RawHeaderFlags);
        assert_eq!(NvFlags::default(), NvFlags::NONE);
    }

    #[test]
    fn nv_flags_set_operations() {
        let combined = NvFlags::NO_COPY_NAME | NvFlags::NO_INDEX;
        assert_eq!(
            combined.bits(),
            NvFlags::NO_COPY_NAME.bits() | NvFlags::NO_INDEX.bits()
        );
        assert!(combined.contains(NvFlags::NO_COPY_NAME));
        assert!(combined.contains(NvFlags::NO_INDEX));
        assert!(!combined.contains(NvFlags::NO_COPY_VALUE));
        assert!(NvFlags::NO_COPY.contains(NvFlags::NONE));
        assert!(!NvFlags::NO_INDEX.contains(NvFlags::NO_COPY_NAME));

        assert!(NvFlags::NONE.is_empty());
        assert!(!combined.is_empty());
        assert_eq!(NvFlags::NONE.union(NvFlags::NO_INDEX), NvFlags::NO_INDEX);
        assert_eq!(
            NvFlags::NO_COPY_NAME.union(NvFlags::NO_COPY_VALUE),
            NvFlags::NO_COPY
        );

        assert_eq!(NvFlags::from_bits_truncate(0xFF), NvFlags::NO_COPY_NO_INDEX);
        assert_eq!(
            NvFlags::from_bits_truncate(0x08),
            NvFlags::NONE,
            "unknown bits are dropped"
        );
        assert!(NvFlags::from_bits_truncate(0).is_empty());

        let mut assigned = NvFlags::NONE;
        assigned |= NvFlags::NO_COPY_VALUE;
        assert_eq!(assigned, NvFlags::NO_COPY_VALUE);
    }

    #[test]
    fn header_field_as_raw_round_trip() {
        let field = HeaderField::new(b":path", b"/index.html");
        let raw = field.as_raw();
        assert_eq!(raw.namelen, field.name().len());
        assert_eq!(raw.valuelen, field.value().len());
        assert_eq!(raw.flags, field.flags().bits());
        // SAFETY: the raw descriptor points into `field`, which is still alive,
        // and the lengths are the ones recorded from those same slices.
        let (name, value) = unsafe {
            (
                core::slice::from_raw_parts(raw.name, raw.namelen),
                core::slice::from_raw_parts(raw.value, raw.valuelen),
            )
        };
        assert_eq!(name, field.name());
        assert_eq!(value, field.value());
        let none_flags = HeaderField::from_static(b"x", b"y", NvFlags::NONE);
        assert_eq!(none_flags.as_raw().flags, NGHTTP2_NV_FLAG_NONE as u8);
    }

    #[test]
    fn owned_header_as_raw_round_trip() {
        let owned = OwnedHeader::with_flags(
            b":authority".as_slice(),
            b"example.com:443".as_slice(),
            NvFlags::NO_COPY_NO_INDEX,
        );
        let raw = owned.as_raw();
        assert_eq!(raw.namelen, owned.name().len());
        assert_eq!(raw.valuelen, owned.value().len());
        assert_eq!(raw.flags, NvFlags::NO_COPY_NO_INDEX.bits());
        // SAFETY: `owned` outlives the reads and the descriptor's lengths come
        // from the header's own buffers.
        let (name, value) = unsafe {
            (
                core::slice::from_raw_parts(raw.name, raw.namelen),
                core::slice::from_raw_parts(raw.value, raw.valuelen),
            )
        };
        assert_eq!(name, owned.name());
        assert_eq!(value, owned.value());
    }

    #[test]
    fn owned_header_constructors_and_view() {
        let owned = OwnedHeader::new(b"x-name".as_slice(), b"value".as_slice());
        assert_eq!(owned.name(), b"x-name");
        assert_eq!(owned.value(), b"value");
        assert_eq!(owned.flags(), NvFlags::NO_COPY);

        let field = owned.as_field();
        assert_eq!(field.name(), owned.name());
        assert_eq!(field.value(), owned.value());
        assert_eq!(field.flags(), owned.flags());

        let flagged = OwnedHeader::with_flags(
            b"x-name".as_slice(),
            b"value".as_slice(),
            NvFlags::NO_COPY_NO_INDEX,
        );
        assert_eq!(flagged.flags(), NvFlags::NO_COPY_NO_INDEX);
        assert_eq!(flagged.as_field().flags(), NvFlags::NO_COPY_NO_INDEX);
    }

    #[test]
    fn owned_header_from_header_field_copies_bytes_and_keeps_flags() {
        let mut name = b"x-name".to_vec();
        let mut value = b"value".to_vec();
        let field = HeaderField::from_static(name.as_slice(), value.as_slice(), NvFlags::NO_INDEX);
        let owned = OwnedHeader::from(field);

        name[0] = b'X';
        value[0] = b'V';

        assert_eq!(owned.name(), b"x-name");
        assert_eq!(owned.value(), b"value");
        assert_eq!(owned.flags(), NvFlags::NO_INDEX);
        assert_eq!(owned.as_field().flags(), NvFlags::NO_INDEX);
    }

    #[test]
    fn check_header_name_matches_upstream_rules() {
        let cases: &[(&[u8], bool)] = &[
            (b":method", true),
            (b":path", true),
            (b"content-type", true),
            (b"x", true),
            (b"", false),
            (b":", false),
            (b"::path", false),
            (b"Content-Type", false),
            (b":Method", false),
            (b"!#$%&'*+-.^_`|~", true), // punctuation entries in VALID_HD_NAME_CHARS
            (b"a\x80", false),          // non-ASCII bytes are not name characters
            (b"foo bar", false),
            (b"foo\0bar", false),
            (b"foo:bar", false),
            (b"foo\x7fbar", false),
        ];
        for (name, expected) in cases {
            assert_eq!(check_header_name(name), *expected, "name {name:?}");
        }
    }

    #[test]
    fn check_header_value_matches_upstream_rules() {
        let cases: &[(&[u8], bool)] = &[
            (b"", true),
            (b"text/html", true),
            (b"a\tb", true),
            (b" leading", true),
            (b"trailing ", true),
            (b"~!@#$%^&*()", true),
            (b"\x80\xFF", true),
            (b"a\rb", false),
            (b"a\nb", false),
            (b"\0", false),
            (b"a\x7Fb", false),
        ];
        for (value, expected) in cases {
            assert_eq!(check_header_value(value), *expected, "value {value:?}");
        }
        for byte in 0u8..=0xFF {
            // C table: HTAB, SP, 0x21..=0x7E, 0x80..=0xFF.
            let expected = matches!(byte, b'\t' | b' ' | 0x21..=0x7E | 0x80..=0xFF);
            assert_eq!(check_header_value(&[byte]), expected, "value {byte:#04x}");
        }
    }

    #[test]
    fn check_header_value_rfc9113_matches_upstream_rules() {
        let cases: &[(&[u8], bool)] = &[
            (b"", true),
            (b"text/html", true),
            (b"a\tb", true),
            (b" leading", false),
            (b"trailing ", false),
            (b"trailing\t", false),
            (b"\tleading", false),
            (b" ", false),
            (b"\t", false),
            (b"a b", true),
            (b"a\rb", false),
            (b"a\x7Fb", false),
        ];
        for (value, expected) in cases {
            assert_eq!(
                check_header_value_rfc9113(value),
                *expected,
                "value {value:?}"
            );
        }
    }

    #[test]
    fn check_authority_matches_upstream_rules() {
        let cases: &[(&[u8], bool)] = &[
            (b"example.com:443", true),
            (b"\x01", false),
            (b"", true),
            (b"[::1]:8443", true),
            (b"user@example.com", true),
            // RFC 3986 allows percent-encoded reg-names; upstream's authority
            // table includes '%'.
            (b"exa%20mple", true),
            (b"exa mple", false),
            (b"example.com/path", false),
            (b"example.com?x", false),
            (b"example.com#frag", false),
            (b"a\tb", false),
        ];
        for (authority, expected) in cases {
            assert_eq!(
                check_authority(authority),
                *expected,
                "authority {authority:?}"
            );
        }
    }

    #[test]
    fn validate_checks_name_before_value() {
        let field = HeaderField::new(b"Bad-Name", b"a\rb");
        assert_eq!(field.validate(), Err(InvalidHeader::Name));
        assert_eq!(field.validate_rfc9113(), Err(InvalidHeader::Name));
        let unnamed = HeaderField::new(b"", b"ok");
        assert_eq!(unnamed.validate(), Err(InvalidHeader::Name));
    }

    #[test]
    fn validate_accepts_regular_fields() {
        let field = HeaderField::new(b"content-type", b"text/html");
        assert_eq!(field.validate(), Ok(()));
        assert_eq!(field.validate_rfc9113(), Ok(()));
    }

    #[test]
    fn validate_rejects_embedded_control_bytes() {
        let field = HeaderField::new(b"x-note", b"a\rb");
        assert_eq!(field.validate(), Err(InvalidHeader::Value));

        let field = HeaderField::new(b"x-note", b"a\nb");
        assert_eq!(field.validate(), Err(InvalidHeader::Value));
    }

    #[test]
    fn validate_rfc9113_rejects_leading_and_trailing_whitespace() {
        let leading = HeaderField::new(b"x-note", b" leading");
        assert_eq!(leading.validate(), Ok(()));
        assert_eq!(leading.validate_rfc9113(), Err(InvalidHeader::Value));

        let trailing = HeaderField::new(b"x-note", b"trailing\t");
        assert_eq!(trailing.validate(), Ok(()));
        assert_eq!(trailing.validate_rfc9113(), Err(InvalidHeader::Value));
    }

    #[test]
    fn validate_rfc9113_checks_authority_charset() {
        let bad = HeaderField::new(b":authority", b"exa mple");
        // SP is a valid header-value byte, so the RFC 7540 check accepts it.
        assert_eq!(bad.validate(), Ok(()));
        assert_eq!(bad.validate_rfc9113(), Err(InvalidHeader::Authority));

        let good = HeaderField::new(b":authority", b"example.com:443");
        assert_eq!(good.validate(), Ok(()));
        assert_eq!(good.validate_rfc9113(), Ok(()));
        let empty = HeaderField::new(b":authority", b"");
        assert_eq!(empty.validate_rfc9113(), Ok(()));
        let leading = HeaderField::new(b":authority", b" host");
        assert_eq!(leading.validate_rfc9113(), Err(InvalidHeader::Authority));
        let userinfo = HeaderField::new(b":authority", b"user@example.com");
        assert_eq!(userinfo.validate_rfc9113(), Err(InvalidHeader::Authority));
        let host = HeaderField::new(b"host", b"exa mple");
        assert_eq!(host.validate_rfc9113(), Err(InvalidHeader::Authority));
    }

    #[test]
    fn owned_header_delegates_validation() {
        let bad = OwnedHeader::new(b"Bad".as_slice(), b"value".as_slice());
        assert_eq!(bad.validate(), Err(InvalidHeader::Name));

        let good = OwnedHeader::new(b"x-good".as_slice(), b"value".as_slice());
        assert_eq!(good.validate(), Ok(()));
        assert_eq!(good.validate_rfc9113(), Ok(()));

        let authority = OwnedHeader::new(b":authority".as_slice(), b"exa mple".as_slice());
        assert_eq!(authority.validate_rfc9113(), Err(InvalidHeader::Authority));
    }

    #[test]
    fn invalid_header_display_strings_are_distinct() {
        let messages = [
            InvalidHeader::Name,
            InvalidHeader::Value,
            InvalidHeader::Authority,
        ]
        .map(|error| error.to_string());
        assert!(messages.iter().all(|message| !message.is_empty()));
        assert_ne!(messages[0], messages[1]);
        assert_ne!(messages[0], messages[2]);
        assert_ne!(messages[1], messages[2]);
    }
}
