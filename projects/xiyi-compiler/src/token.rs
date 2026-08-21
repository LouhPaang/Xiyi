use logos::Logos;

#[derive(Logos, Debug, PartialEq, Clone)]
pub enum Token {
    // ===== 声明与定义 =====
    #[token("fn")]
    Fn,
    #[token("let")]
    Let,
    #[token("var")]
    Var,
    #[token("mut")]
    Mut,
    #[token("const")]
    Const,
    #[token("struct")]
    Struct,
    #[token("enum")]
    Enum,
    #[token("interface")]
    Interface,
    #[token("implement")]
    Implement,
    #[token("type")]
    TypeKw,
    #[token("mod")]
    Mod,
    #[token("use")]
    Use,
    #[token("pub")]
    Pub,
    #[token("priv")]
    Priv,
    #[token("extern")]
    Extern,

    // ===== 路径与模块 =====
    #[token("crate")]
    Crate,
    #[token("super")]
    Super,
    #[token("here")]
    Here,

    // ===== 流程控制 =====
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("lack")]
    Lack,
    #[token("match")]
    Match,
    #[token("for")]
    For,
    #[token("in")]
    In,
    #[token("while")]
    While,
    #[token("loop")]
    Loop,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("return")]
    Return,
    #[token("yield")]
    Yield,

    // ===== 模式匹配与测试 =====
    #[token("bind")]
    Bind,
    #[token("is")]
    Is,
    #[token("as")]
    As,
    #[token("where")]
    Where,

    // ===== 错误处理 =====
    #[token("try")]
    Try,

    // ===== 并发与生成器 =====
    #[token("async")]
    Async,
    #[token("await")]
    Await,
    #[token("gen")]
    Gen,
    #[token("snapshot")]
    Snapshot,
    #[token("ref")]
    Ref,
    #[token("persist")]
    Persist,
    #[token("proto")]
    Proto,

    // ===== 系统与安全 =====
    #[token("unsafe")]
    Unsafe,
    #[token("verify")]
    Verify,
    #[token("deterministic")]
    Deterministic,
    #[token("probabilistic")]
    Probabilistic,
    #[token("checkpoint")]
    Checkpoint,
    #[token("actor")]
    Actor,
    #[token("model")]
    Model,
    #[token("tensor")]
    Tensor,

    // ===== 特殊标识符 =====
    #[token("Self")]
    SelfType,
    #[token("self")]
    SelfLower,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("nil")]
    Nil,
    #[token("bytes")]
    Bytes,

    // ===== 基本类型 =====
    #[token("i8")]
    I8,
    #[token("i16")]
    I16,
    #[token("i32")]
    I32,
    #[token("i64")]
    I64,
    #[token("i128")]
    I128,
    #[token("u8")]
    U8,
    #[token("u16")]
    U16,
    #[token("u32")]
    U32,
    #[token("u64")]
    U64,
    #[token("u128")]
    U128,
    #[token("f16")]
    F16,
    #[token("f32")]
    F32,
    #[token("f64")]
    F64,
    #[token("bool")]
    Bool,
    #[token("char")]
    Char,
    #[token("str")]
    Str,
    #[token("never")]
    Never,

    // ===== 字面量与标识符 =====
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident,
    #[regex(r"[0-9]+")]
    Integer,
    #[regex(r"[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?|[0-9]+[eE][+-]?[0-9]+")]
    Float,
    #[regex(r#""([^"\\]|\\[nrt0"\\])*""#)]
    String,

    // ===== 运算符与分隔符 =====
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("=")]
    Eq,
    #[token("==")]
    EqEq,
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("!=")]
    Neq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("&&")]
    And,
    #[token("||")]
    Or,
    #[token("!")]
    Bang,
    #[token("|")]
    Pipe,
    #[token("&")]
    Amp,
    #[token("?")]
    Question,
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("..")]
    Range,
    #[token("::")]
    PathSep,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token(":")]
    Colon,
    #[token(";")]
    Semicolon,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("#")]
    Pound,

    // ===== 注释 =====
    #[regex(r"///.*", logos::skip)]
    DocComment,
    #[regex(r"//.*", logos::skip)]
    LineComment,
    #[regex(r"/\*([^*]|\*[^/])*\*/", logos::skip)]
    BlockComment,

    // ===== 空白 =====
    #[regex(r"[ \t\n\r]+", logos::skip)]
    Whitespace,
}