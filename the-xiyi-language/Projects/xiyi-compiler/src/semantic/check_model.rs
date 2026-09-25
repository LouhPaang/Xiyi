// src/semantic/check_model.rs
use crate::ast::*;
use super::check_program::TypeChecker;

const FORWARD: &str = "forward";
const TRAINING_CONTEXT: &str = "TrainingContext";
const SENSITIVITY_ATTR: &str = "sensitivity";
const CONST_KEY: &str = "const";

impl TypeChecker {
    // ===== 收集 =====
    pub fn collect_model_def(&mut self, m: &ModelDef) -> Result<(), String> {
        self.register_model_name(m);
        self.register_model_struct(m);
        self.collect_forward_info(m)?;
        Ok(())
    }

    fn register_model_name(&mut self, m: &ModelDef) {
        self.model_names.insert(m.name.clone());
    }

    fn register_model_struct(&mut self, m: &ModelDef) {
        let fields = m.fields.iter()
            .map(|f| StructField {
                name: f.name.clone(),
                ty: self.strip_privacy(&f.ty),
            })
            .collect();
        self.structs.insert(m.name.clone(), StructDef {
            name: m.name.clone(),
            fields,
            generic_params: m.generic_params.clone(),
        });
    }

    fn collect_forward_info(&mut self, m: &ModelDef) -> Result<(), String> {
        let Some(forward) = Self::find_forward(m) else { return Ok(()) };
        if let Some(ret) = &forward.return_type {
            self.model_return_types.insert(m.name.clone(), ret.clone());
        }
        // 关键修复：const_sensitivity 现在返回 Result（rational_arg_to_f64
        // 不再允许吞掉"这个 sensitivity 值写错了"这种情况），这里用 `?`
        // 如实传播，不能再假装"解析失败"等价于"没写这个属性"。
        if let Some(sensitivity) = Self::const_sensitivity(forward)? {
            self.model_sensitivities.insert(m.name.clone(), sensitivity);
        }
        Ok(())
    }

    fn find_forward(m: &ModelDef) -> Option<&FnDef> {
        m.functions.iter().find(|f| f.name == FORWARD)
    }

    // 主入口：只关心"找出 sensitivity 属性的值"。查属性、查
    // key=value 参数、有理数转 f64 这三步的通用工具挪到了 check_attr.rs
    // 的 find_attr/find_key_value_arg/rational_arg_to_f64，这里只是
    // 按 model 领域自己的属性名/键名（SENSITIVITY_ATTR/CONST_KEY）串起来调用。
    //
    // 关键修复：返回类型从 Option<f64> 改成 Result<Option<f64>, String>——
    // "没写 #[sensitivity(...)] 属性"（合法，返回 Ok(None)）和"写了但
    // const 的值不是一个合法有理数"（错误，应该 Err）以前被 rational_arg_to_f64
    // 的 Option 返回值混成了同一种"None"，用户把 sensitivity 值写错时
    // 只会看到"这个 model 没有 sensitivity 标注"，而不是"你的 sensitivity
    // 值写错了"。
    fn const_sensitivity(forward: &FnDef) -> Result<Option<f64>, String> {
        let Some(attr) = Self::find_attr(&forward.attributes, SENSITIVITY_ATTR) else {
            return Ok(None);
        };
        let Some(val) = Self::find_key_value_arg(attr, CONST_KEY) else {
            return Ok(None);
        };
        Self::rational_arg_to_f64(val).map(Some)
    }

    // ===== 检查 =====
    pub fn check_model_def(&mut self, m: &ModelDef) -> Result<(), String> {
        self.ensure_has_forward(m)?;
        self.check_model_params(m)?;
        self.check_functions(m)?;
        Ok(())
    }

    fn ensure_has_forward(&self, m: &ModelDef) -> Result<(), String> {
        if Self::find_forward(m).is_none() {
            return Err(format!("model '{}' must define a 'forward' function", m.name));
        }
        Ok(())
    }

    fn check_model_params(&self, m: &ModelDef) -> Result<(), String> {
        for fn_def in &m.functions {
            for param in &fn_def.params {
                self.check_param_type(&param.ty)?;
            }
        }
        Ok(())
    }

    fn check_param_type(&self, ty: &Type) -> Result<(), String> {
        self.resolve_type(ty).map(|_| ())
    }

    fn check_functions(&mut self, m: &ModelDef) -> Result<(), String> {
        self.in_model = true;
        self.current_self_type = Some(Type::Struct(m.name.clone()));
        for fn_def in &m.functions {
            self.check_func(fn_def)?;
        }
        self.current_self_type = None;
        self.in_model = false;
        Ok(())
    }

    // ===== forward dp 检查 =====
    pub fn check_forward_dp_requirement(&self, fn_def: &FnDef) -> Result<(), String> {
        if !self.in_model || fn_def.name != FORWARD {
            return Ok(());
        }
        let has_dp_input = fn_def.params.iter()
            .any(|p| Self::is_differential(&self.extract_privacy_tag(&p.ty)));
        if !has_dp_input {
            return Ok(());
        }

        let has_ctx = fn_def.params.iter().any(Self::is_mut_training_context_param);
        if !has_ctx {
            return Err("error[PR001]: forward with dp(ε) input requires `&mut TrainingContext`".to_string());
        }
        Ok(())
    }

