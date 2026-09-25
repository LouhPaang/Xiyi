// src/semantic/check_stmt.rs
use std::collections::HashMap;
use crate::ast::*;
use super::check_program::TypeChecker;

impl TypeChecker {
    pub fn check_stmt(&mut self, stmt: &Stmt) -> Result<Type, String> {
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
                    self.resolve_type(ty)?
                } else {
                    init_type.clone()
                };

                if !self.types_equal_allowing_never(&init_type, &resolved_ty) {
                    return Err(format!("type mismatch: expected {:?}, got {:?}", resolved_ty, init_type));
                }

                if let_stmt.persist {
                    let is_tensor = match init_type {
                        Type::Tensor { .. } => true,
                        _ => false,
                    };
                    if !is_tensor {
                        return Err("error[PS002]: persist binding requires Tensor type".to_string());
                    }
                }

                let name = let_stmt.name.clone();
                self.scopes.last_mut().unwrap().insert(name, resolved_ty);
                // let 语句本身不产出值。
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
                    // 都推不出来。现在用 current_return_type（check_func
                    // 进入函数体检查前设置好的）当期望类型传下去，跟函数体
                    // 最后一句表达式享受的待遇一致。
                    let expected = self.current_return_type.clone();
                    self.check_expr_with_expected(expr, expected.as_ref())?;
                }
                // 之前这里硬编码 Ok(Type::I32)，不管 return 的到底是
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
                // 关键修复：以前这里 self.check_expr(&for_stmt.iterable)?
                // 的返回值被直接丢弃，只用来触发检查，循环变量类型硬
                // 编码成 Type::I32。而 check_expr.rs 的 Range 分支自己
                // 也硬编码返回 Type::I32，两处凑在一起，`0..100i64` 这种
                // 写法迭代出来的循环变量被判成 I32，循环体里
                // `let x: i64 = i;` 就会报类型不匹配——Range 那边现在
                // 已经按两端类型正确返回 I32/I64（元素类型即 Range 表达式
                // 本身的类型），这里改成直接拿这个结果当循环变量类型，
                // 不再自己瞎猜一个 I32。
                let elem_ty = self.check_expr(&for_stmt.iterable)?;
                self.scopes.push(HashMap::new());
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(for_stmt.var.clone(), elem_ty);
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
                if !self.types_equal_allowing_never(&expr_ty, &target_ty) {
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
}
