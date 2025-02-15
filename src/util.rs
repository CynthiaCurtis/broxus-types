//! General stuff.

use std::mem::MaybeUninit;

/// Brings [unlikely](core::intrinsics::unlikely) to stable rust.
#[inline(always)]
pub(crate) const fn unlikely(b: bool) -> bool {
    #[allow(clippy::needless_bool, clippy::bool_to_int_with_if)]
    if (1i32).checked_div(if b { 0 } else { 1 }).is_none() {
        true
    } else {
        false
    }
}

#[cfg(any(feature = "base64", test))]
#[inline]
pub(crate) fn encode_base64<T: AsRef<[u8]>>(data: T) -> String {
    use base64::Engine;
    fn encode_base64_impl(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }
    encode_base64_impl(data.as_ref())
}

#[cfg(any(feature = "base64", test))]
#[inline]
pub(crate) fn decode_base64<T: AsRef<[u8]>>(data: T) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    fn decode_base64_impl(data: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
        base64::engine::general_purpose::STANDARD.decode(data)
    }
    decode_base64_impl(data.as_ref())
}

/// Small on-stack vector of max length N.
pub struct ArrayVec<T, const N: usize> {
    inner: [MaybeUninit<T>; N],
    len: u8,
}

impl<T, const N: usize> ArrayVec<T, N> {
    /// Ensure that provided length is small enough.
    const _ASSERT_LEN: () = assert!(N <= u8::MAX as usize);

    /// Returns the number of elements in the vector, also referred to as its ‘length’.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if the vector contains no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends an element to the back of a collection.
    ///
    /// # Safety
    ///
    /// The following must be true:
    /// - The length of this vector is less than `N`.
    #[inline]
    pub unsafe fn push(&mut self, item: T) {
        debug_assert!((self.len as usize) < N);

        *self.inner.get_unchecked_mut(self.len as usize) = MaybeUninit::new(item);
        self.len += 1;
    }

    /// Returns the inner data without dropping its elements.
    ///
    /// # Safety
    ///
    /// The caller is responsible for calling the destructor for
    /// `len` initialized items in the returned array.
    #[inline]
    pub unsafe fn into_inner(self) -> [MaybeUninit<T>; N] {
        let this = std::mem::ManuallyDrop::new(self);
        std::ptr::read(&this.inner)
    }
}

impl<T, const N: usize> Default for ArrayVec<T, N> {
    #[inline]
    fn default() -> Self {
        Self {
            // SAFETY: An uninitialized `[MaybeUninit<_>; LEN]` is valid.
            inner: unsafe { MaybeUninit::<[MaybeUninit<T>; N]>::uninit().assume_init() },
            len: 0,
        }
    }
}

impl<R, const N: usize> AsRef<[R]> for ArrayVec<R, N> {
    #[inline]
    fn as_ref(&self) -> &[R] {
        // SAFETY: {len} elements were initialized
        unsafe { std::slice::from_raw_parts(self.inner.as_ptr() as *const R, self.len as usize) }
    }
}

impl<T: Clone, const N: usize> Clone for ArrayVec<T, N> {
    fn clone(&self) -> Self {
        let mut res = Self::default();
        for item in self.as_ref() {
            // SAFETY: {len} elements were initialized
            unsafe { res.push(item.clone()) };
        }
        res
    }
}

impl<T, const N: usize> Drop for ArrayVec<T, N> {
    fn drop(&mut self) {
        debug_assert!(self.len as usize <= N);

        let references_ptr = self.inner.as_mut_ptr() as *mut T;
        for i in 0..self.len {
            // SAFETY: len items were initialized
            unsafe { std::ptr::drop_in_place(references_ptr.add(i as usize)) };
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum IterStatus {
    /// Iterator is still valid.
    Valid,
    /// Iterator started with a pruned branch cell.
    Pruned,
    /// [`RawDict`] has invalid structure.
    Broken,
}

impl IterStatus {
    #[inline]
    pub(crate) const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }

    #[inline]
    pub(crate) const fn is_pruned(self) -> bool {
        matches!(self, Self::Pruned)
    }
}

/// Used to get a mutable reference of the inner type if possible.
pub trait TryAsMut<T: ?Sized> {
    /// Tries to convert this type into a mutable reference of the (usually inferred) input type.
    fn try_as_mut(&mut self) -> Option<&mut T>;
}

pub(crate) fn debug_tuple_field1_finish(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    value1: &dyn std::fmt::Debug,
) -> std::fmt::Result {
    let mut builder = std::fmt::Formatter::debug_tuple(f, name);
    builder.field(value1);
    builder.finish()
}

pub(crate) fn debug_struct_field1_finish(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    name1: &str,
    value1: &dyn std::fmt::Debug,
) -> std::fmt::Result {
    let mut builder = std::fmt::Formatter::debug_struct(f, name);
    builder.field(name1, value1);
    builder.finish()
}

pub(crate) fn debug_struct_field2_finish(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    name1: &str,
    value1: &dyn std::fmt::Debug,
    name2: &str,
    value2: &dyn std::fmt::Debug,
) -> std::fmt::Result {
    let mut builder = std::fmt::Formatter::debug_struct(f, name);
    builder.field(name1, value1);
    builder.field(name2, value2);
    builder.finish()
}