    // 关键修复：改回显式 match，不用 matches! 宏——你明确说过不想在这个
    // 代码库里用宏，我上一版特意避开了，这个位置不该再加回来。
    fn is_differential(tag: &Option<PrivacyTag>) -> bool {
        match tag {
            Some(PrivacyTag::Differential { .. }) => true,
            _ => false,
        }
    }

    // 同上，改回显式嵌套 match（这里原来是两层嵌套的 matches!，比单层
    // 更没必要用宏——嵌套的 match 本来就比嵌套的宏调用更容易读清楚
    // "先看是不是 &mut 引用，再看内部类型是不是 TrainingContext"这两步）。
    fn is_mut_training_context_param(p: &Param) -> bool {
        match &p.ty {
            Type::Ref { mutable: true, inner } => match &**inner {
                Type::Struct(name) => name == TRAINING_CONTEXT,
                _ => false,
            },
            _ => false,
        }
    }

    // ===== 跨 model 调用 =====
    pub fn try_cross_model_call(&mut self, func: &str, args: &[CallArg]) -> Result<Option<Type>, String> {
        if !self.in_model || args.is_empty() {
            return Ok(None);
        }

        let first_arg = &args[0];
        let receiver_ty = self.check_call_arg(first_arg)?;

        // 情况一：直接调用 model
        if let Some(ret_ty) = self.model_return_type(&receiver_ty) {
            return Ok(Some(self.join_call_result(&receiver_ty, &ret_ty)?));
        }

        // 情况二：通过 self.field 调用 model
        if let Some(ret_ty) = self.self_field_model_return_type(first_arg) {
            return Ok(Some(self.join_call_result(&receiver_ty, &ret_ty)?));
        }

        Ok(None)
    }

    // 专门处理 `self.field` 的情况：把"这是不是 self.xxx 形状"“这个字段
    // 是什么类型”"这个类型是不是已知 model"三步串成一条链，比分别调用
    // 三个函数再逐层判断更利落。
    fn self_field_model_return_type(&self, arg: &CallArg) -> Option<Type> {
        let field_name = Self::self_field_name(arg)?;
        let field_ty = self.current_self_field_type(field_name)?;
        self.model_return_type(&field_ty)
    }

    // 如果 ty（剥掉隐私标签后）是某个已登记的 model 类型，返回它 forward
    // 的返回类型。
    fn model_return_type(&self, ty: &Type) -> Option<Type> {
        if let Type::Struct(name) = self.strip_privacy(ty) {
            if self.model_names.contains(&name) {
                return self.model_return_types.get(&name).cloned();
            }
        }
        None
    }

    // 把调用点接收者的隐私标签和 forward 声明返回值的隐私标签 join 起来，
    // 套回 forward 的返回类型上。
    fn join_call_result(&self, receiver_ty: &Type, ret_ty: &Type) -> Result<Type, String> {
        let joined = self.join_privacy_labels(receiver_ty, ret_ty)?;
        Ok(self.apply_privacy_tag(ret_ty.clone(), joined))
    }

    // 如果第一个实参写的是 `self.field_name` 这种形式，返回 field_name；
    // 否则返回 None。
    fn self_field_name(arg: &CallArg) -> Option<&str> {
        let expr = match arg {
            CallArg::Positional(e) => e,
            _ => return None,
        };
        let (struct_expr, field_name) = match &expr.kind {
            ExprKind::FieldAccess { struct_expr, field_name } => (struct_expr, field_name),
            _ => return None,
        };
        let s = match &struct_expr.kind {
            ExprKind::Ident(s) => s,
            _ => return None,
        };
        if s == "self" {
            Some(field_name)
        } else {
            None
        }
    }

    // 当前 self 类型（必须是具体的 struct）身上某个字段的类型。
    //
    // 关键修复：原来这里第一行写的是
    //   `let Type::Struct(struct_name) = self.current_self_type.as_ref()?;`
    // 一个普通 `let` 后面直接接一个只能匹配 Type 众多变体里一种的
    // refutable pattern——这在 Rust 里编译不过（E0005 "refutable pattern
    // in local binding"），必须是 `if let`，或者用 `let-else` 把不匹配
    // 时该干什么（这里应该是 return None）显式写出来。这个人在
    // collect_forward_info 里其实用对了 let-else 的写法，这一处显然是
    // 漏写了 else 分支。
    fn current_self_field_type(&self, field_name: &str) -> Option<Type> {
        let Type::Struct(struct_name) = self.current_self_type.as_ref()? else {
            return None;
        };
        let struct_def = self.structs.get(struct_name)?;
        struct_def.fields.iter()
            .find(|f| f.name == field_name)
            .map(|f| f.ty.clone())
    }
}
