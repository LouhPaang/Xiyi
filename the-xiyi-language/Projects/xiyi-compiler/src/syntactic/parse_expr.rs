// src/syntactic/parse_expr.rs
use crate::ast::*;
use crate::token::Token;
use super::Parser;

impl Parser {
    // ===== 表达式解析 =====
    // 关键修复：原来函数开头有一句
    // `eprintln!("parse_expr at pos {}: {:?}", self.pos, self.peek());`
    // ——跟 parse_stmt 那句是同一类调试打印，每解析一个表达式（包括
    // 递归下降过程中每一层优先级）就往 stderr 吐一行，编译真实程序会
    // 把终端刷屏刷爆。去掉。
    pub(crate) fn parse_expr(&mut self) -> Result<Expr, String> {
        // 关键：lack 后面既可能跟 if（lack if，无 else 的显式声明），也可能
        // 跟 &[（lack &[T]，空切片字面量）——两者第一个 token 都是 Lack，
        // 得多看一眼第二个 token 才能确定走哪条路。
        if let Some((Token::Lack, _)) = self.peek() {
            if let Some((Token::Amp, _)) = self.peek_nth(1) {
                return self.parse_lack_slice();
            }
            return self.parse_if_expr();
        }
        if let Some((Token::If, _)) = self.peek() {
            return self.parse_if_expr();
        }
        if let Some((Token::Match, _)) = self.peek() {
            return self.parse_match_expr();
        }
        self.parse_range()
    }

    // ===== lack &[T] 空切片字面量 =====
    pub(crate) fn parse_lack_slice(&mut self) -> Result<Expr, String> {
        self.expect(Token::Lack)?;
        self.expect(Token::Amp)?;
        self.expect(Token::LBracket)?;
        let ty = self.parse_type()?;
        self.expect(Token::RBracket)?;
        Ok(Expr {
            id: self.next_expr_id(),
            kind: ExprKind::LackSlice(ty),
        })
    }

    pub(crate) fn parse_if_expr(&mut self) -> Result<Expr, String> {
        let if_kind = if let Some((Token::Lack, _)) = self.peek() {
            self.next();
            IfKind::Lack
        } else {
            IfKind::Normal
        };
        self.expect(Token::If)?;
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let cond = Box::new(self.parse_expr()?);
        self.no_struct_literal = prev;
        let then_expr = Box::new(self.parse_expr()?);
        let else_expr = if let Some((Token::Else, _)) = self.peek() {
            self.next();
            let else_expr = self.parse_expr()?;
            Some(Box::new(else_expr))
        } else {
            None
        };
        Ok(Expr {
            id: self.next_expr_id(),
            kind: ExprKind::If { kind: if_kind, cond, then_expr, else_expr },
        })
    }

