use std::borrow::Borrow;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::Arc;

use crate::cell::finalizer::{CellParts, DefaultFinalizer, Finalizer};
use crate::cell::{
    Cell, CellDescriptor, CellSlice, DynCell, HashBytes, LevelMask, MAX_BIT_LEN, MAX_REF_COUNT,
};
use crate::error::Error;
use crate::util::ArrayVec;

#[cfg(feature = "stats")]
use super::CellTreeStats;

/// A data structure that can be serialized into cells.
pub trait Store {
    /// Tries to store itself into the cell builder.
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error>;
}

impl<T: Store + ?Sized> Store for &T {
    #[inline]
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        <T as Store>::store_into(self, builder, finalizer)
    }
}

impl<T: Store + ?Sized> Store for Box<T> {
    #[inline]
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        <T as Store>::store_into(self.as_ref(), builder, finalizer)
    }
}

impl<T: Store + ?Sized> Store for Arc<T> {
    #[inline]
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        <T as Store>::store_into(self.as_ref(), builder, finalizer)
    }
}

impl<T: Store + ?Sized> Store for Rc<T> {
    #[inline]
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        <T as Store>::store_into(self.as_ref(), builder, finalizer)
    }
}

impl Store for () {
    #[inline]
    fn store_into(&self, _: &mut CellBuilder, _: &mut dyn Finalizer) -> Result<(), Error> {
        Ok(())
    }
}

macro_rules! impl_store_for_tuples {
    ($( ($($field:ident: $t:ident),+) ),*$(,)?) => {$(
        impl<$($t: Store),+> Store for ($($t),*) {
            fn store_into(
                &self,
                builder: &mut CellBuilder,
                finalizer: &mut dyn Finalizer
            ) -> Result<(), Error> {
                let ($($field),+) = self;
                $(ok!($field.store_into(builder, finalizer)));*;
                Ok(())
            }
        }
    )*};
}

impl_store_for_tuples! {
    (t1: T1, t2: T2),
    (t1: T1, t2: T2, t3: T3),
    (t1: T1, t2: T2, t3: T3, t4: T4),
    (t1: T1, t2: T2, t3: T3, t4: T4, t5: T5),
    (t1: T1, t2: T2, t3: T3, t4: T4, t5: T5, t6: T6),
}

impl<T: Store> Store for Option<T> {
    #[inline]
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        match self {
            Some(data) => {
                ok!(builder.store_bit_one());
                data.store_into(builder, finalizer)
            }
            None => builder.store_bit_zero(),
        }
    }
}

impl<'a> Store for CellSlice<'a> {
    #[inline]
    fn store_into(&self, builder: &mut CellBuilder, _: &mut dyn Finalizer) -> Result<(), Error> {
        builder.store_slice(self)
    }
}

impl Store for Cell {
    #[inline]
    fn store_into(&self, builder: &mut CellBuilder, _: &mut dyn Finalizer) -> Result<(), Error> {
        builder.store_reference(self.clone())
    }
}

macro_rules! impl_primitive_store {
    ($($type:ty => |$b:ident, $v:ident| $expr:expr),*$(,)?) => {
        $(impl Store for $type {
            #[inline]
            fn store_into(&self,
                $b: &mut CellBuilder,
                _: &mut dyn Finalizer
            ) -> Result<(), Error> {
                let $v = self;
                $expr
            }
        })*
    };
}

impl_primitive_store! {
    bool => |b, v| b.store_bit(*v),
    u8 => |b, v| b.store_u8(*v),
    i8 => |b, v| b.store_u8(*v as u8),
    u16 => |b, v| b.store_u16(*v),
    i16 => |b, v| b.store_u16(*v as u16),
    u32 => |b, v| b.store_u32(*v),
    i32 => |b, v| b.store_u32(*v as u32),
    u64 => |b, v| b.store_u64(*v),
    i64 => |b, v| b.store_u64(*v as u64),
    u128 => |b, v| b.store_u128(*v),
    i128 => |b, v| b.store_u128(*v as u128),
    HashBytes => |b, v| b.store_u256(v),
}

/// Builder for constructing cells with densely packed data.
pub struct CellBuilder {
    data: [u8; 128],
    level_mask: Option<LevelMask>,
    bit_len: u16,
    is_exotic: bool,
    references: ArrayVec<Cell, MAX_REF_COUNT>,
}

