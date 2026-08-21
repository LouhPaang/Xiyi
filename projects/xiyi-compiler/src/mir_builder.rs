// mir_builder.rs
use crate::ast::*;
use crate::hir::*;
use crate::mir::*;
use std::collections::{HashMap, HashSet};

pub struct MirBuilder;

impl MirBuilder {
    pub fn build(program: &HirProgram) -> MirProgram {
        let mut functions = Vec::new();
        let mut intrinsics_used = HashSet::new();

        // 遍历所有函数（包括 model 里的 forward 和普通函数）
        for f in &program.fns {
            let (mir_fn, used) = Self::build_function(f);
            functions.push(mir_fn);
            intrinsics_used.extend(used);
        }
        for m in &program.models {
            for f in &m.functions {
                let (mir_fn, used) = Self::build_function(f);
                functions.push(mir_fn);
                intrinsics_used.extend(used);
            }
        }
        // 也包含 implement 块里的函数（但已在 program.fns 中收集，视具体情况而定）
        // 注：实际项目中需遍历 program.impls，此处略

        MirProgram {
            functions,
            intrinsics_used: intrinsics_used.into_iter().collect(),
        }
    }

    // -------- 构建单个函数 --------
    fn build_function(f: &HirFn) -> (MirFunction, HashSet<String>) {
        let mut ctx = MirBuilderContext::new(f);
        let mut intrinsics = HashSet::new();

        // 1. 创建初始基本块
        let start_block = ctx.new_block("start");
        ctx.set_current_block(start_block);

        // 2. 处理参数：将 HirParam 映射为 MirArg 和 MirPlace
        let mut arg_places = Vec::new();
        for (idx, p) in f.params.iter().enumerate() {
            let arg_id = ctx.add_arg(&p.name, p.ty.clone());
            arg_places.push(MirPlace::Arg(arg_id));
        }

        // 3. 转换函数体：为语句列表生成 MIR
        let return_ty = f.return_type.clone();
        ctx.lower_block(&f.body, return_ty.as_ref(), &mut intrinsics);

        // 4. 补齐控制流：确保所有基本块都以终止器结束
        // （lower_block 应该已经处理了，但需要显式检查）
        ctx.finalize();

        // 5. 取出结果
        let blocks = ctx.into_blocks();
        let locals = ctx.into_locals();
        let args = ctx.into_args();

        (
            MirFunction {
                name: f.name.clone(),
                generic_params: f.generic_params.iter().map(|p| p.name.clone()).collect(),
                args,
                return_ty: return_ty.clone(),
                locals,
                blocks,
                source_map: HashMap::new(),
                effect_set: f.effects.clone(),
            },
            intrinsics,
        )
    }
}

// ===== 构建上下文（核心状态机） =====
struct MirBuilderContext {
    args: Vec<MirArg>,
    locals: Vec<MirLocal>,
    blocks: Vec<MirBasicBlock>,
    current_block: BasicBlockId,
    temp_counter: usize,
    loop_stack: Vec<LoopContext>,
    return_ty: Option<Type>,
}

struct LoopContext {
    break_block: BasicBlockId,
    continue_block: BasicBlockId,
}

impl MirBuilderContext {
    fn new(f: &HirFn) -> Self {
        MirBuilderContext {
            args: Vec::new(),
            locals: Vec::new(),
            blocks: Vec::new(),
            current_block: 0,
            temp_counter: 0,
            loop_stack: Vec::new(),
            return_ty: f.return_type.clone(),
        }
    }

    // -------- 基本块管理 --------
    fn new_block(&mut self, _label: &str) -> BasicBlockId {
        let id = self.blocks.len();
        self.blocks.push(MirBasicBlock {
            stmts: Vec::new(),
            terminator: MirTerminator::Return(None), // placeholder
        });
        id
    }

    fn set_current_block(&mut self, id: BasicBlockId) {
        self.current_block = id;
    }

    fn current_block_mut(&mut self) -> &mut MirBasicBlock {
        &mut self.blocks[self.current_block]
    }

    fn add_stmt(&mut self, stmt: MirStmt) {
        self.current_block_mut().stmts.push(stmt);
    }

    fn set_terminator(&mut self, term: MirTerminator) {
        self.current_block_mut().terminator = term;
    }

    // -------- 局部变量管理 --------
    fn new_temp(&mut self, ty: Type, persist: bool) -> LocalVarId {
        let id = self.locals.len();
        self.locals.push(MirLocal {
            ty,
            mutability: true, // 临时变量默认可变
            persist,
            name: None,
        });
        id
    }

    fn add_arg(&mut self, name: &str, ty: Type) -> ArgId {
        let id = self.args.len();
        self.args.push(MirArg { name: name.to_string(), ty });
        id
    }

