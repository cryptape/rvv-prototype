// extern crate alloc;
// use alloc::format;
// use ckb_std::syscalls::debug;

use super::constants::*;
use crate::arith::U256;
use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};
use rvv_asm::rvv_asm;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gfp(pub [u64; 4]);

pub const ONE: Gfp = Gfp([1, 0, 0, 0]);
pub const ZERO: Gfp = Gfp([0, 0, 0, 0]);

impl Gfp {
    // TODO: do we need a parallel version of exp?
    pub fn exp(&mut self, bits: &[u64; 4]) {
        let mut sum = [Gfp(RN1)];
        let mut power = [self.clone()];

        for w in bits {
            for bit in 0..64 {
                if (w >> bit) & 1 == 1 {
                    mul_mov(&mut sum, &power);
                }
                square(&mut power);
            }
        }
        mul_mov_scalar(&mut sum, &Gfp(R3));
        self.0 = sum[0].0;
    }

    pub fn invert(&mut self) {
        self.exp(&P_MINUS2)
    }

    pub fn sqrt(&mut self) {
        self.exp(&P_PLUS1_OVER4)
    }

    pub fn new_from_int64(x: i64) -> Self {
        if x >= 0 {
            Gfp([x as u64, 0, 0, 0])
        } else {
            let mut a = [Gfp([(-x) as u64, 0, 0, 0])];
            neg(&mut a);
            a[0].clone()
        }
    }

    pub fn set(&mut self, a: &Gfp) {
        self.0 = a.0;
    }
}

impl From<Gfp> for U256 {
    fn from(a: Gfp) -> Self {
        let mut arr = [a];
        normalize(&mut arr);
        arr[0].clone().into()
    }
}

// TODO: do we want to introduce transmute to:
// 1. Implement ops for reference types
// 2. Eliminate the clones in assignment ops
impl Add for Gfp {
    type Output = Gfp;

    fn add(self, a: Gfp) -> Gfp {
        let mut arr = [self];
        add_mov(&mut arr[..], &[a]);
        let [r] = arr;
        r
    }
}

impl Mul for Gfp {
    type Output = Gfp;

    fn mul(self, a: Gfp) -> Gfp {
        let mut arr = [self];
        mul_mov(&mut arr[..], &[a]);
        let [r] = arr;
        r
    }
}

impl Neg for Gfp {
    type Output = Gfp;

    fn neg(self) -> Gfp {
        let mut arr = [self];
        neg(&mut arr[..]);
        let [r] = arr;
        r
    }
}

impl Sub for Gfp {
    type Output = Gfp;

    fn sub(self, a: Gfp) -> Gfp {
        let mut arr = [self];
        sub_mov(&mut arr[..], &[a]);
        let [r] = arr;
        r
    }
}

impl AddAssign for Gfp {
    fn add_assign(&mut self, other: Gfp) {
        let mut arr = [self.clone()];
        add_mov(&mut arr[..], &[other]);
        self.0 = arr[0].0;
    }
}

impl SubAssign for Gfp {
    fn sub_assign(&mut self, other: Gfp) {
        let mut arr = [self.clone()];
        sub_mov(&mut arr[..], &[other]);
        self.0 = arr[0].0;
    }
}