impl Default for CellBuilder {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for CellBuilder {
    fn clone(&self) -> Self {
        Self {
            data: self.data,
            level_mask: self.level_mask,
            bit_len: self.bit_len,
            is_exotic: self.is_exotic,
            references: self.references.clone(),
        }
    }
}

macro_rules! impl_store_uint {
    ($self:ident, $value:ident, bytes: $bytes:literal, bits: $bits:literal) => {
        if $self.bit_len + $bits <= MAX_BIT_LEN {
            let q = ($self.bit_len / 8) as usize;
            let r = $self.bit_len % 8;
            // SAFETY: q is in range 0..=127, r is in range 0..=7
            unsafe {
                let data_ptr = $self.data.as_mut_ptr().add(q);
                debug_assert!(q + $bytes + usize::from(r > 0) <= 128);
                if r == 0 {
                    // Just append data
                    let value = $value.to_be_bytes();
                    std::ptr::copy_nonoverlapping(value.as_ptr(), data_ptr, $bytes);
                } else {
                    // Append high bits to the last byte
                    *data_ptr |= ($value >> ($bits - 8 + r)) as u8;
                    // Make shifted bytes
                    let value: [u8; $bytes] = ($value << (8 - r)).to_be_bytes();
                    // Write shifted bytes
                    std::ptr::copy_nonoverlapping(value.as_ptr(), data_ptr.add(1), $bytes);
                }
            };
            $self.bit_len += $bits;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    };
}

impl CellBuilder {
    /// Builds a new cell from the specified data using the default finalizer.
    #[inline]
    pub fn build_from<T>(data: T) -> Result<Cell, Error>
    where
        T: Store,
    {
        Self::build_from_ext(data, &mut Cell::default_finalizer())
    }

    /// Builds a new cell from the specified data using the provided finalizer.
    #[inline]
    pub fn build_from_ext<T>(data: T, finalizer: &mut dyn Finalizer) -> Result<Cell, Error>
    where
        T: Store,
    {
        fn build_from_ext_impl(
            data: &dyn Store,
            finalizer: &mut dyn Finalizer,
        ) -> Result<Cell, Error> {
            let mut builder = CellBuilder::new();
            ok!(data.store_into(&mut builder, finalizer));
            builder.build_ext(finalizer)
        }
        build_from_ext_impl(&data, finalizer)
    }

    /// Creates an empty cell builder.
    pub fn new() -> Self {
        Self {
            data: [0; 128],
            level_mask: None,
            bit_len: 0,
            is_exotic: false,
            references: Default::default(),
        }
    }

    /// Tries to create a cell builder with the specified data.
    ///
    /// NOTE: if `bits` is greater than `bytes * 8`, pads the value with zeros (as high bits).
    pub fn from_raw_data(value: &[u8], bits: u16) -> Result<Self, Error> {
        let mut res = Self::new();
        ok!(res.store_raw(value, bits));
        Ok(res)
    }

    /// Returns an underlying cell data.
    #[inline]
    pub fn raw_data(&self) -> &[u8; 128] {
        &self.data
    }

    /// Returns the data size of this cell in bits.
    #[inline]
    pub fn bit_len(&self) -> u16 {
        self.bit_len
    }

    /// Returns remaining data capacity in bits.
    #[inline]
    pub fn spare_bits_capacity(&self) -> u16 {
        MAX_BIT_LEN - self.bit_len
    }

    /// Returns remaining references capacity.
    #[inline]
    pub fn spare_refs_capacity(&self) -> u8 {
        (MAX_REF_COUNT - self.references.len()) as u8
    }

    /// Returns true if there is enough remaining capacity to fit `bits` and `refs`.
    #[inline]
    pub fn has_capacity(&self, bits: u16, refs: u8) -> bool {
        self.bit_len + bits <= MAX_BIT_LEN && self.references.len() + refs as usize <= MAX_REF_COUNT
    }

    /// Explicitly sets the level mask.
    #[inline]
    pub fn with_level_mask(mut self, level_mask: LevelMask) -> Self {
        self.level_mask = Some(level_mask);
        self
    }

    /// Explicitly sets the level mask.
    #[inline]
    pub fn set_level_mask(&mut self, level_mask: LevelMask) {
        self.level_mask = Some(level_mask);
    }

    /// Marks this cell as exotic.
    #[inline]
    pub fn set_exotic(&mut self, is_exotic: bool) {
        self.is_exotic = is_exotic;
    }

    /// Tries to store the specified number of zero bits in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_zeros(&mut self, bits: u16) -> Result<(), Error> {
        if self.bit_len + bits <= MAX_BIT_LEN {
            self.bit_len += bits;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store one zero bit in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_bit_zero(&mut self) -> Result<(), Error> {
        let fits = self.bit_len < MAX_BIT_LEN;
        self.bit_len += fits as u16;
        if fits {
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store one non-zero bit in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_bit_one(&mut self) -> Result<(), Error> {
        if self.bit_len < MAX_BIT_LEN {
            let q = (self.bit_len / 8) as usize;
            let r = self.bit_len % 8;
            unsafe { *self.data.get_unchecked_mut(q) |= 1 << (7 - r) };
            self.bit_len += 1;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store one bit in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_bit(&mut self, value: bool) -> Result<(), Error> {
        if value {
            self.store_bit_one()
        } else {
            self.store_bit_zero()
        }
    }

    /// Tries to store `u8` in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_u8(&mut self, value: u8) -> Result<(), Error> {
        if self.bit_len + 8 <= MAX_BIT_LEN {
            let q = (self.bit_len / 8) as usize;
            let r = self.bit_len % 8;
            unsafe {
                if r == 0 {
                    debug_assert!(q < 128);
                    // xxxxxxxx
                    *self.data.get_unchecked_mut(q) = value;
                } else {
                    debug_assert!(q + 1 < 128);
                    // yyyxxxxx|xxx00000
                    *self.data.get_unchecked_mut(q) |= value >> r;
                    *self.data.get_unchecked_mut(q + 1) = value << (8 - r);
                }
            };
            self.bit_len += 8;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store `u16` in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_u16(&mut self, value: u16) -> Result<(), Error> {
        impl_store_uint!(self, value, bytes: 2, bits: 16)
    }

    /// Tries to store `u32` in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_u32(&mut self, value: u32) -> Result<(), Error> {
        impl_store_uint!(self, value, bytes: 4, bits: 32)
    }

    /// Tries to store `u64` in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_u64(&mut self, value: u64) -> Result<(), Error> {
        impl_store_uint!(self, value, bytes: 8, bits: 64)
    }

    /// Tries to store `u128` in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_u128(&mut self, value: u128) -> Result<(), Error> {
        impl_store_uint!(self, value, bytes: 16, bits: 128)
    }

    /// Tries to store 32 bytes in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_u256(&mut self, value: &HashBytes) -> Result<(), Error> {
        if self.bit_len + 256 <= MAX_BIT_LEN {
            let q = (self.bit_len / 8) as usize;
            let r = self.bit_len % 8;
            unsafe {
                let data_ptr = self.data.as_mut_ptr().add(q);
                debug_assert!(q + 32 + usize::from(r > 0) <= 128);
                if r == 0 {
                    // Just append data
                    std::ptr::copy_nonoverlapping(value.as_ptr(), data_ptr, 32);
                } else {
                    // Interpret 32 bytes as two u128
                    let [mut hi, mut lo]: [u128; 2] = std::mem::transmute_copy(value);

                    // Numbers are in big endian order, swap bytes on little endian arch
                    #[cfg(target_endian = "little")]
                    {
                        hi = hi.swap_bytes();
                        lo = lo.swap_bytes();
                    }

                    let shift = 8 - r;

                    // Append high bits to the last byte
                    *data_ptr |= (hi >> (128 - shift)) as u8;
                    // Make shifted bytes
                    let hi: [u8; 16] = ((hi << shift) | (lo >> (128 - shift))).to_be_bytes();
                    let lo: [u8; 16] = (lo << shift).to_be_bytes();
                    // Write shifted bytes
                    std::ptr::copy_nonoverlapping(hi.as_ptr(), data_ptr.add(1), 16);
                    std::ptr::copy_nonoverlapping(lo.as_ptr(), data_ptr.add(17), 16);
                }
            };
            self.bit_len += 256;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store `u8` in the cell (but only the specified number of bits),
    /// returning `false` if there is not enough remaining capacity.
    ///
    /// NOTE: if `bits` is greater than **8**, pads the value with zeros (as high bits).
    pub fn store_small_uint(&mut self, mut value: u8, mut bits: u16) -> Result<(), Error> {
        if bits == 0 {
            return Ok(());
        }

        if self.bit_len + bits <= MAX_BIT_LEN {
            bits = if let Some(offset) = bits.checked_sub(8) {
                self.bit_len += offset;
                8
            } else {
                bits
            };

            // Ensure that value starts with significant bits
            value <<= 8 - bits;

            let q = (self.bit_len / 8) as usize;
            let r = self.bit_len % 8;
            unsafe {
                debug_assert!(q < 128);
                if r == 0 {
                    // xxxxxxxx
                    *self.data.get_unchecked_mut(q) = value;
                } else {
                    // yyyxxxxx|xxx00000
                    *self.data.get_unchecked_mut(q) |= value >> r;
                    if bits + r > 8 {
                        debug_assert!(q + 1 < 128);
                        *self.data.get_unchecked_mut(q + 1) = value << (8 - r);
                    }
                }
            };
            self.bit_len += bits;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store `u64` in the cell (but only the specified number of bits),
    /// returning `false` if there is not enough remaining capacity.
    ///
    /// NOTE: if `bits` is greater than **64**, pads the value with zeros (as high bits).
    pub fn store_uint(&mut self, mut value: u64, mut bits: u16) -> Result<(), Error> {
        if bits == 0 {
            return Ok(());
        }

        if self.bit_len + bits <= MAX_BIT_LEN {
            // Store zeros if bits is greater than 64
            bits = if let Some(offset) = bits.checked_sub(64) {
                self.bit_len += offset;
                64
            } else {
                bits
            };

            // Ensure that value starts with significant bits
            value <<= 64 - bits;

            let q = (self.bit_len / 8) as usize;
            let r = self.bit_len % 8;
            // SAFETY: q is in range 0..=127, r is in range 0..=7
            unsafe {
                let data_ptr = self.data.as_mut_ptr().add(q);
                if r == 0 {
                    let byte_len = ((bits + 7) / 8) as usize;
                    debug_assert!(q + byte_len <= 128);

                    // Just append data
                    let value = value.to_be_bytes();
                    std::ptr::copy_nonoverlapping(value.as_ptr(), data_ptr, byte_len);
                } else {
                    debug_assert!(q < 128);

                    // Append high bits to the last byte
                    let shift = 8 - r;
                    *data_ptr |= (value >> (64 - shift)) as u8;

                    // If there are some bits left
                    if let Some(bits) = bits.checked_sub(shift) {
                        if bits > 0 {
                            let byte_len = ((bits + 7) / 8) as usize;
                            debug_assert!(q + 1 + byte_len <= 128);

                            // Make shifted bytes
                            let value: [u8; 8] = (value << shift).to_be_bytes();
                            // Write shifted bytes
                            std::ptr::copy_nonoverlapping(
                                value.as_ptr(),
                                data_ptr.add(1),
                                byte_len,
                            );
                        }
                    }
                }
            }
            self.bit_len += bits;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to store bytes in the cell (but only the specified number of bits),
    /// returning `false` if there is not enough remaining capacity.
    ///
    /// NOTE: if `bits` is greater than `bytes * 8`, pads the value with zeros (as high bits).
    pub fn store_raw(&mut self, value: &[u8], bits: u16) -> Result<(), Error> {
        store_raw(&mut self.data, &mut self.bit_len, value, bits)
    }

    /// Tries to store all data bits of the specified cell in the current cell,
    /// returning `false` if there is not enough remaining capacity.
    #[inline]
    pub fn store_cell_data<T>(&mut self, value: T) -> Result<(), Error>
    where
        T: Borrow<DynCell>,
    {
        fn store_cell_data_impl(builder: &mut CellBuilder, value: &DynCell) -> Result<(), Error> {
            store_raw(
                &mut builder.data,
                &mut builder.bit_len,
                value.data(),
                value.bit_len(),
            )
        }
        store_cell_data_impl(self, value.borrow())
    }

    /// Tries to store the remaining slice data in the cell,
    /// returning `false` if there is not enough remaining capacity.
    #[inline]
    pub fn store_slice_data<'a, T>(&mut self, value: T) -> Result<(), Error>
    where
        T: Borrow<CellSlice<'a>>,
    {
        fn store_slice_data_impl(
            builder: &mut CellBuilder,
            value: &CellSlice<'_>,
        ) -> Result<(), Error> {
            let bits = value.remaining_bits();
            if builder.bit_len + bits <= MAX_BIT_LEN {
                let mut slice_data = MaybeUninit::<[u8; 128]>::uninit();
                let slice_data = unsafe { &mut *(slice_data.as_mut_ptr() as *mut [u8; 128]) };
                let slice_data = ok!(value.get_raw(0, slice_data, bits));

                builder.store_raw(slice_data, bits)
            } else {
                Err(Error::CellOverflow)
            }
        }
        store_slice_data_impl(self, value.borrow())
    }

    /// Tries to prepend bytes to the cell data (but only the specified number of bits),
    /// returning `false` if there is not enough capacity.
    ///
    /// NOTE: if `bits` is greater than `bytes * 8`, pads the value with zeros (as high bits).
    pub fn prepend_raw(&mut self, value: &[u8], bits: u16) -> Result<(), Error> {
        if bits == 0 {
            return Ok(());
        }

        // Prevent asm code bloat
        fn store_raw_impl(
            data: &mut [u8; 128],
            bit_len: &mut u16,
            value: &[u8],
            bits: u16,
        ) -> Result<(), Error> {
            store_raw(data, bit_len, value, bits)
        }

        if self.bit_len + bits <= MAX_BIT_LEN {
            let mut data = [0; 128];
            let mut bit_len = 0;
            ok!(store_raw_impl(&mut data, &mut bit_len, value, bits));
            ok!(store_raw_impl(
                &mut data,
                &mut bit_len,
                &self.data,
                self.bit_len
            ));
            self.data = data;
            self.bit_len = bit_len;
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }
}

#[inline]
fn store_raw(
    data: &mut [u8; 128],
    bit_len: &mut u16,
    value: &[u8],
    mut bits: u16,
) -> Result<(), Error> {
    if *bit_len + bits <= MAX_BIT_LEN {
        let max_bit_len = value.len().saturating_mul(8) as u16;
        bits = if let Some(offset) = bits.checked_sub(max_bit_len) {
            *bit_len += offset;
            max_bit_len
        } else {
            bits
        };

        // Do nothing for empty slices or noop store
        if bits == 0 {
            return Ok(());
        }

        let q = (*bit_len / 8) as usize;
        let r = *bit_len % 8;
        // SAFETY: q is in range 0..=127, r is in range 0..=7
        unsafe {
            let mut data_ptr = data.as_mut_ptr().add(q);
            let mut value_ptr = value.as_ptr();

            if r == 0 {
                let byte_len = ((bits + 7) / 8) as usize;
                debug_assert!(q + byte_len <= 128);
                debug_assert!(byte_len <= value.len());

                std::ptr::copy_nonoverlapping(value_ptr, data_ptr, byte_len);

                let bits_r = bits % 8;
                if bits_r != 0 {
                    *data_ptr.add(byte_len - 1) &= 0xff << (8 - bits_r);
                }
            } else {
                let byte_len = ((bits + r + 7) / 8) as usize - 1;
                let value_len = ((bits + 7) / 8) as usize;
                debug_assert!(q + byte_len <= 128);
                debug_assert!(byte_len <= value_len && value_len <= value.len());

                let shift = 8 - r;
                for _ in 0..byte_len {
                    *data_ptr |= *value_ptr >> r;
                    data_ptr = data_ptr.add(1);
                    *data_ptr = *value_ptr << shift;
                    value_ptr = value_ptr.add(1);
                }
                if byte_len < value_len {
                    *data_ptr |= *value_ptr >> r;
                }

                let bits_r = (r + bits) % 8;
                if bits_r != 0 {
                    *data_ptr &= 0xff << (8 - bits_r);
                }
            }
        }
        *bit_len += bits;
        Ok(())
    } else {
        Err(Error::CellOverflow)
    }
}

impl CellBuilder {
    /// Computes the cell level from the level mask.
    pub fn compute_level(&self) -> u8 {
        self.compute_level_mask().level()
    }

    /// Computes the cell level mask from children.
    pub fn compute_level_mask(&self) -> LevelMask {
        if let Some(level_mask) = self.level_mask {
            level_mask
        } else {
            let mut children_mask = LevelMask::EMPTY;
            for child in self.references.as_ref() {
                children_mask |= child.as_ref().descriptor().level_mask();
            }
            children_mask
        }
    }

    /// Returns a slice of the child cells stored in the builder.
    #[inline]
    pub fn references(&self) -> &[Cell] {
        self.references.as_ref()
    }

    /// Tries to store a child in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_reference(&mut self, cell: Cell) -> Result<(), Error> {
        if self.references.len() < MAX_REF_COUNT {
            // SAFETY: reference count is in the valid range
            unsafe { self.references.push(cell) }
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Sets children of the cell.
    pub fn set_references(&mut self, refs: CellRefsBuilder) {
        self.references = refs.0;
    }

    /// Tries to append a builder (its data and references),
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_builder(&mut self, builder: &Self) -> Result<(), Error> {
        if self.bit_len + builder.bit_len <= MAX_BIT_LEN
            && self.references.len() + builder.references.len() <= MAX_REF_COUNT
        {
            ok!(self.store_raw(&builder.data, builder.bit_len));
            for cell in builder.references.as_ref() {
                ok!(self.store_reference(cell.clone()));
            }
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Tries to append a cell slice (its data and references),
    /// returning `false` if there is not enough remaining capacity.
    #[inline]
    pub fn store_slice<'a, T>(&mut self, value: T) -> Result<(), Error>
    where
        T: Borrow<CellSlice<'a>>,
    {
        fn store_slice_impl(builder: &mut CellBuilder, value: &CellSlice<'_>) -> Result<(), Error> {
            if builder.bit_len + value.remaining_bits() <= MAX_BIT_LEN
                && builder.references.len() + value.remaining_refs() as usize <= MAX_REF_COUNT
            {
                ok!(builder.store_slice_data(value));
                for cell in value.references().cloned() {
                    ok!(builder.store_reference(cell));
                }
                Ok(())
            } else {
                Err(Error::CellOverflow)
            }
        }
        store_slice_impl(self, value.borrow())
    }

    /// Tries to build a new cell using the specified finalizer.
    pub fn build_ext(mut self, finalizer: &mut dyn Finalizer) -> Result<Cell, Error> {
        debug_assert!(self.bit_len <= MAX_BIT_LEN);
        debug_assert!(self.references.len() <= MAX_REF_COUNT);

        #[cfg(feature = "stats")]
        let mut stats = CellTreeStats {
            bit_count: self.bit_len as u64,
            cell_count: 1,
        };

        let mut children_mask = LevelMask::EMPTY;
        for child in self.references.as_ref() {
            let child = child.as_ref();
            children_mask |= child.descriptor().level_mask();

            #[cfg(feature = "stats")]
            {
                stats += child.stats();
            }
        }

        let is_exotic = self.is_exotic;
        let level_mask = self.level_mask.unwrap_or(children_mask);

        let d1 = CellDescriptor::compute_d1(level_mask, is_exotic, self.references.len() as u8);
        let d2 = CellDescriptor::compute_d2(self.bit_len);

        let rem = self.bit_len % 8;
        let last_byte = (self.bit_len / 8) as usize;
        if rem > 0 {
            // SAFETY: `last_byte` is in the valid range
            let last_byte = unsafe { self.data.get_unchecked_mut(last_byte) };

            // x0000000 - rem=1, tag_mask=01000000, data_mask=11000000
            // xx000000 - rem=2, tag_mask=00100000, data_mask=11100000
            // xxx00000 - rem=3, tag_mask=00010000, data_mask=11110000
            // xxxx0000 - rem=4, tag_mask=00001000, data_mask=11111000
            // xxxxx000 - rem=5, tag_mask=00000100, data_mask=11111100
            // xxxxxx00 - rem=6, tag_mask=00000010, data_mask=11111110
            // xxxxxxx0 - rem=7, tag_mask=00000001, data_mask=11111111
            let tag_mask: u8 = 1 << (7 - rem);
            let data_mask = !(tag_mask - 1);

            // xxxxyyyy & data_mask -> xxxxy000 | tag_mask -> xxxx1000
            *last_byte = (*last_byte & data_mask) | tag_mask;
        }

        let byte_len = (self.bit_len + 7) / 8;
        let data = &self.data[..std::cmp::min(byte_len as usize, 128)];

        let cell_parts = CellParts {
            #[cfg(feature = "stats")]
            stats,
            bit_len: self.bit_len,
            descriptor: CellDescriptor { d1, d2 },
            children_mask,
            references: self.references,
            data,
        };
        finalizer.finalize_cell(cell_parts)
    }

    /// Tries to build a new cell using the default finalizer.
    ///
    /// See [`default_finalizer`]
    ///
    /// [`default_finalizer`]: fn@DefaultFinalizer::default_finalizer
    pub fn build(self) -> Result<Cell, Error> {
        self.build_ext(&mut Cell::default_finalizer())
    }
}

/// Builder for constructing cell references array.
///
/// Can be used later for [`CellBuilder::set_references`].
#[derive(Default)]
#[repr(transparent)]
pub struct CellRefsBuilder(ArrayVec<Cell, MAX_REF_COUNT>);

impl CellRefsBuilder {
    /// Tries to store a child in the cell,
    /// returning `false` if there is not enough remaining capacity.
    pub fn store_reference(&mut self, cell: Cell) -> Result<(), Error> {
        if self.0.len() < MAX_REF_COUNT {
            // SAFETY: reference count is in the valid range
            unsafe { self.0.push(cell) }
            Ok(())
        } else {
            Err(Error::CellOverflow)
        }
    }

    /// Computes children level mask as a combination of all level masks.
    pub fn compute_level_mask(&self) -> LevelMask {
        let mut result = LevelMask::EMPTY;
        for child in self.0.as_ref() {
            result |= child.as_ref().level_mask();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_builder() {
        let mut builder = CellBuilder::new();
        builder.store_u32(0xdeafbeaf).unwrap();
        let cell1 = builder.clone().build().unwrap();
        let cell2 = builder.clone().build().unwrap();
        assert_eq!(cell1.as_ref(), cell2.as_ref());

        builder.store_u32(0xb00b5).unwrap();
        let cell3 = builder.build().unwrap();
        assert_ne!(cell1.as_ref(), cell3.as_ref());
    }

    #[test]
    #[cfg_attr(miri, ignore)] // takes too long to execute on miri
    fn store_raw() {
        const ONES: &[u8] = &[0xff; 128];
        for offset in 0..8 {
            for bits in 0..=1016 {
                let mut builder = CellBuilder::new();
                builder.store_zeros(offset).unwrap();
                builder.store_raw(ONES, bits).unwrap();
                builder.build().unwrap();
            }
        }
    }

    #[test]
    fn prepend_raw() {
        let mut builder = CellBuilder::new();
        builder.store_raw(&[0xde, 0xaf, 0xbe, 0xaf], 20).unwrap();
        builder.prepend_raw(&[0xaa, 0x55], 5).unwrap();
        let cell = builder.build().unwrap();
        println!("{}", cell.display_tree());
    }

    #[test]
    fn store_slice() -> anyhow::Result<()> {
        const SOME_HASH: &HashBytes = HashBytes::wrap(&[
            0xdf, 0x86, 0xce, 0xbc, 0xe8, 0xd5, 0xab, 0x0c, 0x69, 0xb4, 0xce, 0x33, 0xfe, 0x9b,
            0x0e, 0x2c, 0xdf, 0x69, 0xa3, 0xe1, 0x13, 0x7e, 0x64, 0x85, 0x6b, 0xbc, 0xfd, 0x39,
            0xe7, 0x9b, 0xc1, 0x6f,
        ]);

        let mut builder = CellBuilder::new();
        builder.store_zeros(3)?;
        builder.store_u256(SOME_HASH)?;
        let cell = builder.build()?;
        println!("{}", cell.display_tree());

        let mut builder = CellBuilder::new();
        let mut slice = cell.as_slice()?;
        assert!(slice.try_advance(3, 0));
        builder.store_slice(slice)?;
        let cell = builder.build()?;
        println!("{}", cell.display_tree());
        assert_eq!(cell.data(), SOME_HASH);

        Ok(())
    }
}
