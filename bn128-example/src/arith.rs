use crate::rvv_impl::mul_reduce_internal;
use core::cmp::Ordering;
use crunchy::unroll;

use byteorder::{BigEndian, ByteOrder};

/// 256-bit, stack allocated biginteger for use in prime field
/// arithmetic.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct U256(pub [u128; 2]);

impl From<[u64; 4]> for U256 {
    fn from(d: [u64; 4]) -> Self {
        let mut a = [0u128; 2];
        a[0] = (d[1] as u128) << 64 | d[0] as u128;
        a[1] = (d[3] as u128) << 64 | d[2] as u128;
        U256(a)
    }
}

impl From<u64> for U256 {
    fn from(d: u64) -> Self {
        U256::from([d, 0, 0, 0])
    }
}

/// 512-bit, stack allocated biginteger for use in extension
/// field serialization and scalar interpretation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct U512(pub [u128; 4]);

impl From<[u64; 8]> for U512 {
    fn from(d: [u64; 8]) -> Self {
        let mut a = [0u128; 4];
        a[0] = (d[1] as u128) << 64 | d[0] as u128;
        a[1] = (d[3] as u128) << 64 | d[2] as u128;
        a[2] = (d[5] as u128) << 64 | d[4] as u128;
        a[3] = (d[7] as u128) << 64 | d[6] as u128;
        U512(a)
    }
}

impl U512 {
    /// Multiplies c1 by modulo, adds c0.
    pub fn new(c1: &U256, c0: &U256, modulo: &U256) -> U512 {
        let mut res = [0; 4];

        debug_assert_eq!(c1.0.len(), 2);
        unroll! {
            for i in 0..2 {
                mac_digit(i, &mut res, &modulo.0, c1.0[i]);
            }
        }

        let mut carry = 0;

        debug_assert_eq!(res.len(), 4);
        unroll! {
            for i in 0..2 {
                res[i] = adc(res[i], c0.0[i], &mut carry);
            }
        }

        unroll! {
            for i in 0..2 {
                let (a1, a0) = split_u128(res[i + 2]);
                let (c, r0) = split_u128(a0 + carry);
                let (c, r1) = split_u128(a1 + c);
                carry = c;

                res[i + 2] = combine_u128(r1, r0);
            }
        }

        debug_assert!(0 == carry);

        U512(res)
    }

    pub fn from_slice(s: &[u8]) -> Result<U512, Error> {
        if s.len() != 64 {
            return Err(Error::InvalidLength {
                expected: 32,
                actual: s.len(),
            });
        }

        let mut n = [0; 4];
        n[3] = BigEndian::read_u128(&s[0..]);
        n[2] = BigEndian::read_u128(&s[16..]);
        n[1] = BigEndian::read_u128(&s[32..]);
        n[0] = BigEndian::read_u128(&s[48..]);
        Ok(U512(n))
    }

    pub fn get_bit(&self, n: usize) -> Option<bool> {
        if n >= 512 {
            None
        } else {
            let part = n / 128;
            let bit = n - (128 * part);

            Some(self.0[part] & (1 << bit) > 0)
        }
    }

    /// Divides self by modulo, returning remainder and, if
    /// possible, a quotient smaller than the modulus.
    pub fn divrem(&self, modulo: &U256) -> (Option<U256>, U256) {
        let mut q = Some(U256::zero());
        let mut r = U256::zero();

        for i in (0..512).rev() {
            // NB: modulo's first two bits are always unset
            // so this will never destroy information
            mul2(&mut r.0);
            assert!(r.set_bit(0, self.get_bit(i).unwrap()));
            if &r >= modulo {
                sub_noborrow(&mut r.0, &modulo.0);
                if q.is_some() && !q.as_mut().unwrap().set_bit(i, true) {
                    q = None
                }
            }
        }

        if q.is_some() && (q.as_ref().unwrap() >= modulo) {
            (None, r)
        } else {
            (q, r)
        }
    }

    pub fn interpret(buf: &[u8; 64]) -> U512 {
        let mut n = [0; 4];
        n[3] = BigEndian::read_u128(&buf[0..]);
        n[2] = BigEndian::read_u128(&buf[16..]);
        n[1] = BigEndian::read_u128(&buf[32..]);
        n[0] = BigEndian::read_u128(&buf[48..]);
        U512(n)
    }
}

impl Ord for U512 {
    #[inline]
    fn cmp(&self, other: &U512) -> Ordering {
        if self.0[3] < other.0[3] {
            return Ordering::Less;
        }
        if self.0[3] > other.0[3] {
            return Ordering::Greater;
        }
        if self.0[2] < other.0[2] {
            return Ordering::Less;
        }
        if self.0[2] > other.0[2] {
            return Ordering::Greater;
        }
        if self.0[1] < other.0[1] {
            return Ordering::Less;
        }
        if self.0[1] > other.0[1] {
            return Ordering::Greater;
        }
        self.0[0].cmp(&other.0[0])
    }
}

