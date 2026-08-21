// elaborate.rs
use crate::hir::HirProgram;

pub struct Elaborator;

impl Elaborator {
    pub fn elaborate(program: HirProgram) -> HirProgram {
        // TODO: 实现 for 循环和 ? 操作符的展开
        // 目前直接返回原程序
        program
    }
}