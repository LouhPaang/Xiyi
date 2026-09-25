// src/syntactic/module.rs
mod parse_attr;
mod parse_expr;
mod parse_func;
mod parse_generic;
mod parse_item;
mod parse_model;
mod parse_path;
mod parse_pattern;
mod parse_stmt;
mod parse_type;
mod helpers;
mod literal;

use crate::ast::*;
use crate::lexer::Lexer;
use crate::token::Token;

pub struct Parser {
    pub(crate) tokens: Vec<(Token, String)>,
    pub(crate) pos: usize,
    pub(crate) expr_id_counter: usize,
    pub(crate) generic_scopes: Vec<Vec<String>>,
    pub(crate) no_struct_literal: bool,
}

impl Parser {
    pub fn new(input: &str) -> Self {
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().expect("Lexer error");
        Parser {
            tokens,
            pos: 0,
            expr_id_counter: 0,
            generic_scopes: Vec::new(),
            no_struct_literal: false,
        }
    }

    pub fn parse_program(&mut self) -> Result<Program, String> {
        let mut items = Vec::new();
        while self.peek().is_some() {
            items.extend(self.parse_item()?);
        }
        Ok(Program { items })
    }

    pub(crate) fn peek(&self) -> Option<&(Token, String)> {
        self.tokens.get(self.pos)
    }

    pub(crate) fn peek_nth(&self, n: usize) -> Option<&(Token, String)> {
        self.tokens.get(self.pos + n)
    }

    pub(crate) fn next(&mut self) -> Option<(Token, String)> {
        if self.pos < self.tokens.len() {
            let token = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(token)
        } else {
            None
        }
    }

    pub(crate) fn expect(&mut self, expected: Token) -> Result<String, String> {
        if let Some((token, value)) = self.next() {
            if token == expected {
                Ok(value)
            } else {
                Err(format!("Expected {:?}, got {:?}", expected, token))
            }
        } else {
            Err("Unexpected end of input".to_string())
        }
    }

    pub(crate) fn parse_ident(&mut self) -> Result<String, String> {
        if let Some((Token::Ident, value)) = self.next() {
            Ok(value)
        } else {
            Err("Expected identifier".to_string())
        }
    }

    pub(crate) fn next_expr_id(&mut self) -> usize {
        let id = self.expr_id_counter;
        self.expr_id_counter += 1;
        id
    }

    // ===== 通用回滚 combinator =====
    // 之前 try_parse_privacy_tag 自己保存/还原 self.pos，但 expect(Gt)
    // 失败时那个 `?` 会直接把 Err 网上抛，绕过所有手写的
    // `self.pos = pos`——是真正的 bug（回滚逻辑本该覆盖所有失败路径，
    // 结果漏了一条）。这里提供唯一的还原点：调用方传一个"纯粹只管
    // 解析、失败就老实 Err"的闭包，成功就消费 token 返回 Some，失败
    // 就把 pos 还原到调用前、返回 None，一步到位，不会再漏。
    // 放在 module.rs 而不是 helpers.rs，是因为它直接操作 self.pos，
    // 跟 peek/next/expect 这些 Parser 自身的基础原语是同一类东西。
    pub(crate) fn try_parse<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Option<T> {
        let pos = self.pos;
        match f(self) {
            Ok(v) => Some(v),
            Err(_) => {
                self.pos = pos;
                None
            }
        }
    }

    // ===== 泛型作用域的"保证收尾" combinator =====
    // implement/interface/struct/enum 这几处定义原来都是手写
    // `push_generic_scope(...)` 开头、`pop_generic_scope()` 结尾，中间
    // 一整段全是会用 `?` 直接甩错误的解析代码——只要中间任何一步失败，
    // `?` 会跳过结尾那句 pop，Parser 的 generic_scopes 就永久多出一层
    // 脏数据，污染后面所有解析（比如后面某个同名 T 被误认成活跃泛型
    // 参数）。跟 try_parse 是同一类"失败路径容易漏掉收尾"的问题，这里
    // 用同样的思路给出唯一的收尾点：闭包内部想失败就正常 Err，成功
    // 就正常 Ok，pop 由这个函数自己无条件执行一次，谁都不用惦记。
    pub(crate) fn with_generic_scope<T>(
        &mut self,
        params: &[GenericParam],
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.push_generic_scope(params);
        let result = f(self);
        self.pop_generic_scope();
        result
    }
}