impl PartialOrd for U512 {
    #[inline]
    fn partial_cmp(&self, other: &U512) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for U256 {
    #[inline]
    fn cmp(&self, other: &U256) -> Ordering {
        if self.0[1] < other.0[1] {
            return Ordering::Less;
        }
        if self.0[1] > other.0[1] {
            return Ordering::Greater;
        }
        self.0[0].cmp(&other.0[0])
    }
}

impl PartialOrd for U256 {
    #[inline]
    fn partial_cmp(&self, other: &U256) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// U256/U512 errors
#[derive(Debug)]
pub enum Error {
    InvalidLength { expected: usize, actual: usize },
}

impl U256 {
    /// Initialize U256 from slice of bytes (big endian)
    pub fn from_slice(s: &[u8]) -> Result<U256, Error> {
        if s.len() != 32 {
            return Err(Error::InvalidLength {
                expected: 32,
                actual: s.len(),
            });
        }
        let mut n = [0; 2];
        n[1] = BigEndian::read_u128(&s[0..]);
        n[0] = BigEndian::read_u128(&s[16..]);
        Ok(U256(n))
    }

    pub fn to_big_endian(&self, s: &mut [u8]) -> Result<(), Error> {
        if s.len() != 32 {
            return Err(Error::InvalidLength {
                expected: 32,
                actual: s.len(),
            });
        }
        BigEndian::write_u128(&mut s[0..], self.0[1]);
        BigEndian::write_u128(&mut s[16..], self.0[0]);
        Ok(())
    }

    #[inline]
    pub fn zero() -> U256 {
        U256([0, 0])
    }

    #[inline]
    pub fn one() -> U256 {
        U256([1, 0])
    }

    pub fn is_zero(&self) -> bool {
        self.0[0] == 0 && self.0[1] == 0
    }

    pub fn set_bit(&mut self, n: usize, to: bool) -> bool {
        if n >= 256 {
            false
        } else {
            let part = n / 128;
            let bit = n - (128 * part);

            if to {
                self.0[part] |= 1 << bit;
            } else {
                self.0[part] &= !(1 << bit);
            }

            true
        }
    }

    pub fn get_bit(&self, n: usize) -> Option<bool> {
        if n >= 256 {
            None
        } else {
            let part = n / 128;
            let bit = n - (128 * part);

            Some(self.0[part] & (1 << bit) > 0)
        }
    }

    /// Add `other` to `self` (mod `modulo`)
    pub fn add(&mut self, other: &U256, modulo: &U256) {
        add_nocarry(&mut self.0, &other.0);

        if *self >= *modulo {
            sub_noborrow(&mut self.0, &modulo.0);
        }
    }

    /// Subtract `other` from `self` (mod `modulo`)
    pub fn sub(&mut self, other: &U256, modulo: &U256) {
        if *self < *other {
            add_nocarry(&mut self.0, &modulo.0);
        }

        sub_noborrow(&mut self.0, &other.0);
    }

    /// Multiply `self` by `other` (mod `modulo`) via the Montgomery
    /// multiplication method.
    pub fn mul(&mut self, other: &U256, modulo: &U256, inv: u128) {
        // original one
        if cfg!(any(feature = "use_rvv_vector", feature = "use_rvv_asm")) {
            let inv_high = if inv == 0x9ede7d651eca6ac987d20782e4866389 {
                0xf57a22b791888c6bd8afcbd01833da80u128
            } else if inv == 0x6586864b4c6911b3c2e1f593efffffff {
                0x73f82f1d0d8341b2e39a982899062391
            } else {
                0
            };
            mul_reduce_internal(&mut self.0, &other.0, &modulo.0, inv, inv_high);
        } else {
            mul_reduce(&mut self.0, &other.0, &modulo.0, inv);
            if *self >= *modulo {
                sub_noborrow(&mut self.0, &modulo.0);
            }
        }
    }

    /// Turn `self` into its additive inverse (mod `modulo`)
    pub fn neg(&mut self, modulo: &U256) {
        if *self > Self::zero() {
            let mut tmp = modulo.0;
            sub_noborrow(&mut tmp, &self.0);

            self.0 = tmp;
        }
    }

    #[inline]
    pub fn is_even(&self) -> bool {
        self.0[0] & 1 == 0
    }

    /// Turn `self` into its multiplicative inverse (mod `modulo`)
    pub fn invert(&mut self, modulo: &U256) {
        // Guajardo Kumar Paar Pelzl
        // Efficient Software-Implementation of Finite Fields with Applications to Cryptography
        // Algorithm 16 (BEA for Inversion in Fp)

        let mut u = *self;
        let mut v = *modulo;
        let mut b = U256::one();
        let mut c = U256::zero();

        while u != U256::one() && v != U256::one() {
            while u.is_even() {
                div2(&mut u.0);

                if b.is_even() {
                    div2(&mut b.0);
                } else {
                    add_nocarry(&mut b.0, &modulo.0);
                    div2(&mut b.0);
                }
            }
            while v.is_even() {
                div2(&mut v.0);

                if c.is_even() {
                    div2(&mut c.0);
                } else {
                    add_nocarry(&mut c.0, &modulo.0);
                    div2(&mut c.0);
                }
            }

            if u >= v {
                sub_noborrow(&mut u.0, &v.0);
                b.sub(&c, modulo);
            } else {
                sub_noborrow(&mut v.0, &u.0);
                c.sub(&b, modulo);
            }
        }

        if u == U256::one() {
            self.0 = b.0;
        } else {
            self.0 = c.0;
        }
    }