    pub(crate) fn parse_match_expr(&mut self) -> Result<Expr, String> {
        self.expect(Token::Match)?;
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let cond = Box::new(self.parse_expr()?);
        self.no_struct_literal = prev;
        self.expect(Token::LBrace)?;
        let mut arms = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }
            let pattern = self.parse_pattern()?;
            self.expect(Token::FatArrow)?;
            let expr = Box::new(self.parse_expr()?);
            if let Some((Token::Comma, _)) = self.peek() {
                self.next();
            }
            arms.push(MatchArm { pattern, expr });
        }
        self.expect(Token::RBrace)?;
        Ok(Expr {
            id: self.next_expr_id(),
            kind: ExprKind::Match(MatchExpr { cond, arms }),
        })
    }

    pub(crate) fn parse_range(&mut self) -> Result<Expr, String> {
        let left = self.parse_or()?;
        if let Some((Token::Range, _)) = self.peek() {
            self.next();
            let right = self.parse_or()?;
            Ok(Expr {
                id: self.next_expr_id(),
                kind: ExprKind::Range { start: Box::new(left), end: Box::new(right) },
            })
        } else {
            Ok(left)
        }
    }

    pub(crate) fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while let Some((Token::Or, _)) = self.peek() {
            self.next();
            let right = self.parse_and()?;
            left = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::BinaryOp {
                    op: BinaryOp::Or,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        while let Some((Token::And, _)) = self.peek() {
            self.next();
            let right = self.parse_comparison()?;
            left = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::BinaryOp {
                    op: BinaryOp::And,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_add()?;
        while let Some((token, _)) = self.peek() {
            let op = match token {
                Token::EqEq => { self.next(); BinaryOp::Eq }
                Token::Neq => { self.next(); BinaryOp::Neq }
                Token::Lt => { self.next(); BinaryOp::Lt }
                Token::Gt => { self.next(); BinaryOp::Gt }
                Token::Le => { self.next(); BinaryOp::Le }
                Token::Ge => { self.next(); BinaryOp::Ge }
                _ => break,
            };
            let right = self.parse_add()?;
            left = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::BinaryOp {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_add(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul()?;
        while let Some((token, _)) = self.peek() {
            let op = match token {
                Token::Plus => { self.next(); BinaryOp::Add }
                Token::Minus => { self.next(); BinaryOp::Sub }
                _ => break,
            };
            let right = self.parse_mul()?;
            left = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::BinaryOp {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    // ===== parse_mul 包含 % 支持 =====
    pub(crate) fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_cast()?;
        while let Some((token, _)) = self.peek() {
            let op = match token {
                Token::Star => { self.next(); BinaryOp::Mul }
                Token::Slash => { self.next(); BinaryOp::Div }
                Token::Percent => { self.next(); BinaryOp::Mod }
                _ => break,
            };
            let right = self.parse_cast()?;
            left = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::BinaryOp {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            // !x 用真正的 Unary 节点，不脱糖成调用一个不存在的 "not" 函数。
            Some((Token::Bang, _)) => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(expr),
                    },
                })
            }
            // 一元负号 -x
            Some((Token::Minus, _)) => {
                self.next();
                let expr = self.parse_unary()?;
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr: Box::new(expr),
                    },
                })
            }
            _ => self.parse_postfix(),
        }
    }

    // ===== 后缀索引 bytes[i]，支持连续 arr[i][j] =====
    pub(crate) fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;
        while let Some((Token::LBracket, _)) = self.peek() {
            self.next();
            let index = self.parse_expr()?;
            self.expect(Token::RBracket)?;
            expr = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::Index {
                    expr: Box::new(expr),
                    index: Box::new(index),
                },
            };
        }
        Ok(expr)
    }

    // ===== as 类型转换，优先级在一元运算符外面一层
    // （-x as i32 会解析成 (-x) as i32，跟 Rust 习惯一致）=====
    pub(crate) fn parse_cast(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_unary()?;
        while let Some((Token::As, _)) = self.peek() {
            self.next();
            let ty = self.parse_type()?;
            expr = Expr {
                id: self.next_expr_id(),
                kind: ExprKind::Cast {
                    expr: Box::new(expr),
                    ty,
                },
            };
        }
        Ok(expr)
    }

    // ===== 主表达式（包含所有分支） =====
    // 关键修复（这次拆分）：整数/浮点/字符串/字节字符串/布尔/单元
    // 六种字面量原来直接内联在这个大 match 里，现在挪到了 literal.rs
    // 的 parse_literal/try_parse_unit_literal，这里只是先尝试委托，
    // 命中就直接返回，不命中（返回 None）才继续走下面这个不含字面量
    // 分支的 match。
    pub(crate) fn parse_primary(&mut self) -> Result<Expr, String> {
        if let Some(result) = self.parse_literal() {
            return result;
        }
        if let Some(unit_expr) = self.try_parse_unit_literal() {
            return Ok(unit_expr);
        }

        let peek_token = self.peek().cloned();
        match peek_token {
            Some((Token::Pipe, _)) => {
                let closure = self.parse_closure()?;
                if let Some((Token::LParen, _)) = self.peek() {
                    self.next();
                    let args = self.parse_call_args()?;
                    self.expect(Token::RParen)?;
                    let mut all_args = vec![CallArg::Positional(closure)];
                    all_args.extend(args);
                    Ok(Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::Call {
                            qualifier: None,
                            func: "closure_call".to_string(),
                            args: all_args,
                            is_method: false,
                        },
                    })
                } else {
                    Ok(closure)
                }
            }
            // ===== 处理 self（方法调用接收者） =====
            Some((Token::SelfLower, _)) => {
                self.next(); // consume 'self'
                let expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Ident("self".to_string()),
                };
                // 点链 .xxx 或 .xxx() 统一交给 parse_dot_chain 处理。
                self.parse_dot_chain(expr)
            }
            // ===== SelfType 分支 =====
            Some((Token::SelfType, _)) => {
                self.next(); // consume 'Self'

                let expr = if let Some((Token::LBrace, _)) = self.peek() {
                    self.next(); // consume '{'
                    let fields = self.parse_struct_fields()?;
                    Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::StructInit {
                            struct_name: "Self".to_string(),
                            fields,
                        }
                    }
                } else {
                    Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::Ident("Self".to_string()),
                    }
                };

                self.parse_dot_chain(expr)
            }
            Some((Token::Unsafe, _)) => {
                let unsafe_stmt = self.parse_unsafe_block()?;
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::UnsafeBlock(unsafe_stmt),
                })
            }
            Some((Token::Ident, name)) => {
                let func_name = name.clone();
                self.next();

                if name == "Sym" {
                    if let Some((Token::Lt, _)) = self.peek() {
                        self.next();
                        let sym_name = self.parse_ident()?;
                        self.expect(Token::Gt)?;
                        return Ok(Expr {
                            id: self.next_expr_id(),
                            kind: ExprKind::Sym(sym_name),
                        });
                    }
                }

                // ===== PathSep 分支（生成 EnumVariantConstruction） =====
                // "peek 到 :: 就消费并读下一段"这条原语现在收在
                // parse_path.rs 的 try_read_path_segment 里——跟
                // parse_pattern.rs 读 Ident::Ident 限定名时用的是同一个
                // 函数，不用各自手写一遍。
                if let Some(result) = self.try_read_path_segment() {
                    let variant_name = result?;

                    if let Some((Token::LParen, _)) = self.peek() {
                        self.next(); // consume '('
                        let args = self.parse_call_args()?;
                        self.expect(Token::RParen)?;

                        return Ok(Expr {
                            id: self.next_expr_id(),
                            kind: ExprKind::EnumVariantConstruction {
                                enum_name: name,
                                variant_name: variant_name,
                                args: args,
                            },
                        });
                    }

                    return Ok(Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::EnumVariantAccess {
                            enum_name: name,
                            variant_name,
                        },
                    });
                }

                if let Some((Token::LParen, _)) = self.peek() {
                    self.next();
                    let args = self.parse_call_args()?;
                    self.expect(Token::RParen)?;
                    return Ok(Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::Call {
                            qualifier: None,
                            func: func_name,
                            args,
                            is_method: false,
                        },
                    });
                }

                // 结构体初始化（无条件进入）
                if !self.no_struct_literal {
                    if let Some((Token::LBrace, _)) = self.peek() {
                    self.next(); // consume '{'
                    let fields = self.parse_struct_fields()?;
                    let expr = Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::StructInit {
                            struct_name: name.clone(),
                            fields,
                        },
                    };
                    return self.parse_dot_chain(expr);
                    }
                }

                // 普通标识符 + 点链
                let expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Ident(name),
                };
                self.parse_dot_chain(expr)
            }
            // 关键：try_parse_unit_literal 已经在函数开头把 `()` 这种
            // 情况处理掉了，走到这里的 LParen 一定不是单元字面量，直接
            // 按分组括号处理（消费 '('，解析内部表达式，期望 ')'）。
            Some((Token::LParen, _)) => {
                self.next();
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            Some((Token::LBracket, _)) => {
                self.next();
                let mut elements = Vec::new();
                while let Some((token, _)) = self.peek() {
                    if *token == Token::RBracket { break; }
                    let expr = self.parse_expr()?;
                    elements.push(expr);
                    match self.peek() {
                        Some((Token::Comma, _)) => { self.next(); continue; }
                        _ => break,
                    }
                }
                self.expect(Token::RBracket)?;
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::ArrayLiteral(elements),
                })
            }
            Some((Token::LBrace, _)) => {
                let block = self.parse_block()?;
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Block(block),
                })
            }
            _ => {
                // 关键修复：原来这里在报错前用一串 eprintln! 把当前
                // token、前 5 个、后 5 个全部倒到 stderr，再返回一句
                // 跟这些信息毫无关联的 "Expected expression"——调试时
                // 留下的痕迹，真正返回给调用方的错误反而丢了这些
                // 上下文。跟其它几处调试输出一个毛病：编译真实程序会
                // 刷屏，而且信息没有跟着 Err 走，调用方拿到的错误
                // 反而更少。现在把当前 token 和位置直接折进 Err
                // 本身，不再往 stderr 单独倒东西。
                Err(format!(
                    "Expected expression, found {:?} at position {}",
                    self.peek(),
                    self.pos
                ))
            }
        }
    }

    // ===== 点链：`.field` / `.method(args)` 连续访问 =====
    // 从 parse_primary 的 SelfLower、SelfType、Ident（含 Ident 分支内部
    // 走结构体初始化那条路）四个位置抽出来的公共逻辑——原来这四处各自
    // 内联一份逐字符相同的 while 循环，以后想改点链的规则（比如支持
    // `?.` 之类）要同时改四个地方，容易漏改。现在四处各自只负责构造好
    // 点链要作用的初始 expr，剩下的全部交给这一个函数。
    //
    // 项目约定不使用任何 Rust 宏（包括 matches!），is_call 的判断用
    // 显式 match 写，不走 matches! 宏。
    pub(crate) fn parse_dot_chain(&mut self, mut expr: Expr) -> Result<Expr, String> {
        while let Some((Token::Dot, _)) = self.peek() {
            self.next();
            let field_name = self.parse_ident()?;
            let is_call = match self.peek() {
                Some((Token::LParen, _)) => true,
                _ => false,
            };
            if is_call {
                self.next();
                let args = self.parse_call_args()?;
                self.expect(Token::RParen)?;
                let mut all_args = vec![CallArg::Positional(expr)];
                all_args.extend(args);
                expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Call {
                        qualifier: None,
                        func: field_name,
                        args: all_args,
                        is_method: true,
                    },
                };
            } else {
                expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::FieldAccess {
                        struct_expr: Box::new(expr),
                        field_name,
                    },
                };
            }
        }
        Ok(expr)
    }

    // ===== 结构体初始化字段（支持简写） =====
    pub(crate) fn parse_struct_fields(&mut self) -> Result<Vec<(String, Expr)>, String> {
        let mut fields = Vec::new();

        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }

            // 解析字段名
            let field_name = self.parse_ident()?;

            // 关键：提前复制下一个 token 类型，避免借用冲突
            let next_token_type = self.peek().map(|(t, _)| t.clone());

            // 判断是否为简写：下一个 token 是逗号或右花括号
            let is_shorthand = match next_token_type {
                Some(Token::Comma) => true,
                Some(Token::RBrace) => true,
                _ => false,
            };

            if is_shorthand {
                // 简写：field -> field: field
                let expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Ident(field_name.clone()),
                };
                fields.push((field_name, expr));

                // 消费逗号（如果有）
                if let Some((Token::Comma, _)) = self.peek() {
                    self.next();
                }
                continue;
            }

            // ---- 正常字段：field: expr ----
            self.expect(Token::Colon)?;
            let expr = self.parse_expr()?;
            fields.push((field_name, expr));

            // 消费逗号（如果有）
            if let Some((Token::Comma, _)) = self.peek() {
                self.next();
            }
        }

        self.expect(Token::RBrace)?;
        Ok(fields)
    }

    // ===== 调用参数 =====
    pub(crate) fn parse_call_args(&mut self) -> Result<Vec<CallArg>, String> {
        let mut args = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RParen { break; }
            // Token::In / Token::Else 混进"像标识符"的判断不是历史遗留、
            // 也不是笔误——它们都是全局关键字，词法层会把 `in`、`else`
            // 切成各自的专用 token，而不是 Token::Ident。但标准库规范里
            // 明确用这两个词当命名参数名：`linear(in: 784, out: 256)`
            // 用 `in:`，`tensor.cond(..., else: |t| t)` 用 `else:`。这里
            // 不认它们，这些合法调用就会被错误地拒绝。以后如果规范里
            // 又冒出别的关键字被用作命名参数名（比如 `for:`），照这个
            // 格式加一条就行；如果确定某个关键字永远不会出现在这个
            // 位置，也不用主动加。
            let is_ident_like = match token {
                Token::Ident | Token::In | Token::Else => true,
                _ => false,
            };
            if is_ident_like && self.peek_nth(1).map(|(t, _)| *t == Token::Colon).unwrap_or(false) {
                let name = match self.next() {
                    Some((_, s)) => s,
                    _ => return Err("Expected parameter name".to_string()),
                };
                self.next();
                let expr = self.parse_expr()?;
                args.push(CallArg::Named(name, expr));
            } else {
                let expr = self.parse_expr()?;
                args.push(CallArg::Positional(expr));
            }
            match self.peek() {
                Some((Token::Comma, _)) => { self.next(); continue; }
                _ => break,
            }
        }
        Ok(args)
    }

    // ===== 闭包 =====
    pub(crate) fn parse_closure(&mut self) -> Result<Expr, String> {
        self.expect(Token::Pipe)?;
        let param = self.parse_ident()?;
        self.expect(Token::Pipe)?;
        let body = Box::new(self.parse_expr()?);
        Ok(Expr {
            id: self.next_expr_id(),
            kind: ExprKind::Closure { param, body },
        })
    }
}
