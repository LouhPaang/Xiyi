use crate::ast::*;
use crate::hir::*;
use std::collections::HashMap;

type BuildResult<T> = Result<T, String>;

pub struct HirBuilder;

impl HirBuilder {
    // ===== 辅助函数：将 T 包装成 BuildResult<T> =====
    fn ok<T>(val: T) -> BuildResult<T> {
        Ok(val)
    }

    // ===== 辅助函数：ast::GenericParam -> HIR 里统一用的 Vec<HirGenericParam> =====
    // 抽出来复用，避免 build_struct/build_enum/build_fn/build_implement/build_interface
    // 各写一份、以后 GenericParam 加新变体时到处漏改
    // 注：现在把 bounds 也透传进 HIR 了（ast::GenericParam::Type 已经带 bounds），
    // 之前那版直接 `..` 丢弃 bounds 只是为了先能编译过，这版补上。
    fn build_generic_params(generic_params: &[GenericParam]) -> Vec<HirGenericParam> {
        generic_params
            .iter()
            .map(|gp| match gp {
                GenericParam::Type { name, bounds } => HirGenericParam {
                    name: name.clone(),
                    bounds: bounds.clone(),
                },
            })
            .collect()
    }

    pub fn build(
        program: &Program,
        expr_types: &HashMap<usize, Type>,
    ) -> BuildResult<HirProgram> {
        let mut models = Vec::new();
        let mut fns = Vec::new();
        let mut structs = Vec::new();
        let mut enums = Vec::new();
        let mut consts = Vec::new();
        let mut protos = Vec::new();
        let mut impls = Vec::new();
        let mut interfaces = Vec::new();

        for item in &program.items {
            match item {
                Item::ModelDef(m) => models.push(Self::build_model(m, expr_types)?),
                Item::FnDef(f) => fns.push(Self::build_fn(f, expr_types, None)?),
                Item::StructDef(s) => structs.push(Self::build_struct(s)?),
                Item::EnumDef(e) => enums.push(Self::build_enum(e)?),
                Item::ConstDef(c) => consts.push(Self::build_const(c, expr_types)?),
                Item::ProtoDef(p) => protos.push(Self::build_proto(p)?),
                Item::Use(_) => {
                    // 暂不处理，后续实现模块解析时再使用
                }
                Item::Implement(imp) => {
                    impls.push(Self::build_implement(imp, expr_types)?);
                }
                Item::Interface(iface) => {
                    interfaces.push(Self::build_interface(iface)?);
                }
            }
        }

        Ok(HirProgram {
            models,
            fns,
            structs,
            enums,
            consts,
            protos,
            impls,
            interfaces,
        })
    }

    fn build_implement(
        imp: &ImplementDef,
        expr_types: &HashMap<usize, Type>,
    ) -> BuildResult<HirImplement> {
        let functions = imp
            .functions
            .iter()
            .map(|f| Self::build_fn(f, expr_types, None))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(HirImplement {
            generic_params: Self::build_generic_params(&imp.generic_params),
            target_type: imp.target_type.clone(),
            interface_name: imp.interface_name.clone(),
            functions,
        })
    }

    fn build_interface(iface: &InterfaceDef) -> BuildResult<HirInterface> {
        let methods = iface
            .methods
            .iter()
            .map(|sig| HirFnSig {
                name: sig.name.clone(),
                generic_params: Self::build_generic_params(&sig.generic_params),
                params: sig
                    .params
                    .iter()
                    .map(|p| HirParam {
                        name: p.name.clone(),
                        ty: p.ty.clone(),
                    })
                    .collect(),
                return_type: sig.return_type.clone(),
            })
            .collect();

        Ok(HirInterface {
            name: iface.name.clone(),
            generic_params: Self::build_generic_params(&iface.generic_params),
            methods,
        })
    }