    fn into_locals(self) -> Vec<MirLocal> { self.locals }
    fn into_args(self) -> Vec<MirArg> { self.args }
    fn into_blocks(self) -> Vec<MirBasicBlock> { self.blocks }

    // -------- 核心降维：Block --------
    fn lower_block(
        &mut self,
        block: &HirBlock,
        expected_ty: Option<&Type>,
        intrinsics: &mut HashSet<String>,
    ) -> MirRvalue {
        let len = block.stmts.len();
        if len == 0 {
            return MirRvalue::Unit;
        }

        // 遍历语句，除了最后一条，其余都作为 "语句" 降维
        for (i, stmt) in block.stmts.iter().enumerate() {
            if i == len - 1 {
                // 最后一条语句：如果是表达式，则作为块的值返回
                match stmt {
                    HirStmt::Expr { expr, .. } => {
                        return self.lower_expr(expr, intrinsics);
                    }
                    _ => {
                        self.lower_stmt(stmt, intrinsics);
                        return MirRvalue::Unit;
                    }
                }
            } else {
                self.lower_stmt(stmt, intrinsics);
            }
        }
        MirRvalue::Unit // fallback
    }

    // -------- 降维：语句 --------
    fn lower_stmt(&mut self, stmt: &HirStmt, intrinsics: &mut HashSet<String>) {
        match stmt {
            HirStmt::Let { name, ty, init, mutable, persist, span: _ } => {
                // 1. 计算初始值
                let init_rv = self.lower_expr(init, intrinsics);
                // 2. 分配局部变量
                let var_ty = ty.clone().unwrap_or_else(|| init.ty.clone());
                let id = self.locals.len();
                self.locals.push(MirLocal {
                    ty: var_ty,
                    mutability: *mutable,
                    persist: *persist,
                    name: Some(name.clone()),
                });
                // 3. 生成 Assign 语句
                let place = MirPlace::Local(id);
                self.add_stmt(MirStmt::Assign { place, rvalue: init_rv });
            }
            HirStmt::Expr { expr, span: _ } => {
                self.lower_expr(expr, intrinsics);
            }
            HirStmt::Return { expr: Some(e), span: _ } => {
                let rv = self.lower_expr(e, intrinsics);
                self.set_terminator(MirTerminator::Return(Some(rv)));
                // 创建一个新的 unreachable 基本块（后续连接）
                let next_block = self.new_block("after_return");
                self.set_current_block(next_block);
            }
            HirStmt::Return { expr: None, span: _ } => {
                self.set_terminator(MirTerminator::Return(None));
                let next_block = self.new_block("after_return");
                self.set_current_block(next_block);
            }
            HirStmt::While { cond, body, span: _ } => {
                let cond_block = self.new_block("while_cond");
                let body_block = self.new_block("while_body");
                let end_block = self.new_block("while_end");

                // 压入循环上下文
                self.loop_stack.push(LoopContext {
                    break_block: end_block,
                    continue_block: cond_block,
                });

                // 1. 当前块跳转到 cond_block
                self.set_terminator(MirTerminator::Goto(cond_block));

                // 2. cond_block: 判断条件
                self.set_current_block(cond_block);
                let cond_rv = self.lower_expr(cond, intrinsics);
                self.set_terminator(MirTerminator::If {
                    cond: cond_rv,
                    then_block: body_block,
                    else_block: end_block,
                });

                // 3. body_block: 执行循环体
                self.set_current_block(body_block);
                self.lower_block(body, None, intrinsics);
                // 循环体末尾跳回 cond_block
                if let MirTerminator::Return(_) = self.current_block_mut().terminator {
                    // 如果 body 内有 return，则已设置终止器，不覆盖
                } else {
                    self.set_terminator(MirTerminator::Goto(cond_block));
                }

                // 4. 恢复上下文并设置当前块为 end_block
                self.loop_stack.pop();
                self.set_current_block(end_block);
            }
            HirStmt::For { var, iterable, body, span: _ } => {
                // 自举期简化：将其降维为 while + match（即 into_iter + next）
                // 真正的迭代协议溶解应生成显式结构，此处留空占位
                // 实际生产需调用 intrinsics 里的 IntoIterator/Iterator
                unimplemented!("For loop lowering with Iterator protocol is pending");
            }
            HirStmt::Assign { target, expr, span: _ } => {
                let target_place = self.lower_place(target);
                let rv = self.lower_expr(expr, intrinsics);
                self.add_stmt(MirStmt::Assign { place: target_place, rvalue: rv });
            }
            HirStmt::Loop { body, span: _ } => {
                let body_block = self.new_block("loop_body");
                let end_block = self.new_block("loop_end");

                self.loop_stack.push(LoopContext {
                    break_block: end_block,
                    continue_block: body_block,
                });

                // 当前块跳入循环体
                self.set_terminator(MirTerminator::Goto(body_block));
                self.set_current_block(body_block);
                self.lower_block(body, None, intrinsics);
                // 循环体末尾跳回自己
                if let MirTerminator::Return(_) = self.current_block_mut().terminator {
                    // 保持 return
                } else {
                    self.set_terminator(MirTerminator::Goto(body_block));
                }

                self.loop_stack.pop();
                self.set_current_block(end_block);
            }
            HirStmt::Break { span: _ } => {
                if let Some(ctx) = self.loop_stack.last() {
                    self.set_terminator(MirTerminator::Goto(ctx.break_block));
                    let unreachable = self.new_block("after_break");
                    self.set_current_block(unreachable);
                } else {
                    panic!("Break outside loop");
                }
            }
            HirStmt::UnsafeBlock { kind: _, body, span: _ } => {
                // unsafe 块在 MIR 层只加一个标记，内部展开
                self.add_stmt(MirStmt::EffectCheck { effect: "unsafe".to_string() });
                self.lower_block(body, None, intrinsics);
            }
            // 其他未实现的语句（如 Continue）同理
            _ => unimplemented!("MIR lowering for this statement"),
        }
    }