/// WARNING: all functions involving inline rvv assembly must explicitly be
/// marked with `#[inlnie(never)]`!!!!!!!
/// We have noticed errors when multiple functions here are called together,
/// inlining them would lead to the compiler optimizing away certain operations.
/// A more proper way should be adding memory barriers, until we can find the
/// correct way for inserting memory barriers, we have to mark then as non-inlinable.
#[inline(never)]
pub fn mul_mov(dst: &mut [Gfp], src: &[Gfp]) {
    debug_assert_eq!(dst.len(), src.len());

    // let debug_val = [0u64; 1024];

    unsafe {
        // 4 registers as a group, that gives us 8 free v registers to use
        // t1: vl
        // t2: remaining element length
        // t3/t4: destination/source address variables
        // t5: free variable
        // Only v0, v4, v8, v16, v24 and v28 are used. v8/v16 can be used as
        // 8-register group, rest are only used as 4-register group
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "mv t4, {src}",
            "1:",
            "vsetvli t1, t2, e256, m4",
            // Load np => v24, p2 => v28
            "mv t5, {np}",
            "vlse256.v v24, (t5), x0",
            "mv t5, {p2}",
            "vlse256.v v28, (t5), x0",
            // Load operands
            "vle256.v v0, (t3)",
            "vle256.v v4, (t4)",
            // T = mul(a, b) => v8
            "vwmulu.vv v8, v0, v4",
            // Extract T[0..4] => v0
            "vnsrl.wx v0, v8, x0",
            // m = halfMul(T[0..4], np) => v4
            "vmul.vv v4, v0, v24",
            // t = mul(m, p2)=> v16
            "vwmulu.vv v16, v4, v28",
            // c = t + T = > v8, with carry in v0
            // Temporarily enlarging vlen to deal with bigger adds
            "vsetvli t1, t1, e512, m4",
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            "vsetvli t1, t1, e256, m4",
            // Extract c[4..8] => v4
            "li t5, 256",
            "vnsrl.wx v4, v8, t5",
            // gfpCarry using v4 in c[4..8], with carry in v0
            // c[4..8] - p2 => v16, with carry in v8
            "vmsbc.vv v8, v4, v28",
            "vsub.vv v16, v4, v28",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v4, otherwise use value in v16
            "vmerge.vvm v4, v16, v4, v0",
            // Store result
            "vse256.v v4, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "add t4, t4, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            np = in (reg) NP.as_ptr(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
            src = in (reg) src.as_ptr(),
            // debug_val = in (reg) debug_val.as_ptr(),
        );
    }
    // debug(format!("debug_val: {:?}", debug_val));
}

#[inline(never)]
pub fn square(dst: &mut [Gfp]) {
    unsafe {
        // 4 registers as a group, that gives us 8 free v registers to use
        // t1: vl
        // t2: remaining element length
        // t3: destination/source address variables
        // t5: free variable
        // Only v0, v4, v8, v16, v24 and v28 are used. v8/v16 can be used as
        // 8-register group, rest are only used as 4-register group
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "1:",
            "vsetvli t1, t2, e256, m4",
            // Load np => v24, p2 => v28
            "mv t5, {np}",
            "vlse256.v v24, (t5), x0",
            "mv t5, {p2}",
            "vlse256.v v28, (t5), x0",
            // Load operands
            "vle256.v v0, (t3)",
            "vle256.v v4, (t3)",
            // T = mul(a, b) => v8
            "vwmulu.vv v8, v0, v4",
            // Extract T[0..4] => v0
            "vnsrl.wx v0, v8, x0",
            // m = halfMul(T[0..4], np) => v4
            "vmul.vv v4, v0, v24",
            // t = mul(m, p2)=> v16
            "vwmulu.vv v16, v4, v28",
            // c = t + T = > v8, with carry in v0
            // Temporarily enlarging vlen to deal with bigger adds
            "vsetvli t1, t1, e512, m4",
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            "vsetvli t1, t1, e256, m4",
            // Extract c[4..8] => v4
            "li t5, 256",
            "vnsrl.wx v4, v8, t5",
            // gfpCarry using v4 in c[4..8], with carry in v0
            // c[4..8] - p2 => v16, with carry in v8
            "vmsbc.vv v8, v4, v28",
            "vsub.vv v16, v4, v28",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v4, otherwise use value in v16
            "vmerge.vvm v4, v16, v4, v0",
            // Store result
            "vse256.v v4, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            np = in (reg) NP.as_ptr(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn mul_mov_scalar(dst: &mut [Gfp], src: &Gfp) {
    unsafe {
        // 4 registers as a group, that gives us 8 free v registers to use
        // t1: vl
        // t2: remaining element length
        // t3/t4: destination/source address variables
        // t5: free variable
        // Only v0, v4, v8, v16, v24 and v28 are used. v8/v16 can be used as
        // 8-register group, rest are only used as 4-register group
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "mv t4, {src}",
            "1:",
            "vsetvli t1, t2, e256, m4",
            // Load np => v24, p2 => v28
            "mv t5, {np}",
            "vlse256.v v24, (t5), x0",
            "mv t5, {p2}",
            "vlse256.v v28, (t5), x0",
            // Load operands
            "vle256.v v0, (t3)",
            "vlse256.v v4, (t4), x0",
            // T = mul(a, b) => v8
            "vwmulu.vv v8, v0, v4",
            // Extract T[0..4] => v0
            "vnsrl.wx v0, v8, x0",
            // m = halfMul(T[0..4], np) => v4
            "vmul.vv v4, v0, v24",
            // t = mul(m, p2)=> v16
            "vwmulu.vv v16, v4, v28",
            // c = t + T = > v8, with carry in v0
            // Temporarily enlarging vlen to deal with bigger adds
            "vsetvli t1, t1, e512, m4",
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            "vsetvli t1, t1, e256, m4",
            // Extract c[4..8] => v4
            "li t5, 256",
            "vnsrl.wx v4, v8, t5",
            // gfpCarry using v4 in c[4..8], with carry in v0
            // c[4..8] - p2 => v16, with carry in v8
            "vmsbc.vv v8, v4, v28",
            "vsub.vv v16, v4, v28",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v4, otherwise use value in v16
            "vmerge.vvm v4, v16, v4, v0",
            // Store result
            "vse256.v v4, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            np = in (reg) NP.as_ptr(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
            src = in (reg) src as *const Gfp,
        );
    }
}

