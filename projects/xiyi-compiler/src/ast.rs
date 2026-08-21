// 程序 = 一组项
#[derive(Debug, PartialEq, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Item {
    FnDef(FnDef),
    StructDef(StructDef),
    EnumDef(EnumDef),
    ConstDef(ConstDef),
    ModelDef(ModelDef),
    ProtoDef(ProtoDef),
    Use(UseStmt),
    Implement(ImplementDef),
    Interface(InterfaceDef),
}

// ===== 属性系统 =====
#[derive(Debug, PartialEq, Clone)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<AttributeArg>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum AttributeArg {
    Ident(String),
    StringLit(String),
    Int(i64),
    Float(f64),
    Rational(String),
    KeyValue(String, Box<AttributeArg>),
}

// ===== 泛型参数（已修改） =====
#[derive(Debug, PartialEq, Clone)]
pub enum GenericParam {
    Type { name: String, bounds: Vec<String> },
}

// ===== 函数定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct FnDef {
    pub attributes: Vec<Attribute>,
    pub name: String,
    pub generic_params: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Block,
}

// ===== 结构体定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct StructDef {
    pub name: String,
    pub generic_params: Vec<GenericParam>,
    pub fields: Vec<StructField>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: Type,
}

// ===== 枚举定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct EnumDef {
    pub name: String,
    pub generic_params: Vec<GenericParam>,
    pub variants: Vec<EnumVariant>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub ty: Option<Type>,
}

// ===== const 常量定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct ConstDef {
    pub name: String,
    pub ty: Type,
    pub value: Box<Expr>,
}

// ===== model 块定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct ModelDef {
    pub attributes: Vec<Attribute>,
    pub name: String,
    pub generic_params: Vec<GenericParam>,
    pub fields: Vec<ModelField>,
    pub functions: Vec<FnDef>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct ModelField {
    pub name: String,
    pub ty: Type,
}

// ===== proto 协议定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct ProtoDef {
    pub name: String,
    pub variants: Vec<ProtoVariant>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct ProtoVariant {
    pub name: String,
    pub ty: Option<Type>,
}

// ===== use 语句 =====
#[derive(Debug, PartialEq, Clone)]
pub struct UseStmt {
    pub path: String,
    pub alias: Option<String>,
}

// ===== implement 块 =====
#[derive(Debug, PartialEq, Clone)]
pub struct ImplementDef {
    pub attributes: Vec<Attribute>,
    pub generic_params: Vec<GenericParam>,
    pub target_type: Type,
    pub interface_name: Option<String>,
    pub functions: Vec<FnDef>,
    pub where_clause: Vec<WhereClause>,
}

// ===== interface 定义 =====
#[derive(Debug, PartialEq, Clone)]
pub struct InterfaceDef {
    pub attributes: Vec<Attribute>,
    pub name: String,
    pub generic_params: Vec<GenericParam>,
    pub methods: Vec<FnSig>,
}

// ===== 方法签名（用于 interface） =====
#[derive(Debug, PartialEq, Clone)]
pub struct FnSig {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub generic_params: Vec<GenericParam>,
}

