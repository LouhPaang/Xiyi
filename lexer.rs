use crate::token::Token;
use logos::Logos;   // ← 补上这一行

pub struct Lexer<'a> {
    inner: logos::Lexer<'a, Token>,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            inner: Token::lexer(input),
        }
    }

    pub fn tokenize(&mut self) -> Vec<(Token, String)> {
        let mut tokens = Vec::new();
        while let Some(token_result) = self.inner.next() {
            match token_result {
                Ok(token) => {
                    let slice = self.inner.slice().to_string();
                    tokens.push((token, slice));
                }
                Err(_) => {
                    // 遇到无法识别的字符，跳过
                    continue;
                }
            }
        }
        tokens
    }
}