#[inline(never)]
pub fn add_mov(dst: &mut [Gfp], src: &[Gfp]) {
    debug_assert_eq!(dst.len(), src.len());

    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3/t4: destination/source address variables
        // t5: free variable
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "mv t4, {src}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load operands
            "vle256.v v8, (t3)",
            "vle256.v v16, (t4)",
            // Add operands together
            // c = a + b => v8, with carry in v0
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            // gfpCarry on c
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // c - p2 => v24, with carry in v16
            "vmsbc.vv v16, v8, v24",
            "vsub.vv v24, v8, v24",
            // Combine carries
            "vmandnot.mm v0, v16, v0",
            // Select value, if carry is 1, use value in v8 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v8, v0",
            // Store result
            "vse256.v v8, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "add t4, t4, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
            src = in (reg) src.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn double(dst: &mut [Gfp]) {
    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3: destination/source address variables
        // t5: free variable
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load operands
            "vle256.v v8, (t3)",
            "vle256.v v16, (t3)",
            // Add operands together
            // c = a + b => v8, with carry in v0
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            // gfpCarry on c
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // c - p2 => v24, with carry in v16
            "vmsbc.vv v16, v8, v24",
            "vsub.vv v24, v8, v24",
            // Combine carries
            "vmandnot.mm v0, v16, v0",
            // Select value, if carry is 1, use value in v8 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v8, v0",
            // Store result
            "vse256.v v8, (t3)",
            // Update t2/t3, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn sub_mov(dst: &mut [Gfp], src: &[Gfp]) {
    debug_assert_eq!(dst.len(), src.len());

    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3/t4: destination/source address variables
        // t5: free variable
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "mv t4, {src}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // Load a into v8, b into v16
            "vle256.v v8, (t3)",
            "vle256.v v16, (t4)",
            // d = p2 - b => v16, carry is ignored
            "vsub.vv v16, v24, v16",
            // c = a + d => v16, with carry in v0
            "vmadc.vv v0, v8, v16",
            "vadd.vv v16, v8, v16",
            // gfpCarry on c with carry
            // c - p2 => v24, with carry in v8
            "vmsbc.vv v8, v16, v24",
            "vsub.vv v24, v16, v24",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v16 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v16, v0",
            // Store result
            "vse256.v v8, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "add t4, t4, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
            src = in (reg) src.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn neg(dst: &mut [Gfp]) {
    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3: destination/source address variables
        // t5: free variable
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // Load operand d into v8
            "vle256.v v8, (t3)",
            // c = p2 - d => v16, carry is cleared in v0
            "vsub.vv v16, v24, v8",
            "vmxor.mm v0, v0, v0",
            // gfpCarry on c with carry
            // c - p2 => v24, with carry in v8
            "vmsbc.vv v8, v16, v24",
            "vsub.vv v24, v16, v24",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v16 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v16, v0",
            // Store result
            "vse256.v v8, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
        );
    }
}