// ===== where 子句 =====
#[derive(Debug, PartialEq, Clone)]
pub struct WhereClause {
    pub type_name: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

// ===== 各类语句 =====
#[derive(Debug, PartialEq, Clone)]
pub struct ForStmt {
    pub var: String,
    pub iterable: Box<Expr>,
    pub body: Block,
}

#[derive(Debug, PartialEq, Clone)]
pub struct AssignStmt {
    // ===== 关键修改：name: String -> target: Box<Expr> =====
    // 原来只能表达"给一个裸变量名赋值"（i = expr），self.len = expr、
    // arr[i] = expr 这类写法完全表达不出来。不新开一套"左值"语法，直接
    // 复用现成的 Expr——Ident/FieldAccess/Index 本来就都是合法表达式，
    // "这个表达式能不能被赋值"这件事留给 sema 去检查，ast 层不区分。
    pub target: Box<Expr>,
    pub expr: Box<Expr>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct LoopStmt {
    pub body: Block,
}

#[derive(Debug, PartialEq, Clone)]
pub struct BreakStmt {}

#[derive(Debug, PartialEq, Clone)]
pub struct MatchExpr {
    pub cond: Box<Expr>,
    pub arms: Vec<MatchArm>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub expr: Box<Expr>,
}

// ===== Pattern 枚举 =====
#[derive(Debug, PartialEq, Clone)]
pub enum Pattern {
    EnumVariant {
        enum_name: String,
        variant_name: String,
    },
    EnumVariantWithBinding {
        enum_name: String,
        variant_name: String,
        binding: String,
    },
    Wildcard,
}

// ===== 语句枚举 =====
#[derive(Debug, PartialEq, Clone)]
pub enum Stmt {
    Let(LetStmt),
    ExprStmt(Expr),
    Return(Option<Expr>),
    While(WhileStmt),
    For(ForStmt),
    Assign(AssignStmt),
    Loop(LoopStmt),
    Break(BreakStmt),
    UnsafeBlock(UnsafeBlockStmt),
}

#[derive(Debug, PartialEq, Clone)]
pub struct LetStmt {
    pub name: String,
    pub ty: Option<Type>,
    pub init: Box<Expr>,
    pub mutable: bool,
    pub persist: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub struct UnsafeBlockStmt {
    pub kind: UnsafeKind,
    pub body: Block,
}

#[derive(Debug, PartialEq, Clone)]
pub enum UnsafeKind {
    Normal,
    Verify,
}

#[derive(Debug, PartialEq, Clone)]
pub struct WhileStmt {
    pub cond: Box<Expr>,
    pub body: Block,
}

// ===== 调用参数 =====
#[derive(Debug, PartialEq, Clone)]
pub enum CallArg {
    Positional(Expr),
    Named(String, Expr),
}

// ===== 表达式 =====
#[derive(Debug, PartialEq, Clone)]
pub struct Expr {
    pub id: usize,
    pub kind: ExprKind,
}

#[derive(Debug, PartialEq, Clone)]
pub enum ExprKind {
    Literal(Literal),
    Ident(String),
    Sym(String),
    BinaryOp {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Call {
        // ===== 新增：限定路径调用（如 Rational::gcd(a, b)）=====
        // None = 普通调用/方法调用（跟以前完全一样，不影响现有代码）
        // Some("Rational") = 静态限定调用，不是枚举变体构造、也不是方法调用
        qualifier: Option<String>,
        func: String,
        args: Vec<CallArg>,
        is_method: bool,
    },
    Block(Block),
    StructInit {
        struct_name: String,
        fields: Vec<(String, Expr)>,
    },
    FieldAccess {
        struct_expr: Box<Expr>,
        field_name: String,
    },
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
    },
    EnumVariantAccess {
        enum_name: String,
        variant_name: String,
    },
    EnumVariantConstruction {
        enum_name: String,
        variant_name: String,
        args: Vec<CallArg>,
    },
    Match(MatchExpr),
    Closure {
        param: String,
        body: Box<Expr>,
    },
    If {
        // ===== 新增：Normal / Lack，跟 UnsafeKind::{Normal,Verify} 同一个
        // 模式——Lack 表示 `lack if cond { ... }`，语义上强制没有 else、
        // then 分支必须是 Unit（纯副作用，不产出值）；Normal 就是原来的
        // if，必须带 else。两条规则的强制检查在 sema 层做，这里只是
        // 结构上把"写的是哪种 if"记下来。
        kind: IfKind,
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Option<Box<Expr>>,
    },
    ArrayLiteral(Vec<Expr>),
    UnsafeBlock(UnsafeBlockStmt),
    // ===== 新增：一元运算符（目前用于一元负号 -x，Not 一并加上，
    // 方便以后把 not(x) 那个坑改成真正的 !x 语法时直接复用这个节点）=====
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    // ===== 新增：as 类型转换，如 x as u128 =====
    Cast {
        expr: Box<Expr>,
        ty: Type,
    },
    // ===== 新增：索引表达式 expr[idx]，如 bytes[i] =====
    Index {
        expr: Box<Expr>,
        index: Box<Expr>,
    },
    // ===== 新增：lack &[T] 空切片字面量，类型是 &[T]，长度恒为 0 =====
    // T 必须是具体类型（禁止泛型参数/never/impl Trait，这条约束交给
    // sema 检查），这里直接存 Type，不需要额外包一层结构。
    LackSlice(Type),
}

// ===== 一元运算符 =====
#[derive(Debug, PartialEq, Clone)]
pub enum UnaryOp {
    Neg,
    Not,
}

// ===== if 的两种语义标记，跟 UnsafeKind::{Normal,Verify} 同一个模式 =====
// Normal：普通 if，必须带 else，两分支类型必须一致
// Lack：`lack if cond { ... }`，显式声明"没有 else、纯副作用"，
//        强制没有 else 分支、then 分支必须是 Unit 类型
#[derive(Debug, PartialEq, Clone)]
pub enum IfKind {
    Normal,
    Lack,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit,
    // ===== 新增：bytes"..." 字节字符串字面量，类型是 &[u8] =====
    // 内容规范上要求每字节都是合法 ASCII（0x00–0x7F），这条约束留给
    // parser（词法/语法层面检查）或 sema 去做，ast 这一层只负责装数据。
    ByteString(Vec<u8>),
}

#[derive(Debug, PartialEq, Clone)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

// ===== 类型系统（完整版本） =====
#[derive(Debug, PartialEq, Clone)]
pub enum Type {
    // 有符号整数
    I8, I16, I32, I64, I128,
    // 无符号整数
    U8, U16, U32, U64, U128,
    // 浮点数
    F16, F32, F64,
    SymInt,
    Bool,
    Char,
    Str,
    Struct(String),
    Enum(String),
    TypeParam(String),
    Generic(String, Vec<Type>),
    Tensor {
        dtype: Box<Type>,
        shape: Vec<ShapeDim>,
    },
    Privacy(Box<Type>, PrivacyTag),
    SelfType,
    ConstIntArray(Vec<i64>),
    Ref {
        mutable: bool,
        inner: Box<Type>,
    },
    // ===== 新增：切片类型 [T]，跟已有的 Ref 组合表达 &[T] =====
    // 单独存在没有意义（这门语言里切片必须借用），实际写法永远是
    // `Ref{ mutable, inner: Box::new(Slice(T)) }`，但拆成两层而不是直接
    // 搞一个 `Type::SliceRef(bool, Box<Type>)`，是为了跟 Rust 的
    // `&[T]`/`&mut [T]` 结构保持一致，以后如果要支持裸切片（比如
    // Box<[T]> 那种场景）不用再改类型结构。
    Slice(Box<Type>),
    Unit,
}

// ===== 隐私标签 =====
#[derive(Debug, PartialEq, Clone)]
pub enum PrivacyTag {
    Public,
    Private,
    Differential { eps: String, delta: Option<String> },
}

// ===== 形状维度 =====
#[derive(Debug, PartialEq, Clone)]
pub enum ShapeDim {
    Const(usize),
    Sym(String),
    Dyn,
}