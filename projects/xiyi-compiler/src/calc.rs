// calc.rs
use crate::ast::Expr;
use crate::mir::MirRvalue;

pub struct Calc;

impl Calc {
    /// 对 MIR 右值进行常量折叠，若无法折叠则返回原值
    pub fn fold_rvalue(rv: &MirRvalue) -> MirRvalue {
        // TODO: 实现常量折叠逻辑
        rv.clone()
    }

    /// 对 AST 表达式求值（用于 HIR 阶段的常量计算）
    pub fn eval_expr(expr: &Expr) -> Option<i64> {
        // TODO: 实现常量表达式求值
        None
    }
}