    // -------- 降维：表达式（返回 Rvalue，并可能产生副作用赋值给临时变量） --------
    fn lower_expr(&mut self, expr: &HirExpr, intrinsics: &mut HashSet<String>) -> MirRvalue {
        match &expr.kind {
            HirExprKind::Literal(lit) => MirRvalue::Literal(lit.clone()),
            HirExprKind::Ident(name) => {
                // 注意：此处简化，未查作用域。实际需判断是局部变量还是参数
                // 这里假设局部变量，生产环境需从上下文中查找
                unimplemented!("Place lookup for identifier '{}'", name);
            }
            HirExprKind::BinaryOp { op, left, right } => {
                let l = self.lower_expr(left, intrinsics);
                let r = self.lower_expr(right, intrinsics);
                MirRvalue::BinaryOp {
                    op: op.clone(),
                    left: Box::new(l),
                    right: Box::new(r),
                }
            }
            HirExprKind::Call { qualifier, func, generic_args, args, is_method } => {
                // 收集内建函数
                if let Some(q) = qualifier {
                    intrinsics.insert(format!("{}::{}", q, func));
                } else {
                    intrinsics.insert(func.clone());
                }

                // 降维参数
                let mut mir_args = Vec::new();
                for arg in args {
                    match arg {
                        HirCallArg::Positional(e) => mir_args.push(self.lower_expr(e, intrinsics)),
                        HirCallArg::Named(_, e) => mir_args.push(self.lower_expr(e, intrinsics)),
                    }
                }

                // 构造一个调用终止器或 Rvalue
                // 这里简化：普通函数调用返回 Rvalue，不改变控制流（由外层决定赋值给谁）
                MirRvalue::IntrinsicCall {
                    func: func.clone(),
                    args: mir_args,
                    generic_args: generic_args.clone(),
                }
            }
            HirExprKind::StructInit { struct_name, generic_args: _, fields } => {
                let mut mir_fields = Vec::new();
                for (name, e) in fields {
                    mir_fields.push((name.clone(), self.lower_expr(e, intrinsics)));
                }
                MirRvalue::StructInit {
                    struct_name: struct_name.clone(),
                    fields: mir_fields,
                }
            }
            HirExprKind::Block(b) => {
                self.lower_block(b, None, intrinsics)
            }
            _ => unimplemented!("MIR lowering for expression {:?}", expr.kind),
        }
    }

    // -------- 降维：Place（左值） --------
    fn lower_place(&mut self, expr: &HirExpr) -> MirPlace {
        match &expr.kind {
            HirExprKind::Ident(name) => {
                // 查找 local 或 arg
                // 这里简化，实际需查表
                unimplemented!("Place lowering for ident {}", name);
            }
            HirExprKind::FieldAccess { struct_expr, field_name } => {
                let base = self.lower_place(struct_expr);
                let field_ty = Type::I32;
                MirPlace::Field {
                    base: Box::new(base),
                    field_name: field_name.clone(),
                    field_ty,
                }
            }
            _ => unimplemented!("Place lowering for {:?}", expr.kind),
        }
    }

    // 所有 block 补齐终止器
    fn finalize(&mut self) {
        for block in &mut self.blocks {
            if let MirTerminator::Return(_) = block.terminator {
            } else {
                block.terminator = MirTerminator::Return(Some(MirRvalue::Unit));
            }
        }
    }
}