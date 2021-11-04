use std::collections::HashMap;

use anyhow::{anyhow, bail, Error};
use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use syn::token;

use rvv_assembler::{Imm, Ivi, Ivv, Ivx, Uimm, VConfig, VInst, VReg, Vlmul, Vtypei, XReg};

use crate::ast::{
    BareFnArg, Block, Expression, FnArg, ItemFn, Pattern, ReturnType, Signature, Statement, Type,
    TypedExpression,
};

// =============================
// ==== impl ToTokens for T ====
// =============================

#[derive(Default)]
pub struct Registers {
    pub category: &'static str,
    pub max_number: u8,
    pub last_number: u8,
    // ident_name => (register_number, is_function_argument)
    pub mapping: HashMap<String, (u8, bool)>,
}

impl Registers {
    pub fn new(category: &'static str, max_number: u8) -> Registers {
        Registers {
            category,
            max_number,
            last_number: 0,
            mapping: HashMap::default(),
        }
    }

    pub fn next_register(&mut self) -> Option<u8> {
        if self.last_number < self.max_number {
            self.last_number += 1;
            let tmp_var_name = format!("__tmp_{}_var{}", self.category, self.last_number);
            self.mapping.insert(tmp_var_name, (self.last_number, false));
            return Some(self.last_number);
        }
        None
    }

    pub fn search_reg(&self, reg: u8) -> Option<(String, bool)> {
        for (name, (number, is_fn_arg)) in &self.mapping {
            if *number == reg {
                return Some((name.clone(), *is_fn_arg));
            }
        }
        None
    }

    pub fn get_reg(&self, var_name: &str) -> Result<(u8, bool), String> {
        self.mapping
            .get(var_name)
            .cloned()
            .ok_or_else(|| format!("Unrecognized {} variable name: {}", self.category, var_name))
    }

    pub fn insert(&mut self, var_name: String, value: (u8, bool)) {
        self.mapping.insert(var_name, value);
    }
}

#[derive(Default)]
pub struct CodegenContext {
    // vector registers
    v_registers: Registers,
    // general registers
    x_registers: Registers,

    var_regs: HashMap<String, u8>,
    // expr_id => register_number
    expr_values: HashMap<usize, u8>,
    // expr_id => ethereum_types::U256 variable name
    expr_names: HashMap<usize, String>,
    // FIXME: fill in current module
    // ident => (mutability, Type)
    variables: HashMap<syn::Ident, (bool, Box<Type>)>,

    // [When update v_config]
    //   1. When first vector instruction used update v_config and insert asm!()
    //   2. When vector config changed:
    //     * reset x_registers
    //     * dump all x register data to memory
    //     * update v_config and insert asm!()
    v_config: Option<VConfig>,
}

impl CodegenContext {
    pub fn new(variables: HashMap<syn::Ident, (bool, Box<Type>)>) -> CodegenContext {
        CodegenContext {
            v_registers: Registers::new("vector", 32),
            x_registers: Registers::new("general", 32),
            var_regs: HashMap::default(),
            expr_values: HashMap::default(),
            expr_names: HashMap::default(),
            variables,
            v_config: None,
        }
    }

