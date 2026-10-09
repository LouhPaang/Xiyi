// elaborate.rs
use crate::hir::*;
use crate::ast::{BinaryOp, Literal, Pattern, Type, UnaryOp, IfKind, UnsafeKind};

/// 从容器类型推导"元素类型"
/// 支持 Vec<T>、&[T]、Option<T> 等单参数泛型，及切片和引用
fn extract_elem_type(ty: &Type) -> Type {
    match ty {
        Type::Generic(name, args) if args.len() == 1 => args[0].clone(),
        Type::Ref { inner, .. } => extract_elem_type(inner),
        Type::Slice(inner) => (**inner).clone(),
        // 这条路径意味着 for 循环的迭代对象根本不是已知的容器形状（sema
        // 应该已经拦住了这种情况，走到这里说明两边检查不一致）。用
        // Unit 权充元素类型会把一个本该报错的情况悄悄伪装成合法结果；
        // Never 才如实表达"这里推不出真正的元素类型"——它能跟任何类型
        // 统一，不会在下游制造新的假类型错误。
        _ => Type::Never,
    }
}

// ===== 常用 Pattern 构造 helper =====
//
// expand_for / expand_try 里反复出现 `Pattern::EnumVariantWithBinding {
// enum_name: "Option".to_string(), variant_name: "Some".to_string(), ... }`
// 这种样板，抽成四个小函数，调用点直接读出"这是 Some/None/Ok/Err 模式"，
// 不用每次都在一堆字符串字面量里确认 enum_name/variant_name 有没有抄对。

fn some_pattern(binding: &str) -> Pattern {
    Pattern::EnumVariantWithBinding {
        enum_name: "Option".to_string(),
        variant_name: "Some".to_string(),
        binding: binding.to_string(),
    }
}

fn none_pattern() -> Pattern {
    Pattern::EnumVariant {
        enum_name: "Option".to_string(),
        variant_name: "None".to_string(),
    }
}

fn ok_pattern(binding: &str) -> Pattern {
    Pattern::EnumVariantWithBinding {
        enum_name: "Result".to_string(),
        variant_name: "Ok".to_string(),
        binding: binding.to_string(),
    }
}

fn err_pattern(binding: &str) -> Pattern {
    Pattern::EnumVariantWithBinding {
        enum_name: "Result".to_string(),
        variant_name: "Err".to_string(),
        binding: binding.to_string(),
    }
}

// ===== 构造 HirExpr 的 helper =====
//
// elaborate 阶段生成的 HirExpr 绝大多数是编译器合成的节点：没有真实的
// 隐私标签（privacy_tag: None）、sensitivity 恒为 Unknown。这几个函数
// 只是把这几个重复字段收口到一处。它们都不读写 ElaborateContext 的状态
// （return_ty / temp_counter）——之前挂在 ElaborateContext 的 impl 块
// 下面、接收 &self，但函数体里一次都没用到 self，&self 纯粹是摆设。
// 现在搬成模块级自由函数：调用点从 ctx.mk_xxx(...) 变成 mk_xxx(...)，
// ElaborateContext 从"什么都装"收缩成真正只管"当前展开到哪了"这一件事。

fn mk_expr(kind: HirExprKind, ty: Type, effects: EffectSet, span: Span) -> HirExpr {
    HirExpr {
        kind,
        ty,
        privacy_tag: None,
        sensitivity: Sensitivity::Unknown,
        effects,
        span,
    }
}

fn mk_expr_default(kind: HirExprKind, ty: Type, span: Span) -> HirExpr {
    mk_expr(kind, ty, EffectSet::default(), span)
}

fn mk_call(func: &str, args: Vec<HirCallArg>, ty: Type, span: Span) -> HirExpr {
    mk_expr_default(
        HirExprKind::Call {
            qualifier: None,
            func: func.to_string(),
            generic_args: Vec::new(),
            args,
            is_method: true,
        },
        ty,
        span,
    )
}

fn mk_ident(name: &str, ty: Type, span: Span) -> HirExpr {
    mk_expr_default(HirExprKind::Ident(name.to_string()), ty, span)
}

