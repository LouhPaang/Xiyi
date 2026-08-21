// mir.rs
use crate::ast::{Type, PrivacyTag, Literal, BinaryOp, UnaryOp, ShapeDim};
use std::collections::HashMap;

pub type BasicBlockId = usize;
pub type LocalVarId = usize;
pub type ArgId = usize;

// ===== MIR 程序 =====
#[derive(Debug, Clone)]
pub struct MirProgram {
    pub functions: Vec<MirFunction>,
    pub intrinsics_used: Vec<String>, // 供 codegen 收集需要链接的内建函数
}

// ===== MIR 函数 =====
#[derive(Debug, Clone)]
pub struct MirFunction {
    pub name: String,
    pub generic_params: Vec<String>, // 保留泛型名，供后端单态化参考
    pub args: Vec<MirArg>,
    pub return_ty: Option<Type>,
    pub locals: Vec<MirLocal>,       // 局部变量（含编译器临时变量）
    pub blocks: Vec<MirBasicBlock>,
    pub source_map: HashMap<BasicBlockId, Span>, // 调试用（可选）
    pub effect_set: EffectSet,       // 从 HIR 透传
}

#[derive(Debug, Clone)]
pub struct MirArg {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct MirLocal {
    pub ty: Type,
    pub mutability: bool,    // 是否 var
    pub persist: bool,       // 是否为 persist 域穿透张量
    pub name: Option<String>, // 用户定义的名称（编译器临时变量为 None）
}

// ===== 基本块 =====
#[derive(Debug, Clone)]
pub struct MirBasicBlock {
    pub stmts: Vec<MirStmt>,
    pub terminator: MirTerminator,
}

// ===== 语句（不改变控制流） =====
#[derive(Debug, Clone)]
pub enum MirStmt {
    /// 赋值：Place = Rvalue
    Assign {
        place: MirPlace,
        rvalue: MirRvalue,
    },
    /// 显式 Drop（所有权边界 / 作用域结束）
    Drop {
        place: MirPlace,
    },
    /// 存储元数据（如隐私标签的 Join 结果标记，供后端生成校验代码）
    SetMetadata {
        place: MirPlace,
        key: String,
        value: String, // 可扩展为更复杂的元数据结构
    },
    /// 内联效果标记（比如进入 deterministic 前的断言）
    EffectCheck {
        effect: String,
    },
}

// ===== 终止器（改变控制流） =====
#[derive(Debug, Clone)]
pub enum MirTerminator {
    /// 返回，携带可选的返回值（若函数返回 never，则无值）
    Return(Option<MirRvalue>),
    /// 无条件跳转
    Goto(BasicBlockId),
    /// 条件跳转（条件必须是 bool 类型）
    If {
        cond: MirRvalue,
        then_block: BasicBlockId,
        else_block: BasicBlockId,
    },
    /// 函数调用（含方法调用），需要单独占一个终止器以处理返回值赋给局部变量
    Call {
        func: String,
        args: Vec<MirRvalue>,
        destination: MirPlace,      // 返回值写入的局部变量
        target_block: BasicBlockId, // 调用成功后跳转
        is_method: bool,
        generic_args: Vec<Type>,    // 显式泛型实参
    },
    /// 内建调用（如 panic，不返回）
    BuiltinCallNoReturn {
        func: String,
        args: Vec<MirRvalue>,
    },
    /// Switch（用于 match 的低级降维，暂时可只用 if 链）
    Switch {
        discr: MirRvalue,
        targets: Vec<(i64, BasicBlockId)>,
        default: BasicBlockId,
    },
}

// ===== 位置（Place）——左值 =====
#[derive(Debug, Clone)]
pub enum MirPlace {
    /// 局部变量
    Local(LocalVarId),
    /// 参数（只读，但在 MIR 层依然可视为 Place）
    Arg(ArgId),
    /// 静态/常量（如 i128::MAX）
    Static(String),
    /// 字段访问
    Field {
        base: Box<MirPlace>,
        field_name: String,
        field_ty: Type, // 缓存类型方便后端
    },
    /// 索引访问（数组/切片/张量）
    Index {
        base: Box<MirPlace>,
        index: Box<MirRvalue>, // 索引本身是一个右值
    },
    /// 解引用（*ptr）—— 为系统编程预留
    Deref {
        base: Box<MirPlace>,
    },
}

impl MirPlace {
    /// 获取该位置的类型（由 builder 构建时填入）
    pub fn ty(&self) -> Type {
        match self {
            MirPlace::Local(id) => unimplemented!("需要从 MirFunction 的 locals 中查"),
            MirPlace::Arg(id) => unimplemented!("需要从 MirFunction 的 args 中查"),
            MirPlace::Static(_) => Type::I32, // placeholder
            MirPlace::Field { field_ty, .. } => field_ty.clone(),
            MirPlace::Index { base, .. } => {
                // 简化：取基础类型的元素类型（暂不实现精确推导）
                Type::I32
            }
            MirPlace::Deref { base } => base.ty(),
        }
    }
}

// ===== 右值（Rvalue）——只读值 =====
#[derive(Debug, Clone)]
pub enum MirRvalue {
    /// 字面量
    Literal(Literal),
    /// 常量值（如数组常量）
    ConstArray(Vec<MirRvalue>),
    /// 读取一个位置（Move 语义：消耗该位置的所有权）
    Move(MirPlace),
    /// 读取一个位置（Copy 语义：仅当类型为 Copy 时可用）
    Copy(MirPlace),
    /// 二元运算
    BinaryOp {
        op: BinaryOp,
        left: Box<MirRvalue>,
        right: Box<MirRvalue>,
    },
    /// 一元运算
    UnaryOp {
        op: UnaryOp,
        operand: Box<MirRvalue>,
    },
    /// 类型转换 as
    Cast {
        operand: Box<MirRvalue>,
        target_ty: Type,
    },
    /// 闭包（生成匿名结构体）
    Closure {
        captures: Vec<MirPlace>,
        body: Box<MirProgram>, // 嵌套 MIR（暂时简化，自举期先不实现闭包 MIR）
    },
    /// 结构体初始化
    StructInit {
        struct_name: String,
        fields: Vec<(String, MirRvalue)>,
    },
    /// 枚举变体构造
    EnumVariantConstruction {
        enum_name: String,
        variant_name: String,
        args: Vec<MirRvalue>,
    },
    /// 内建/固有方法调用（返回右值，不影响控制流）
    IntrinsicCall {
        func: String,
        args: Vec<MirRvalue>,
        generic_args: Vec<Type>,
    },
    /// 无操作占位
    Unit,
}

// ===== 辅助定义（从 HIR 复用的数据结构） =====
#[derive(Debug, Clone)]
pub struct EffectSet {
    pub has_io: bool,
    pub has_rng: bool,
    pub has_ai: bool,
    pub has_ffi: bool,
    pub has_panic: bool,
}

impl Default for EffectSet {
    fn default() -> Self {
        EffectSet {
            has_io: false,
            has_rng: false,
            has_ai: false,
            has_ffi: false,
            has_panic: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl Default for Span {
    fn default() -> Self {
        Span { start: 0, end: 0, line: 0, col: 0 }
    }
}