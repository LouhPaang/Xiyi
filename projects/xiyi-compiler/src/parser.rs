use crate::ast::*;
use crate::lexer::Lexer;
use crate::token::Token;

pub struct Parser {
    tokens: Vec<(Token, String)>,
    pos: usize,
}

impl Parser {
    pub fn new(input: &str) -> Self {
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize();
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&(Token, String)> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<(Token, String)> {
        if self.pos < self.tokens.len() {
            let token = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(token)
        } else {
            None
        }
    }

    fn expect(&mut self, expected: Token) -> Result<String, String> {
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

    fn parse_ident(&mut self) -> Result<String, String> {
        if let Some((Token::Ident, value)) = self.next() {
            Ok(value)
        } else {
            Err("Expected identifier".to_string())
        }
    }

    pub fn parse_program(&mut self) -> Result<Program, String> {
        let mut items = Vec::new();
        while self.peek().is_some() {
            items.push(self.parse_item()?);
        }
        Ok(Program { items })
    }

    fn parse_item(&mut self) -> Result<Item, String> {
        match self.peek() {
            Some((Token::Fn, _)) => Ok(Item::FnDef(self.parse_fn_def()?)),
            _ => Err("Expected function definition".to_string()),
        }
    }

    fn parse_fn_def(&mut self) -> Result<FnDef, String> {
        self.expect(Token::Fn)?;
        let name = self.parse_ident()?;
        self.expect(Token::LParen)?;
        let params = self.parse_params()?;
        self.expect(Token::RParen)?;
        let ret_type = if let Some((Token::Arrow, _)) = self.peek() {
            self.next();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        Ok(FnDef {
            name,
            params,
            return_type: ret_type,
            body,
        })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, String> {
        let mut params = Vec::new();
        while let Some((Token::Ident, _)) = self.peek() {
            let name = self.parse_ident()?;
            self.expect(Token::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param { name, ty });
            match self.peek() {
                Some((Token::Comma, _)) => {
                    self.next();
                }
                _ => break,
            }
        }
        Ok(params)
    }

    fn parse_type(&mut self) -> Result<Type, String> {
        match self.peek() {
            Some((Token::I32, _)) => {
                self.next();
                Ok(Type::I32)
            }
            Some((Token::I64, _)) => {
                self.next();
                Ok(Type::I64)
            }
            Some((Token::F32, _)) => {
                self.next();
                Ok(Type::F32)
            }
            Some((Token::Bool, _)) => {
                self.next();
                Ok(Type::Bool)
            }
            _ => Err("Expected type".to_string()),
        }
    }

    fn parse_block(&mut self) -> Result<Block, String> {
        self.expect(Token::LBrace)?;
        let mut stmts = Vec::new();

        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace {
                break;
            }
            // 解析一个语句（可能包含分号，也可能不带）
            let stmt = self.parse_stmt()?;
            stmts.push(stmt);
        }

        self.expect(Token::RBrace)?;
        Ok(Block { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Some((Token::Let, _)) => self.parse_let_stmt(),
            Some((Token::Return, _)) => self.parse_return_stmt(),
            Some((Token::If, _)) => self.parse_if_stmt(),
            Some((Token::While, _)) => self.parse_while_stmt(),
            _ => self.parse_expr_stmt(),
        }
    }

    fn parse_let_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::Let)?;
        let name = self.parse_ident()?;
        let ty = if let Some((Token::Colon, _)) = self.peek() {
            self.next();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(Token::Eq)?;
        let init = self.parse_expr()?;
        self.expect(Token::Semicolon)?;
        Ok(Stmt::Let(LetStmt {
            name,
            ty,
            init: Box::new(init),
        }))
    }

    fn parse_return_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::Return)?;
        if let Some((Token::Semicolon, _)) = self.peek() {
            self.next();
            Ok(Stmt::Return(None))
        } else {
            let expr = self.parse_expr()?;
            self.expect(Token::Semicolon)?;
            Ok(Stmt::Return(Some(expr)))
        }
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::If)?;
        let cond = self.parse_expr()?;
        let then_block = self.parse_block()?;
        let else_block = if let Some((Token::Else, _)) = self.peek() {
            self.next();
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(Stmt::If(IfStmt {
            cond: Box::new(cond),
            then_block,
            else_block,
        }))
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::While)?;
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::While(WhileStmt {
            cond: Box::new(cond),
            body,
        }))
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt, String> {
        let expr = self.parse_expr()?;
        // 如果后面是分号，消费它；否则，说明是块的最后一项，不带分号
        if let Some((Token::Semicolon, _)) = self.peek() {
            self.next();
        }
        Ok(Stmt::ExprStmt(expr))
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while let Some((Token::Or, _)) = self.peek() {
            self.next();
            let right = self.parse_and()?;
            left = Expr::BinaryOp {
                op: BinaryOp::Or,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        while let Some((Token::And, _)) = self.peek() {
            self.next();
            let right = self.parse_comparison()?;
            left = Expr::BinaryOp {
                op: BinaryOp::And,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_add()?;
        while let Some((token, _)) = self.peek() {
            let op = match token {
                Token::EqEq => {
                    self.next();
                    BinaryOp::Eq
                }
                Token::Neq => {
                    self.next();
                    BinaryOp::Neq
                }
                Token::Lt => {
                    self.next();
                    BinaryOp::Lt
                }
                Token::Gt => {
                    self.next();
                    BinaryOp::Gt
                }
                Token::Le => {
                    self.next();
                    BinaryOp::Le
                }
                Token::Ge => {
                    self.next();
                    BinaryOp::Ge
                }
                _ => break,
            };
            let right = self.parse_add()?;
            left = Expr::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul()?;
        while let Some((token, _)) = self.peek() {
            let op = match token {
                Token::Plus => {
                    self.next();
                    BinaryOp::Add
                }
                Token::Minus => {
                    self.next();
                    BinaryOp::Sub
                }
                _ => break,
            };
            let right = self.parse_mul()?;
            left = Expr::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        while let Some((token, _)) = self.peek() {
            let op = match token {
                Token::Star => {
                    self.next();
                    BinaryOp::Mul
                }
                Token::Slash => {
                    self.next();
                    BinaryOp::Div
                }
                _ => break,
            };
            let right = self.parse_unary()?;
            left = Expr::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some((Token::Bang, _)) => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(Expr::Call {
                    func: "not".to_string(),
                    args: vec![expr],
                })
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        let peek_token = self.peek().cloned();
        match peek_token {
            Some((Token::Integer, value)) => {
                self.next();
                let num = value.parse::<i64>().unwrap();
                Ok(Expr::Literal(Literal::Int(num)))
            }
            Some((Token::Float, value)) => {
                self.next();
                let num = value.parse::<f64>().unwrap();
                Ok(Expr::Literal(Literal::Float(num)))
            }
            Some((Token::True, _)) => {
                self.next();
                Ok(Expr::Literal(Literal::Bool(true)))
            }
            Some((Token::False, _)) => {
                self.next();
                Ok(Expr::Literal(Literal::Bool(false)))
            }
            Some((Token::Ident, name)) => {
                self.next();
                if let Some((Token::LParen, _)) = self.peek() {
                    self.next();
                    let args = self.parse_call_args()?;
                    self.expect(Token::RParen)?;
                    Ok(Expr::Call { func: name, args })
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            Some((Token::LParen, _)) => {
                self.next();
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            Some((Token::LBrace, _)) => {
                let block = self.parse_block()?;
                Ok(Expr::Block(block))
            }
            _ => Err("Expected expression".to_string()),
        }
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RParen {
                break;
            }
            args.push(self.parse_expr()?);
            match self.peek() {
                Some((Token::Comma, _)) => {
                    self.next();
                }
                _ => break,
            }
        }
        Ok(args)
    }
}
