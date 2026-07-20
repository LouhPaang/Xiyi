use logos::Logos;

#[derive(Logos, Debug, PartialEq, Clone)]
pub enum Token {
    // 关键字
    #[token("fn")]
    Fn,
    #[token("let")]
    Let,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("while")]
    While,
    #[token("return")]
    Return,
    #[token("true")]
    True,
    #[token("false")]
    False,

    // 类型关键字
    #[token("i32")]
    I32,
    #[token("i64")]
    I64,
    #[token("f32")]
    F32,
    #[token("bool")]
    Bool,

    // 标识符
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident,

    // 字面量
    #[regex(r"[0-9]+")]
    Integer,
    #[regex(r"[0-9]+\.[0-9]+")]
    Float,

    // 运算符与分隔符
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("=")]
    Eq,
    #[token("==")]
    EqEq,
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
    #[token("->")]
    Arrow,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
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

    // 注释（跳过）
    #[regex(r"//.*", logos::skip)]
    Comment,

    // 空白（跳过）
    #[regex(r"[ \t\n\r]+", logos::skip)]
    Whitespace,

}