/// `Ok(temp) => temp` / `Some(temp) => temp` 这类分支：把被匹配值原样
/// 绑定到新名字上。跟 mk_ident 的区别是这里必须原样继承被匹配表达式
/// 的类型、隐私标签、effects——它绑定的是一个真实的值，不是凭空合成
/// 出来的临时量，直接套 mk_ident（privacy_tag 恒 None、effects 恒
/// default）会把这些信息悄悄丢掉。
fn mk_bound_ident(name: &str, from: &HirExpr) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Ident(name.to_string()),
        ty: from.ty.clone(),
        privacy_tag: from.privacy_tag.clone(),
        sensitivity: Sensitivity::Unknown,
        effects: from.effects.clone(),
        span: from.span,
    }
}

/// expand_for 的 `None => break`、expand_try 的 `Err(e) => return
/// Err(e.into())` / `None => return None` 都是同一个模式："用一个
/// Block 包一条发散语句（break/return），把这条分支的类型标成
/// Never"。
fn mk_diverging_block(stmts: Vec<HirStmt>, span: Span) -> HirExpr {
    mk_expr_default(HirExprKind::Block(HirBlock { stmts, span }), Type::Never, span)
}

// ===== `?` 操作符：Result 还是 Option =====
//
// expand_try 原来要先调 supports_try() 问"能不能用"，再调
// is_result()/is_option() 问"该走哪条路"——这是同一件事（认出
// return_ty 的形状）被问了两遍。合并成一个返回值：None 就是"不能
// 用"，Some(kind) 同时回答了"能用"和"走哪条路"。
#[derive(Clone, Copy, PartialEq, Eq)]
enum TryKind {
    Result,
    Option,
}

// ===== 展开器上下文 =====
//
// 收缩之后，这里只剩两件事：当前函数的返回类型（决定 `?` 怎么展开），
// 和临时变量计数器（保证 __elab_N 不重名）。

struct ElaborateContext {
    return_ty: Option<Type>,
    temp_counter: usize,
}

impl ElaborateContext {
    fn new(return_ty: Option<Type>) -> Self {
        Self {
            return_ty,
            temp_counter: 0,
        }
    }

    /// 生成一个唯一的临时变量名
    fn next_temp(&mut self) -> String {
        let id = self.temp_counter;
        self.temp_counter += 1;
        format!("__elab_{}", id)
    }

    /// `?` 能不能用、该按哪条路展开，只取决于 return_ty 的形状。
    fn try_kind(&self) -> Option<TryKind> {
        match &self.return_ty {
            Some(Type::Generic(name, _)) if name == "Result" => Some(TryKind::Result),
            Some(Type::Generic(name, _)) if name == "Option" => Some(TryKind::Option),
            _ => None,
        }
    }

    /// 获取错误类型参数（用于 `?` 的 `Err(e) => return Err(e.into())`）
    /// 如果返回类型是 `Result<T, E>`，返回 `E`；否则返回 `Type::Never`
    /// ——这个分支只在 try_kind() 已经确认 return_ty 是
    /// `Some(Generic("Result", _))` 之后、args 长度却不是 2 时才会走到，
    /// 属于"类型系统已经检查过、理论上不该发生"的防御性兜底，用 Never
    /// 如实标记，不用 Unit 假装这是一个正常情况。
    fn error_type(&self) -> Type {
        match &self.return_ty {
            Some(Type::Generic(name, args)) if name == "Result" && args.len() == 2 => {
                args[1].clone()
            }
            _ => Type::Never,
        }
    }
}

// ===== 展开器主结构体 =====

pub struct Elaborate;

impl Elaborate {
    pub fn elaborate(program: HirProgram) -> Result<HirProgram, String> {
        let mut ctx = ElaborateContext::new(None);
        Self::elaborate_program(program, &mut ctx)
    }

    // ===== 顶层遍历 =====