/// Some input values might be larger than p, this normalizes the value so they
/// remain regular
#[inline(never)]
pub fn normalize(dst: &mut [Gfp]) {
    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3: destination/source address variables
        // t5: free variable
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {dst}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load operand c
            "vle256.v v8, (t3)",
            // Clear mask
            "vmxor.mm v0, v0, v0",
            // gfpCarry
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // c - p2 => v24, with carry in v16
            "vmsbc.vv v16, v8, v24",
            "vsub.vv v24, v8, v24",
            // Combine carries
            "vmandnot.mm v0, v16, v0",
            // Select value, if carry is 1, use value in v8 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v8, v0",
            // Store result
            "vse256.v v8, (t3)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "blt x0, t2, 1b",
            len = in (reg) dst.len(),
            p2 = in (reg) P2.as_ptr(),
            dst = in (reg) dst.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn mul(a: &[Gfp], b: &[Gfp], c: &mut [Gfp]) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert_eq!(b.len(), c.len());

    unsafe {
        // 4 registers as a group, that gives us 8 free v registers to use
        // t1: vl
        // t2: remaining element length
        // t3/t4: source address variables
        // t5: free variable
        // t6: destination variable
        // Only v0, v4, v8, v16, v24 and v28 are used. v8/v16 can be used as
        // 8-register group, rest are only used as 4-register group
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {a}",
            "mv t4, {b}",
            "mv t6, {c}",
            "1:",
            "vsetvli t1, t2, e256, m4",
            // Load np => v24, p2 => v28
            "mv t5, {np}",
            "vlse256.v v24, (t5), x0",
            "mv t5, {p2}",
            "vlse256.v v28, (t5), x0",
            // Load operands
            "vle256.v v0, (t3)",
            "vle256.v v4, (t4)",
            // T = mul(a, b) => v8
            "vwmulu.vv v8, v0, v4",
            // Extract T[0..4] => v0
            "vnsrl.wx v0, v8, x0",
            // m = halfMul(T[0..4], np) => v4
            "vmul.vv v4, v0, v24",
            // t = mul(m, p2)=> v16
            "vwmulu.vv v16, v4, v28",
            // c = t + T = > v8, with carry in v0
            // Temporarily enlarging vlen to deal with bigger adds
            "vsetvli t1, t1, e512, m4",
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            "vsetvli t1, t1, e256, m4",
            // Extract c[4..8] => v4
            "li t5, 256",
            "vnsrl.wx v4, v8, t5",
            // gfpCarry using v4 in c[4..8], with carry in v0
            // c[4..8] - p2 => v16, with carry in v8
            "vmsbc.vv v8, v4, v28",
            "vsub.vv v16, v4, v28",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v4, otherwise use value in v16
            "vmerge.vvm v4, v16, v4, v0",
            // Store result
            "vse256.v v4, (t6)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "add t4, t4, t1",
            "add t6, t6, t1",
            "blt x0, t2, 1b",
            len = in (reg) a.len(),
            np = in (reg) NP.as_ptr(),
            p2 = in (reg) P2.as_ptr(),
            a = in (reg) a.as_ptr(),
            b = in (reg) b.as_ptr(),
            c = in (reg) c.as_ptr(),
            // debug_val = in (reg) debug_val.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn add(a: &[Gfp], b: &[Gfp], c: &mut [Gfp]) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert_eq!(b.len(), c.len());

    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3/t4: source address variables
        // t5: free variable
        // t6: destination address
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {a}",
            "mv t4, {b}",
            "mv t6, {c}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load operands
            "vle256.v v8, (t3)",
            "vle256.v v16, (t4)",
            // Add operands together
            // c = a + b => v8, with carry in v0
            "vmadc.vv v0, v8, v16",
            "vadd.vv v8, v8, v16",
            // gfpCarry on c
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // c - p2 => v24, with carry in v16
            "vmsbc.vv v16, v8, v24",
            "vsub.vv v24, v8, v24",
            // Combine carries
            "vmandnot.mm v0, v16, v0",
            // Select value, if carry is 1, use value in v8 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v8, v0",
            // Store result
            "vse256.v v8, (t6)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "add t4, t4, t1",
            "add t6, t6, t1",
            "blt x0, t2, 1b",
            len = in (reg) a.len(),
            p2 = in (reg) P2.as_ptr(),
            a = in (reg) a.as_ptr(),
            b = in (reg) b.as_ptr(),
            c = in (reg) c.as_ptr(),
        );
    }
}