    /// Return an Iterator<Item=bool> over all bits from
    /// MSB to LSB.
    pub fn bits(&self) -> BitIterator {
        BitIterator { int: &self, n: 256 }
    }
}

pub struct BitIterator<'a> {
    int: &'a U256,
    n: usize,
}

impl<'a> Iterator for BitIterator<'a> {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.n == 0 {
            None
        } else {
            self.n -= 1;

            self.int.get_bit(self.n)
        }
    }
}

/// Divide by two
#[inline]
fn div2(a: &mut [u128; 2]) {
    let tmp = a[1] << 127;
    a[1] >>= 1;
    a[0] >>= 1;
    a[0] |= tmp;
}

/// Multiply by two
#[inline]
fn mul2(a: &mut [u128; 2]) {
    let tmp = a[0] >> 127;
    a[0] <<= 1;
    a[1] <<= 1;
    a[1] |= tmp;
}

#[inline(always)]
fn split_u128(i: u128) -> (u128, u128) {
    (i >> 64, i & 0xFFFFFFFFFFFFFFFF)
}

#[inline(always)]
fn combine_u128(hi: u128, lo: u128) -> u128 {
    (hi << 64) | lo
}

#[inline]
fn adc(a: u128, b: u128, carry: &mut u128) -> u128 {
    let (c, d) = a.overflowing_add(b);
    let (e, f) = c.overflowing_add(*carry);
    *carry = (d | f) as u128;
    e
}

#[inline]
fn add_nocarry(a: &mut [u128; 2], b: &[u128; 2]) {
    let (c, d) = a[0].overflowing_add(b[0]);
    a[0] = c;
    a[1] = a[1] + b[1] + d as u128;
}

#[inline]
fn sub_noborrow(a: &mut [u128; 2], b: &[u128; 2]) {
    let (c, d) = a[0].overflowing_sub(b[0]);
    a[0] = c;
    a[1] = a[1] - b[1] - d as u128;
}

// TODO: Make `from_index` a const param
#[inline(always)]
fn mac_digit(from_index: usize, acc: &mut [u128; 4], b: &[u128; 2], c: u128) {
    #[inline]
    fn mac_with_carry(a: u128, b: u128, c: u128, carry: &mut u128) -> u128 {
        let (b_hi, b_lo) = split_u128(b);
        let (c_hi, c_lo) = split_u128(c);

        let (a_hi, a_lo) = split_u128(a);
        let (carry_hi, carry_lo) = split_u128(*carry);
        let (x_hi, x_lo) = split_u128(b_lo * c_lo + a_lo + carry_lo);
        let (y_hi, y_lo) = split_u128(b_lo * c_hi);
        let (z_hi, z_lo) = split_u128(b_hi * c_lo);
        // Brackets to allow better ILP
        let (r_hi, r_lo) = split_u128((x_hi + y_lo) + (z_lo + a_hi) + carry_hi);

        *carry = (b_hi * c_hi) + r_hi + y_hi + z_hi;

        combine_u128(r_lo, x_lo)
    }

    if c == 0 {
        return;
    }

    let mut carry = 0;

    debug_assert_eq!(acc.len(), 4);
    unroll! {
        for i in 0..2 {
            let a_index = i + from_index;
            acc[a_index] = mac_with_carry(acc[a_index], b[i], c, &mut carry);
        }
    }
    unroll! {
        for i in 0..2 {
            let a_index = i + from_index + 2;
            if a_index < 4 {
                let (a, b) = acc[a_index].overflowing_add(carry);
                acc[a_index] = a;
                carry = b as u128;
            }
        }
    }

    debug_assert!(carry == 0);
}

#[inline]
fn mul_reduce(this: &mut [u128; 2], by: &[u128; 2], modulus: &[u128; 2], inv: u128) {
    // The Montgomery reduction here is based on Algorithm 14.32 in
    // Handbook of Applied Cryptography
    // <http://cacr.uwaterloo.ca/hac/about/chap14.pdf>.

    let mut res = [0; 2 * 2];
    unroll! {
        for i in 0..2 {
            mac_digit(i, &mut res, by, this[i]);
        }
    }

    unroll! {
        for i in 0..2 {
            let k = inv.wrapping_mul(res[i]);
            mac_digit(i, &mut res, modulus, k);
        }
    }

    this.copy_from_slice(&res[2..]);
}
