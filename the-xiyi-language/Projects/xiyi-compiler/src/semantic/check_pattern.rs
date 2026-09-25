// src/semantic/check_pattern.rs
use std::collections::HashMap;
use crate::ast::*;
use super::check_program::TypeChecker;

impl TypeChecker {
    // ===== match 表达式检查：条件必须是枚举类型，逐个分支检查模式 + 类型 =====
    // 从 check_expr 的 `ExprKind::Match(match_expr) => { ... }` 分支搬
    // 过来，独立成方法，check_expr.rs 那边只是一句委托调用。
    pub fn check_match_expr(&mut self, match_expr: &MatchExpr) -> Result<Type, String> {
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

        let mut arm_types = Vec::new();
        for arm in &match_expr.arms {
            self.scopes.push(HashMap::new());

            match &arm.pattern {
                Pattern::EnumVariant { enum_name: pat_enum, variant_name } => {
                    if pat_enum != &enum_name {
                        return Err("pattern enum name mismatch".to_string());
                    }
                    if !self.has_variant(&enum_name, variant_name) {
                        return Err(format!("enum {} has no variant {}", enum_name, variant_name));
                    }
                }
                Pattern::EnumVariantWithBinding { enum_name: pat_enum, variant_name, binding } => {
                    if pat_enum != &enum_name {
                        return Err("pattern enum name mismatch".to_string());
                    }
                    // 合并原来"contains 检查 + find 取值"两次查找为一次——
                    // 原来那次 contains 通过之后，find 必然成功，`ok_or_else(||
                    // "variant not found")` 是永远不会走到的死代码。
                    let variant = self.resolve_variant_in(&enum_name, variant_name)
                        .ok_or_else(|| format!("enum {} has no variant {}", enum_name, variant_name))?;

                    // ===== binding_ty 推断逻辑 =====
                    // 关键修复：原来这里硬编码"泛型参数名字必须叫 T"，
                    // `enum Result<T, E> { Ok(T), Err(E) }` 里 Ok(x) 能
                    // 推到（名字碰巧是 T），但 Err(e) 推不出来——payload
                    // 类型是 Type::Struct("E")，两个分支都对不上 "T"，
                    // 落到 `_ => param_ty.clone()`，绑定成裸的 Struct("E")
                    // 而不是调用点实际传入的类型。改成不按名字猜，用
                    // enum_def.generic_params 查出真正的参数名列表和
                    // 下标，payload 类型如果恰好是某个参数名本身，就用
                    // generic_args 里对应位置的实参替换——这样 Err 是
                    // 枚举第 1 个（下标 1）泛型参数也能正确处理，不再
                    // 只能处理第 0 个。
                    let param_names = Self::generic_param_names(&enum_def.generic_params);
                    let binding_ty = if let Some(param_ty) = &variant.ty {
                        match param_ty {
                            Type::Struct(name) | Type::Generic(name, _) => {
                                if let Some(idx) = param_names.iter().position(|n| n == name) {
                                    generic_args.get(idx).cloned().ok_or_else(|| {
                                        format!("missing generic argument for `{}`", name)
                                    })?
                                } else {
                                    param_ty.clone()
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
                _ => {
                    return Err(format!("unsupported pattern in match: {:?}", arm.pattern));
                }
            }

            let is_panic_arm = match &arm.expr.kind {
                ExprKind::Call { func, .. } => func == "panic",
                _ => false,
            };
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
}
