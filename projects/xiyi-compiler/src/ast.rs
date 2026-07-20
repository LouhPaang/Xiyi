// 程序 = 一组语句/项
#[derive(Debug, PartialEq, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Item {
    FnDef(FnDef),
    // StructDef(StructDef), // 暂不实现结构体
}

#[derive(Debug, PartialEq, Clone)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Block,
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

#[derive(Debug, PartialEq, Clone)]
pub enum Stmt {
    Let(LetStmt),
    ExprStmt(Expr),
    Return(Option<Expr>),
    If(IfStmt),
    While(WhileStmt),
}

#[derive(Debug, PartialEq, Clone)]
pub struct LetStmt {
    pub name: String,
    pub ty: Option<Type>,
    pub init: Box<Expr>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct IfStmt {
    pub cond: Box<Expr>,
    pub then_block: Block,
    pub else_block: Option<Block>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct WhileStmt {
    pub cond: Box<Expr>,
    pub body: Block,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Expr {
    Literal(Literal),
    Ident(String),
    BinaryOp {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Call {
        func: String,
        args: Vec<Expr>,
    },
    Block(Block),
}

#[derive(Debug, PartialEq, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
}

#[derive(Debug, PartialEq, Clone)]
pub enum BinaryOp {
    Add, Sub, Mul, Div,
    Eq, Neq, Lt, Gt, Le, Ge,
    And, Or,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Type {
    I32,
    I64,
    F32,
    Bool,
}