#[inline(never)]
pub fn sub(a: &[Gfp], b: &[Gfp], c: &mut [Gfp]) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert_eq!(b.len(), c.len());

    unsafe {
        // 8 registers as a group since add is simple and can do with less
        // registers
        // t1: vl
        // t2: remaining element length
        // t3/t4: source address variables
        // t5: free variable
        // t6: destination address
        // v0, v8, v16, v24 are used.
        rvv_asm!(
            "mv t2, {len}",
            "mv t3, {a}",
            "mv t4, {b}",
            "mv t6, {c}",
            "1:",
            "vsetvli t1, t2, e256, m8",
            // Load p2 into v24
            "mv t5, {p2}",
            "vlse256.v v24, (t5), x0",
            // Load a into v8, b into v16
            "vle256.v v8, (t3)",
            "vle256.v v16, (t4)",
            // d = p2 - b => v16, carry is ignored
            "vsub.vv v16, v24, v16",
            // c = a + d => v16, with carry in v0
            "vmadc.vv v0, v8, v16",
            "vadd.vv v16, v8, v16",
            // gfpCarry on c with carry
            // c - p2 => v24, with carry in v8
            "vmsbc.vv v8, v16, v24",
            "vsub.vv v24, v16, v24",
            // Combine carries
            "vmandnot.mm v0, v8, v0",
            // Select value, if carry is 1, use value in v16 (c),
            // otherwise use value in v24 (c - p2)
            "vmerge.vvm v8, v24, v16, v0",
            // Store result
            "vse256.v v8, (t6)",
            // Update t2/t3/t4, start the next loop if required, t2 contains the count
            // of elements, so we do substraction using value in t1 directly.
            "sub t2, t2, t1",
            // t3/t4, on the other hand, stores the address, we will need to consider
            // element length asl well. A single element is 32 bytes, a shift left
            // by 5 on t1 will do the task
            "slli t1, t1, 5",
            "add t3, t3, t1",
            "add t4, t4, t1",
            "add t6, t6, t1",
            "blt x0, t2, 1b",
            len = in (reg) a.len(),
            p2 = in (reg) P2.as_ptr(),
            a = in (reg) a.as_ptr(),
            b = in (reg) b.as_ptr(),
            c = in (reg) c.as_ptr(),
        );
    }
}

pub fn mont_encode(dst: &mut [Gfp]) {
    mul_mov_scalar(dst, &Gfp(R2));
}

pub fn mont_decode(dst: &mut [Gfp]) {
    mul_mov_scalar(dst, &Gfp([1, 0, 0, 0]));
}
