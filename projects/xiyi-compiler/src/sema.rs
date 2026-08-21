// sema.rs
use std::collections::{HashMap, HashSet};
use std::mem;
use std::fs;
use crate::ast::*;
use crate::hir;
use crate::hir_builder;

// 存一个 implement 块里的某个方法：方法本身的定义，加上这个 implement 块
// 的 target_type（比如 `implement<T> Option<T> { fn is_some(self) -> bool }`
// 里的 `Option<T>`，里面的 T 会是 Type::TypeParam("T")）。调用点要靠
// target_type 把 receiver 的具体类型（比如 Option<i32>）跟方法签名里的
// 类型变量对上号。
#[derive(Clone)]
struct MethodInfo {
    fn_def: FnDef,
    target_type: Type,
}

pub struct TypeChecker {
    pub scopes: Vec<HashMap<String, Type>>,
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    pub consts: HashMap<String, Type>,
    pub functions: HashMap<String, FnDef>,
    // 类型名 -> 方法名 -> MethodInfo。之前 implement 块从来没被收集过，
    // 方法调用只能靠"查不到就返回 I32"这种兜底，这张表补上之后，方法调用
    // 才能真正按方法自己的签名做类型检查。
    methods: HashMap<String, HashMap<String, MethodInfo>>,
    pub in_model: bool,
    pub fn_stack: Vec<String>,
    pub model_names: HashSet<String>,
    pub model_return_types: HashMap<String, Type>,
    pub model_sensitivities: HashMap<String, f64>,
    pub current_self_type: Option<Type>,
    // 新增：当前正在检查的函数的声明返回类型，供 Stmt::Return 用来给
    // return 语句里的表达式（比如裸 Err(...)）传递期望类型提示。
    pub current_return_type: Option<Type>,
    pub expr_types: HashMap<usize, Type>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            structs: HashMap::new(),
            enums: HashMap::new(),
            consts: HashMap::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            in_model: false,
            fn_stack: Vec::new(),
            model_names: HashSet::new(),
            model_return_types: HashMap::new(),
            model_sensitivities: HashMap::new(),
            current_self_type: None,
            current_return_type: None,
            expr_types: HashMap::new(),
        }
    }

    // 从一个类型里提取"用来查 methods 表的 key"——Struct/Enum/Generic 都是
    // 拿类型名本身当 key（Generic 也只取名字，不含泛型实参，因为同一个
    // `implement<T> Option<T>` 要覆盖所有 Option<具体类型>，不分开存）。
    // 其他类型（Tensor、I32 这些内置标量）暂时没有方法表，返回 None，
    // 调用点会退回旧的兜底行为。
    fn type_key_for_impl_target(ty: &Type) -> Option<String> {
        match ty {
            Type::Struct(name) => Some(name.clone()),
            Type::Enum(name) => Some(name.clone()),
            Type::Generic(name, _) => Some(name.clone()),
            _ => None,
        }
    }

    fn resolve_import(&self, module_path: &str, stdlib_path: &str) -> Result<String, String> {
        let path = format!("{}/xiyi-core/src/{}.xiyi", stdlib_path, module_path);
        if fs::metadata(&path).is_ok() {
            Ok(path)
        } else {
            Err(format!("module not found: {}", module_path))
        }
    }

    fn parse_rational(s: &str) -> Option<(i128, u128)> {
        let s = s.trim();
        if s.contains('/') {
            let parts: Vec<&str> = s.split('/').collect();
            if parts.len() == 2 {
                let num = parts[0].trim().parse::<i128>().ok()?;
                let den = parts[1].trim().parse::<u128>().ok()?;
                if den == 0 { return None; }
                return Some((num, den));
            }
            None
        } else if let Ok(num) = s.parse::<i128>() {
            Some((num, 1))
        } else if let Some((int_part, frac_part)) = s.split_once('.') {
            let sign = if s.starts_with('-') { -1 } else { 1 };
            let int_val = if int_part.is_empty() || int_part == "-" {
                0
            } else {
                int_part.parse::<i128>().ok()?
            };
            let frac_str = frac_part.trim_end_matches('0');
            let den = 10u128.pow(frac_str.len() as u32);
            let frac_val = if frac_str.is_empty() { 0 } else { frac_str.parse::<i128>().ok()? };
            let num = int_val * den as i128 + sign * frac_val;
            Some((num, den))
        } else {
            None
        }
    }

    fn rational_le(a: &str, b: &str) -> bool {
        let (na, da) = Self::parse_rational(a).unwrap_or((0, 1));
        let (nb, db) = Self::parse_rational(b).unwrap_or((0, 1));
        na * db as i128 <= nb * da as i128
    }

    fn rational_min(a: &str, b: &str) -> String {
        if Self::rational_le(a, b) { a.to_string() } else { b.to_string() }
    }

    fn rational_eq(a: &str, b: &str) -> bool {
        let (na, da) = Self::parse_rational(a).unwrap_or((0, 1));
        let (nb, db) = Self::parse_rational(b).unwrap_or((0, 1));
        na * db as i128 == nb * da as i128
    }

    pub fn check_program(&mut self, program: &Program) -> Result<hir::HirProgram, String> {
        for item in &program.items {
            match item {
                Item::FnDef(f) => {
                    self.functions.insert(f.name.clone(), f.clone());
                }
                Item::StructDef(s) => {
                    self.structs.insert(s.name.clone(), s.clone());
                }
                Item::EnumDef(e) => {
                    self.enums.insert(e.name.clone(), e.clone());
                }
                Item::ConstDef(c) => {
                    self.consts.insert(c.name.clone(), c.ty.clone());
                }
                Item::ModelDef(m) => {
                    self.model_names.insert(m.name.clone());

                    let struct_fields: Vec<StructField> = m.fields
                        .iter()
                        .map(|f| StructField {
                            name: f.name.clone(),
                            ty: self.strip_privacy(&f.ty),
                        })
                        .collect();
                    let struct_def = StructDef {
                        name: m.name.clone(),
                        fields: struct_fields,
                        generic_params: m.generic_params.clone(),  // ← 从 model 复制泛型参数
                    };
                    self.structs.insert(m.name.clone(), struct_def);

                    for fn_def in &m.functions {
                        if fn_def.name == "forward" {
                            if let Some(ret) = &fn_def.return_type {
                                self.model_return_types.insert(m.name.clone(), ret.clone());
                            }
                            for attr in &fn_def.attributes {
                                if attr.name == "sensitivity" {
                                    for arg in &attr.args {
                                        if let AttributeArg::KeyValue(key, val) = arg {
                                            if key == "const" {
                                                if let AttributeArg::Rational(r) = &**val {
                                                    let val: f64 = r.parse().unwrap_or(1.0);
                                                    self.model_sensitivities.insert(m.name.clone(), val);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Item::ProtoDef(_) => {}
                Item::Use(_) => {}
                Item::Implement(imp) => {
                    if let Some(key) = Self::type_key_for_impl_target(&imp.target_type) {
                        let entry = self.methods.entry(key).or_insert_with(HashMap::new);
                        for fn_def in &imp.functions {
                            entry.insert(
                                fn_def.name.clone(),
                                MethodInfo {
                                    fn_def: fn_def.clone(),
                                    target_type: imp.target_type.clone(),
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        for item in &program.items {
            match item {
                Item::FnDef(f) => self.check_fn_def(f)?,
                Item::ConstDef(c) => {
                    let value_type = self.check_expr(&c.value)?;
                    if !self.types_equal(&value_type, &c.ty) {
                        return Err(format!("const type mismatch: expected {:?}, got {:?}", c.ty, value_type));
                    }
                }
                Item::ModelDef(m) => {
                    let has_forward = m.functions.iter().any(|f| f.name == "forward");
                    if !has_forward {
                        return Err(format!("model '{}' must define a 'forward' function", m.name));
                    }
                    self.in_model = true;
                    self.current_self_type = Some(Type::Struct(m.name.clone()));
                    for fn_def in &m.functions {
                        for param in &fn_def.params {
                            if let Type::Struct(name) = &param.ty {
                                if !self.structs.contains_key(name) {
                                    return Err(format!("undefined struct type: {}", name));
                                }
                            }
                            if let Type::Enum(name) = &param.ty {
                                if !self.enums.contains_key(name) {
                                    return Err(format!("undefined enum type: {}", name));
                                }
                            }
                        }
                        self.check_fn_def(fn_def)?;
                    }
                    self.current_self_type = None;
                    self.in_model = false;
                }
                Item::ProtoDef(_) => {}
                Item::Use(_) => {}
                // 之前这里落在 _ => {} 里，implement 块里的方法体从来没被
                // 检查过——`is_some` 里的 `match self {...}` 对不对，压根
                // 没人验证。现在真正把每个方法当函数体检查一遍，`self` 的
                // 类型设成这个 implement 块的 target_type（跟 ModelDef 那边
                // 的处理方式一致）。
                Item::Implement(imp) => {
                    self.current_self_type = Some(imp.target_type.clone());
                    for fn_def in &imp.functions {
                        self.check_fn_def(fn_def)?;
                    }
                    self.current_self_type = None;
                }
                _ => {}
            }
        }

        let hir = hir_builder::HirBuilder::build(program, &self.expr_types)?;
        Ok(hir)
    }

    fn check_fn_def(&mut self, fn_def: &FnDef) -> Result<(), String> {
        if self.fn_stack.contains(&fn_def.name) {
            return Err("error[MD002]: recursion not allowed in model block; graph must be topologically sortable".to_string());
        }
        self.fn_stack.push(fn_def.name.clone());

        self.scopes.push(HashMap::new());
        for param in &fn_def.params {
            let ty = if param.name == "self" {
                self.current_self_type.clone().unwrap_or(Type::SelfType)
            } else {
                param.ty.clone()
            };
            self.scopes.last_mut().unwrap().insert(param.name.clone(), ty);
        }

        // 保存旧值再设置新值，支持嵌套函数检查时不互相污染。
        // 注意：不能直接用 ? 提前返回——那样一旦 check_block_with_expected
        // 报错，下面恢复旧值那行会被跳过，current_return_type 就一直脏
        // 着，污染后续的检查。用 match 显式处理两种结果，保证恢复动作
        // 无论成功失败都会执行。
        let prev_return_type = self.current_return_type.clone();
        self.current_return_type = fn_def.return_type.clone();

        let body_type_result = self.check_block_with_expected(&fn_def.body, fn_def.return_type.as_ref());

        self.current_return_type = prev_return_type;

        let body_type = body_type_result?;

        if let Some(expected) = &fn_def.return_type {
            if !self.types_equal(&body_type, expected) {
                return Err(format!("expected return type {:?}, got {:?}", expected, body_type));
            }
            if !self.types_equal_with_privacy(&body_type, expected) {
                return Err(format!("privacy label mismatch: expected {:?}, got {:?}", expected, body_type));
            }
        }

        if self.in_model && fn_def.name == "forward" {
            let has_dp_input = fn_def.params.iter().any(|p| {
                self.extract_privacy_tag(&p.ty)
                    .map_or(false, |tag| matches!(tag, PrivacyTag::Differential { .. }))
            });
            if has_dp_input {
                let has_ctx = fn_def.params.iter().any(|p| {
                    match &p.ty {
                        Type::Ref { mutable, inner } if *mutable => {
                            match &**inner {
                                Type::Struct(name) if name == "TrainingContext" => true,
                                _ => false,
                            }
                        }
                        _ => false,
                    }
                });
                if !has_ctx {
                    return Err("error[PR001]: forward with dp(ε) input requires `&mut TrainingContext`".to_string());
                }
            }
        }

        self.scopes.pop();
        self.fn_stack.pop();
        Ok(())
    }

    fn types_equal_with_privacy(&self, a: &Type, b: &Type) -> bool {
        match (a, b) {
            (Type::Privacy(inner1, tag1), Type::Privacy(inner2, tag2)) => {
                self.types_equal(inner1, inner2) && self.privacy_tags_equal(tag1, tag2)
            }
            (Type::Privacy(inner, _), other) => self.types_equal(inner, other),
            (other, Type::Privacy(inner, _)) => self.types_equal(other, inner),
            _ => self.types_equal(a, b),
        }
    }

    fn privacy_tags_equal(&self, a: &PrivacyTag, b: &PrivacyTag) -> bool {
        match (a, b) {
            (PrivacyTag::Public, PrivacyTag::Public) => true,
            (PrivacyTag::Private, PrivacyTag::Private) => true,
            (PrivacyTag::Differential { eps: e1, delta: d1 }, PrivacyTag::Differential { eps: e2, delta: d2 }) => {
                Self::rational_eq(e1, e2)
                    && match (d1, d2) {
                        (Some(d1), Some(d2)) => Self::rational_eq(d1, d2),
                        (None, None) => true,
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    fn join_privacy_tags(&self, a: &PrivacyTag, b: &PrivacyTag) -> PrivacyTag {
        match (a, b) {
            (PrivacyTag::Public, x) => x.clone(),
            (x, PrivacyTag::Public) => x.clone(),
            (PrivacyTag::Private, _) => PrivacyTag::Private,
            (_, PrivacyTag::Private) => PrivacyTag::Private,
            (PrivacyTag::Differential { eps: e1, delta: d1 }, PrivacyTag::Differential { eps: e2, delta: d2 }) => {
                let eps = Self::rational_min(e1, e2);
                let delta = match (d1, d2) {
                    (Some(d1), Some(d2)) => {
                        if Self::rational_le(d1, d2) { Some(d2.clone()) } else { Some(d1.clone()) }
                    }
                    (Some(d), None) | (None, Some(d)) => Some(d.clone()),
                    (None, None) => None,
                };
                PrivacyTag::Differential { eps, delta }
            }
        }
    }

    fn extract_privacy_tag(&self, ty: &Type) -> Option<PrivacyTag> {
        match ty {
            Type::Privacy(_, tag) => Some(tag.clone()),
            Type::Ref { inner, .. } => self.extract_privacy_tag(inner),
            _ => None,
        }
    }

    fn apply_privacy_tag(&self, ty: Type, tag: Option<PrivacyTag>) -> Type {
        match tag {
            Some(t) => Type::Privacy(Box::new(ty), t),
            None => ty,
        }
    }

    fn join_privacy_labels(&self, a: &Type, b: &Type) -> Option<PrivacyTag> {
        let tag_a = self.extract_privacy_tag(a);
        let tag_b = self.extract_privacy_tag(b);
        match (tag_a, tag_b) {
            (Some(t1), Some(t2)) => Some(self.join_privacy_tags(&t1, &t2)),
            (Some(t), None) => Some(t),
            (None, Some(t)) => Some(t),
            (None, None) => None,
        }
    }

    fn strip_privacy(&self, ty: &Type) -> Type {
        match ty {
            Type::Privacy(inner, _) => self.strip_privacy(inner),
            Type::Ref { mutable, inner } => Type::Ref {
                mutable: *mutable,
                inner: Box::new(self.strip_privacy(inner)),
            },
            _ => ty.clone(),
        }
    }

    // 看穿引用拿到底层类型（&str -> str，&&T -> T），方法调用时要用——
    // Rust 自己也是这样自动解引用去找方法的。
    fn strip_ref(&self, ty: &Type) -> Type {
        match ty {
            Type::Ref { inner, .. } => self.strip_ref(inner),
            other => other.clone(),
        }
    }

    // ===== 内建方法表：基础类型（str/整数等）身上"自带"的方法 =====
    // 这些类型是原语，没有对应的 implement 块能被查到（str 用户没法给它
    // implement，标准库也没这么做）——不补这张表，任何调用都会落进"查不到
    // 就默认 I32"的兜底，产出一个几乎总是错的类型。这里不是想做一套完整
    // 的基础类型方法体系，只覆盖标准库已经实际用到、会踩坑的这几个；
    // 以后再冒出新的（比如 i32.to_string()），照这个格式加一行就行。
    fn builtin_primitive_method_return_type(&self, receiver: &Type, method: &str) -> Option<Type> {
        match (receiver, method) {
            (Type::Str, "len") => Some(Type::U64),
            (Type::Str, "is_empty") => Some(Type::Bool),
            (Type::Str, "as_bytes") => Some(Type::Ref {
                mutable: false,
                inner: Box::new(Type::Slice(Box::new(Type::U8))),
            }),
            // .abs() 只对有符号数值类型有意义，返回类型跟接收者一致
            (t, "abs") if self.is_signed_numeric_type(t) => Some(t.clone()),
            _ => None,
        }
    }

    fn is_compile_time_constant(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Literal(_) => true,
            ExprKind::Sym(_) => true,
            ExprKind::Ident(name) => self.consts.contains_key(name),
            ExprKind::BinaryOp { left, right, .. } => {
                self.is_compile_time_constant(left) && self.is_compile_time_constant(right)
            }
            // 新增：一元负号也可能出现在常量表达式里（比如 -1 as ConstIntArray 元素）
            ExprKind::Unary { op: UnaryOp::Neg, expr } => self.is_compile_time_constant(expr),
            // 新增：lack &[T] 规范里明确写着"均为编译期常量"
            ExprKind::LackSlice(_) => true,
            _ => false,
        }
    }

    fn eval_const_int_expr(&self, expr: &Expr) -> Option<i64> {
        match &expr.kind {
            ExprKind::Literal(Literal::Int(v)) => Some(*v),
            ExprKind::Unary { op: UnaryOp::Neg, expr } => {
                self.eval_const_int_expr(expr).map(|v| -v)
            }
            ExprKind::BinaryOp { op, left, right } => {
                let l = self.eval_const_int_expr(left)?;
                let r = self.eval_const_int_expr(right)?;
                match op {
                    BinaryOp::Add => Some(l + r),
                    BinaryOp::Sub => Some(l - r),
                    BinaryOp::Mul => Some(l * r),
                    BinaryOp::Div => {
                        if r == 0 { None } else { Some(l / r) }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn shape_dim_to_i64(&self, dim: &ShapeDim) -> Result<i64, String> {
        match dim {
            ShapeDim::Const(c) => Ok(*c as i64),
            _ => Err("expected constant dimension".to_string()),
        }
    }

    fn check_block(&mut self, block: &Block) -> Result<Type, String> {
        self.check_block_with_expected(block, None)
    }

    // ===== check_block 的"带期望类型提示"版本 =====
    // 目前只有 check_fn_def 会传 expected（函数声明的返回类型），用来让
    // block 最后一句是 Ok(...)/Err(...) 这类裸枚举变体构造、或结构体初始化
    // 时，能正确推导出泛型参数，而不是留一个没绑定的 TypeParam 卡在那儿
    // 跟声明的返回类型对不上。中间的语句该怎么检查还怎么检查，只有最后
    // 一条、且是不带分号的尾随表达式（Stmt::ExprStmt）时才用这个提示。
    fn check_block_with_expected(&mut self, block: &Block, expected: Option<&Type>) -> Result<Type, String> {
        let mut last_type = Type::I32;
        let last_index = block.stmts.len().checked_sub(1);
        for (i, stmt) in block.stmts.iter().enumerate() {
            if Some(i) == last_index {
                if let Stmt::ExprStmt(expr) = stmt {
                    last_type = self.check_expr_with_expected(expr, expected)?;
                    continue;
                }
            }
            last_type = self.check_stmt(stmt)?;
        }
        Ok(last_type)
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<Type, String> {
        match stmt {
            Stmt::Let(let_stmt) => {
                // 关键修复：以前这里是"先用标注独立算出 resolved_ty，再单独调用
                // check_expr 算出 init_type，最后拿两个结果比较"——两次计算完全
                // 独立，标注里的信息（比如 `Result<i32, i32>` 里的 i32、i32）
                // 从来没有传进 check_expr 去参与右边表达式自己的泛型绑定推导。
                // 这导致 `let _: Result<i32, i32> = Result::Ok(1);` 这种写法，
                // 右边 `Result::Ok(1)` 只能从参数 `1` 推出 T=i32，E 没有任何
                // 参数能提供信息，只能留成 TypeParam("E")，跟左边标注里写明的
                // E=i32 对不上，报一个用户已经把答案写在标注里、编译器却没去看
                // 的假类型不匹配。
                //
                // 现在改成 check_expr_with_expected：把标注（如果有）作为提示
                // 一起传进去，只调用一次，EnumVariantConstruction/StructInit
                // 这两种会产生未绑定泛型参数的构造，会拿这个提示去补全 bindings。
                let init_type = self.check_expr_with_expected(&let_stmt.init, let_stmt.ty.as_ref())?;

                let resolved_ty = if let Some(ty) = &let_stmt.ty {
                    match ty {
                        Type::Struct(name) => {
                            if self.structs.contains_key(name) {
                                Type::Struct(name.clone())
                            } else if self.enums.contains_key(name) {
                                Type::Enum(name.clone())
                            } else {
                                return Err(format!("undefined type: {}", name));
                            }
                        }
                        Type::Tensor { dtype, shape } => Type::Tensor {
                            dtype: dtype.clone(),
                            shape: shape.clone(),
                        },
                        other => other.clone(),
                    }
                } else {
                    init_type.clone()
                };

                if !self.types_equal(&init_type, &resolved_ty) {
                    return Err(format!("type mismatch: expected {:?}, got {:?}", resolved_ty, init_type));
                }

                if let_stmt.persist {
                    if !matches!(init_type, Type::Tensor { .. }) {
                        return Err("error[PS002]: persist binding requires Tensor type".to_string());
                    }
                }

                let name = let_stmt.name.clone();
                self.scopes.last_mut().unwrap().insert(name, resolved_ty);
                // 关键修复：let 语句本身不产出值，之前硬编码 I32，是同一批
                // "语句被当成表达式类型用、却随手写了个 I32 占位"的历史遗留，
                // 跟 Stmt::Return 那次是同一类问题。
                Ok(Type::Unit)
            }
            Stmt::ExprStmt(expr) => self.check_expr(expr),
            Stmt::Return(expr_opt) => {
                if let Some(expr) = expr_opt {
                    // 关键修复：之前这里用的是普通 check_expr，`return`
                    // 后面的表达式从来拿不到"函数声明的返回类型"这个提示——
                    // `lack if den == 0 { return Err(()); }` 这种写法里，
                    // Err(()) 的泛型参数 E 全程没人告诉它该绑成什么，生成
                    // 的 Rust 代码里 Err(()) 本身也是模糊的，连 rustc 自己
                    // 都推不出来。现在用 current_return_type（check_fn_def
                    // 进入函数体检查前设置好的）当期望类型传下去，跟函数体
                    // 最后一句表达式享受的待遇一致。
                    let expected = self.current_return_type.clone();
                    self.check_expr_with_expected(expr, expected.as_ref())?;
                }
                // 关键修复：之前这里硬编码 Ok(Type::I32)，不管 return 的到底是
                // 什么类型。return 语句会让函数立刻退出，它所在的 block 并不会
                // 真的把这个类型"产出"给外层——语义上更接近"发散"，跟任何期望
                // 类型都该兼容。这里没有真正的 never/bottom 类型，用 Unit 作为
                // 实用近似：这样 `{ return xxx; }` 这种"整个 block 只有一句
                // return"的写法，会被视为 Unit 类型，能满足 `lack if` 的
                // "then 分支必须是 Unit"要求，也不会因为 I32 硬编码而跟其他
                // 类型的返回值意外冲突。
                Ok(Type::Unit)
            }
            Stmt::While(while_stmt) => {
                if self.in_model && !self.is_compile_time_constant(&while_stmt.cond) {
                    return Err(
                        "error[MD004]: while loop upper bound must be compile-time constant or `Sym<N>`; runtime variable bounds require `tensor.while_loop`"
                            .to_string(),
                    );
                }
                let cond_ty = self.check_expr(&while_stmt.cond)?;
                if cond_ty != Type::Bool {
                    return Err("while condition must be bool".to_string());
                }
                self.check_block(&while_stmt.body)?;
                // 同上：while 语句本身不产出值
                Ok(Type::Unit)
            }
            Stmt::For(for_stmt) => {
                if self.in_model {
                    if let ExprKind::Range { start, end } = &for_stmt.iterable.kind {
                        if !self.is_compile_time_constant(start) || !self.is_compile_time_constant(end) {
                            return Err(
                                "error[MD005]: runtime iterators not allowed in model block; use compile-time ranges or `Sym<N>` ranges"
                                    .to_string(),
                            );
                        }
                    } else {
                        return Err(
                            "error[MD005]: runtime iterators not allowed in model block; use compile-time ranges or `Sym<N>` ranges"
                                .to_string(),
                        );
                    }
                }
                self.check_expr(&for_stmt.iterable)?;
                self.scopes.push(HashMap::new());
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(for_stmt.var.clone(), Type::I32);
                let body_type = self.check_block(&for_stmt.body)?;
                self.scopes.pop();
                Ok(body_type)
            }
            Stmt::Assign(assign_stmt) => {
                // 关键修改：target 从裸变量名换成了任意表达式
                // （Ident/FieldAccess/Index），先检查这是不是一个"能被赋值
                // 的位置"（不能对字面量、函数调用结果这类东西赋值），
                // 类型本身直接用 check_expr 检查这个目标表达式即可——
                // Ident 会走原来"查作用域"那条路，FieldAccess/Index 会走
                // 各自已有的类型检查逻辑，undefined variable 这类报错
                // 自然由它们各自产生，不用在这里重复判断。
                if !self.is_assignable(&assign_stmt.target) {
                    return Err(format!(
                        "invalid assignment target: {:?}（只能对变量、字段、索引赋值）",
                        assign_stmt.target.kind
                    ));
                }
                let target_ty = self.check_expr(&assign_stmt.target)?;
                // 用带期望类型的版本检查右边——`self.cap = if cond { 1 } else
                // { self.cap * 2 };` 这类写法里的字面量分支，得靠这个才能
                // 正确迁就 target 的真实类型（跟 let 那边是同一套机制）。
                let expr_ty = self.check_expr_with_expected(&assign_stmt.expr, Some(&target_ty))?;
                if !self.types_equal(&expr_ty, &target_ty) {
                    return Err(format!(
                        "type mismatch in assignment: expected {:?}, got {:?}",
                        target_ty, expr_ty
                    ));
                }
                // 同上：赋值语句本身不产出值
                Ok(Type::Unit)
            }
            Stmt::Loop(loop_stmt) => {
                let body_type = self.check_block(&loop_stmt.body)?;
                Ok(body_type)
            }
            Stmt::Break(_) => Ok(Type::Unit),
            Stmt::UnsafeBlock(unsafe_stmt) => self.check_block(&unsafe_stmt.body),
        }
    }

    fn check_closure(
        &mut self,
        closure_expr: &Expr,
        param_ty: &Type,
        expected_ret: Option<&Type>,
    ) -> Result<Type, String> {
        let (param_name, body) = match &closure_expr.kind {
            ExprKind::Closure { param, body } => (param, body),
            _ => return Err("Expected a closure".to_string()),
        };

        let old_scopes = mem::take(&mut self.scopes);
        self.scopes.push(HashMap::new());
        self.scopes.last_mut().unwrap().insert(param_name.clone(), param_ty.clone());

        let ret_ty = self.check_expr(body);

        self.scopes = old_scopes;

        let ret_ty = ret_ty?;
        if let Some(expected) = expected_ret {
            if !self.types_equal_with_privacy(&ret_ty, expected) {
                return Err(format!(
                    "Closure return type {:?} does not match expected {:?}",
                    ret_ty, expected
                ));
            }
        }
        Ok(ret_ty)
    }

    fn get_call_arg_by_pos_or_name<'a>(
        &self,
        args: &'a [CallArg],
        pos: usize,
        name: &str,
    ) -> Result<(&'a Expr, String), String> {
        if let Some(arg) = args.get(pos) {
            match arg {
                CallArg::Positional(e) => return Ok((e, format!("positional #{}", pos + 1))),
                CallArg::Named(n, e) => {
                    if n == name {
                        return Ok((e, n.clone()));
                    }
                }
            }
        }

        for arg in args {
            if let CallArg::Named(n, e) = arg {
                if n == name {
                    return Ok((e, n.clone()));
                }
            }
        }

        Err(format!("argument '{}' not found (tried position {} and name '{}')", name, pos + 1, name))
    }

    fn extract_int_arg(&self, args: &[CallArg], name: &str) -> Result<i64, String> {
        for arg in args {
            if let CallArg::Named(n, expr) = arg {
                if n == name {
                    if let Some(v) = self.eval_const_int_expr(expr) {
                        return Ok(v);
                    } else {
                        return Err(format!(
                            "parameter '{}' is not a constant integer expression",
                            name
                        ));
                    }
                }
            }
        }
        Err(format!("parameter '{}' not found", name))
    }

    fn get_arg_type(&mut self, arg: &CallArg) -> Result<Type, String> {
        match arg {
            CallArg::Positional(expr) => self.check_expr(expr),
            CallArg::Named(_, expr) => self.check_expr(expr),
        }
    }

    fn try_cross_model_call(&mut self, func: &str, args: &[CallArg]) -> Result<Option<Type>, String> {
        if !self.in_model {
            return Ok(None);
        }
        if args.is_empty() {
            return Ok(None);
        }

        let first_arg = &args[0];
        let receiver_ty = self.get_arg_type(first_arg)?;
        let stripped_ty = self.strip_privacy(&receiver_ty);

        if let Type::Struct(model_name) = &stripped_ty {
            if self.model_names.contains(model_name) {
                if let Some(ret_ty) = self.model_return_types.get(model_name) {
                    let caller_label = self.extract_privacy_tag(&receiver_ty);
                    let callee_label = self.extract_privacy_tag(ret_ty);
                    let joined = match (caller_label, callee_label) {
                        (Some(c), Some(d)) => Some(self.join_privacy_tags(&c, &d)),
                        (Some(c), None) => Some(c),
                        (None, Some(d)) => Some(d),
                        (None, None) => None,
                    };
                    let result_ty = self.apply_privacy_tag(ret_ty.clone(), joined);
                    return Ok(Some(result_ty));
                }
            }
        }

        if let CallArg::Positional(expr) = first_arg {
            if let ExprKind::FieldAccess { struct_expr, field_name } = &expr.kind {
                if let ExprKind::Ident(s) = &struct_expr.kind {
                    if s == "self" {
                        if let Some(self_ty) = &self.current_self_type {
                            if let Type::Struct(struct_name) = self_ty {
                                if let Some(struct_def) = self.structs.get(struct_name) {
                                    for field in &struct_def.fields {
                                        if field.name == *field_name {
                                            let field_ty = self.strip_privacy(&field.ty);
                                            if let Type::Struct(model_name) = &field_ty {
                                                if self.model_names.contains(model_name) {
                                                    if let Some(ret_ty) = self.model_return_types.get(model_name) {
                                                        let caller_label = self.extract_privacy_tag(&receiver_ty);
                                                        let callee_label = self.extract_privacy_tag(ret_ty);
                                                        let joined = match (caller_label, callee_label) {
                                                            (Some(c), Some(d)) => Some(self.join_privacy_tags(&c, &d)),
                                                            (Some(c), None) => Some(c),
                                                            (None, Some(d)) => Some(d),
                                                            (None, None) => None,
                                                        };
                                                        let result_ty = self.apply_privacy_tag(ret_ty.clone(), joined);
                                                        return Ok(Some(result_ty));
                                                    }
                                                }
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    fn check_call_arg(&mut self, arg: &CallArg) -> Result<Type, String> {
        match arg {
            CallArg::Positional(expr) => self.check_expr(expr),
            CallArg::Named(_, expr) => self.check_expr(expr),
        }
    }

    fn check_binary_op(&self, op: &BinaryOp, left: &Type, right: &Type, left_expr: &Expr, right_expr: &Expr) -> Result<Type, String> {
        let left_inner = self.strip_privacy(left);
        let right_inner = self.strip_privacy(right);

        // 关键修复：裸整数字面量（0、1、2...）在 check_expr 里被固定标成
        // I32，但字面量本身没有"真实类型"——`while b != 0` 这种写法里的 0
        // 应该能跟 b 的 u128 兼容，不该反过来强制字面量也写成 u128。
        // 用"这一侧的原始表达式是不是字面量"来判断能不能放宽，而不是笼统
        // 放宽所有 I32——这样真正的 i32 变量跟 u128 变量比较时，还是会被
        // 正确地拦下来，不会被误放行。
        fn is_int_literal(e: &Expr) -> bool {
            matches!(e.kind, ExprKind::Literal(Literal::Int(_)))
        }
        let left_is_literal = is_int_literal(left_expr);
        let right_is_literal = is_int_literal(right_expr);

        let both_numeric = self.is_numeric_type(&left_inner) && self.is_numeric_type(&right_inner);
        let types_compatible = self.types_equal(&left_inner, &right_inner)
            || (both_numeric && (left_is_literal || right_is_literal));

        // 结果类型：两边类型本来就一致就直接用；不一致但被字面量放宽了，
        // 就用"非字面量那侧"的真实类型（字面量给真类型让步）
        let result_ty = if self.types_equal(&left_inner, &right_inner) {
            left_inner.clone()
        } else if left_is_literal {
            right_inner.clone()
        } else {
            left_inner.clone()
        };

        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                // 关键修复：之前这里只认 I32/F32，rational.xiyi 里满篇的
                // i128/u128 算术（gcd 的取模、num/den 的乘加）全部会被
                // "invalid operands for arithmetic" 拦下来。改成认所有数值
                // 类型，两边类型仍然必须完全一致（不做隐式类型提升——
                // 混合类型算术得先显式 `as` 转换，跟 Rust 的严格风格一致），
                // 除非其中一边是裸字面量（见上面 types_compatible）。
                let is_scalar = both_numeric && types_compatible;
                let is_tensor = match (&left_inner, &right_inner) {
                    (Type::Tensor { .. }, Type::Tensor { .. }) => {
                        self.types_equal(&left_inner, &right_inner)
                    }
                    _ => false,
                };
                if is_scalar || is_tensor {
                    let tag = self.join_privacy_labels(left, right);
                    Ok(self.apply_privacy_tag(result_ty, tag))
                } else {
                    Err(format!(
                        "invalid operands for arithmetic: {:?} and {:?}",
                        left, right
                    ))
                }
            }
            BinaryOp::Eq
            | BinaryOp::Neq
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::Le
            | BinaryOp::Ge => {
                if types_compatible {
                    let tag = self.join_privacy_labels(left, right);
                    Ok(self.apply_privacy_tag(Type::Bool, tag))
                } else {
                    Err(format!(
                        "comparison between different types: {:?} and {:?}",
                        left, right
                    ))
                }
            }
            BinaryOp::And | BinaryOp::Or => {
                if left_inner == Type::Bool && right_inner == Type::Bool {
                    let tag = self.join_privacy_labels(left, right);
                    Ok(self.apply_privacy_tag(Type::Bool, tag))
                } else {
                    Err("logical operators require bool operands".to_string())
                }
            }
        }
    }

    // ===== 数值类型判断，Cast（as）、一元负号、这次放宽的算术运算都要用 =====
    fn is_numeric_type(&self, ty: &Type) -> bool {
        matches!(
            ty,
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128
                | Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::U128
                | Type::F16 | Type::F32 | Type::F64
        )
    }

    // 一元负号只对"有符号"的数值类型有意义，U8..U128 排除在外
    // （对无符号数取负在这门语言里不打算隐式允许，想要的话得先 as 成有符号类型）
    fn is_signed_numeric_type(&self, ty: &Type) -> bool {
        matches!(
            ty,
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128
                | Type::F16 | Type::F32 | Type::F64
        )
    }

    // 索引表达式 expr[idx] 里的 idx 得是整数，不能是浮点数
    // （注：这门语言目前 Type 枚举里没有 usize/isize，vec.xiyi/string.xiyi
    // 里大量出现的 usize 现在其实解析不出来——这是另一个独立缺口，跟这次
    // 的 Index 表达式无关，等 parser 那边处理 usize 的时候一起补。这里先
    // 只要求"是整数类型"，不强求具体是哪一种）
    fn is_integer_type(&self, ty: &Type) -> bool {
        matches!(
            ty,
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128
                | Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::U128
        )
    }

    // 赋值语句的 target 从裸变量名换成任意表达式之后，得单独判断"这个
    // 表达式是不是一个能被赋值的位置"——变量、字段、索引可以，字面量、
    // 函数调用结果、二元运算结果这些都不行。目前允许的范围跟标准库
    // 实际用到的写法（i = ...、self.len = ...、arr[i] = ...）对齐，以后
    // 真要支持解引用赋值（*ptr = ...）再回来加 Unary 那个分支。
    fn is_assignable(&self, expr: &Expr) -> bool {
        matches!(
            expr.kind,
            ExprKind::Ident(_) | ExprKind::FieldAccess { .. } | ExprKind::Index { .. }
        )
    }

    fn types_equal(&self, a: &Type, b: &Type) -> bool {
        match (a, b) {
            // ----- 整数类型 -----
            (Type::I8, Type::I8) => true,
            (Type::I16, Type::I16) => true,
            (Type::I32, Type::I32) => true,
            (Type::I64, Type::I64) => true,
            (Type::I128, Type::I128) => true,
            (Type::U8, Type::U8) => true,
            (Type::U16, Type::U16) => true,
            (Type::U32, Type::U32) => true,
            (Type::U64, Type::U64) => true,
            (Type::U128, Type::U128) => true,

            // ----- 浮点类型 -----
            (Type::F16, Type::F16) => true,
            (Type::F32, Type::F32) => true,
            (Type::F64, Type::F64) => true,

            // ----- 其他基本类型 -----
            (Type::Bool, Type::Bool) => true,
            (Type::Char, Type::Char) => true,
            (Type::Str, Type::Str) => true,
            (Type::Unit, Type::Unit) => true,

            // ----- 结构体、枚举、泛型等 -----
            (Type::Struct(name1), Type::Struct(name2)) => name1 == name2,
            (Type::Enum(name1), Type::Enum(name2)) => name1 == name2,
            (Type::Generic(name1, args1), Type::Generic(name2, args2)) => {
                if name1 != name2 {
                    return false;
                }
                if args1.len() != args2.len() {
                    return false;
                }
                args1.iter().zip(args2.iter()).all(|(a, b)| self.types_equal(a, b))
            }
            (Type::Ref { mutable: m1, inner: i1 }, Type::Ref { mutable: m2, inner: i2 }) => {
                m1 == m2 && self.types_equal(i1, i2)
            }
            // ===== 新增：切片类型 [T]，递归比较元素类型 =====
            // 没有这条的话，Slice 会直接落到最下面的 `_ => false`，导致
            // &[u8] 跟 &[u8] 永远被判定成不相等——这不是"漏了一个具体
            // case"，是 Slice 这个类型压根还没接进 types_equal 里。
            (Type::Slice(t1), Type::Slice(t2)) => self.types_equal(t1, t2),
            (
                Type::Tensor {
                    dtype: d1,
                    shape: s1,
                },
                Type::Tensor {
                    dtype: d2,
                    shape: s2,
                },
            ) => {
                if !self.types_equal(&**d1, &**d2) {
                    return false;
                }
                if s1.len() != s2.len() {
                    return false;
                }
                for (dim1, dim2) in s1.iter().zip(s2.iter()) {
                    match (dim1, dim2) {
                        (ShapeDim::Const(c1), ShapeDim::Const(c2)) => {
                            if c1 != c2 {
                                return false;
                            }
                        }
                        (ShapeDim::Sym(s1_name), ShapeDim::Sym(s2_name)) => {
                            if s1_name != s2_name {
                                return false;
                            }
                        }
                        (ShapeDim::Dyn, ShapeDim::Dyn) => {}
                        _ => return false,
                    }
                }
                true
            }
            (Type::ConstIntArray(v1), Type::ConstIntArray(v2)) => v1 == v2,
            (Type::Privacy(inner1, _), Type::Privacy(inner2, _)) => {
                self.types_equal(inner1, inner2)
            }
            (Type::Privacy(inner, _), other) => self.types_equal(inner, other),
            (other, Type::Privacy(inner, _)) => self.types_equal(other, inner),
            // ----- 泛型类型变量：只在“同一个变量名”时相等 -----
            // 注意：这里保持严格——types_equal 用在函数体内部（比如检查
            // `fn id<T>(x: T) -> T { x }` 的函数体返回值跟声明的返回类型
            // 是否一致），此时 T 应该被当成一个不透明的抽象类型，不能悄悄
            // 跟任何具体类型相等。真正“T 可以绑定成任意具体类型”这件事，
            // 只发生在调用点/构造点，交给下面的 unify_type，不要混进这里。
            (Type::TypeParam(n1), Type::TypeParam(n2)) => n1 == n2,
            _ => false,
        }
    }

    // ===== 辅助：ast::GenericParam -> Vec<String>，跟 hir_builder.rs 里
    // 新加的 build_generic_params 是同一个用途，sema.rs 这边独立需要一份 =====
    // 关键修复：ast.rs 把 GenericParam::Type(String) 改成了
    // GenericParam::Type { name, bounds }（结构体变体，带 trait bound），
    // 这里的模式匹配没跟上，会直接编译不过。bounds 目前 sema 还不做检查，
    // 先用 `..` 吃掉。
    fn generic_param_names(params: &[GenericParam]) -> Vec<String> {
        params
            .iter()
            .map(|gp| match gp {
                GenericParam::Type { name, .. } => name.clone(),
            })
            .collect()
    }

    // ===== 泛型实例化核心：合一（unify） =====
    //
    // expected 是声明里写的类型（可能包含 Type::TypeParam），actual 是调用点/
    // 构造点实际算出来的类型。bindings 记录这一次调用/构造过程中每个类型变量
    // 已经绑定成了什么，跨多个参数/字段共享——保证同一个类型变量在一次调用里
    // 前后绑定一致（比如 `fn pair<T>(a: T, b: T) -> T`，a、b 必须绑定成同一个
    // 具体类型，不能一个绑 i32 一个绑 bool）。
    //
    // 目前处理了“类型变量出现在最外层”和“嵌套在 Ref/Privacy/Tensor/Generic
    // 内部”两类情况的递归匹配，没有实现完整的高阶合一——现有测试用例（裸类型
    // 变量、或嵌套一层）都在这个范围内，遇到更复杂的场景再扩。
    fn unify_type(&self, actual: &Type, expected: &Type, bindings: &mut HashMap<String, Type>) -> bool {
        match expected {
            Type::TypeParam(name) => {
                if let Some(bound) = bindings.get(name) {
                    self.types_equal(bound, actual)
                } else {
                    bindings.insert(name.clone(), actual.clone());
                    true
                }
            }
            Type::Ref { mutable: em, inner: einner } => {
                if let Type::Ref { mutable: am, inner: ainner } = actual {
                    em == am && self.unify_type(ainner, einner, bindings)
                } else {
                    false
                }
            }
            // ===== 新增：切片类型 [T]——跟 Ref 同一个套路，递归 unify 元素
            // 类型，这样 &[T] 里的 T 才能在调用点正确绑定，不是只靠
            // types_equal 死板比较（那样永远绑不出 T）。
            Type::Slice(einner) => {
                if let Type::Slice(ainner) = actual {
                    self.unify_type(ainner, einner, bindings)
                } else {
                    false
                }
            }
            Type::Privacy(einner, _) => {
                let stripped_actual = self.strip_privacy(actual);
                self.unify_type(&stripped_actual, einner, bindings)
            }
            Type::Generic(ename, eargs) => {
                if let Type::Generic(aname, aargs) = actual {
                    ename == aname
                        && eargs.len() == aargs.len()
                        && eargs.iter().zip(aargs.iter()).all(|(e, a)| self.unify_type(a, e, bindings))
                } else {
                    false
                }
            }
            Type::Tensor { dtype: edtype, shape: eshape } => {
                if let Type::Tensor { dtype: adtype, shape: ashape } = actual {
                    self.unify_type(adtype, edtype, bindings) && eshape == ashape
                } else {
                    false
                }
            }
            _ => self.types_equal(actual, expected),
        }
    }

    // 把 bindings 里记录的绑定代入 ty 中所有出现的 Type::TypeParam，结构性
    // 递归替换（Ref/Privacy/Tensor/Generic 内部也会替换）。没被绑定的类型
    // 变量原样保留成 TypeParam，不用假类型占位——调用点没能推导出来是真的
    // 推不出来，应该让后面用到它的地方去决定报错还是怎么处理，而不是悄悄
    // 塞一个 I32 掩盖过去。
    fn substitute_type(&self, ty: &Type, bindings: &HashMap<String, Type>) -> Type {
        match ty {
            Type::TypeParam(name) => bindings.get(name).cloned().unwrap_or_else(|| ty.clone()),
            Type::Ref { mutable, inner } => Type::Ref {
                mutable: *mutable,
                inner: Box::new(self.substitute_type(inner, bindings)),
            },
            // 同 Ref，递归替换切片元素类型里的泛型参数
            Type::Slice(inner) => Type::Slice(Box::new(self.substitute_type(inner, bindings))),
            Type::Privacy(inner, tag) => {
                Type::Privacy(Box::new(self.substitute_type(inner, bindings)), tag.clone())
            }
            Type::Generic(name, args) => Type::Generic(
                name.clone(),
                args.iter().map(|a| self.substitute_type(a, bindings)).collect(),
            ),
            Type::Tensor { dtype, shape } => Type::Tensor {
                dtype: Box::new(self.substitute_type(dtype, bindings)),
                shape: shape.clone(),
            },
            other => other.clone(),
        }
    }

    // ===== EnumVariantConstruction 的检查逻辑，独立成方法 =====
    //
    // 改用 unify_type/bindings 而不是死板的 types_equal，同时让无参变体
    // （比如 Option::None）在枚举本身有泛型参数时也一致返回 Type::Generic，
    // 而不是裸 Type::Enum（后者会跟同一个 match 里其他分支推出来的
    // Type::Generic 对不上，导致 match 分支类型不一致的假报错）。
    //
    // 新增的 `expected` 参数：如果调用方（目前只有 check_expr_with_expected）
    // 手头有一个外部期望类型（典型来源是显式类型标注，比如
    // `let _: Result<i32, i32> = Result::Ok(1);` 里的 `Result<i32, i32>`），
    // 就把它按声明顺序预置进 bindings——这样即使某个泛型参数完全没出现在
    // 这次构造的参数里（`Result<T, E>` 构造 `Ok(1)` 时，参数只能推出 T，
    // E 单靠参数永远推不出来），也能借助标注拿到值。这不是完整的双向类型
    // 推导，只处理“构造表达式外面刚好套了一层显式标注”这一种情况，够用。
    fn check_enum_variant_construction(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        args: &[CallArg],
        expected: Option<&Type>,
    ) -> Result<Type, String> {
        // 关键新增：`Rational::gcd(a, b)` 这种"限定路径调用"在 parser.rs 里
        // 跟 `Result::Ok(1)` 长得一模一样（都是 Ident::Ident(args)），parser
        // 没法只靠语法区分"枚举变体构造"和"调用某个类型 impl 块里的静态
        // 函数"，索性统一解析成 EnumVariantConstruction，把区分这件事留给
        // 这里——sema 手里有完整的符号表：先按枚举变体构造尝试，如果
        // enum_name 根本不是已知枚举，退一步查 self.methods（Item::Implement
        // 注册进去的函数表），当成限定路径的静态调用检查。两边都查不到
        // 才真正报错。
        if !self.enums.contains_key(enum_name) {
            if self.methods.get(enum_name).map_or(false, |m| m.contains_key(variant_name)) {
                return self.check_qualified_static_call(enum_name, variant_name, args, expected);
            }
            return Err(format!("undefined enum: {}", enum_name));
        }

        let enum_def = self.enums.get(enum_name)
            .ok_or_else(|| format!("undefined enum: {}", enum_name))?
            .clone();

        let variant = enum_def.variants.iter()
            .find(|v| v.name == *variant_name)
            .ok_or_else(|| format!("enum {} has no variant {}", enum_name, variant_name))?
            .clone();

        let mut bindings: HashMap<String, Type> = HashMap::new();

        // 关键新增：预置来自外部期望类型的绑定
        if let Some(Type::Generic(exp_name, exp_args)) = expected {
            if exp_name == enum_name && exp_args.len() == enum_def.generic_params.len() {
                let generic_names = Self::generic_param_names(&enum_def.generic_params);
                for (name, ty) in generic_names.iter().zip(exp_args.iter()) {
                    bindings.insert(name.clone(), ty.clone());
                }
            }
        }

        if let Some(expected_ty) = &variant.ty {
            if args.len() != 1 {
                return Err(format!("variant expects 1 argument"));
            }
            let arg_expr = match &args[0] {
                CallArg::Positional(e) => e,
                CallArg::Named(_, e) => e,
            };
            let arg_ty = self.check_call_arg(&args[0])?;
            // 关键修复：跟 check_struct_init 同一个坑——裸整数字面量默认是
            // I32，但变体payload 声明的可能是 i128/u64 这类别的数值类型
            // （目前已绑定的泛型参数也可能已经把 expected_ty 具体化成这类
            // 类型），字面量应该让步迁就真实类型。
            let resolved_expected = self.substitute_type(expected_ty, &bindings);
            let effective_arg_ty = if matches!(arg_expr.kind, ExprKind::Literal(Literal::Int(_)))
                && self.is_numeric_type(&self.strip_privacy(&resolved_expected))
            {
                resolved_expected.clone()
            } else {
                arg_ty.clone()
            };
            // 注意：unify_type 遇到已经在 bindings 里的名字，会去校验一致性
            // 而不是直接覆盖——所以就算标注和实参同时提供了同一个类型变量的
            // 信息，这里仍然会检查两者是否矛盾，而不是标注说了算、实参不用管。
            if !self.unify_type(&effective_arg_ty, expected_ty, &mut bindings) {
                return Err(format!("type mismatch: expected {:?}, got {:?}", expected_ty, arg_ty));
            }
        } else if !args.is_empty() {
            return Err(format!("variant takes no arguments"));
        }

        if enum_def.generic_params.is_empty() {
            Ok(Type::Enum(enum_name.to_string()))
        } else {
            let generic_names = Self::generic_param_names(&enum_def.generic_params);
            let type_args: Vec<Type> = generic_names
                .iter()
                .map(|name| {
                    bindings
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| Type::TypeParam(name.clone()))
                })
                .collect();
            Ok(Type::Generic(enum_name.to_string(), type_args))
        }
    }

    // ===== 限定路径静态调用：`TypeName::func(args)`，没有 self 接收者 =====
    // 跟已有的方法调用解析逻辑（check_expr 里 is_method 那段，查 self.methods
    // 表 → unify 参数 → 替换返回类型）共享同一套思路，唯一区别是没有
    // receiver，不需要先做"receiver 类型 vs impl 块 target_type"的合一，
    // 所有形参直接按位置对实参。
    fn check_qualified_static_call(
        &mut self,
        type_name: &str,
        func_name: &str,
        args: &[CallArg],
        expected: Option<&Type>,
    ) -> Result<Type, String> {
        let method_info = self
            .methods
            .get(type_name)
            .and_then(|m| m.get(func_name))
            .cloned()
            .ok_or_else(|| format!("undefined function `{}::{}`", type_name, func_name))?;

        let mut bindings: HashMap<String, Type> = HashMap::new();

        // 关键新增：跟 check_enum_variant_construction 同一个套路——用外部
        // 期望类型预置绑定。Vec::new()/Vec::with_capacity() 这类"返回值带
        // 泛型参数 T，但参数列表里完全看不到 T"的静态函数，光靠参数
        // 没法推出 T 到底是什么——`String { vec: Vec::new() }` 报的
        // "expected Generic(Vec,[U8]), got Generic(Vec,[TypeParam(T)])"
        // 就是这么来的：T 全程没人告诉它该是 U8。这里从"这个返回值将被
        // 用在什么类型的位置"反推。尽力而为，unify 不上就放着不报错——
        // 真正的类型不匹配，交给外层（比如 check_struct_init 的字段
        // 比较）在真正比较的时候报错，不在这一步抢先报错。
        if let (Some(expected_ty), Some(ret_ty)) = (expected, &method_info.fn_def.return_type) {
            self.unify_type(expected_ty, ret_ty, &mut bindings);
        }

        let params: Vec<&Param> = method_info
            .fn_def
            .params
            .iter()
            .filter(|p| p.name != "self")
            .collect();
        if params.len() != args.len() {
            return Err(format!(
                "`{}::{}` expects {} argument(s), got {}",
                type_name, func_name, params.len(), args.len()
            ));
        }
        for (param, arg) in params.iter().zip(args) {
            let arg_ty = self.check_call_arg(arg)?;
            if !self.unify_type(&arg_ty, &param.ty, &mut bindings) {
                return Err(format!(
                    "type mismatch in call to `{}::{}`: parameter `{}` expected {:?}, got {:?}",
                    type_name, func_name, param.name, param.ty, arg_ty
                ));
            }
        }

        Ok(method_info
            .fn_def
            .return_type
            .clone()
            .map(|ret| self.substitute_type(&ret, &bindings))
            .unwrap_or(Type::Unit))
    }

    // ===== StructInit 的检查逻辑，独立成方法，同样的 expected 提示套路 =====
    fn check_struct_init(
        &mut self,
        struct_name: &str,
        fields: &[(String, Expr)],
        expected: Option<&Type>,
    ) -> Result<Type, String> {
        let struct_def = self
            .structs
            .get(struct_name)
            .ok_or_else(|| format!("undefined struct: {}", struct_name))?
            .clone();
        if fields.len() != struct_def.fields.len() {
            return Err(format!(
                "struct {} expects {} fields, got {}",
                struct_name,
                struct_def.fields.len(),
                fields.len()
            ));
        }
        let mut field_map = HashMap::new();
        for field in &struct_def.fields {
            field_map.insert(field.name.clone(), field.ty.clone());
        }
        let mut bindings: HashMap<String, Type> = HashMap::new();

        if let Some(Type::Generic(exp_name, exp_args)) = expected {
            if exp_name == struct_name && exp_args.len() == struct_def.generic_params.len() {
                let generic_names = Self::generic_param_names(&struct_def.generic_params);
                for (name, ty) in generic_names.iter().zip(exp_args.iter()) {
                    bindings.insert(name.clone(), ty.clone());
                }
            }
        }

        for (field_name, field_expr) in fields {
            let expected_ty = field_map
                .get(field_name)
                .ok_or_else(|| format!("unknown field '{}' in struct {}", field_name, struct_name))?
                .clone();
            // 顺手用带期望类型的版本检查字段表达式——这样字段本身是
            // Ok(...)/Err(...)/嵌套 StructInit 时，也能像函数返回值那样
            // 受益于期望类型驱动的泛型参数推导（上一轮给 check_fn_def
            // 加的那套逻辑）。
            let actual_ty = self.check_expr_with_expected(field_expr, Some(&expected_ty))?;
            // 关键修复：裸整数字面量（0、1...）默认类型是 I32，但字段声明
            // 的是 i128/u64 这类别的数值类型时，字面量应该让步迁就字段的
            // 真实类型——跟 check_binary_op 里对字面量的处理是同一个道理，
            // 只是那次只覆盖了二元运算，没覆盖到结构体字段初始化这条独立
            // 路径。
            let effective_ty = if matches!(field_expr.kind, ExprKind::Literal(Literal::Int(_)))
                && self.is_numeric_type(&self.strip_privacy(&expected_ty))
            {
                expected_ty.clone()
            } else {
                actual_ty.clone()
            };
            if !self.unify_type(&effective_ty, &expected_ty, &mut bindings) {
                return Err(format!(
                    "field '{}' type mismatch: expected {:?}, got {:?}",
                    field_name, expected_ty, actual_ty
                ));
            }
        }
        if struct_def.generic_params.is_empty() {
            Ok(Type::Struct(struct_name.to_string()))
        } else {
            let generic_names = Self::generic_param_names(&struct_def.generic_params);
            let type_args: Vec<Type> = generic_names
                .iter()
                .map(|name| {
                    bindings
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| Type::TypeParam(name.clone()))
                })
                .collect();
            Ok(Type::Generic(struct_name.to_string(), type_args))
        }
    }

    // ===== check_expr 的“带期望类型提示”版本 =====
    //
    // 目前只在 EnumVariantConstruction / StructInit 这两个会产生尚未绑定
    // 泛型参数的构造上使用这个提示；其他表达式种类原样委托给 check_expr，
    // 行为不变。调用方目前只有 Stmt::Let（当有显式类型标注时）。
    fn check_expr_with_expected(&mut self, expr: &Expr, expected: Option<&Type>) -> Result<Type, String> {
        match (&expr.kind, expected) {
            (ExprKind::EnumVariantConstruction { enum_name, variant_name, args }, Some(expected_ty)) => {
                self.check_enum_variant_construction(enum_name, variant_name, args, Some(expected_ty))
            }
            (ExprKind::StructInit { struct_name, fields }, Some(expected_ty)) => {
                self.check_struct_init(struct_name, fields, Some(expected_ty))
            }
            // 关键新增：裸 Ok(...)/Err(...)/Some(...)/None 这类写法（没有
            // Result::/Option:: 前缀）解析出来是 ExprKind::Call，不是
            // EnumVariantConstruction，之前这里漏了这条，导致 `Ok(...)`
            // 作为函数体最后一句、要跟声明的返回类型对齐推导泛型参数时，
            // 完全走不到这个"带期望类型"的分支，泛型参数（比如
            // Result<Rational, E> 里的 E）就留在没绑定的状态，跟声明的
            // 返回类型（比如 Result<Rational, ()>）对不上。
            (ExprKind::Call { qualifier: None, func, args, is_method: false }, Some(expected_ty)) => {
                let bare_matches: Vec<String> = self
                    .enums
                    .iter()
                    .filter(|(_, e)| e.variants.iter().any(|v| v.name == *func))
                    .map(|(name, _)| name.clone())
                    .collect();
                if bare_matches.len() == 1 {
                    self.check_enum_variant_construction(&bare_matches[0], func, args, Some(expected_ty))
                } else {
                    self.check_expr(expr)
                }
            }
            // 关键新增：裸整数字面量直接迁就期望类型。之前字面量的类型
            // 只在"直接出现在某个已知目标类型的位置"（结构体字段、二元
            // 运算的一侧、枚举变体参数）才会被特殊处理，`if` 分支这种
            // "字面量被 if 包了一层"的情况完全没人管，这里先把最基础的
            // 一层补上，供下面 If 分支递归调用时使用。
            (ExprKind::Literal(Literal::Int(_)), Some(expected_ty))
                if self.is_numeric_type(&self.strip_privacy(expected_ty)) =>
            {
                Ok(expected_ty.clone())
            }
            // 关键新增：字面量的一元负号（比如 -1）同理——`-1` 现在解析成
            // Unary{Neg, Literal(1)}，不再是一个裸的 Literal(-1)，得单独
            // 认一下，不然 `let sign: i128 = if cond { -1 } else { 1 };`
            // 这种写法里的 -1 永远没法迁就 i128。
            (ExprKind::Unary { op: UnaryOp::Neg, expr: inner }, Some(expected_ty))
                if matches!(inner.kind, ExprKind::Literal(Literal::Int(_)))
                    && self.is_signed_numeric_type(&self.strip_privacy(expected_ty)) =>
            {
                Ok(expected_ty.clone())
            }
            // 关键新增：if 表达式——把期望类型递归传给 then/else 两个分支，
            // 这样 `let sign: i128 = if cond { -1 } else { 1 };` 这种写法，
            // 标注里的 i128 才能真正传到 -1/1 这两个字面量分支上，而不是
            // 各自独立按默认的 I32 检查、跟外层标注对不上。
            // 关键新增（补上上一轮漏掉的一层）：if 的 then/else 分支永远是
            // `{ ... }` 包起来的 Block，不是裸表达式——`{ -1 }` 实际上是
            // ExprKind::Block(Block{stmts:[ExprStmt(Unary{Neg,Literal(1)})]})。
            // 上一轮新增的 If 分支递归调用 check_expr_with_expected 时，
            // then_expr/else_expr 是 Block，根本匹配不到裸 Literal/Unary
            // 那两个分支，期望类型的传递在跨进 `{ }` 的那一刻就断了。这里
            // 补上 Block 分支，直接复用 check_block_with_expected（跟函数体
            // 那次用的是同一个函数），把期望类型继续往块里最后一句传。
            (ExprKind::Block(block), Some(expected_ty)) => {
                self.check_block_with_expected(block, Some(expected_ty))
            }
            (ExprKind::If { kind: if_kind, cond, then_expr, else_expr }, Some(expected_ty)) => {
                let cond_ty = self.check_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err("if condition must be bool".to_string());
                }
                let then_ty = self.check_expr_with_expected(then_expr, Some(expected_ty))?;
                match if_kind {
                    IfKind::Normal => {
                        let else_ty = match else_expr {
                            Some(e) => self.check_expr_with_expected(e, Some(expected_ty))?,
                            None => {
                                return Err(
                                    "if expression requires an else branch（如果这个 if 不需要产出值、纯粹是副作用，请显式写成 `lack if`）"
                                        .to_string(),
                                )
                            }
                        };
                        if !self.types_equal_with_privacy(&then_ty, &else_ty) {
                            return Err(format!(
                                "if branches have different types: then = {:?}, else = {:?}",
                                then_ty, else_ty
                            ));
                        }
                        let joined_tag = self.join_privacy_labels(&then_ty, &else_ty);
                        let base_ty = self.strip_privacy(&then_ty);
                        Ok(self.apply_privacy_tag(base_ty, joined_tag))
                    }
                    IfKind::Lack => {
                        if else_expr.is_some() {
                            return Err(
                                "`lack if` must not have an else branch（既然写了 else，就该用普通 if，不要用 lack if）"
                                    .to_string(),
                            );
                        }
                        let then_inner = self.strip_privacy(&then_ty);
                        if then_inner != Type::Unit {
                            return Err(format!(
                                "`lack if` 的 then 分支必须是 Unit 类型（纯副作用、不产出值），得到 {:?}",
                                then_ty
                            ));
                        }
                        Ok(Type::Unit)
                    }
                }
            }
            _ => self.check_expr(expr),
        }
    }

    // ==================== check_expr ====================
    fn check_expr(&mut self, expr: &Expr) -> Result<Type, String> {
        let result = match &expr.kind {
            ExprKind::Literal(lit) => match lit {
                Literal::Int(_) => Ok(Type::I32),
                Literal::Float(_) => Ok(Type::F32),
                Literal::Bool(_) => Ok(Type::Bool),
                Literal::String(_) => Ok(Type::Str),
                Literal::Unit => Ok(Type::Unit),
                // 新增：bytes"..." 字节字符串字面量，类型是 &[u8]
                Literal::ByteString(_) => Ok(Type::Ref {
                    mutable: false,
                    inner: Box::new(Type::Slice(Box::new(Type::U8))),
                }),
            },
            ExprKind::Ident(name) => {
                if name == "self" {
                    if let Some(ty) = &self.current_self_type {
                        return Ok(ty.clone());
                    }
                }
                for scope in self.scopes.iter().rev() {
                    if let Some(ty) = scope.get(name) {
                        return Ok(ty.clone());
                    }
                }
                if let Some(ty) = self.consts.get(name) {
                    return Ok(ty.clone());
                }
                Err(format!("undefined variable or constant: {}", name))
            }
            ExprKind::Sym(_) => Ok(Type::I32),
            ExprKind::Closure { param, body } => {
                self.scopes.push(HashMap::new());
                self.scopes.last_mut().unwrap().insert(param.clone(), Type::F32);
                let body_ty = self.check_expr(body)?;
                self.scopes.pop();
                Ok(body_ty)
            }
            ExprKind::BinaryOp { op, left, right } => {
                let left_ty = self.check_expr(left)?;
                let right_ty = self.check_expr(right)?;
                self.check_binary_op(op, &left_ty, &right_ty, left, right)
            }
            // ===== 新增：一元运算符（Neg / Not）=====
            ExprKind::Unary { op, expr } => {
                let inner_ty = self.check_expr(expr)?;
                let stripped = self.strip_privacy(&inner_ty);
                match op {
                    UnaryOp::Neg => {
                        if !self.is_signed_numeric_type(&stripped) {
                            return Err(format!(
                                "cannot apply unary `-` to {:?}（只支持有符号数值类型，\
                                无符号类型想取负得先 `as` 成有符号类型）",
                                inner_ty
                            ));
                        }
                        let privacy_tag = self.extract_privacy_tag(&inner_ty);
                        Ok(self.apply_privacy_tag(stripped, privacy_tag))
                    }
                    UnaryOp::Not => {
                        if stripped != Type::Bool {
                            return Err(format!(
                                "cannot apply `!` to non-bool type: {:?}",
                                inner_ty
                            ));
                        }
                        let privacy_tag = self.extract_privacy_tag(&inner_ty);
                        Ok(self.apply_privacy_tag(Type::Bool, privacy_tag))
                    }
                }
            }
            // ===== 新增：as 类型转换，目前只放开数值类型互转 =====
            ExprKind::Cast { expr, ty: cast_ty } => {
                let inner_ty = self.check_expr(expr)?;
                let stripped = self.strip_privacy(&inner_ty);
                if !self.is_numeric_type(&stripped) || !self.is_numeric_type(cast_ty) {
                    return Err(format!(
                        "invalid cast: `as` 目前只支持数值类型之间的转换，得到 {:?} as {:?}",
                        inner_ty, cast_ty
                    ));
                }
                let privacy_tag = self.extract_privacy_tag(&inner_ty);
                Ok(self.apply_privacy_tag(cast_ty.clone(), privacy_tag))
            }
            // ===== 新增：索引表达式 expr[idx] =====
            ExprKind::Index { expr, index } => {
                let base_ty = self.check_expr(expr)?;
                let idx_ty = self.check_expr(index)?;
                let idx_stripped = self.strip_privacy(&idx_ty);
                if !self.is_integer_type(&idx_stripped) {
                    return Err(format!(
                        "index must be an integer type, got {:?}",
                        idx_ty
                    ));
                }

                // 剥掉引用/隐私标签，一路往里找"元素类型"：
                // - &T / &mut T -> 直接看里面的 T
                // - [T]（真正的切片类型，现在有了）-> T
                // - Vec<T>（或任何单参数泛型，兜底用）-> T
                // - Str -> 按字节索引，元素是 U8（对应 bytes[i] 这种写法）
                fn element_type(ty: &Type) -> Result<Type, String> {
                    match ty {
                        Type::Ref { inner, .. } => element_type(inner),
                        Type::Slice(inner) => Ok((**inner).clone()),
                        Type::Generic(_, args) if args.len() == 1 => Ok(args[0].clone()),
                        Type::Str => Ok(Type::U8),
                        other => Err(format!("type {:?} does not support indexing", other)),
                    }
                }
                let stripped_base = self.strip_privacy(&base_ty);
                let elem_ty = element_type(&stripped_base)?;
                let privacy_tag = self.extract_privacy_tag(&base_ty);
                Ok(self.apply_privacy_tag(elem_ty, privacy_tag))
            }
            // ===== lack &[T] 空切片字面量 =====
            ExprKind::LackSlice(elem_ty) => {
                // 关键修正：作者更新了规范（8.2.1）——泛型参数其实是【允许，
                // 单态化后验证】的，跟"必须是具体类型"这条旧规则正好相反。
                // 之前这里禁止 TypeParam 是按旧版规范写的，现在改成放开。
                //
                // 规范里真正该禁止的两类——impl Trait（error[LI009]）、
                // 未受约束的裸关联类型如 T::Item（error[LI008]）——这门
                // 语言的类型系统（ast::Type）里目前根本没有这两个概念
                // 对应的变体，语法层面写不出来，天然就不可能出现，所以
                // 这里没有对应的检查代码：不是漏检查，是压根不存在能触发
                // 它们的输入。等以后这门语言真的支持 impl Trait / 关联
                // 类型语法了，再回来把这两条错误码接上。
                //
                // never（warn[NE004]）同理：这门语言目前也没有 Type::Never
                // 这个类型，而且这套类型检查器目前只有"报错"这一种反馈
                // 机制（返回 Result<Type, String>），没有独立于报错之外的
                // "警告"通道——等 Never 类型和警告机制都补上了，再回来加
                // 这条 NE004。
                Ok(Type::Ref {
                    mutable: false,
                    inner: Box::new(Type::Slice(Box::new(elem_ty.clone()))),
                })
            }
            ExprKind::Call {
                qualifier,
                func,
                args,
                is_method,
            } => {
                // 关键新增：qualifier 非空说明这是 parser 侧未来可能产出的
                // 限定路径调用（目前 parser.rs 实际上还是把 `Type::func(...)`
                // 统一走 EnumVariantConstruction 那条路，check_enum_variant_construction
                // 里已经加了同样的兜底——这里加上是为了不管以后 parser 从哪条
                // 路产出 qualifier: Some(_)，sema 都认得，不用再改一遍。
                if let Some(q) = qualifier {
                    return self.check_qualified_static_call(q, func, args, None);
                }

                // 关键新增：裸 Ok(...)/Err(...)/Some(...)/None 这类写法——
                // 没有 Result::/Option:: 前缀，parser 只能把它们解析成普通
                // Call（qualifier: None），不是 EnumVariantConstruction。
                // 语言规范里这些写法就是不加前缀直接用的（等同于 Rust 里
                // Option::{Some,None}/Result::{Ok,Err} 被自动放进 prelude
                // 作用域）。这里不是专门为 Ok/Err/Some/None 硬编码四个名字，
                // 而是通用规则：在所有已注册的枚举里找"哪个枚举有一个恰好
                // 叫这个名字的变体"，找到且唯一就当枚举变体构造处理；同一个
                // 名字被多个枚举用作变体名时（真撞了）就报错让用户写限定
                // 路径消歧义，不去猜。
                if !is_method {
                    let matches: Vec<String> = self
                        .enums
                        .iter()
                        .filter(|(_, e)| e.variants.iter().any(|v| v.name == *func))
                        .map(|(name, _)| name.clone())
                        .collect();
                    if matches.len() == 1 {
                        return self.check_enum_variant_construction(&matches[0], func, args, None);
                    } else if matches.len() > 1 {
                        return Err(format!(
                            "ambiguous bare variant `{}`: matches multiple enums ({}), use a qualified path like EnumName::{}(...)",
                            func,
                            matches.join(", "),
                            func
                        ));
                    }
                }

                if func == "print" {
                    if self.in_model {
                        return Err(
                            "error[MD001]: side-effect not allowed in model block".to_string()
                        );
                    }
                    for arg in args {
                        self.check_call_arg(arg)?;
                    }
                    return Ok(Type::I32);
                }

                // 关键修复：`!expr` 以前在 parser.rs 里被脱糖成
                // Call{ func: "not", args: [expr] }，这里靠字符串名字硬凑
                // 类型检查。现在 parser.rs 已经改成产出真正的
                // ExprKind::Unary{ op: UnaryOp::Not, .. } 了，这个特判是死
                // 代码——对应的类型检查逻辑挪到下面新增的 ExprKind::Unary
                // 分支里去了，这里直接删掉，免得留着误导人。


                // `panic(msg)`——之前完全没注册过，sema.rs 查不到就报
                // "undefined function or method: panic"，codegen.rs 那边现在
                // 已经在处理 func == "panic" 时生成 Rust 的 panic!(...) 宏了。
                // 这里补类型检查：唯一参数必须是字符串。
                //
                // 返回类型比较特殊：panic 在运行时永远不会真的"返回"，这门
                // 语言的 Type 枚举里没有 Rust 那种 `!`（never）类型，不打算
                // 为这一个内置函数专门加一个新 Type 变体。这里先返回
                // Type::Unit，真正让它能出现在"其他分支返回 i32/bool/..."的
                // match 里的，是下面 ExprKind::Match 那段新加的豁免——分支
                // 表达式如果就是裸的 panic(...) 调用，不参与"所有分支类型
                // 必须一致"的比较（效果上等价于 Rust 的 `!` 能兼容任何类型）。
                if func == "panic" {
                    if args.len() != 1 {
                        return Err("`panic` expects exactly 1 argument (a message)".to_string());
                    }
                    let arg_ty = self.check_call_arg(&args[0])?;
                    if self.strip_privacy(&arg_ty) != Type::Str {
                        return Err(format!(
                            "`panic` expects a string argument, got {:?}",
                            arg_ty
                        ));
                    }
                    return Ok(Type::Unit);
                }

                // `from_utf8_unchecked(bytes)`——同样是标准库里用了、但从没
                // 被定义过的内建函数（string.xiyi 的 as_str 方法用它把
                // &[u8] 强转成 &str），跟 panic 是同一类问题。参数必须是
                // &[u8]，返回 &str；不检查调用点是不是真的在 unsafe 块内——
                // 这门编译器目前没有追踪"当前是否处于 unsafe 上下文"的机制，
                // 属于另一个独立的、更大的安全检查缺口，不在这次范围内。
                if func == "from_utf8_unchecked" {
                    if args.len() != 1 {
                        return Err(
                            "`from_utf8_unchecked` expects exactly 1 argument".to_string()
                        );
                    }
                    let arg_ty = self.check_call_arg(&args[0])?;
                    let expected_arg_ty = Type::Ref {
                        mutable: false,
                        inner: Box::new(Type::Slice(Box::new(Type::U8))),
                    };
                    if !self.types_equal(&self.strip_privacy(&arg_ty), &expected_arg_ty) {
                        return Err(format!(
                            "`from_utf8_unchecked` expects &[u8], got {:?}",
                            arg_ty
                        ));
                    }
                    return Ok(Type::Ref {
                        mutable: false,
                        inner: Box::new(Type::Str),
                    });
                }

                if let Some(ty) = self.try_cross_model_call(func, args)? {
                    return Ok(ty);
                }

                if self.in_model && self.fn_stack.contains(func) {
                    return Err(
                        "error[MD002]: recursion not allowed in model block; graph must be topologically sortable"
                            .to_string(),
                    );
                }

                // ---- 普通函数调用（含泛型函数：fn id<T>(x: T) -> T） ----
                if !*is_method {
                    if let Some(fn_def) = self.functions.get(func) {
                        let fn_params = fn_def.params.clone();
                        let fn_return = fn_def.return_type.clone();

                        if fn_params.len() != args.len() {
                            return Err(format!(
                                "function `{}` expects {} arguments, got {}",
                                func, fn_params.len(), args.len()
                            ));
                        }

                        // 关键修复：以前这里用 types_equal 死板比较，`id(42)` 这种
                        // 调用会拿 I32 去跟声明里写的 T（Type::TypeParam("T")）比较，
                        // 永远不相等。现在改成 unify_type——遇到 T 就记录“T 绑定成了
                        // 什么”，同一个函数调用里所有参数共享同一张绑定表，保证
                        // `fn pair<T>(a: T, b: T)` 这种多处用到同一个 T 的场景绑定一致。
                        let mut bindings: HashMap<String, Type> = HashMap::new();
                        for (param, arg) in fn_params.iter().zip(args) {
                            let arg_ty = self.check_call_arg(arg)?;
                            if !self.unify_type(&arg_ty, &param.ty, &mut bindings) {
                                return Err(format!(
                                    "type mismatch in call to `{}`: parameter `{}` expected {:?}, got {:?}",
                                    func, param.name, param.ty, arg_ty
                                ));
                            }
                        }

                        // 返回类型里出现的 T 也要代入绑定结果，否则 `id(42)` 的返回类型
                        // 还是裸的 Type::TypeParam("T")，后面 `let _ = id(42);` 之类的赋值
                        // 检查又会因为类型对不上而报错。
                        // 关键修复：函数没声明返回类型时，之前兜底成
                        // I32——一个"没写返回类型"的函数语义上该是 Unit
                        // （不返回有意义的值），跟 I32 完全是两码事，硬编码
                        // 成 I32 会在这类函数的调用结果被用在别处时，跟
                        // 真实语义对不上。
                        let result_ty = fn_return
                            .map(|ret| self.substitute_type(&ret, &bindings))
                            .unwrap_or(Type::Unit);
                        return Ok(result_ty);
                    }
                }

                // ===== tensor.cond =====
                if func == "tensor.cond" {
                    let input_expr = self.get_call_arg_by_pos_or_name(args, 0, "input")?.0;
                    let cond_expr = self.get_call_arg_by_pos_or_name(args, 1, "condition")?.0;
                    let then_expr = self.get_call_arg_by_pos_or_name(args, 2, "then")?.0;
                    let else_expr = self.get_call_arg_by_pos_or_name(args, 3, "else")?.0;

                    let input_ty = self.check_expr(input_expr)?;
                    let _ = self.check_closure(cond_expr, &input_ty, Some(&Type::Bool))?;
                    let then_ty = self.check_closure(then_expr, &input_ty, Some(&input_ty))?;
                    let else_ty = self.check_closure(else_expr, &input_ty, Some(&input_ty))?;
                    if !self.types_equal_with_privacy(&then_ty, &else_ty) {
                        return Err("tensor.cond 'then' and 'else' branches have different types".to_string());
                    }
                    return Ok(input_ty);
                }

                // ===== tensor.while_loop =====
                if func == "tensor.while_loop" {
                    let init_expr = self.get_call_arg_by_pos_or_name(args, 0, "init")?.0;
                    let cond_expr = self.get_call_arg_by_pos_or_name(args, 1, "cond")?.0;
                    let body_expr = self.get_call_arg_by_pos_or_name(args, 2, "body")?.0;

                    let init_ty = self.check_expr(init_expr)?;
                    let _ = self.check_closure(cond_expr, &init_ty, Some(&Type::Bool))?;
                    let _ = self.check_closure(body_expr, &init_ty, Some(&init_ty))?;
                    return Ok(init_ty);
                }

                if func == "embedding" {
                    if args.len() < 2 {
                        return Err("embedding requires at least two arguments".to_string());
                    }
                    let receiver_ty = self.get_arg_type(&args[0])?;
                    let _num_embeddings = self.extract_int_arg(args, "num_embeddings")?;
                    let embedding_dim = self.extract_int_arg(args, "embedding_dim")?;
                    if let Type::Tensor { dtype, ref shape } = self.strip_privacy(&receiver_ty) {
                        let mut new_shape = shape.clone();
                        new_shape.push(ShapeDim::Const(embedding_dim as usize));
                        let privacy_tag = self.extract_privacy_tag(&receiver_ty);
                        let result_ty = Type::Tensor {
                            dtype: Box::new(Type::F32),
                            shape: new_shape,
                        };
                        return Ok(self.apply_privacy_tag(result_ty, privacy_tag));
                    } else {
                        return Err(format!("embedding expects a tensor, got {:?}", receiver_ty));
                    }
                }

                if func == "linear" {
                    if args.len() < 2 {
                        return Err("linear requires at least two arguments".to_string());
                    }
                    let receiver_ty = self.get_arg_type(&args[0])?;
                    let in_val = self.extract_int_arg(args, "in")?;
                    let out_val = self.extract_int_arg(args, "out")?;
                    if let Type::Tensor { dtype, ref shape } = self.strip_privacy(&receiver_ty) {
                        if let Some(last) = shape.last() {
                            if let ShapeDim::Const(c) = last {
                                if *c != in_val as usize {
                                    return Err(format!(
                                        "shape mismatch in linear: input last dim {} does not match 'in' value {}",
                                        c, in_val
                                    ));
                                }
                            }
                            let mut new_shape = shape.clone();
                            if let Some(last) = new_shape.last_mut() {
                                *last = ShapeDim::Const(out_val as usize);
                            } else {
                                return Err("tensor must have at least one dimension".to_string());
                            }
                            let privacy_tag = self.extract_privacy_tag(&receiver_ty);
                            let result_ty = Type::Tensor { dtype, shape: new_shape };
                            return Ok(self.apply_privacy_tag(result_ty, privacy_tag));
                        } else {
                            return Err("tensor must have at least one dimension for linear".to_string());
                        }
                    } else {
                        return Err(format!("linear expects a tensor, got {:?}", receiver_ty));
                    }
                }

                // ===== 以下这几个内置函数（conv2d/max_pool2d/flatten/reshape/
                // relu/dropout/layer_norm/sum）在上一版发我的文件里被写成
                // "为了简洁，此处省略，实际文件应包含完整实现" 这样一句注释，
                // 内容是空的。这里补回来的是更早一轮你发过的、经过验证的完整
                // 实现，逻辑没有改动，只是物理位置挪到这里、跟 unify_type 无关。=====

                if func == "conv2d" {
                    if args.len() < 2 {
                        return Err("conv2d requires at least 2 arguments".to_string());
                    }
                    let receiver_ty = self.get_arg_type(&args[0])?;
                    let stripped = self.strip_privacy(&receiver_ty);
                    if let Type::Tensor { dtype, ref shape } = stripped {
                        if shape.len() != 3 && shape.len() != 4 {
                            return Err("conv2d expects 3D [C, H, W] or 4D [B, C, H, W] tensor".to_string());
                        }
                        let (c_idx, h_idx, w_idx) = if shape.len() == 4 { (1, 2, 3) } else { (0, 1, 2) };

                        let _in_channels = self.extract_int_arg(args, "in")?;
                        let out_channels = self.extract_int_arg(args, "out")?;
                        let kernel = self.extract_int_arg(args, "kernel")?;
                        let stride = self.extract_int_arg(args, "stride").unwrap_or(1);
                        let padding = self.extract_int_arg(args, "padding").unwrap_or(0);

                        let h_out = match &shape[h_idx] {
                            ShapeDim::Const(h) => {
                                let h = *h as i64;
                                let h_out = (h + 2 * padding - kernel) / stride + 1;
                                if h_out <= 0 {
                                    return Err("conv2d output height non-positive".to_string());
                                }
                                ShapeDim::Const(h_out as usize)
                            }
                            _ => ShapeDim::Dyn,
                        };
                        let w_out = match &shape[w_idx] {
                            ShapeDim::Const(w) => {
                                let w = *w as i64;
                                let w_out = (w + 2 * padding - kernel) / stride + 1;
                                if w_out <= 0 {
                                    return Err("conv2d output width non-positive".to_string());
                                }
                                ShapeDim::Const(w_out as usize)
                            }
                            _ => ShapeDim::Dyn,
                        };

                        let mut new_shape = shape.clone();
                        new_shape[c_idx] = ShapeDim::Const(out_channels as usize);
                        new_shape[h_idx] = h_out;
                        new_shape[w_idx] = w_out;

                        let privacy_tag = self.extract_privacy_tag(&receiver_ty);
                        let result_ty = Type::Tensor { dtype, shape: new_shape };
                        return Ok(self.apply_privacy_tag(result_ty, privacy_tag));
                    } else {
                        return Err(format!("conv2d expects a tensor, got {:?}", receiver_ty));
                    }
                }

                if func == "max_pool2d" {
                    if args.len() < 1 {
                        return Err("max_pool2d requires at least 1 argument".to_string());
                    }
                    let receiver_ty = self.get_arg_type(&args[0])?;
                    let kernel = self.extract_int_arg(args, "kernel")?;
                    let stride = self.extract_int_arg(args, "stride").unwrap_or(kernel);
                    if let Type::Tensor { dtype, ref shape } = self.strip_privacy(&receiver_ty) {
                        if shape.len() != 3 && shape.len() != 4 {
                            return Err("max_pool2d expects 3D or 4D tensor".to_string());
                        }
                        let (h_idx, w_idx) = if shape.len() == 4 { (2, 3) } else { (1, 2) };

                        let h_out = match &shape[h_idx] {
                            ShapeDim::Const(h) => {
                                let h = *h as i64;
                                let h_out = (h - kernel) / stride + 1;
                                if h_out <= 0 {
                                    return Err("max_pool2d output height non-positive".to_string());
                                }
                                ShapeDim::Const(h_out as usize)
                            }
                            _ => ShapeDim::Dyn,
                        };
                        let w_out = match &shape[w_idx] {
                            ShapeDim::Const(w) => {
                                let w = *w as i64;
                                let w_out = (w - kernel) / stride + 1;
                                if w_out <= 0 {
                                    return Err("max_pool2d output width non-positive".to_string());
                                }
                                ShapeDim::Const(w_out as usize)
                            }
                            _ => ShapeDim::Dyn,
                        };
                        let mut new_shape = shape.clone();
                        new_shape[h_idx] = h_out;
                        new_shape[w_idx] = w_out;
                        let privacy_tag = self.extract_privacy_tag(&receiver_ty);
                        let result_ty = Type::Tensor { dtype, shape: new_shape };
                        return Ok(self.apply_privacy_tag(result_ty, privacy_tag));
                    } else {
                        return Err(format!("max_pool2d expects a tensor, got {:?}", receiver_ty));
                    }
                }

                if func == "flatten" {
                    if args.len() < 1 {
                        return Err("flatten requires at least 1 argument".to_string());
                    }
                    let receiver_ty = self.get_arg_type(&args[0])?;
                    if let Type::Tensor { dtype, ref shape } = self.strip_privacy(&receiver_ty) {
                        let mut total = 1;
                        let mut all_const = true;
                        for dim in shape {
                            if let ShapeDim::Const(c) = dim {
                                total *= *c;
                            } else {
                                all_const = false;
                                break;
                            }
                        }
                        let privacy_tag = self.extract_privacy_tag(&receiver_ty);
                        let result_ty = if all_const {
                            let new_shape = vec![ShapeDim::Const(total)];
                            Type::Tensor { dtype, shape: new_shape }
                        } else {
                            Type::Tensor {
                                dtype: dtype.clone(),
                                shape: shape.clone(),
                            }
                        };
                        return Ok(self.apply_privacy_tag(result_ty, privacy_tag));
                    } else {
                        return Err(format!("flatten expects a tensor, got {:?}", receiver_ty));
                    }
                }

                if func == "reshape" {
                    if args.len() < 2 {
                        return Err("reshape requires target shape".to_string());
                    }
                    let receiver_ty = self.get_arg_type(&args[0])?;
                    let shape_vals = match &args[1] {
                        CallArg::Positional(expr) | CallArg::Named(_, expr) => {
                            let arg_ty = self.check_expr(expr)?;
                            if let Type::ConstIntArray(vals) = arg_ty {
                                vals
                            } else {
                                return Err(
                                    "reshape expects a constant integer array for shape".to_string()
                                );
                            }
                        }
                    };
                    let new_shape: Vec<ShapeDim> = shape_vals
                        .iter()
                        .map(|&v| ShapeDim::Const(v as usize))
                        .collect();
                    let dtype = match self.strip_privacy(&receiver_ty) {
                        Type::Tensor { dtype, .. } => dtype,
                        _ => return Err("reshape expects a tensor".to_string()),
                    };
                    let privacy_tag = self.extract_privacy_tag(&receiver_ty);
                    let result_ty = Type::Tensor { dtype, shape: new_shape };
                    return Ok(self.apply_privacy_tag(result_ty, privacy_tag));
                }

                if func == "relu" || func == "dropout" || func == "layer_norm" {
                    if args.len() < 1 {
                        return Err(format!("{} requires at least 1 argument", func));
                    }
                    let arg_ty = self.get_arg_type(&args[0])?;
                    return Ok(arg_ty);
                }

                if func == "sum" {
                    if args.len() < 1 {
                        return Err("sum requires at least 1 argument".to_string());
                    }
                    let _ = self.get_arg_type(&args[0])?;
                    return Ok(Type::F32);
                }

                // 方法调用：查 methods 表，按方法自己的签名（含泛型）检查
                if *is_method {
                    if args.is_empty() {
                        return Err("method call requires a receiver".to_string());
                    }
                    let receiver_ty = self.check_call_arg(&args[0])?;
                    let receiver_stripped = self.strip_privacy(&receiver_ty);

                    // 关键新增：内建方法表，先查这个，再查 self.methods。
                    // str/整数这类基础类型没有对应的 implement 块（str 是
                    // 原语，标准库也没给它 implement），之前查不到就无差别
                    // 回退成 I32——这正是 `s.len()` 被判成 I32、跟
                    // Vec::with_capacity 要求的 U64 对不上号的根源。这里
                    // strip_ref 是因为 receiver 常常是 &str 这种带引用的
                    // 形式，方法调用要先看穿引用查到底层类型（就像 Rust
                    // 的自动解引用）。
                    let receiver_base = self.strip_ref(&receiver_stripped);
                    if let Some(ret_ty) =
                        self.builtin_primitive_method_return_type(&receiver_base, func)
                    {
                        for arg in &args[1..] {
                            self.check_call_arg(arg)?;
                        }
                        return Ok(ret_ty);
                    }

                    if let Some(key) = Self::type_key_for_impl_target(&receiver_stripped) {
                        if let Some(method_info) = self.methods.get(&key).and_then(|m| m.get(func)).cloned() {
                            let mut bindings: HashMap<String, Type> = HashMap::new();
                            // 先把 receiver 的具体类型（比如 Option<i32>）跟这个
                            // implement 块的 target_type（Option<T>）合一，解出
                            // T 绑定成了什么，再用这份绑定去对方法自己的参数/
                            // 返回类型做替换。
                            if !self.unify_type(&receiver_stripped, &method_info.target_type, &mut bindings) {
                                return Err(format!(
                                    "receiver type {:?} does not match implement target {:?} for method `{}`",
                                    receiver_ty, method_info.target_type, func
                                ));
                            }

                            let non_self_params: Vec<&Param> = method_info
                                .fn_def
                                .params
                                .iter()
                                .filter(|p| p.name != "self")
                                .collect();
                            let extra_args = &args[1..];
                            if non_self_params.len() != extra_args.len() {
                                return Err(format!(
                                    "method `{}` expects {} argument(s), got {}",
                                    func, non_self_params.len(), extra_args.len()
                                ));
                            }
                            for (param, arg) in non_self_params.iter().zip(extra_args) {
                                let arg_ty = self.check_call_arg(arg)?;
                                if !self.unify_type(&arg_ty, &param.ty, &mut bindings) {
                                    return Err(format!(
                                        "type mismatch in call to `{}`: parameter `{}` expected {:?}, got {:?}",
                                        func, param.name, param.ty, arg_ty
                                    ));
                                }
                            }

                            // 同上：方法没声明返回类型时该是 Unit，不是 I32
                            let result_ty = method_info
                                .fn_def
                                .return_type
                                .clone()
                                .map(|ret| self.substitute_type(&ret, &bindings))
                                .unwrap_or(Type::Unit);
                            return Ok(result_ty);
                        }
                    }

                    // 查不到已注册的方法（比如 receiver 是内置标量/张量类型，
                    // 或者方法确实没被任何 implement 块定义过）——退回旧的
                    // 兜底行为，只检查剩余参数、返回 I32，不阻断这类调用。
                    for arg in &args[1..] {
                        self.check_call_arg(arg)?;
                    }
                    return Ok(Type::I32);
                }

                if self.in_model && self.model_names.contains(func) {
                    if let Some(ty) = self.try_cross_model_call(func, args)? {
                        return Ok(ty);
                    }
                }

                for arg in args {
                    self.check_call_arg(arg)?;
                }
                Err(format!("undefined function or method: {}", func))
            }
            ExprKind::Block(block) => self.check_block(block),
            ExprKind::StructInit { struct_name, fields } => {
                self.check_struct_init(struct_name, fields, None)
            }
            ExprKind::FieldAccess { struct_expr, field_name } => {
                let struct_ty = self.check_expr(struct_expr)?;
                let stripped_ty = self.strip_privacy(&struct_ty);
                match stripped_ty {
                    Type::Struct(name) => {
                        let struct_def = self
                            .structs
                            .get(&name)
                            .ok_or_else(|| format!("undefined struct: {}", name))?;
                        for field in &struct_def.fields {
                            if field.name == *field_name {
                                return Ok(self.strip_privacy(&field.ty));
                            }
                        }
                        Err(format!("field '{}' not found in struct {}", field_name, name))
                    }
                    // ===== 关键新增：泛型结构体（比如 Vec<T>）=====
                    // current_self_type 在泛型 implement 块里存的是
                    // Generic("Vec", [TypeParam("T")])，不是 Struct("Vec")——
                    // Vec 自己的方法体访问 self.len/self.cap/self.ptr 这些
                    // 字段时，一直被前面那个 Struct 分支漏掉，落进最下面的
                    // "field access on non-struct type" 兜底错误。这里按
                    // 结构体名找到定义，同时用泛型实参（这里是 [TypeParam("T")]，
                    // 还没具体化）替换字段声明类型里的泛型参数——
                    // 保证以后要是有字段类型直接引用 T（比如 ptr: *mut T），
                    // 取出来的类型也是正确替换过的，不是裸的 TypeParam。
                    Type::Generic(name, type_args) => {
                        let struct_def = self
                            .structs
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| format!("undefined struct: {}", name))?;
                        let param_names: Vec<String> = struct_def
                            .generic_params
                            .iter()
                            .map(|gp| match gp {
                                GenericParam::Type { name, .. } => name.clone(),
                            })
                            .collect();
                        let bindings: HashMap<String, Type> = param_names
                            .into_iter()
                            .zip(type_args.into_iter())
                            .collect();
                        for field in &struct_def.fields {
                            if field.name == *field_name {
                                let field_ty = self.substitute_type(&field.ty, &bindings);
                                return Ok(self.strip_privacy(&field_ty));
                            }
                        }
                        Err(format!("field '{}' not found in struct {}", field_name, name))
                    }
                    Type::SelfType => {
                        if let Some(ty) = &self.current_self_type {
                            if let Type::Struct(name) = ty {
                                let struct_def = self
                                    .structs
                                    .get(name)
                                    .ok_or_else(|| format!("undefined struct: {}", name))?;
                                for field in &struct_def.fields {
                                    if field.name == *field_name {
                                        return Ok(self.strip_privacy(&field.ty));
                                    }
                                }
                                return Err(format!(
                                    "field '{}' not found in struct {}",
                                    field_name, name
                                ));
                            }
                        }
                        Err("field access on SelfType with no current self type".to_string())
                    }
                    other => Err(format!("field access on non-struct type: {:?}", other)),
                }
            }
            ExprKind::Range { start, end } => {
                let start_ty = self.check_expr(start)?;
                let end_ty = self.check_expr(end)?;
                if (start_ty == Type::I32 || start_ty == Type::I64)
                    && (end_ty == Type::I32 || end_ty == Type::I64)
                {
                    Ok(Type::I32)
                } else {
                    Err("range bounds must be integers".to_string())
                }
            }
            ExprKind::If { kind: if_kind, cond, then_expr, else_expr } => {
                if self.in_model {
                    let cond_ty = self.check_expr(cond)?;
                    let cond_stripped = self.strip_privacy(&cond_ty);
                    if let Type::Tensor { .. } = cond_stripped {
                        return Err("error[MD003]: runtime tensor condition must use explicit dynamic operator `tensor.cond`".to_string());
                    }
                }

                let cond_ty = self.check_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err("if condition must be bool".to_string());
                }
                let then_ty = self.check_expr(then_expr)?;

                match if_kind {
                    // ===== Normal：老规矩，else 必须有，两分支类型必须一致 =====
                    IfKind::Normal => {
                        let else_ty = if let Some(else_expr) = else_expr {
                            self.check_expr(else_expr)?
                        } else {
                            // 关键：不写 lack 又不写 else，依旧是错——这门语言
                            // 喜欢显式声明，"没有 else"这件事必须靠 `lack if`
                            // 明说，不能靠"忘了写"蒙混过去。
                            return Err(
                                "if expression requires an else branch（如果这个 if 不需要产出值、纯粹是副作用，请显式写成 `lack if`）"
                                    .to_string(),
                            );
                        };
                        if !self.types_equal_with_privacy(&then_ty, &else_ty) {
                            return Err(format!(
                                "if branches have different types: then = {:?}, else = {:?}",
                                then_ty, else_ty
                            ));
                        }
                        let joined_tag = self.join_privacy_labels(&then_ty, &else_ty);
                        let base_ty = self.strip_privacy(&then_ty);
                        Ok(self.apply_privacy_tag(base_ty, joined_tag))
                    }
                    // ===== Lack：反过来，else 必须没有，then 必须是 Unit =====
                    IfKind::Lack => {
                        if else_expr.is_some() {
                            return Err(
                                "`lack if` must not have an else branch（既然写了 else，就该用普通 if，不要用 lack if）"
                                    .to_string(),
                            );
                        }
                        let then_inner = self.strip_privacy(&then_ty);
                        if then_inner != Type::Unit {
                            return Err(format!(
                                "`lack if` 的 then 分支必须是 Unit 类型（纯副作用、不产出值），得到 {:?}（如果这个 if 需要产出值，请改成普通 if 并补上 else 分支）",
                                then_ty
                            ));
                        }
                        Ok(Type::Unit)
                    }
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                if elements.is_empty() {
                    return Err("array literal cannot be empty".to_string());
                }
                let mut values = Vec::new();
                for elem in elements {
                    let ty = self.check_expr(elem)?;
                    if let Type::I32 | Type::I64 = ty {
                        if let Some(v) = self.eval_const_int_expr(elem) {
                            values.push(v);
                        } else {
                            return Err(format!(
                                "array element is not a constant integer: {:?}",
                                elem
                            ));
                        }
                    } else {
                        return Err(format!(
                            "array literal elements must be integers, got {:?}",
                            ty
                        ));
                    }
                }
                Ok(Type::ConstIntArray(values))
            }
            ExprKind::EnumVariantAccess { enum_name, variant_name } => {
                let enum_def = self
                    .enums
                    .get(enum_name)
                    .ok_or_else(|| format!("undefined enum: {}", enum_name))?;
                if !enum_def.variants.iter().any(|v| v.name == *variant_name) {
                    return Err(format!(
                        "enum {} has no variant named {}",
                        enum_name, variant_name
                    ));
                }
                Ok(Type::Enum(enum_name.clone()))
            }
            // ---- EnumVariantConstruction：抽成独立方法 check_enum_variant_construction，
            // 这里只是委托调用（expected 传 None，即“没有外部期望类型提示”这个默认情况）。
            ExprKind::EnumVariantConstruction { enum_name, variant_name, args } => {
                self.check_enum_variant_construction(enum_name, variant_name, args, None)
            }
            // ---- 修改后的 Match 分支（克隆 enum_def + 增强泛型替换） ----
            ExprKind::Match(match_expr) => {
                let cond_ty = self.check_expr(&match_expr.cond)?;

                // 提取枚举名和泛型参数
                let (enum_name, generic_args) = match &cond_ty {
                    Type::Enum(name) => (name.clone(), vec![]),
                    Type::Generic(name, args) => (name.clone(), args.clone()),
                    _ => return Err("match expression must be on an enum type".to_string()),
                };

                // 先克隆 enum_def，释放 self.enums 的借用
                let enum_def = self.enums.get(&enum_name)
                    .ok_or_else(|| format!("undefined enum: {}", enum_name))?
                    .clone();

                // 预先收集变体名称
                let variant_names: Vec<String> = enum_def.variants.iter()
                    .map(|v| v.name.clone())
                    .collect();

                let mut arm_types = Vec::new();
                for arm in &match_expr.arms {
                    self.scopes.push(HashMap::new());

                    match &arm.pattern {
                        Pattern::EnumVariant { enum_name: pat_enum, variant_name } => {
                            if pat_enum != &enum_name {
                                return Err("pattern enum name mismatch".to_string());
                            }
                            if !variant_names.contains(variant_name) {
                                return Err(format!("enum {} has no variant {}", enum_name, variant_name));
                            }
                        }
                        Pattern::EnumVariantWithBinding { enum_name: pat_enum, variant_name, binding } => {
                            if pat_enum != &enum_name {
                                return Err("pattern enum name mismatch".to_string());
                            }
                            if !variant_names.contains(variant_name) {
                                return Err(format!("enum {} has no variant {}", enum_name, variant_name));
                            }

                            let variant = enum_def.variants.iter()
                                .find(|v| v.name == *variant_name)
                                .ok_or_else(|| format!("variant not found"))?;

                            // ===== 修正后的 binding_ty 推断逻辑 =====
                            let binding_ty = if let Some(param_ty) = &variant.ty {
                                match param_ty {
                                    // 泛型占位符 T（以 Struct 或 Generic 形式出现）
                                    Type::Struct(name) if name == "T" => {
                                        if let Some(real_ty) = generic_args.get(0) {
                                            real_ty.clone()
                                        } else {
                                            return Err("missing generic argument".to_string());
                                        }
                                    }
                                    Type::Generic(name, _) if name == "T" => {
                                        if let Some(real_ty) = generic_args.get(0) {
                                            real_ty.clone()
                                        } else {
                                            return Err("missing generic argument".to_string());
                                        }
                                    }
                                    _ => param_ty.clone(),
                                }
                            } else {
                                return Err("variant has no parameter but pattern has binding".to_string());
                            };

                            self.scopes.last_mut().unwrap().insert(binding.clone(), binding_ty);
                        }
                        Pattern::Wildcard => {}
                    }

                    let is_panic_arm = matches!(
                        &arm.expr.kind,
                        ExprKind::Call { func, .. } if func == "panic"
                    );
                    let arm_ty = self.check_expr(&arm.expr)?;
                    arm_types.push((arm_ty, is_panic_arm));
                    self.scopes.pop();
                }

                // panic(...) 分支不参与"所有分支类型必须一致"的比较——它在
                // 运行时永远不会真正返回，效果上应该能兼容其他任何分支的
                // 类型（类似 Rust 的 `!` never 类型），而不是被当成 Unit
                // 硬要求跟别的分支一样。
                let real_types: Vec<&Type> = arm_types
                    .iter()
                    .filter(|(_, is_panic)| !is_panic)
                    .map(|(ty, _)| ty)
                    .collect();

                if let Some(first) = real_types.first() {
                    for (i, ty) in real_types.iter().enumerate().skip(1) {
                        if !self.types_equal(ty, first) {
                            return Err(format!("match arm {} type mismatch", i + 1));
                        }
                    }
                    Ok((*first).clone())
                } else if let Some((first_ty, _)) = arm_types.first() {
                    // 极端情况：所有分支都是 panic。没有任何真实类型可参考，
                    // 原样用第一个分支的类型（Unit）兜底，不报错。
                    Ok(first_ty.clone())
                } else {
                    Ok(Type::Unit)
                }
            }
            // 关键修复：之前这里完全无视 unsafe 块里到底写了什么，无条件
            // 判成 I32。`as_str` 里 `unsafe { from_utf8_unchecked(...) }`
            // 这种写法，不管内部表达式真实类型是什么，永远被判成 I32，
            // 跟函数声明的 &str 返回类型对不上——这是本轮排查历史遗留
            // 问题时挖出的最严重的一处，不是"某个具体方法漏判"，是整个
            // unsafe-块-当表达式用 这条路径从一开始就没接对，改成真的去
            // 检查 block 内部内容。
            ExprKind::UnsafeBlock(unsafe_stmt) => self.check_block(&unsafe_stmt.body),
        };

        if let Ok(ty) = &result {
            self.expr_types.insert(expr.id, ty.clone());
        }
        result
    }
}