    // Generate raw asm statements for top level expression
    fn gen_tokens(&mut self, expr: &TypedExpression, top_level: bool) -> TokenStream {
        let (left, op, right, is_assign) = match &expr.expr {
            Expression::AssignOp { left, op, right } => (left, op, right, true),
            Expression::Binary { left, op, right } => (left, op, right, false),
            _ => panic!("invalid top level expression: {:?}", expr),
        };
        if !top_level && is_assign {
            panic!("assign op in inner top level expression");
        }

        let mut tokens = TokenStream::new();

        if top_level {
            let left_type_name = left.type_name();
            let right_type_name = right.type_name();
            let left_type_name_str = left_type_name.as_ref().map(|s| s.as_str());
            let right_type_name_str = right_type_name.as_ref().map(|s| s.as_str());
            match (left_type_name_str, right_type_name_str) {
                (Some("U256"), Some("U256")) => {
                    let v_config = VConfig::Vsetvli {
                        rd: XReg::Zero,
                        rs1: XReg::T0,
                        vtypei: Vtypei::new(256, Vlmul::M1, true, true),
                    };
                    if self.v_config.as_ref() != Some(&v_config) {
                        self.v_config = Some(v_config.clone());
                        // vsetvli x0, t0, e256, m1, ta, ma
                        let [b0, b1, b2, b3] = VInst::VConfig(v_config.clone()).encode_bytes();
                        let ts = quote! {
                            unsafe {
                                asm!(
                                    "li t0, 1",  // AVL = 1
                                    ".byte {0}, {1}, {2}, {3}",
                                    const #b0, const #b1, const #b2, const #b3,
                                )
                            }
                        };
                        tokens.extend(Some(ts));
                    }
                }
                _ => {
                    left.to_tokens(&mut tokens, self);
                    op.to_tokens(&mut tokens);
                    right.to_tokens(&mut tokens, self);
                    return tokens;
                }
            }
        }

        for typed_expr in vec![left, right] {
            if let Some(var_ident) = typed_expr.expr.var_ident() {
                let var_name = var_ident.to_string();
                if let Some(vreg) = self.var_regs.get(&var_name) {
                    self.expr_values.insert(typed_expr.id, *vreg);
                } else {
                    // Load256
                    let vreg = self.v_registers.next_register().unwrap();
                    let [b0, b1, b2, b3] = VInst::VleV {
                        width: 256,
                        vd: VReg::from_u8(vreg),
                        rs1: XReg::T0,
                        vm: false,
                    }
                    .encode_bytes();
                    let ts = quote! {
                        unsafe {
                            asm!(
                                "mv t0, {0}",
                                ".byte {1}, {2}, {3}, {4}",
                                in(reg) #var_ident.to_le_bytes().as_ptr(),
                                const #b0, const #b1, const #b2, const #b3,
                            )
                        }
                    };
                    tokens.extend(Some(ts));
                    self.var_regs.insert(var_name, vreg);
                    self.expr_values.insert(typed_expr.id, vreg);
                }
            } else {
                let ts = self.gen_tokens(typed_expr, false);
                tokens.extend(Some(ts));
            }
        }

        match op {
            syn::BinOp::Add(_) => {
                let dvreg = self.v_registers.next_register().unwrap();
                let svreg1 = self.expr_values.get(&left.id).unwrap();
                let svreg2 = self.expr_values.get(&right.id).unwrap();
                let [b0, b1, b2, b3] = VInst::VaddVv(Ivv {
                    vd: VReg::from_u8(dvreg),
                    vs2: VReg::from_u8(*svreg2),
                    vs1: VReg::from_u8(*svreg1),
                    vm: false,
                })
                .encode_bytes();
                let ts = quote! {
                    unsafe {
                        asm!(
                            ".byte {0}, {1}, {2}, {3}",
                            const #b0, const #b1, const #b2, const #b3,
                        )
                    }
                };
                tokens.extend(Some(ts));
                self.expr_values.insert(expr.id, dvreg);
            }
            syn::BinOp::Sub(_) => {
                let dvreg = self.v_registers.next_register().unwrap();
                let svreg1 = self.expr_values.get(&left.id).unwrap();
                let svreg2 = self.expr_values.get(&right.id).unwrap();
                let [b0, b1, b2, b3] = VInst::VsubVv(Ivv {
                    vd: VReg::from_u8(dvreg),
                    vs2: VReg::from_u8(*svreg2),
                    vs1: VReg::from_u8(*svreg1),
                    vm: false,
                })
                .encode_bytes();
                let ts = quote! {
                    unsafe {
                        asm!(
                            ".byte {0}, {1}, {2}, {3}",
                            const #b0, const #b1, const #b2, const #b3,
                        )
                    }
                };
                tokens.extend(Some(ts));
                self.expr_values.insert(expr.id, dvreg);
            }
            syn::BinOp::Mul(_) => {
                let dvreg = self.v_registers.next_register().unwrap();
                let svreg1 = self.expr_values.get(&left.id).unwrap();
                let svreg2 = self.expr_values.get(&right.id).unwrap();
                let [b0, b1, b2, b3] = VInst::VmulVv(Ivv {
                    vd: VReg::from_u8(dvreg),
                    vs2: VReg::from_u8(*svreg2),
                    vs1: VReg::from_u8(*svreg1),
                    vm: false,
                })
                .encode_bytes();
                let ts = quote! {
                    unsafe {
                        asm!(
                            ".byte {0}, {1}, {2}, {3}",
                            const #b0, const #b1, const #b2, const #b3,
                        )
                    }
                };
                tokens.extend(Some(ts));
                self.expr_values.insert(expr.id, dvreg);
            }
            syn::BinOp::Rem(_) => {
                let dvreg = self.v_registers.next_register().unwrap();
                let svreg1 = self.expr_values.get(&left.id).unwrap();
                let svreg2 = self.expr_values.get(&right.id).unwrap();
                let [b0, b1, b2, b3] = VInst::VremuVv(Ivv {
                    vd: VReg::from_u8(dvreg),
                    vs2: VReg::from_u8(*svreg2),
                    vs1: VReg::from_u8(*svreg1),
                    vm: false,
                })
                .encode_bytes();
                let ts = quote! {
                    unsafe {
                        asm!(
                            ".byte {0}, {1}, {2}, {3}",
                            const #b0, const #b1, const #b2, const #b3,
                        )
                    }
                };
                tokens.extend(Some(ts));
                self.expr_values.insert(expr.id, dvreg);
            }
            syn::BinOp::Shl(_) => {
                unimplemented!()
            }
            syn::BinOp::Shr(_) => {
                unimplemented!()
            }
            _ => {
                unimplemented!()
            }
        }

        if top_level && !is_assign {
            let vreg = self.expr_values.get(&expr.id).unwrap();
            let [b0, b1, b2, b3] = VInst::VseV {
                width: 256,
                vs3: VReg::from_u8(*vreg),
                rs1: XReg::T0,
                vm: false,
            }
            .encode_bytes();
            tokens.extend(Some(quote! {
                let mut rvv_vector_out_buf = [0u8; 32];
                unsafe {
                    asm!(
                        "mv t0, {0}",
                        // This should be vse256
                        ".byte {1}, {2}, {3}, {4}",
                        in(reg) rvv_vector_out_buf.as_mut_ptr(),
                        const #b0, const #b1, const #b2, const #b3,
                    )
                };
                U256::from_le_bytes(&rvv_vector_out_buf)
            }));
            let mut rv = TokenStream::new();
            token::Brace::default().surround(&mut rv, |inner| {
                inner.extend(Some(tokens));
            });
            rv
        } else {
            tokens
        }
    }
}