    fn elaborate_program(mut program: HirProgram, ctx: &mut ElaborateContext) -> Result<HirProgram, String> {
        // fns / models 里的函数 / impls 里的函数，三处原来是结构相同的
        // for 循环，各自手动 push 进一个新 Vec。抽成 elaborate_inputs
        // 后，三处都只是"取出函数列表 -> elaborate_inputs -> 塞回去"。
        program.fns = Self::elaborate_inputs(program.fns, ctx)?;

        program.models = program
            .models
            .into_iter()
            .map(|m| {
                Ok(HirModel {
                    functions: Self::elaborate_inputs(m.functions, ctx)?,
                    ..m
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        program.impls = program
            .impls
            .into_iter()
            .map(|imp| {
                Ok(HirImplement {
                    functions: Self::elaborate_inputs(imp.functions, ctx)?,
                    ..imp
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        // interface 方法无 body，consts 的值暂不展开
        Ok(program)
    }

    /// 对一组函数逐个 elaborate_fn，中途任何一个出错就整体短路返回。
    fn elaborate_inputs(fns: Vec<HirFn>, ctx: &mut ElaborateContext) -> Result<Vec<HirFn>, String> {
        fns.into_iter().map(|f| Self::elaborate_fn(f, ctx)).collect()
    }

    // ===== 函数级展开 =====

    fn elaborate_fn(mut f: HirFn, ctx: &mut ElaborateContext) -> Result<HirFn, String> {
        let mut fn_ctx = ElaborateContext::new(f.return_type.clone());
        let body = Self::elaborate_block(f.body, &mut fn_ctx)?;
        f.body = body;
        Ok(f)
    }

    // ===== 块级展开 =====

    fn elaborate_block(mut block: HirBlock, ctx: &mut ElaborateContext) -> Result<HirBlock, String> {
        block.stmts = block
            .stmts
            .into_iter()
            .map(|s| Self::elaborate_stmt(s, ctx))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(block)
    }

    // ===== 语句级展开 =====

    fn elaborate_stmt(stmt: HirStmt, ctx: &mut ElaborateContext) -> Result<HirStmt, String> {
        match stmt {
            HirStmt::For { var, iterable, body, span } => {
                Self::expand_for(var, iterable, body, span, ctx)
            }

            HirStmt::Let { name, ty, init, mutable, persist, span } => {
                let init = Self::elaborate_expr(init, ctx)?;
                Ok(HirStmt::Let { name, ty, init, mutable, persist, span })
            }
            HirStmt::Expr { expr, span } => {
                let expr = Self::elaborate_expr(expr, ctx)?;
                Ok(HirStmt::Expr { expr, span })
            }
            HirStmt::Return { expr, span } => {
                let expr = expr.map(|e| Self::elaborate_expr(e, ctx)).transpose()?;
                Ok(HirStmt::Return { expr, span })
            }
            HirStmt::While { cond, body, span } => {
                let cond = Self::elaborate_expr(cond, ctx)?;
                let body = Self::elaborate_block(body, ctx)?;
                Ok(HirStmt::While { cond, body, span })
            }
            HirStmt::Assign { target, expr, span } => {
                let target = Self::elaborate_expr(*target, ctx)?;
                let expr = Self::elaborate_expr(expr, ctx)?;
                Ok(HirStmt::Assign { target: Box::new(target), expr, span })
            }
            HirStmt::Loop { body, span } => {
                let body = Self::elaborate_block(body, ctx)?;
                Ok(HirStmt::Loop { body, span })
            }
            HirStmt::Break { span } => Ok(HirStmt::Break { span }),
            HirStmt::Continue { span } => Ok(HirStmt::Continue { span }),
            HirStmt::UnsafeBlock { kind, body, span } => {
                let body = Self::elaborate_block(body, ctx)?;
                Ok(HirStmt::UnsafeBlock { kind, body, span })
            }
        }
    }

    // ===== for 循环展开 =====
    //
    // 拆成 build_into_iter_let / build_for_next_match 两个子步骤后，
    // expand_for 本身只剩"组装"逻辑，每一步在干什么一眼看得出来。

    fn expand_for(
        var: String,
        iterable: HirExpr,
        body: HirBlock,
        span: Span,
        ctx: &mut ElaborateContext,
    ) -> Result<HirStmt, String> {
        let iter_var = ctx.next_temp();
        let elem_ty = extract_elem_type(&iterable.ty);
        let iterable_ty = iterable.ty.clone();
        let iterable_effects = iterable.effects.clone();

        // 1-2. let __iter = iterable.into_iter();
        let let_stmt = Self::build_into_iter_let(iter_var.clone(), iterable, span.clone());

        // 3-6. match __iter.next() { Some(var) => { body }, None => { break } }
        let (match_expr, match_effects) = Self::build_for_next_match(
            &iter_var,
            elem_ty,
            var,
            body,
            iterable_ty,
            iterable_effects,
            span.clone(),
        );

        // 7. loop { match ... }
        let loop_block = HirBlock {
            stmts: vec![HirStmt::Expr { expr: match_expr, span: span.clone() }],
            span: span.clone(),
        };
        let loop_stmt = HirStmt::Loop {
            body: loop_block,
            span: span.clone(),
        };

        // 8. 追加 Unit 表达式，强制块类型为 ()
        let unit_expr = mk_expr_default(HirExprKind::Literal(Literal::Unit), Type::Unit, span.clone());
        let block = HirBlock {
            stmts: vec![let_stmt, loop_stmt, HirStmt::Expr { expr: unit_expr, span: span.clone() }],
            span: span.clone(),
        };

        // 外层块的整体副作用 = let_stmt(无) + match_effects + unit(无)
        Ok(HirStmt::Expr {
            expr: mk_expr(HirExprKind::Block(block), Type::Unit, match_effects, span.clone()),
            span,
        })
    }

    fn build_into_iter_let(iter_var: String, iterable: HirExpr, span: Span) -> HirStmt {
        let ty = iterable.ty.clone();
        let effects = iterable.effects.clone();
        let into_iter_call = mk_expr(
            HirExprKind::Call {
                qualifier: None,
                func: "into_iter".to_string(),
                generic_args: Vec::new(),
                args: vec![HirCallArg::Positional(iterable)],
                is_method: true,
            },
            ty,
            effects,
            span.clone(),
        );
        HirStmt::Let {
            name: iter_var,
            ty: None,
            init: into_iter_call,
            mutable: true,
            persist: false,
            span,
        }
    }

    fn build_for_next_match(
        iter_var: &str,
        elem_ty: Type,
        var: String,
        body: HirBlock,
        iterable_ty: Type,
        iterable_effects: EffectSet,
        span: Span,
    ) -> (HirExpr, EffectSet) {
        // elem_ty 后面 match_expr 的类型还要再用一次，Type 没有 derive
        // Copy，这里不 clone 的话第二次用就是 "use of moved value"
        // （E0382）。
        let next_call = mk_expr(
            HirExprKind::Call {
                qualifier: None,
                func: "next".to_string(),
                generic_args: Vec::new(),
                args: vec![HirCallArg::Positional(mk_ident(iter_var, iterable_ty, span.clone()))],
                is_method: true,
            },
            Type::Generic("Option".to_string(), vec![elem_ty.clone()]),
            // 继续传播迭代对象自身的副作用
            iterable_effects,
            span.clone(),
        );
        let next_effects = next_call.effects.clone();

        let body_effects = Self::collect_effects_from_block(&body);
        let some_arm = HirMatchArm {
            pattern: some_pattern(&var),
            expr: mk_expr(HirExprKind::Block(body), Type::Unit, body_effects.clone(), span.clone()),
        };

        // `None => { break }`：break 直接跳出外层 loop，不会把控制流
        // 交回这条 match 分支、也不会产出一个值，标成 Never 才是诚实的
        // 表达（跟 some_arm 真实的 Unit 不是"碰巧类型一样"，而是一个
        // 发散、一个正常完成）。
        let break_expr = mk_diverging_block(vec![HirStmt::Break { span: span.clone() }], span.clone());

        // 先用 break_expr.effects 算出 match_effects，再把 break_expr
        // 本体移进 none_arm——顺序对了就不需要为了"还要再读一次
        // effects"而提前 clone 整个 HirExpr。
        let match_effects = EffectSet::merge(&[&next_effects, &body_effects, &break_expr.effects]);
        let none_arm = HirMatchArm {
            pattern: none_pattern(),
            expr: break_expr,
        };

        let match_expr = mk_expr(
            HirExprKind::Match {
                cond: Box::new(next_call),
                arms: vec![some_arm, none_arm],
            },
            Type::Generic("Option".to_string(), vec![elem_ty]),
            match_effects.clone(),
            span,
        );
        (match_expr, match_effects)
    }

    // ===== 表达式级展开 =====

    fn elaborate_expr(mut expr: HirExpr, ctx: &mut ElaborateContext) -> Result<HirExpr, String> {
        match expr.kind {
            // ---- ? 操作符展开 ----
            HirExprKind::Call { qualifier, func, generic_args, args, is_method } => {
                if qualifier.is_none() && !is_method && func == "try" && args.len() == 1 {
                    let arg = match &args[0] {
                        HirCallArg::Positional(e) => e.clone(),
                        HirCallArg::Named(_, e) => e.clone(),
                    };
                    return Self::expand_try(arg, ctx);
                }

                let new_args = Self::elaborate_call_args(args, ctx)?;
                expr.kind = HirExprKind::Call {
                    qualifier,
                    func,
                    generic_args,
                    args: new_args,
                    is_method,
                };
                Ok(expr)
            }

            // ---- 其他表达式：递归展开子表达式 ----
            HirExprKind::Literal(_) => Ok(expr),
            HirExprKind::Ident(_) => Ok(expr),
            HirExprKind::Sym(_) => Ok(expr),

            HirExprKind::BinaryOp { op, left, right } => {
                let left = Self::elaborate_boxed(left, ctx)?;
                let right = Self::elaborate_boxed(right, ctx)?;
                expr.kind = HirExprKind::BinaryOp { op, left, right };
                Ok(expr)
            }

            HirExprKind::Unary { op, expr: inner } => {
                expr.kind = HirExprKind::Unary { op, expr: Self::elaborate_boxed(inner, ctx)? };
                Ok(expr)
            }

            HirExprKind::Cast { expr: inner, ty } => {
                expr.kind = HirExprKind::Cast { expr: Self::elaborate_boxed(inner, ctx)?, ty };
                Ok(expr)
            }

            HirExprKind::Block(block) => {
                let block = Self::elaborate_block(block, ctx)?;
                expr.kind = HirExprKind::Block(block);
                Ok(expr)
            }

            HirExprKind::Match { cond, arms } => {
                let cond = Self::elaborate_boxed(cond, ctx)?;
                let mut new_arms = Vec::new();
                for arm in arms {
                    let arm_expr = Self::elaborate_expr(arm.expr, ctx)?;
                    new_arms.push(HirMatchArm {
                        pattern: arm.pattern,
                        expr: arm_expr,
                    });
                }
                expr.kind = HirExprKind::Match { cond, arms: new_arms };
                Ok(expr)
            }

            HirExprKind::If { kind, cond, then_expr, else_expr } => {
                let cond = Self::elaborate_boxed(cond, ctx)?;
                let then_expr = Self::elaborate_boxed(then_expr, ctx)?;
                let else_expr = else_expr.map(|e| Self::elaborate_boxed(e, ctx)).transpose()?;
                expr.kind = HirExprKind::If { kind, cond, then_expr, else_expr };
                Ok(expr)
            }

            HirExprKind::StructInit { struct_name, generic_args, fields } => {
                let mut new_fields = Vec::new();
                for (name, e) in fields {
                    new_fields.push((name, Self::elaborate_expr(e, ctx)?));
                }
                expr.kind = HirExprKind::StructInit {
                    struct_name,
                    generic_args,
                    fields: new_fields,
                };
                Ok(expr)
            }

            HirExprKind::FieldAccess { struct_expr, field_name } => {
                expr.kind = HirExprKind::FieldAccess {
                    struct_expr: Self::elaborate_boxed(struct_expr, ctx)?,
                    field_name,
                };
                Ok(expr)
            }

            HirExprKind::Index { expr: base, index } => {
                let base = Self::elaborate_boxed(base, ctx)?;
                let index = Self::elaborate_boxed(index, ctx)?;
                expr.kind = HirExprKind::Index { expr: base, index };
                Ok(expr)
            }

            HirExprKind::Range { start, end } => {
                let start = Self::elaborate_boxed(start, ctx)?;
                let end = Self::elaborate_boxed(end, ctx)?;
                expr.kind = HirExprKind::Range { start, end };
                Ok(expr)
            }

            HirExprKind::Closure { param, body } => {
                expr.kind = HirExprKind::Closure { param, body: Self::elaborate_boxed(body, ctx)? };
                Ok(expr)
            }

            HirExprKind::ArrayLiteral(elements) => {
                let mut new_elements = Vec::new();
                for e in elements {
                    new_elements.push(Self::elaborate_expr(e, ctx)?);
                }
                expr.kind = HirExprKind::ArrayLiteral(new_elements);
                Ok(expr)
            }

            HirExprKind::UnsafeBlock { kind, body, span } => {
                let body = Self::elaborate_block(body, ctx)?;
                expr.kind = HirExprKind::UnsafeBlock { kind, body, span };
                Ok(expr)
            }

            HirExprKind::EnumVariantAccess { .. } => Ok(expr),
            HirExprKind::EnumVariantConstruction { enum_name, generic_args, variant_name, args } => {
                let new_args = Self::elaborate_call_args(args, ctx)?;
                expr.kind = HirExprKind::EnumVariantConstruction {
                    enum_name,
                    generic_args,
                    variant_name,
                    args: new_args,
                };
                Ok(expr)
            }

            HirExprKind::LackSlice(_) => Ok(expr),

            HirExprKind::Ref { mutable, expr: inner } => {
                expr.kind = HirExprKind::Ref {
                    mutable,
                    expr: Self::elaborate_boxed(inner, ctx)?,
                };
                Ok(expr)
            }
            HirExprKind::Deref(inner) => {
                expr.kind = HirExprKind::Deref(Self::elaborate_boxed(inner, ctx)?);
                Ok(expr)
            }
        }
    }

    /// 递归展开一个 `Box<HirExpr>`。Unary/Cast/FieldAccess/Closure/Index/
    /// Range/BinaryOp/If/Match 里全是"拆箱 -> elaborate_expr -> 再装箱"
    /// 这一个动作，抽出来后每个 match arm 只需要关心自己特有的字段。
    ///
    /// 注：这里没有引入一个带默认递归 + 通配 `_ => expr.kind` 的
    /// HirExprFolder trait。elaborate_expr 对 HirExprKind 的所有变体都
    /// 显式列出、没有通配分支——hir.rs 每加一个新变体，这里的 match 就
    /// 会因为"非穷尽"编译不过，逼着回来决定新变体要不要特殊处理。通配
    /// 分支会悄悄放弃这层保护：新变体默认"什么都不做地透传"，如果它其实
    /// 需要 elaborate（比如内部也带 Box<HirExpr>），编译器不会提醒，只
    /// 会在运行时表现出诡异行为。所以这里只抽取"重复的递归动作"，保留
    /// 穷尽匹配本身。
    fn elaborate_boxed(e: Box<HirExpr>, ctx: &mut ElaborateContext) -> Result<Box<HirExpr>, String> {
        Ok(Box::new(Self::elaborate_expr(*e, ctx)?))
    }

    /// Call / EnumVariantConstruction 展开各自参数列表的逻辑完全一样。
    fn elaborate_call_args(
        args: Vec<HirCallArg>,
        ctx: &mut ElaborateContext,
    ) -> Result<Vec<HirCallArg>, String> {
        args.into_iter()
            .map(|arg| {
                Ok(match arg {
                    HirCallArg::Positional(e) => HirCallArg::Positional(Self::elaborate_expr(e, ctx)?),
                    HirCallArg::Named(name, e) => HirCallArg::Named(name, Self::elaborate_expr(e, ctx)?),
                })
            })
            .collect()
    }

    // ===== ? 操作符展开 =====

    fn expand_try(expr: HirExpr, ctx: &mut ElaborateContext) -> Result<HirExpr, String> {
        let Some(kind) = ctx.try_kind() else {
            return Err(format!(
                "`?` cannot be used in a function that returns {:?}; \
                 only functions returning `Result<T, E>` or `Option<T>` support `?`",
                ctx.return_ty
            ));
        };

        let temp_var = ctx.next_temp();
        match kind {
            TryKind::Result => Self::build_result_try(expr, temp_var, ctx),
            TryKind::Option => Self::build_option_try(expr, temp_var, ctx),
        }
    }

    // build_result_try / build_option_try 原来是两段接近 60 行、结构
    // 完全同构的代码：取 span -> 造成功分支（绑定 temp，值 =
    // mk_bound_ident）-> 造失败分支（pattern + mk_diverging_block 包一条
    // return）-> merge effects -> 从 return_ty 里按形状取成功类型 ->
    // 拼 Match。两者的差异只在"失败分支具体 return 哪个值"（Result 要
    // 先造 err_var 和 e.into() 调用，Option 直接是 None）和"按哪个
    // 泛型名/第几个参数取成功类型"。现在这两个函数各自只负责造这几个
    // 特有零件，拼装步骤收口到 assemble_try_match。

    fn build_result_try(
        expr: HirExpr,
        temp_var: String,
        ctx: &mut ElaborateContext,
    ) -> Result<HirExpr, String> {
        let span = expr.span;

        // Err(e) => return Err(e.into())
        let err_var = ctx.next_temp();
        let into_call = mk_call(
            "into",
            vec![HirCallArg::Positional(mk_ident(&err_var, ctx.error_type(), span))],
            ctx.error_type(),
            span,
        );
        let err_return_value = mk_expr_default(
            HirExprKind::Call {
                qualifier: Some("Result".to_string()),
                func: "Err".to_string(),
                generic_args: Vec::new(),
                args: vec![HirCallArg::Positional(into_call)],
                is_method: false,
            },
            // 这里是"要 return 出去的那个值"自身的类型，即
            // Result<T, E>；unwrap_or 的 Never 兜底只在 try_kind() 已经
            // 确认过 return_ty 形状之后，理论上不该走到。
            ctx.return_ty.clone().unwrap_or(Type::Never),
            span,
        );

        Ok(Self::assemble_try_match(
            expr,
            ok_pattern(&temp_var),
            &temp_var,
            err_pattern(&err_var),
            err_return_value,
            "Result",
            2,
            span,
            ctx,
        ))
    }

    fn build_option_try(
        expr: HirExpr,
        temp_var: String,
        ctx: &mut ElaborateContext,
    ) -> Result<HirExpr, String> {
        let span = expr.span;

        // None => return None
        let none_return_value = mk_expr_default(
            HirExprKind::Call {
                qualifier: Some("Option".to_string()),
                func: "None".to_string(),
                generic_args: Vec::new(),
                args: Vec::new(),
                is_method: false,
            },
            ctx.return_ty.clone().unwrap_or(Type::Never),
            span,
        );

        Ok(Self::assemble_try_match(
            expr,
            some_pattern(&temp_var),
            &temp_var,
            none_pattern(),
            none_return_value,
            "Option",
            1,
            span,
            ctx,
        ))
    }

    /// `?` 展开的共用拼装步骤。成功/失败分支各自特有的部分（pattern、
    /// 失败分支具体 return 哪个值、从 return_ty 里按哪个泛型名和第几个
    /// 参数取出成功类型）由调用方（build_result_try / build_option_try）
    /// 造好传进来；这里只管"拼分支、算 effects、取 result_ty、包成
    /// Match"这套两边完全一样的逻辑，不关心背后是 Result 还是 Option。
    fn assemble_try_match(
        expr: HirExpr,
        success_pattern: Pattern,
        temp_var: &str,
        failure_pattern: Pattern,
        failure_return_value: HirExpr,
        return_ty_name: &str,
        return_ty_arg_count: usize,
        span: Span,
        ctx: &ElaborateContext,
    ) -> HirExpr {
        // Ok(temp) => temp / Some(temp) => temp
        let success_arm = HirMatchArm {
            pattern: success_pattern,
            expr: mk_bound_ident(temp_var, &expr),
        };

        // 失败分支必须是"提前 return"，而不是把 Err(...)/None 这个值
        // 直接当 match 分支的结果（它的类型是 Result<T,E>/Option<T>，
        // 跟 success_arm 的 T 对不上）。用 mk_diverging_block 包一条
        // Return 语句，把分支类型标成 Never，就能跟 success_arm 的 T
        // 正常统一。
        let failure_arm = HirMatchArm {
            pattern: failure_pattern,
            expr: mk_diverging_block(vec![HirStmt::Return { expr: Some(failure_return_value), span }], span),
        };

        let match_effects = EffectSet::merge(&[&expr.effects, &success_arm.expr.effects, &failure_arm.expr.effects]);

        // try_kind() 已经在 expand_try 里确认过 return_ty 的形状，这里
        // 理论上一定能走进第一条分支；走到 `_` 说明两边检查不一致，用
        // Never 如实标记"不该发生"，不用 Unit 假装这是个正常结果。
        let result_ty = match &ctx.return_ty {
            Some(Type::Generic(name, args)) if name == return_ty_name && args.len() == return_ty_arg_count => {
                args[0].clone()
            }
            _ => Type::Never,
        };

        mk_expr(
            HirExprKind::Match { cond: Box::new(expr), arms: vec![success_arm, failure_arm] },
            result_ty,
            match_effects,
            span,
        )
    }

    // ===== 辅助：收集 HirBlock 中所有语句的 EffectSet =====
    //
    // 原来是"先摘出整个 block（递归地）涉及到的所有 HirExpr 引用塞进一个
    // Vec<&HirExpr>，再 map 成 Vec<&EffectSet>，最后一次性 merge"——两次
    // 中间 Vec，分配量随 block 里的表达式个数线性增长。改成边遍历边把
    // 当前已知的 effects 累到一个 EffectSet 累加器里，不再经过任何
    // 中间 Vec。EffectSet 没有确认过提供 "就地合并一个" 的方法（只有
    // 文件里已经在用的 EffectSet::merge(&[...]) 这个关联函数），所以
    // fold_effect 没有新造一个假设存在的接口，而是复用这个已验证可行
    // 的签名：每次都重新 merge 出一个新 EffectSet 赋回累加器。

    fn collect_effects_from_block(block: &HirBlock) -> EffectSet {
        let mut acc = EffectSet::default();
        Self::accumulate_effects_block(block, &mut acc);
        acc
    }

    fn accumulate_effects_block(block: &HirBlock, acc: &mut EffectSet) {
        for stmt in &block.stmts {
            Self::accumulate_effects_stmt(stmt, acc);
        }
    }

    fn accumulate_effects_stmt(stmt: &HirStmt, acc: &mut EffectSet) {
        match stmt {
            HirStmt::Expr { expr, .. } => Self::fold_effect(acc, &expr.effects),
            HirStmt::Let { init, .. } => Self::fold_effect(acc, &init.effects),
            HirStmt::Return { expr: Some(e), .. } => Self::fold_effect(acc, &e.effects),
            HirStmt::Return { expr: None, .. } => {}
            HirStmt::While { cond, body, .. } => {
                Self::fold_effect(acc, &cond.effects);
                Self::accumulate_effects_block(body, acc);
            }
            HirStmt::For { body, .. } => Self::accumulate_effects_block(body, acc),
            HirStmt::Loop { body, .. } => Self::accumulate_effects_block(body, acc),
            HirStmt::Assign { target, expr, .. } => {
                Self::fold_effect(acc, &target.effects);
                Self::fold_effect(acc, &expr.effects);
            }
            HirStmt::UnsafeBlock { body, .. } => Self::accumulate_effects_block(body, acc),
            HirStmt::Break { .. } => {}
            HirStmt::Continue { .. } => {}
        }
    }

    fn fold_effect(acc: &mut EffectSet, other: &EffectSet) {
        *acc = EffectSet::merge(&[&*acc, other]);
    }
}