    fn build_model(
        m: &ModelDef,
        expr_types: &HashMap<usize, Type>,
    ) -> BuildResult<HirModel> {
        let mut syms = Vec::new();
        if let Some(forward) = m.functions.iter().find(|f| f.name == "forward") {
            Self::collect_syms_from_type(&forward.return_type, &mut syms);
            for param in &forward.params {
                Self::collect_syms_from_type(&Some(param.ty.clone()), &mut syms);
            }
        }
        let mut unique_syms = Vec::new();
        for s in syms {
            if !unique_syms.contains(&s) {
                unique_syms.push(s);
            }
        }
        unique_syms.sort();

        // 注：这些 symbolic dims 来自 forward 的张量形状，不是用户写的 <T: Bound>，
        // 天生没有 bounds 概念。这里只是包一层 HirGenericParam 让类型跟其它
        // generic_params 字段保持一致，bounds 恒为空，不代表真的支持约束。
        let generic_params = unique_syms
            .into_iter()
            .map(|name| HirGenericParam {
                name,
                bounds: Vec::new(),
            })
            .collect();

        let fields = m
            .fields
            .iter()
            .map(|f| HirField {
                name: f.name.clone(),
                ty: f.ty.clone(),
            })
            .collect();

        let functions = m
            .functions
            .iter()
            .map(|f| Self::build_fn(f, expr_types, Some(m.name.clone())))
            .collect::<Result<Vec<_>, _>>()?;

        let sensitivity = None;

        // 检查是否需要 TrainingContext
        let training_context_required = if let Some(forward) =
            m.functions.iter().find(|f| f.name == "forward")
        {
            forward.params.iter().any(|p| {
                matches!(&p.ty, Type::Privacy(_, PrivacyTag::Differential { .. }))
            })
        } else {
            false
        };

        // 提取隐私预算 eps
        let privacy_eps = if let Some(forward) =
            m.functions.iter().find(|f| f.name == "forward")
        {
            forward.params.iter().find_map(|p| {
                if let Type::Privacy(_, PrivacyTag::Differential { eps, delta: _ }) = &p.ty {
                    Some(eps.clone())
                } else {
                    None
                }
            })
        } else {
            None
        };

        Ok(HirModel {
            name: m.name.clone(),
            generic_params,
            fields,
            functions,
            sensitivity,
            training_context_required,
            privacy_eps,
        })
    }

    fn build_proto(p: &ProtoDef) -> BuildResult<HirProto> {
        Ok(HirProto {
            name: p.name.clone(),
            variants: p
                .variants
                .iter()
                .map(|v| HirProtoVariant {
                    name: v.name.clone(),
                    ty: v.ty.clone(),
                })
                .collect(),
        })
    }

    fn build_fn(
        f: &FnDef,
        expr_types: &HashMap<usize, Type>,
        model_owner: Option<String>,
    ) -> BuildResult<HirFn> {
        let params = f
            .params
            .iter()
            .map(|p| HirParam {
                name: p.name.clone(),
                ty: p.ty.clone(),
            })
            .collect();

        let body = Self::build_block(&f.body, expr_types)?;
        let return_type = f.return_type.clone();
        let effects = EffectSet::default();
        let sensitivity = None;

        Ok(HirFn {
            name: f.name.clone(),
            generic_params: Self::build_generic_params(&f.generic_params),
            params,
            return_type,
            body,
            effects,
            sensitivity,
            is_forward: f.name == "forward" && model_owner.is_some(),
        })
    }