pub trait ToTokenStream {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext);
    fn to_token_stream(&self, context: &mut CodegenContext) -> TokenStream {
        let mut tokens = TokenStream::new();
        self.to_tokens(&mut tokens, context);
        tokens
    }
    fn into_token_stream(self, context: &mut CodegenContext) -> TokenStream
    where
        Self: Sized,
    {
        self.to_token_stream(context)
    }
}

impl ToTokenStream for ReturnType {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        match self {
            ReturnType::Default => {}
            ReturnType::Type(ty) => {
                token::RArrow::default().to_tokens(tokens);
                ty.to_tokens(tokens, context);
            }
        }
    }
}
impl ToTokenStream for BareFnArg {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        if let Some(ident) = self.name.as_ref() {
            ident.to_tokens(tokens);
            token::Colon::default().to_tokens(tokens);
        }
        self.ty.to_tokens(tokens, context);
    }
}
impl ToTokenStream for Type {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        match self {
            Type::Array { elem, len } => {
                token::Bracket::default().surround(tokens, |inner| {
                    elem.to_tokens(inner, context);
                    token::Semi::default().to_tokens(inner);
                    len.to_tokens(inner, context);
                });
            }
            Type::BareFn { inputs, output } => {
                token::Fn::default().to_tokens(tokens);
                token::Paren::default().surround(tokens, |inner| {
                    for input in inputs {
                        input.to_tokens(inner, context);
                    }
                });
                output.to_tokens(tokens, context);
            }
            Type::Path(path) => {
                path.to_tokens(tokens);
            }
            Type::Reference {
                lifetime,
                mutability,
                elem,
            } => {
                token::And::default().to_tokens(tokens);
                if let Some(lifetime) = lifetime {
                    lifetime.to_tokens(tokens);
                }
                if *mutability {
                    token::Mut::default().to_tokens(tokens);
                }
                elem.to_tokens(tokens, context);
            }
            Type::Slice(ty) => {
                token::Bracket::default().surround(tokens, |inner| {
                    ty.to_tokens(inner, context);
                });
            }
            Type::Tuple(types) => token::Paren::default().surround(tokens, |inner| {
                for (idx, ty) in types.iter().enumerate() {
                    ty.to_tokens(inner, context);
                    if idx != types.len() - 1 {
                        token::Comma::default().to_tokens(inner);
                    }
                }
            }),
        }
    }
}
impl ToTokenStream for Pattern {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        match self {
            Pattern::Ident { mutability, ident } => {
                if *mutability {
                    token::Mut::default().to_tokens(tokens);
                }
                ident.to_tokens(tokens);
            }
            Pattern::Type { pat, ty } => {
                pat.to_tokens(tokens, context);
                token::Colon::default().to_tokens(tokens);
                ty.to_tokens(tokens, context);
            }
            Pattern::Range { lo, limits, hi } => {
                lo.to_tokens(tokens, context);
                match limits {
                    syn::RangeLimits::HalfOpen(inner) => {
                        inner.to_tokens(tokens);
                    }
                    syn::RangeLimits::Closed(inner) => {
                        inner.to_tokens(tokens);
                    }
                }
                hi.to_tokens(tokens, context);
            }
            Pattern::Path(path) => {
                path.to_tokens(tokens);
            }
            Pattern::Wild => {
                token::Underscore::default().to_tokens(tokens);
            }
        }
    }
}
impl ToTokenStream for Expression {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {}
}

