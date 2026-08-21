// simplify.rs
use crate::mir::MirProgram;

pub struct Simplify;

impl Simplify {
    pub fn run(program: MirProgram) -> MirProgram {
        // TODO: 实现常量折叠（调用 calc）和死代码消除
        program
    }
}