    fn build_block(block: &Block, expr_types: &HashMap<usize, Type>) -> BuildResult<HirBlock> {
        let mut stmts = Vec::new();
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let(let_stmt) => {
                    let init = Self::build_expr(&let_stmt.init, expr_types)?;
                    stmts.push(HirStmt::Let {
                        name: let_stmt.name.clone(),
                        ty: let_stmt.ty.clone(),
                        init,
                        mutable: let_stmt.mutable,
                        persist: let_stmt.persist,
                        span: Span::default(),
                    });
                }
                Stmt::ExprStmt(expr) => {
                    let expr = Self::build_expr(expr, expr_types)?;
                    stmts.push(HirStmt::Expr {
                        expr,
                        span: Span::default(),
                    });
                }
                Stmt::Return(expr_opt) => {
                    let expr = expr_opt
                        .as_ref()
                        .map(|e| Self::build_expr(e, expr_types))
                        .transpose()?;
                    stmts.push(HirStmt::Return {
                        expr,
                        span: Span::default(),
                    });
                }
                Stmt::While(while_stmt) => {
                    let cond = Self::build_expr(&while_stmt.cond, expr_types)?;
                    let body = Self::build_block(&while_stmt.body, expr_types)?;
                    stmts.push(HirStmt::While {
                        cond,
                        body,
                        span: Span::default(),
                    });
                }
                Stmt::For(for_stmt) => {
                    let iterable = Self::build_expr(&for_stmt.iterable, expr_types)?;
                    let body = Self::build_block(&for_stmt.body, expr_types)?;
                    stmts.push(HirStmt::For {
                        var: for_stmt.var.clone(),
                        iterable,
                        body,
                        span: Span::default(),
                    });
                }
                Stmt::Assign(assign_stmt) => {
                    let target = Self::build_expr(&assign_stmt.target, expr_types)?;
                    let expr = Self::build_expr(&assign_stmt.expr, expr_types)?;
                    stmts.push(HirStmt::Assign {
                        target: Box::new(target),
                        expr,
                        span: Span::default(),
                    });
                }
                Stmt::Loop(loop_stmt) => {
                    let body = Self::build_block(&loop_stmt.body, expr_types)?;
                    stmts.push(HirStmt::Loop {
                        body,
                        span: Span::default(),
                    });
                }
                Stmt::Break(_) => {
                    stmts.push(HirStmt::Break {
                        span: Span::default(),
                    });
                }
                Stmt::UnsafeBlock(unsafe_block) => {
                    let body = Self::build_block(&unsafe_block.body, expr_types)?;
                    stmts.push(HirStmt::UnsafeBlock {
                        kind: unsafe_block.kind.clone(),
                        body,
                        span: Span::default(),
                    });
                }
            }
        }
        Ok(HirBlock {
            stmts,
            span: Span::default(),
        })
    }

    fn build_expr(expr: &Expr, expr_types: &HashMap<usize, Type>) -> BuildResult<HirExpr> {
        let ty = expr_types.get(&expr.id).cloned().unwrap_or(Type::I32);
        let privacy_tag = Self::extract_privacy_tag(&ty);
        let sensitivity = Sensitivity::Unknown;
        let mut effects = EffectSet::default();

        let kind = match &expr.kind {
            ExprKind::Literal(lit) => HirExprKind::Literal(lit.clone()),
            ExprKind::Ident(name) => HirExprKind::Ident(name.clone()),
            ExprKind::Sym(name) => HirExprKind::Sym(name.clone()),
            ExprKind::BinaryOp { op, left, right } => {
                let left = Self::build_expr(left, expr_types)?;
                let right = Self::build_expr(right, expr_types)?;
                effects = EffectSet {
                    has_io: left.effects.has_io || right.effects.has_io,
                    has_rng: left.effects.has_rng || right.effects.has_rng,
                    has_ai: left.effects.has_ai || right.effects.has_ai,
                    has_ffi: left.effects.has_ffi || right.effects.has_ffi,
                    has_panic: left.effects.has_panic || right.effects.has_panic,
                };
                HirExprKind::BinaryOp {
                    op: op.clone(),
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
            ExprKind::Call {
                qualifier,
                func,
                args,
                is_method,
            } => {
                let hir_args: Vec<HirCallArg> = args
                    .iter()
                    .map(|arg| match arg {
                        CallArg::Positional(e) => {
                            let expr = Self::build_expr(e, expr_types)?;
                            Self::ok(HirCallArg::Positional(expr))
                        }
                        CallArg::Named(name, e) => {
                            let expr = Self::build_expr(e, expr_types)?;
                            Self::ok(HirCallArg::Named(name.clone(), expr))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if func == "print" {
                    effects.has_io = true;
                }
                for arg in &hir_args {
                    let e = match arg {
                        HirCallArg::Positional(e) => e,
                        HirCallArg::Named(_, e) => e,
                    };
                    if e.effects.has_io {
                        effects.has_io = true;
                    }
                    if e.effects.has_rng {
                        effects.has_rng = true;
                    }
                    if e.effects.has_ai {
                        effects.has_ai = true;
                    }
                    if e.effects.has_ffi {
                        effects.has_ffi = true;
                    }
                    if e.effects.has_panic {
                        effects.has_panic = true;
                    }
                }
                HirExprKind::Call {
                    qualifier: qualifier.clone(),
                    func: func.clone(),
                    // TODO(sema): 目前 ast::ExprKind::Call 还没有显式泛型实参语法（如
                    // identity::<i32>(1)），先占位空 Vec，等 parser/ast 支持后这里改成
                    // 从 ast 侧透传
                    generic_args: Vec::new(),
                    args: hir_args,
                    is_method: *is_method,
                }
            }
            ExprKind::Block(block) => {
                let hir_block = Self::build_block(block, expr_types)?;
                for stmt in &hir_block.stmts {
                    match stmt {
                        HirStmt::Expr { expr, .. }
                        | HirStmt::Let { init: expr, .. }
                        | HirStmt::Return { expr: Some(expr), .. } => {
                            if expr.effects.has_io {
                                effects.has_io = true;
                            }
                            if expr.effects.has_rng {
                                effects.has_rng = true;
                            }
                            if expr.effects.has_ai {
                                effects.has_ai = true;
                            }
                            if expr.effects.has_ffi {
                                effects.has_ffi = true;
                            }
                            if expr.effects.has_panic {
                                effects.has_panic = true;
                            }
                        }
                        HirStmt::UnsafeBlock { body, .. } => {
                            for inner_stmt in &body.stmts {
                                if let HirStmt::Expr { expr, .. } = inner_stmt {
                                    if expr.effects.has_io {
                                        effects.has_io = true;
                                    }
                                    if expr.effects.has_rng {
                                        effects.has_rng = true;
                                    }
                                    if expr.effects.has_ai {
                                        effects.has_ai = true;
                                    }
                                    if expr.effects.has_ffi {
                                        effects.has_ffi = true;
                                    }
                                    if expr.effects.has_panic {
                                        effects.has_panic = true;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                HirExprKind::Block(hir_block)
            }
            ExprKind::StructInit {
                struct_name,
                fields,
            } => {
                let hir_fields: Vec<(String, HirExpr)> = fields
                    .iter()
                    .map(|(name, e)| {
                        let expr = Self::build_expr(e, expr_types)?;
                        Self::ok((name.clone(), expr))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                for (_, e) in &hir_fields {
                    if e.effects.has_io {
                        effects.has_io = true;
                    }
                    if e.effects.has_rng {
                        effects.has_rng = true;
                    }
                    if e.effects.has_ai {
                        effects.has_ai = true;
                    }
                    if e.effects.has_ffi {
                        effects.has_ffi = true;
                    }
                    if e.effects.has_panic {
                        effects.has_panic = true;
                    }
                }
                HirExprKind::StructInit {
                    struct_name: struct_name.clone(),
                    // TODO(sema): 同 Call，ast 侧暂无显式泛型实参，先占位空 Vec
                    generic_args: Vec::new(),
                    fields: hir_fields,
                }
            }
            ExprKind::FieldAccess {
                struct_expr,
                field_name,
            } => {
                let struct_expr = Self::build_expr(struct_expr, expr_types)?;
                effects = struct_expr.effects.clone();
                HirExprKind::FieldAccess {
                    struct_expr: Box::new(struct_expr),
                    field_name: field_name.clone(),
                }
            }
            ExprKind::Range { start, end } => {
                let start = Self::build_expr(start, expr_types)?;
                let end = Self::build_expr(end, expr_types)?;
                effects = EffectSet {
                    has_io: start.effects.has_io || end.effects.has_io,
                    has_rng: start.effects.has_rng || end.effects.has_rng,
                    has_ai: start.effects.has_ai || end.effects.has_ai,
                    has_ffi: start.effects.has_ffi || end.effects.has_ffi,
                    has_panic: start.effects.has_panic || end.effects.has_panic,
                };
                HirExprKind::Range {
                    start: Box::new(start),
                    end: Box::new(end),
                }
            }
            ExprKind::EnumVariantAccess {
                enum_name,
                variant_name,
            } => HirExprKind::EnumVariantAccess {
                enum_name: enum_name.clone(),
                variant_name: variant_name.clone(),
            },
            // ===== 新增：枚举变体构造 =====
            ExprKind::EnumVariantConstruction {
                enum_name,
                variant_name,
                args,
            } => {
                let hir_args: Vec<HirCallArg> = args
                    .iter()
                    .map(|arg| match arg {
                        CallArg::Positional(e) => {
                            let expr = Self::build_expr(e, expr_types)?;
                            Self::ok(HirCallArg::Positional(expr))
                        }
                        CallArg::Named(name, e) => {
                            let expr = Self::build_expr(e, expr_types)?;
                            Self::ok(HirCallArg::Named(name.clone(), expr))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                HirExprKind::EnumVariantConstruction {
                    enum_name: enum_name.clone(),
                    // TODO(sema): 同 Call，ast 侧暂无显式泛型实参，先占位空 Vec
                    generic_args: Vec::new(),
                    variant_name: variant_name.clone(),
                    args: hir_args,
                }
            }
            ExprKind::Match(match_expr) => {
                let cond = Self::build_expr(&match_expr.cond, expr_types)?;
                let mut arms = Vec::new();
                for arm in &match_expr.arms {
                    let arm_expr = Self::build_expr(&arm.expr, expr_types)?;
                    arms.push(HirMatchArm {
                        pattern: arm.pattern.clone(),
                        expr: arm_expr,
                    });
                }
                effects = cond.effects.clone();
                for arm in &arms {
                    if arm.expr.effects.has_io {
                        effects.has_io = true;
                    }
                    if arm.expr.effects.has_rng {
                        effects.has_rng = true;
                    }
                    if arm.expr.effects.has_ai {
                        effects.has_ai = true;
                    }
                    if arm.expr.effects.has_ffi {
                        effects.has_ffi = true;
                    }
                    if arm.expr.effects.has_panic {
                        effects.has_panic = true;
                    }
                }
                HirExprKind::Match {
                    cond: Box::new(cond),
                    arms,
                }
            }
            ExprKind::Closure { param, body } => {
                let body = Self::build_expr(body, expr_types)?;
                effects = body.effects.clone();
                HirExprKind::Closure {
                    param: param.clone(),
                    body: Box::new(body),
                }
            }
            ExprKind::If {
                kind: if_kind,
                cond,
                then_expr,
                else_expr,
            } => {
                let cond = Self::build_expr(cond, expr_types)?;
                let then_expr = Self::build_expr(then_expr, expr_types)?;
                let else_expr = else_expr
                    .as_ref()
                    .map(|e| Self::build_expr(e, expr_types))
                    .transpose()?
                    .map(Box::new);
                effects = cond.effects.clone();
                if then_expr.effects.has_io {
                    effects.has_io = true;
                }
                if then_expr.effects.has_rng {
                    effects.has_rng = true;
                }
                if then_expr.effects.has_ai {
                    effects.has_ai = true;
                }
                if then_expr.effects.has_ffi {
                    effects.has_ffi = true;
                }
                if then_expr.effects.has_panic {
                    effects.has_panic = true;
                }
                if let Some(e) = &else_expr {
                    if e.effects.has_io {
                        effects.has_io = true;
                    }
                    if e.effects.has_rng {
                        effects.has_rng = true;
                    }
                    if e.effects.has_ai {
                        effects.has_ai = true;
                    }
                    if e.effects.has_ffi {
                        effects.has_ffi = true;
                    }
                    if e.effects.has_panic {
                        effects.has_panic = true;
                    }
                }
                HirExprKind::If {
                    kind: if_kind.clone(),
                    cond: Box::new(cond),
                    then_expr: Box::new(then_expr),
                    else_expr,
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                let hir_elements: Vec<HirExpr> = elements
                    .iter()
                    .map(|e| Self::build_expr(e, expr_types))
                    .collect::<Result<Vec<_>, _>>()?;
                for e in &hir_elements {
                    if e.effects.has_io {
                        effects.has_io = true;
                    }
                    if e.effects.has_rng {
                        effects.has_rng = true;
                    }
                    if e.effects.has_ai {
                        effects.has_ai = true;
                    }
                    if e.effects.has_ffi {
                        effects.has_ffi = true;
                    }
                    if e.effects.has_panic {
                        effects.has_panic = true;
                    }
                }
                HirExprKind::ArrayLiteral(hir_elements)
            }
            ExprKind::UnsafeBlock(unsafe_block) => {
                let body = Self::build_block(&unsafe_block.body, expr_types)?;
                for stmt in &body.stmts {
                    match stmt {
                        HirStmt::Expr { expr, .. }
                        | HirStmt::Let { init: expr, .. }
                        | HirStmt::Return { expr: Some(expr), .. } => {
                            if expr.effects.has_io {
                                effects.has_io = true;
                            }
                            if expr.effects.has_rng {
                                effects.has_rng = true;
                            }
                            if expr.effects.has_ai {
                                effects.has_ai = true;
                            }
                            if expr.effects.has_ffi {
                                effects.has_ffi = true;
                            }
                            if expr.effects.has_panic {
                                effects.has_panic = true;
                            }
                        }
                        _ => {}
                    }
                }
                HirExprKind::UnsafeBlock {
                    kind: unsafe_block.kind.clone(),
                    body,
                    span: Span::default(),
                }
            }
            // ===== 新增：一元运算符 =====
            ExprKind::Unary { op, expr } => {
                let inner = Self::build_expr(expr, expr_types)?;
                effects = inner.effects.clone();
                HirExprKind::Unary {
                    op: op.clone(),
                    expr: Box::new(inner),
                }
            }
            // ===== 新增：as 类型转换 =====
            ExprKind::Cast { expr, ty: cast_ty } => {
                let inner = Self::build_expr(expr, expr_types)?;
                effects = inner.effects.clone();
                HirExprKind::Cast {
                    expr: Box::new(inner),
                    ty: cast_ty.clone(),
                }
            }
            // ===== 新增：索引表达式 expr[idx]（比如 bytes[i]）=====
            ExprKind::Index { expr, index } => {
                let base = Self::build_expr(expr, expr_types)?;
                let idx = Self::build_expr(index, expr_types)?;
                effects = EffectSet {
                    has_io: base.effects.has_io || idx.effects.has_io,
                    has_rng: base.effects.has_rng || idx.effects.has_rng,
                    has_ai: base.effects.has_ai || idx.effects.has_ai,
                    has_ffi: base.effects.has_ffi || idx.effects.has_ffi,
                    has_panic: base.effects.has_panic || idx.effects.has_panic,
                };
                HirExprKind::Index {
                    expr: Box::new(base),
                    index: Box::new(idx),
                }
            }
            // ===== 新增：lack &[T] 空切片字面量——纯编译期常量，effect 为
            // none，跟规范里"b""/lack &[T] 均为编译期常量"这条一致，
            // effects 保持默认（全 false），不用合并任何子表达式的副作用
            // （它本来就没有子表达式）。
            ExprKind::LackSlice(ty) => HirExprKind::LackSlice(ty.clone()),
        };

        Ok(HirExpr {
            kind,
            ty,
            privacy_tag,
            sensitivity,
            effects,
            span: Span::default(),
        })
    }

    fn extract_privacy_tag(ty: &Type) -> Option<PrivacyTag> {
        match ty {
            Type::Privacy(_, tag) => Some(tag.clone()),
            _ => None,
        }
    }

    fn build_struct(s: &StructDef) -> BuildResult<HirStruct> {
        let generic_params = Self::build_generic_params(&s.generic_params);

        Ok(HirStruct {
            name: s.name.clone(),
            generic_params,
            fields: s
                .fields
                .iter()
                .map(|f| HirField {
                    name: f.name.clone(),
                    ty: f.ty.clone(),
                })
                .collect(),
        })
    }

    fn build_enum(e: &EnumDef) -> BuildResult<HirEnum> {
        let generic_params = Self::build_generic_params(&e.generic_params);

        Ok(HirEnum {
            name: e.name.clone(),
            generic_params,
            variants: e
                .variants
                .iter()
                .map(|v| HirEnumVariant {
                    name: v.name.clone(),
                    ty: v.ty.clone(),
                })
                .collect(),
        })
    }

    fn build_const(c: &ConstDef, expr_types: &HashMap<usize, Type>) -> BuildResult<HirConst> {
        Ok(HirConst {
            name: c.name.clone(),
            ty: c.ty.clone(),
            value: Self::build_expr(&c.value, expr_types)?,
        })
    }

    fn collect_syms_from_type(ty_opt: &Option<Type>, syms: &mut Vec<String>) {
        if let Some(ty) = ty_opt {
            Self::collect_syms_from_type_inner(ty, syms);
        }
    }

    fn collect_syms_from_type_inner(ty: &Type, syms: &mut Vec<String>) {
        match ty {
            Type::Tensor { shape, .. } => {
                for dim in shape {
                    if let ShapeDim::Sym(s) = dim {
                        syms.push(s.clone());
                    }
                }
            }
            Type::Privacy(inner, _) => Self::collect_syms_from_type_inner(inner, syms),
            _ => {}
        }
    }
}