impl ToTokenStream for TypedExpression {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        match &self.expr {
            Expression::Array(arr) => {
                arr.to_tokens(tokens);
            }
            // FIXME: NOT supported yet.
            // x = y + x;
            Expression::Assign { left, right } => {
                // === ASM ===
                // asm!("xxx");
                // asm!("xxx");
                // asm!("xxx");
                // asm!("xxx", in(reg) left.as_mut_ptr());
                // === Simulator ===
                // {
                //     x = #y.overflowing_add(#z).0
                // }

                // FIXME: use rvv assembler
                left.to_tokens(tokens, context);
                token::Eq::default().to_tokens(tokens);
                right.to_tokens(tokens, context);
            }
            // x += y;
            Expression::AssignOp { left, op, right } => {
                // asm!("xxx");
                // asm!("xxx");
                // asm!("xxx");
                // asm!("xxx", in(reg) left.as_mut_ptr());

                // FIXME: use rvv assembler
                tokens.extend(Some(context.gen_tokens(self, true)));
            }
            Expression::Binary { left, op, right } => {
                // {
                //     let rvv_vector_out: U256;
                //     asm!("xxx");
                //     asm!("xxx");
                //     asm!("xxx");
                //     asm!("xxx", in(reg) rvv_vector_out.as_mut_ptr());
                //     rvv_vector_out
                // }

                // FIXME: use rvv assembler
                tokens.extend(Some(context.gen_tokens(self, true)));
            }
            Expression::Call { func, args } => {
                func.to_tokens(tokens, context);
                token::Paren::default().surround(tokens, |inner| {
                    for (idx, ty) in args.iter().enumerate() {
                        ty.to_tokens(inner, context);
                        if idx != args.len() - 1 {
                            token::Comma::default().to_tokens(inner);
                        }
                    }
                });
            }
            Expression::MethodCall {
                receiver,
                method,
                args,
            } => {
                // FIXME: use rvv assembler (overflowing_add/overflowing_sub ...)
                receiver.to_tokens(tokens, context);
                token::Dot::default().to_tokens(tokens);
                method.to_tokens(tokens);
                token::Paren::default().surround(tokens, |inner| {
                    for (idx, ty) in args.iter().enumerate() {
                        ty.to_tokens(inner, context);
                        if idx != args.len() - 1 {
                            token::Comma::default().to_tokens(inner);
                        }
                    }
                });
            }
            Expression::Macro(mac) => {
                mac.to_tokens(tokens);
            }
            Expression::Unary { op, expr } => {
                op.to_tokens(tokens);
                expr.to_tokens(tokens, context);
            }
            Expression::Field { base, member } => {
                base.to_tokens(tokens, context);
                token::Dot::default().to_tokens(tokens);
                member.to_tokens(tokens);
            }
            Expression::Cast { expr, ty } => {
                expr.to_tokens(tokens, context);
                token::As::default().to_tokens(tokens);
                ty.to_tokens(tokens, context);
            }
            Expression::Repeat { expr, len } => {
                token::Bracket::default().surround(tokens, |inner| {
                    expr.to_tokens(inner, context);
                    token::Semi::default().to_tokens(inner);
                    len.to_tokens(inner, context);
                });
            }
            Expression::Lit(lit) => {
                lit.to_tokens(tokens);
            }
            Expression::Paren(expr) => {
                token::Paren::default().surround(tokens, |inner| {
                    expr.to_tokens(inner, context);
                });
            }
            Expression::Reference { mutability, expr } => {
                token::And::default().to_tokens(tokens);
                if *mutability {
                    token::Mut::default().to_tokens(tokens);
                }
                expr.to_tokens(tokens, context);
            }
            Expression::Index { expr, index } => {
                expr.to_tokens(tokens, context);
                token::Bracket::default().surround(tokens, |inner| {
                    index.to_tokens(inner, context);
                });
            }
            Expression::Path(path) => {
                path.to_tokens(tokens);
            }
            Expression::Break => {
                token::Break::default().to_tokens(tokens);
            }
            Expression::Continue => {
                token::Continue::default().to_tokens(tokens);
            }
            Expression::Return(expr_opt) => {
                token::Return::default().to_tokens(tokens);
                if let Some(expr) = expr_opt.as_ref() {
                    expr.to_tokens(tokens, context);
                }
            }
            Expression::Block(block) => {
                block.to_tokens(tokens, context);
            }
            Expression::If {
                cond,
                then_branch,
                else_branch,
            } => {
                token::If::default().to_tokens(tokens);
                cond.to_tokens(tokens, context);
                then_branch.to_tokens(tokens, context);
                if let Some(expr) = else_branch.as_ref() {
                    token::Else::default().to_tokens(tokens);
                    expr.to_tokens(tokens, context);
                }
            }
            Expression::Range { from, limits, to } => {
                if let Some(expr) = from.as_ref() {
                    expr.to_tokens(tokens, context);
                }
                match limits {
                    syn::RangeLimits::HalfOpen(inner) => {
                        inner.to_tokens(tokens);
                    }
                    syn::RangeLimits::Closed(inner) => {
                        inner.to_tokens(tokens);
                    }
                }
                if let Some(expr) = to.as_ref() {
                    expr.to_tokens(tokens, context);
                }
            }
            Expression::Loop(body) => {
                token::Loop::default().to_tokens(tokens);
                body.to_tokens(tokens, context);
            }
            Expression::ForLoop { pat, expr, body } => {
                token::For::default().to_tokens(tokens);
                pat.to_tokens(tokens, context);
                token::In::default().to_tokens(tokens);
                expr.to_tokens(tokens, context);
                body.to_tokens(tokens, context);
            }
        }
        if self.id == usize::max_value() {
            panic!("Current expression not assgined with an id: {:?}", self);
        }
    }
}
impl ToTokenStream for Statement {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        match self {
            Statement::Local { pat, init } => {
                token::Let::default().to_tokens(tokens);
                pat.to_tokens(tokens, context);
                token::Eq::default().to_tokens(tokens);
                init.to_tokens(tokens, context);
                token::Semi::default().to_tokens(tokens);
            }
            Statement::Expr(expr) => {
                expr.to_tokens(tokens, context);
            }
            Statement::Semi(expr) => {
                expr.to_tokens(tokens, context);
                token::Semi::default().to_tokens(tokens);
            }
        }
    }
}
impl ToTokenStream for Block {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        token::Brace::default().surround(tokens, |inner| {
            for stmt in &self.stmts {
                stmt.to_tokens(inner, context);
            }
        })
    }
}
impl ToTokenStream for FnArg {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        if self.mutability {
            token::Mut::default().to_tokens(tokens);
        }
        self.name.to_tokens(tokens);
        token::Colon::default().to_tokens(tokens);
        self.ty.to_tokens(tokens, context);
    }
}
impl ToTokenStream for Signature {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        token::Fn::default().to_tokens(tokens);
        self.ident.to_tokens(tokens);
        token::Paren::default().surround(tokens, |inner| {
            for (idx, input) in self.inputs.iter().enumerate() {
                // let mut #xxx = ethereum_types::U256::from_little_endian(&#var.to_le_bytes()[..]);
                // let mut #yyy = ethereum_types::U256::from_little_endian(&#var.to_le_bytes()[..]);
                input.to_tokens(inner, context);
                if idx != self.inputs.len() - 1 {
                    token::Comma::default().to_tokens(inner);
                }
            }
        });
        self.output.to_tokens(tokens, context);
    }
}
impl ToTokenStream for ItemFn {
    fn to_tokens(&self, tokens: &mut TokenStream, context: &mut CodegenContext) {
        self.vis.to_tokens(tokens);
        self.sig.to_tokens(tokens, context);
        self.block.to_tokens(tokens, context);
    }
}

#[cfg(test)]
mod test {
    use std::convert::TryFrom;

    use super::*;
    use crate::type_checker::{CheckerContext, TypeChecker};

    fn rvv_test(item: TokenStream) -> Result<TokenStream, Error> {
        let input: syn::ItemFn = syn::parse2(item).unwrap();
        let mut out = ItemFn::try_from(&input)?;
        let mut checker_context = CheckerContext::default();
        out.check_types(&mut checker_context)?;

        println!("[variables]: ");
        for (ident, (mutability, ty)) in &checker_context.variables {
            if *mutability {
                println!("  [mut {:6}] => {:?}", ident, ty);
            } else {
                println!("  [{:10}] => {:?}", ident, ty);
            }
        }
        println!("<< type checked >>");

        let mut tokens = TokenStream::new();
        let mut codegen_context = CodegenContext::new(checker_context.variables);
        out.to_tokens(&mut tokens, &mut codegen_context);
        println!("out: {:#?}", out);
        Ok(TokenStream::from(quote!(#tokens)))
    }

    #[test]
    fn test_simple() {
        let input = quote! {
            fn simple(x: u32, mut y: u64, z: &mut u64) -> u128 {
                let qqq: bool = false;
                let jjj: () = {
                    y += 3;
                };
                *z += 3;
                if z > 5 {
                    y = y * 6;
                } else {
                    y = y * 3;
                }
                y = y >> 1;
                for i in 0..6u32 {
                    if i == 3 {
                        continue;
                    }
                    z += 1;
                    if z > 6 {
                        break;
                    }
                }
                let rv = if z > 6 {
                    (x as u64) + y + *z
                } else {
                    (x as u64) * y + *z
                };
                (rv + 3) as u128
            }
        };
        let input_string = input.to_string();
        println!("[input ]: {}", input_string);
        let output = rvv_test(input).unwrap();
        let output_string = output.to_string();
        println!("[otuput]: {}", output_string);
        assert_eq!(input_string, output_string);
    }

    #[test]
    fn test_u256() {
        let input = quote! {
            fn comp_u256(x: U256, y: U256) -> U256 {
                let mut z: U256 = x + y * x;
                z = z + z;
                z
            }
        };
        println!("[input ]: {}", input);
        let output = rvv_test(input).unwrap();
        println!("[otuput]: {}", output);
    }
}