// parser.rs
use crate::ast::*;
use crate::lexer::Lexer;
use crate::token::Token;

pub struct Parser {
    tokens: Vec<(Token, String)>,
    pos: usize,
    expr_id_counter: usize,
    generic_scopes: Vec<Vec<String>>,
    no_struct_literal: bool,
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

    // ===== 泛型作用域栈的三个辅助方法 =====
    fn push_generic_scope(&mut self, params: &[GenericParam]) {
    let names: Vec<String> = params
        .iter()
        .map(|gp| match gp {
            GenericParam::Type { name, .. } => name.clone(),
        })
        .collect();
    self.generic_scopes.push(names);
    }

    fn pop_generic_scope(&mut self) {
        self.generic_scopes.pop();
    }

    fn is_active_generic(&self, name: &str) -> bool {
        self.generic_scopes
            .iter()
            .any(|scope| scope.iter().any(|n| n == name))
    }

    fn next_expr_id(&mut self) -> usize {
        let id = self.expr_id_counter;
        self.expr_id_counter += 1;
        id
    }

    fn peek(&self) -> Option<&(Token, String)> {
        self.tokens.get(self.pos)
    }

    fn peek_nth(&self, n: usize) -> Option<&(Token, String)> {
        self.tokens.get(self.pos + n)
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

    // ===== 辅助：把 bytes"..." 里的原始文本解码成 Vec<u8> =====
    // 支持规范里列的转义：\n \r \t \\ \" \0 \xNN（NN 两位十六进制，00–7F）；
    // 不支持 \u{...} Unicode 转义（字节字符串不含 Unicode 语义）。非转义
    // 字符必须本身就是单字节 ASCII（0x00–0x7F），否则报 error[BS001]。
    fn decode_byte_string(s: &str) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => bytes.push(b'\n'),
                    Some('r') => bytes.push(b'\r'),
                    Some('t') => bytes.push(b'\t'),
                    Some('\\') => bytes.push(b'\\'),
                    Some('"') => bytes.push(b'"'),
                    Some('0') => bytes.push(0u8),
                    Some('x') => {
                        let hi = chars.next().ok_or("incomplete \\x escape in byte string")?;
                        let lo = chars.next().ok_or("incomplete \\x escape in byte string")?;
                        let hex: String = [hi, lo].iter().collect();
                        let v = u8::from_str_radix(&hex, 16)
                            .map_err(|_| format!("invalid \\x escape in byte string: \\x{}", hex))?;
                        if v > 0x7F {
                            return Err(format!(
                                "error[BS001]: byte string contains non-ASCII byte 0x{:02X}",
                                v
                            ));
                        }
                        bytes.push(v);
                    }
                    Some(other) => {
                        return Err(format!("unknown escape sequence in byte string: \\{}", other))
                    }
                    None => return Err("incomplete escape sequence in byte string".to_string()),
                }
            } else {
                if (c as u32) > 0x7F {
                    return Err(format!(
                        "error[BS001]: byte string contains non-ASCII byte 0x{:X}",
                        c as u32
                    ));
                }
                bytes.push(c as u8);
            }
        }
        Ok(bytes)
    }

    fn parse_ident(&mut self) -> Result<String, String> {
        if let Some((Token::Ident, value)) = self.next() {
            Ok(value)
        } else {
            Err("Expected identifier".to_string())
        }
    }

    // ===== parse_program =====
    pub fn parse_program(&mut self) -> Result<Program, String> {
        let mut items = Vec::new();
        while self.peek().is_some() {
            items.extend(self.parse_item()?);
        }
        Ok(Program { items })
    }

    // ===== parse_item：支持所有顶层项 =====
    //
    // 返回类型是 Result<Vec<Item>, String>，不是单个 Item——因为：
    // 1. `mod name;` 声明解析完之后没有对应的 Item 可以产出（返回空 Vec）。
    // 2. `use a::b::{X, Y};` 这种花括号多导入，一次要产出好几个 Item::Use
    //    （每个导入名一个），单个 Option<Item> 装不下。
    fn parse_item(&mut self) -> Result<Vec<Item>, String> {
        let attrs = self.parse_attributes()?;

        // ===== 顶层 pub/priv 前缀 =====
        // 之前只有 parse_fn_def/parse_fn_sig 内部会消费一个前置的 pub/priv
        // （针对 implement/interface 块里的方法），但顶层项（`pub use ...;`、
        // `pub mod ...;`）从来没有被消费过——parse_item 的大 match 直接拿到
        // Token::Pub，哪个分支都对不上，一路落到最后那句
        // "Expected function, model, implement, interface, use, struct,
        // enum, or const definition"。这里统一吃掉，不区分 pub/priv（这两
        // 个修饰符目前对顶层项没有实际的可见性检查，跟 `Item::Use` 现在也
        // 只是被解析出来、不真正生效是一回事）。
        if let Some((Token::Pub, _)) | Some((Token::Priv, _)) = self.peek() {
            self.next();
        }

        // ===== mod 声明 =====
        // `mod name;` / `pub mod name;`：现在的架构没有真正的模块系统，
        // mod 声明目前纯粹是语法占位，解析掉、不产出 Item。
        if let Some((Token::Mod, _)) = self.peek() {
            if !attrs.is_empty() {
                return Err("Attributes not allowed on mod declaration".to_string());
            }
            self.next();
            self.parse_ident()?;
            self.expect(Token::Semicolon)?;
            return Ok(Vec::new());
        }

        match self.peek() {
            Some((Token::Fn, _)) => Ok(vec![Item::FnDef(self.parse_fn_def(attrs)?)]),
            Some((Token::Model, _)) => Ok(vec![Item::ModelDef(self.parse_model_def(attrs)?)]),
            Some((Token::Implement, _)) => Ok(vec![self.parse_implement_def(attrs)?]),
            Some((Token::Interface, _)) => Ok(vec![self.parse_interface_def(attrs)?]),
            Some((Token::Use, _)) => {
                if !attrs.is_empty() {
                    return Err("Attributes not allowed on use statement".to_string());
                }
                self.next(); // consume 'use'
                self.parse_use_item()
            }
            _ => {
                if !attrs.is_empty() {
                    return Err("Attributes are not supported for this item type".to_string());
                }
                match self.peek() {
                    Some((Token::Struct, _)) => Ok(vec![Item::StructDef(self.parse_struct_def()?)]),
                    Some((Token::Enum, _)) => Ok(vec![Item::EnumDef(self.parse_enum_def()?)]),
                    Some((Token::Const, _)) => Ok(vec![Item::ConstDef(self.parse_const_def()?)]),
                    _ => Err("Expected function, model, implement, interface, use, struct, enum, or const definition".to_string()),
                }
            }
        }
    }

    // ===== parse_use_item =====
    // ===== 新增：路径的第一段除了普通标识符，还可能是 crate/super/here
    // 这三个路径关键字（crate:: 当前 crate 根、super:: 父模块、here::
    // 当前模块）。只有第一段会是这几个词，后续路径段（crate::iter::Iterator
    // 里的 iter、Iterator）永远是普通标识符，不用特殊处理。
    fn parse_path_root(&mut self) -> Result<String, String> {
        match self.peek() {
            Some((Token::Crate, _)) => {
                self.next();
                Ok("crate".to_string())
            }
            Some((Token::Super, _)) => {
                self.next();
                Ok("super".to_string())
            }
            Some((Token::Here, _)) => {
                self.next();
                Ok("here".to_string())
            }
            _ => self.parse_ident(),
        }
    }

    fn parse_use_item(&mut self) -> Result<Vec<Item>, String> {
        let mut parts = Vec::new();
        let first = self.parse_path_root()?;
        parts.push(first);
        while let Some((Token::PathSep, _)) = self.peek() {
            self.next();

            // ===== 花括号多导入：use a::b::{X, Y, Z}; =====
            // 之前这里只会无条件调用 parse_ident()，'{' 不是标识符，直接
            // 报 "Expected identifier"——这正是 io.xiyi 里
            // `use symint::{SymInt, SymExpr};` 撞上的那个 bug。
            if let Some((Token::LBrace, _)) = self.peek() {
                self.next();
                let base_path = parts.join("::");
                let mut items = Vec::new();
                while let Some((token, _)) = self.peek() {
                    if *token == Token::RBrace { break; }
                    let name = self.parse_ident()?;
                    let full_path = format!("{}::{}", base_path, name);
                    let alias = if let Some((Token::As, _)) = self.peek() {
                        self.next();
                        Some(self.parse_ident()?)
                    } else {
                        None
                    };
                    items.push(Item::Use(UseStmt { path: full_path, alias }));
                    match self.peek() {
                        Some((Token::Comma, _)) => { self.next(); }
                        _ => break,
                    }
                }
                self.expect(Token::RBrace)?;
                self.expect(Token::Semicolon)?;
                return Ok(items);
            }

            let part = self.parse_ident()?;
            parts.push(part);
        }
        let path = parts.join("::");

        let alias = if let Some((Token::As, _)) = self.peek() {
            self.next();
            Some(self.parse_ident()?)
        } else {
            None
        };

        self.expect(Token::Semicolon)?;
        Ok(vec![Item::Use(UseStmt { path, alias })])
    }

    // ===== parse_implement_def =====
    fn parse_implement_def(&mut self, attributes: Vec<Attribute>) -> Result<Item, String> {
        self.expect(Token::Implement)?;

        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        // 从这里开始，target_type/interface_name/where_clause/函数体全都可能
        // 引用到自己声明的泛型参数（比如 `implement<T> Container<T>`），必须
        // 在解析 target_type 之前就把作用域压进去。
        self.push_generic_scope(&generic_params);

        // ===== 修复：implement X for Y 里 X/Y 谁是谁 =====
        // 规范（docx §6.2）写得很明确：
        //     implement Drawable for Point { ... }
        // Drawable 是接口名，Point 才是被实现的目标类型——"for" 前面是接口，
        // 后面是目标。但这里没法在看到 "for" 之前就知道第一段该按哪种身份
        // 解析（`implement<T> Container<T> { ... }` 完全没有 "for"，第一段
        // 直接就是目标类型），只能先按"类型"解析出第一段，再看后面有没有
        // "for" 来决定：没有 "for"，第一段就是目标类型（固有实现）；有
        // "for"，说明第一段其实是接口名，"for" 后面那个才是真正的目标类型。
        // 之前的实现在有 "for" 的情况下，把这两者的身份搞反了。
        let first_type = self.parse_type()?;

        let (target_type, interface_name) = if let Some((Token::For, _)) = self.peek() {
            self.next(); // consume 'for'
            let interface_name = match &first_type {
                Type::Struct(name) => name.clone(),
                Type::Generic(name, _) => name.clone(),
                _ => {
                    self.pop_generic_scope();
                    return Err("Expected interface name before 'for'".to_string());
                }
            };
            let target_type = self.parse_type()?;
            (target_type, Some(interface_name))
        } else {
            (first_type, None)
        };

        let where_clause = if let Some((Token::Where, _)) = self.peek() {
            self.next();
            self.parse_where_clause()?
        } else {
            Vec::new()
        };

        // 跳过可能残留的 '('
        while let Some((Token::LParen, _)) = self.peek() {
            eprintln!("Skipping stray LParen at position {}", self.pos);
            self.next();
        }

        self.expect(Token::LBrace)?;
        let mut functions = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }
            let fn_attrs = self.parse_attributes()?;
            functions.push(self.parse_fn_def(fn_attrs)?);
        }
        self.expect(Token::RBrace)?;
        self.pop_generic_scope();

        Ok(Item::Implement(ImplementDef {
            attributes,
            generic_params,
            target_type,
            interface_name,
            functions,
            where_clause,
        }))
    }

    // ===== parse_interface_def =====
    fn parse_interface_def(&mut self, attributes: Vec<Attribute>) -> Result<Item, String> {
        self.expect(Token::Interface)?;
        let name = self.parse_ident()?;

        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.push_generic_scope(&generic_params);

        self.expect(Token::LBrace)?;
        let mut methods = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }
            methods.push(self.parse_fn_sig()?);
        }
        self.expect(Token::RBrace)?;
        self.pop_generic_scope();

        Ok(Item::Interface(InterfaceDef {
            attributes,
            name,
            generic_params,
            methods,
        }))
    }

    // ===== parse_fn_sig（支持可见性修饰符） =====
    fn parse_fn_sig(&mut self) -> Result<FnSig, String> {
        // 可选可见性修饰符
        if let Some((Token::Pub, _)) = self.peek() {
            self.next();
        } else if let Some((Token::Priv, _)) = self.peek() {
            self.next();
        }
        self.expect(Token::Fn)?;
        let name = self.parse_ident()?;
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.push_generic_scope(&generic_params);
        self.expect(Token::LParen)?;
        let params = self.parse_params()?;
        self.expect(Token::RParen)?;
        let return_type = if let Some((Token::Arrow, _)) = self.peek() {
            self.next();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.pop_generic_scope();
        self.expect(Token::Semicolon)?;
        Ok(FnSig {
            name,
            params,
            return_type,
            generic_params,
        })
    }

    // ===== parse_where_clause =====
    fn parse_where_clause(&mut self) -> Result<Vec<WhereClause>, String> {
        let mut clauses = Vec::new();
        while let Some((Token::Ident, name)) = self.peek() {
            let name = name.clone();
            self.next();
            self.expect(Token::Colon)?;
            let mut bounds = Vec::new();
            while let Some((Token::Ident, bound)) = self.peek() {
                bounds.push(bound.clone());
                self.next();
                if let Some((Token::Comma, _)) = self.peek() {
                    self.next();
                    break;
                }
            }
            clauses.push(WhereClause { type_name: name, bounds });
        }
        Ok(clauses)
    }

    // ===== 属性解析 =====
    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, String> {
        let mut attrs = Vec::new();
        while let Some((Token::Pound, _)) = self.peek() {
            self.next();
            self.expect(Token::LBracket)?;
            let name = self.parse_ident()?;
            let mut args = Vec::new();
            if let Some((Token::LParen, _)) = self.peek() {
                self.next();
                while let Some((token, _)) = self.peek() {
                    if *token == Token::RParen { break; }
                    args.push(self.parse_attribute_arg()?);
                    match self.peek() {
                        Some((Token::Comma, _)) => { self.next(); }
                        _ => break,
                    }
                }
                self.expect(Token::RParen)?;
            }
            self.expect(Token::RBracket)?;
            attrs.push(Attribute { name, args });
        }
        Ok(attrs)
    }

    fn parse_attribute_arg(&mut self) -> Result<AttributeArg, String> {
        if let Some((_, key)) = self.peek() {
            let key = key.clone();
            if let Some((Token::Eq, _)) = self.peek_nth(1) {
                self.next();
                self.next();
                let value = self.parse_attribute_arg_value()?;
                return Ok(AttributeArg::KeyValue(key, Box::new(value)));
            }
        }
        self.parse_attribute_arg_value()
    }

    fn parse_attribute_arg_value(&mut self) -> Result<AttributeArg, String> {
        match self.peek() {
            Some((Token::Ident, v)) => {
                let v = v.clone();
                self.next();
                Ok(AttributeArg::Ident(v))
            }
            Some((Token::String, v)) => {
                let v = v.clone();
                self.next();
                Ok(AttributeArg::StringLit(v))
            }
            Some((Token::Integer, v)) => {
                let int_part = v.clone();
                if let Some((Token::Slash, _)) = self.peek_nth(1) {
                    self.next();
                    self.next();
                    if let Some((Token::Integer, den)) = self.next() {
                        let rational = format!("{}/{}", int_part, den);
                        return Ok(AttributeArg::Rational(rational));
                    } else {
                        return Err("Expected integer after '/' in rational".to_string());
                    }
                } else {
                    self.next();
                    let num = int_part.parse().unwrap();
                    Ok(AttributeArg::Int(num))
                }
            }
            Some((Token::Float, v)) => {
                let v = v.clone();
                self.next();
                Ok(AttributeArg::Float(v.parse().unwrap()))
            }
            _ => Err("Expected attribute argument value".to_string()),
        }
    }

    // ===== 泛型参数 =====
    fn parse_generic_params(&mut self) -> Result<Vec<GenericParam>, String> {
        self.expect(Token::Lt)?;
        let mut params = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::Gt { break; }
            let name = self.parse_ident()?;
            let mut bounds = Vec::new();
            if let Some((Token::Colon, _)) = self.peek() {
                self.next();
                loop {
                    bounds.push(self.parse_ident()?);
                    if let Some((Token::Lt, _)) = self.peek() {
                    self.parse_generic_params()?;
                    }
                match self.peek() {
                    Some((Token::Plus, _)) => { self.next(); continue; }
                    _ => break,
                    }
                }
            }
            params.push(GenericParam::Type { name, bounds });
            match self.peek() {
                Some((Token::Comma, _)) => { self.next(); }
                _ => break,
            }
        }
        self.expect(Token::Gt)?;
        Ok(params)
    }

    // ===== 函数定义（支持可见性修饰符） =====
    fn parse_fn_def(&mut self, attributes: Vec<Attribute>) -> Result<FnDef, String> {
        // 可选可见性修饰符
        if let Some((Token::Pub, _)) = self.peek() {
            self.next();
        } else if let Some((Token::Priv, _)) = self.peek() {
            self.next();
        }
        self.expect(Token::Fn)?;
        let name = self.parse_ident()?;
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.push_generic_scope(&generic_params);
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
        self.pop_generic_scope();
        Ok(FnDef {
            attributes,
            name,
            generic_params,
            params,
            return_type: ret_type,
            body,
        })
    }

    // ===== parse_params（使用 Token::SelfLower） =====
    fn parse_params(&mut self) -> Result<Vec<Param>, String> {
        let mut params = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RParen { break; }

            // ===== 处理 &mut self / &self =====
            let handled_self = if let Some((Token::Amp, _)) = self.peek() {
                self.next(); // consume '&'
                let is_mut = if let Some((Token::Mut, _)) = self.peek() {
                    self.next();
                    true
                } else {
                    false
                };
                // 检查是否是 self（现在是 Token::SelfLower）
                if let Some((Token::SelfLower, _)) = self.peek() {
                    self.next(); // consume 'self'
                    let ty = Type::Ref {
                        mutable: is_mut,
                        inner: Box::new(Type::SelfType),
                    };
                    params.push(Param {
                        name: "self".to_string(),
                        ty,
                    });
                    if let Some((Token::Comma, _)) = self.peek() {
                        self.next();
                    }
                    true
                } else {
                    return Err("Expected 'self' after '&'".to_string());
                }
            } else if let Some((Token::SelfLower, _)) = self.peek() {
                // ===== 处理裸 self =====
                self.next(); // consume 'self'
                params.push(Param {
                    name: "self".to_string(),
                    ty: Type::SelfType,
                });
                if let Some((Token::Comma, _)) = self.peek() {
                    self.next();
                }
                true
            } else {
                false
            };

            if handled_self {
                continue;
            }

            // ===== 普通参数 =====
            let name = self.parse_ident()?;
            self.expect(Token::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param { name, ty });

            match self.peek() {
                Some((Token::Comma, _)) => { self.next(); continue; }
                _ => break,
            }
        }
        Ok(params)
    }

    // ===== parse_model_def =====
    fn parse_model_def(&mut self, attributes: Vec<Attribute>) -> Result<ModelDef, String> {
        self.expect(Token::Model)?;
        let name = self.parse_ident()?;
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.expect(Token::LBrace)?;

        let mut fields = Vec::new();
        let mut functions = Vec::new();

        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }

            if *token == Token::Var || *token == Token::Let {
                self.next();
                let field_name = self.parse_ident()?;
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                if let Some((Token::Eq, _)) = self.peek() {
                    self.next();
                    self.parse_expr()?;
                }
                self.expect(Token::Semicolon)?;
                fields.push(ModelField { name: field_name, ty });
            } else if let Some((Token::Fn, _)) = self.peek() {
                let fn_attrs = self.parse_attributes()?;
                functions.push(self.parse_fn_def(fn_attrs)?);
            } else {
                return Err("Expected field (var/let) or function (fn) definition in model block".to_string());
            }
        }

        self.expect(Token::RBrace)?;
        Ok(ModelDef {
            attributes,
            name,
            generic_params,
            fields,
            functions,
        })
    }

    // ===== 类型解析 =====
    fn parse_type(&mut self) -> Result<Type, String> {
        if let Some((Token::Amp, _)) = self.peek() {
            self.next();
            let mutable = if let Some((Token::Mut, _)) = self.peek() {
                self.next();
                true
            } else {
                false
            };
            // 新增：&[T] / &mut [T] 切片类型
            if let Some((Token::LBracket, _)) = self.peek() {
                self.next();
                let elem_ty = self.parse_type()?;
                self.expect(Token::RBracket)?;
                return Ok(Type::Ref {
                    mutable,
                    inner: Box::new(Type::Slice(Box::new(elem_ty))),
                });
            }
            let inner = Box::new(self.parse_type()?);
            return Ok(Type::Ref { mutable, inner });
        }
        let base = self.parse_base_type()?;
        if let Some((Token::Lt, _)) = self.peek() {
            if let Ok(tag) = self.try_parse_privacy_tag() {
                return Ok(Type::Privacy(Box::new(base), tag));
            }
        }
        Ok(base)
    }

    // ===== 修改后的 parse_base_type（支持泛型参数识别） =====
    fn parse_base_type(&mut self) -> Result<Type, String> {
        // 首先检查 Tensor
        if let Some((Token::Ident, name)) = self.peek() {
            if name == "Tensor" {
                self.next();
                self.expect(Token::Lt)?;
                let dtype = Box::new(self.parse_base_type()?);
                self.expect(Token::Comma)?;
                let shape = self.parse_shape()?;
                self.expect(Token::Gt)?;
                return Ok(Type::Tensor { dtype, shape });
            }
        }
        match self.peek() {
            Some((Token::I8, _)) => { self.next(); Ok(Type::I8) }
            Some((Token::I16, _)) => { self.next(); Ok(Type::I16) }
            Some((Token::I32, _)) => { self.next(); Ok(Type::I32) }
            Some((Token::I64, _)) => { self.next(); Ok(Type::I64) }
            Some((Token::I128, _)) => { self.next(); Ok(Type::I128) }
            Some((Token::U8, _)) => { self.next(); Ok(Type::U8) }
            Some((Token::U16, _)) => { self.next(); Ok(Type::U16) }
            Some((Token::U32, _)) => { self.next(); Ok(Type::U32) }
            Some((Token::U64, _)) => { self.next(); Ok(Type::U64) }
            Some((Token::U128, _)) => { self.next(); Ok(Type::U128) }
            Some((Token::F16, _)) => { self.next(); Ok(Type::F16) }
            Some((Token::F32, _)) => { self.next(); Ok(Type::F32) }
            Some((Token::F64, _)) => { self.next(); Ok(Type::F64) }
            Some((Token::Bool, _)) => { self.next(); Ok(Type::Bool) }
            Some((Token::Char, _)) => { self.next(); Ok(Type::Char) }
            Some((Token::Str, _)) => { self.next(); Ok(Type::Str) }
            Some((Token::SelfType, _)) => { self.next(); Ok(Type::SelfType) }
            Some((Token::LParen, _)) => {
                self.next(); // consume '('
                if let Some((Token::RParen, _)) = self.peek() {
                    self.next();
                    Ok(Type::Unit)
                } else {
                    Err("Tuple types not supported yet".to_string())
                }
            }
            Some((Token::Ident, name)) => {
                let name_clone = name.clone();
                self.next();
                if let Some((Token::Lt, _)) = self.peek() {
                    // 泛型类型实例化：Option<T> → Type::Generic
                    self.next();
                    let mut args = Vec::new();
                    while let Some((token, _)) = self.peek() {
                        if *token == Token::Gt { break; }
                        let ty = self.parse_base_type()?;
                        args.push(ty);
                        if let Some((Token::Comma, _)) = self.peek() {
                            self.next();
                        } else if let Some((Token::Gt, _)) = self.peek() {
                            break;
                        } else {
                            return Err("Expected ',' or '>' in generic type".to_string());
                        }
                    }
                    self.expect(Token::Gt)?;
                    Ok(Type::Generic(name_clone, args))
                } else {
                    // ===== 判断是否是泛型参数 =====
                    // 不再靠"名字是不是 T/U/E/K/V"这种硬编码猜测——改成查真正
                    // 的作用域栈：当前站在哪个 fn/struct/enum/implement/interface
                    // 声明内部，这些声明各自把自己的 generic_params 压栈在
                    // push_generic_scope 里，这里只要查名字在不在里面。
                    // 这样任何合法的泛型参数名都认得出来，也不会误伤一个真的
                    // 叫 "T" 的普通 struct（只要它不是在某个声明了 T 作为泛型
                    // 参数的作用域内被引用）。
                    if name_clone == "usize" {
                        Ok(Type::U64)
                    } else if name_clone == "isize" {
                        Ok(Type::I64)
                    } else if self.is_active_generic(&name_clone) {
                        Ok(Type::TypeParam(name_clone))
                    } else {
                        Ok(Type::Struct(name_clone))
                    }
                }
            }
            _ => Err("Expected type".to_string()),
        }
    }

    // ===== 隐私标签 =====
    fn try_parse_privacy_tag(&mut self) -> Result<PrivacyTag, String> {
        let pos = self.pos;
        if let Some((Token::Lt, _)) = self.peek() {
            self.next();
            let tag = match self.peek() {
                Some((Token::Ident, name)) if name == "public" => {
                    self.next();
                    PrivacyTag::Public
                }
                Some((Token::Ident, name)) if name == "private" => {
                    self.next();
                    PrivacyTag::Private
                }
                Some((Token::Ident, name)) if name == "dp" => {
                    self.next();
                    self.expect(Token::LParen)?;
                    if let Some((Token::Ident, key)) = self.next() {
                        if key != "eps" {
                            return Err(format!("Expected 'eps', got '{}'", key));
                        }
                    } else {
                        return Err("Expected 'eps'".to_string());
                    }
                    self.expect(Token::Colon)?;
                    let eps = self.parse_rational_literal()?;
                    let delta = if let Some((Token::Comma, _)) = self.peek() {
                        self.next();
                        if let Some((Token::Ident, key)) = self.next() {
                            if key != "delta" {
                                return Err(format!("Expected 'delta', got '{}'", key));
                            }
                        } else {
                            return Err("Expected 'delta'".to_string());
                        }
                        self.expect(Token::Colon)?;
                        Some(self.parse_rational_literal()?)
                    } else {
                        None
                    };
                    self.expect(Token::RParen)?;
                    PrivacyTag::Differential { eps, delta }
                }
                _ => {
                    self.pos = pos;
                    return Err("Expected privacy tag".to_string());
                }
            };
            self.expect(Token::Gt)?;
            Ok(tag)
        } else {
            self.pos = pos;
            Err("Expected '<' for privacy tag".to_string())
        }
    }

    fn parse_rational_literal(&mut self) -> Result<String, String> {
        match self.peek() {
            Some((Token::Integer, v)) => {
                let int_part = v.clone();
                self.next();
                if let Some((Token::Slash, _)) = self.peek() {
                    self.next();
                    if let Some((Token::Integer, den)) = self.next() {
                        Ok(format!("{}/{}", int_part, den))
                    } else {
                        Err("Expected integer after '/'".to_string())
                    }
                } else {
                    Ok(int_part)
                }
            }
            Some((Token::Float, v)) => {
                let v = v.clone();
                self.next();
                Ok(v)
            }
            _ => Err("Expected rational literal".to_string()),
        }
    }

    // ===== 形状 =====
    fn parse_shape(&mut self) -> Result<Vec<ShapeDim>, String> {
        self.expect(Token::LBracket)?;
        let mut dims = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RBracket { break; }
            let (token, value) = self.next().unwrap();
            let dim = match token {
                Token::Ident => {
                    if value == "Dyn" {
                        ShapeDim::Dyn
                    } else if value == "Sym" {
                        self.expect(Token::Lt)?;
                        let sym_name = self.parse_ident()?;
                        self.expect(Token::Gt)?;
                        ShapeDim::Sym(sym_name)
                    } else {
                        ShapeDim::Sym(value)
                    }
                }
                Token::Integer => {
                    let num = value.parse::<usize>().unwrap();
                    ShapeDim::Const(num)
                }
                _ => return Err("Expected dimension in shape".to_string()),
            };
            dims.push(dim);
            match self.peek() {
                Some((Token::Comma, _)) => {
                    self.next();
                    if let Some((Token::RBracket, _)) = self.peek() { break; }
                }
                Some((Token::RBracket, _)) => break,
                _ => return Err("Expected comma or closing bracket in shape".to_string()),
            }
        }
        self.expect(Token::RBracket)?;
        Ok(dims)
    }

    // ===== 修改后的 parse_struct_def（支持泛型） =====
    fn parse_struct_def(&mut self) -> Result<StructDef, String> {
        self.expect(Token::Struct)?;
        let name = self.parse_ident()?;

        // ===== 解析泛型参数：struct Vec<T> =====
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.push_generic_scope(&generic_params);

        self.expect(Token::LBrace)?;
        let mut fields = Vec::new();

        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }
            let field_name = self.parse_ident()?;
            self.expect(Token::Colon)?;
            let ty = self.parse_type()?;
            fields.push(StructField { name: field_name, ty });
            match self.peek() {
                Some((Token::Comma, _)) => {
                    self.next();
                    if let Some((Token::RBrace, _)) = self.peek() { break; }
                }
                Some((Token::RBrace, _)) => break,
                _ => return Err("Expected comma or closing brace".to_string()),
            }
        }

        self.expect(Token::RBrace)?;
        self.pop_generic_scope();
        Ok(StructDef {
            name,
            generic_params,
            fields,
        })
    }

    // ===== 修改后的 parse_enum_def（支持泛型） =====
    fn parse_enum_def(&mut self) -> Result<EnumDef, String> {
        self.expect(Token::Enum)?;
        let name = self.parse_ident()?;

        // ===== 解析泛型参数：enum Option<T> =====
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.push_generic_scope(&generic_params);

        self.expect(Token::LBrace)?;
        let mut variants = Vec::new();

        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }
            let variant_name = self.parse_ident()?;

            let ty = if let Some((Token::LParen, _)) = self.peek() {
                self.next();
                let param_ty = self.parse_type()?;
                self.expect(Token::RParen)?;
                Some(param_ty)
            } else {
                None
            };

            variants.push(EnumVariant { name: variant_name, ty });

            match self.peek() {
                Some((Token::Comma, _)) => {
                    self.next();
                    if let Some((Token::RBrace, _)) = self.peek() { break; }
                }
                Some((Token::RBrace, _)) => break,
                _ => return Err("Expected comma or closing brace".to_string()),
            }
        }

        self.expect(Token::RBrace)?;
        self.pop_generic_scope();
        Ok(EnumDef {
            name,
            generic_params,
            variants,
        })
    }

    fn parse_const_def(&mut self) -> Result<ConstDef, String> {
        self.expect(Token::Const)?;
        let name = self.parse_ident()?;
        self.expect(Token::Colon)?;
        let ty = self.parse_type()?;
        self.expect(Token::Eq)?;
        let value = Box::new(self.parse_expr()?);
        self.expect(Token::Semicolon)?;
        Ok(ConstDef { name, ty, value })
    }

    // ===== 块和语句 =====
    fn parse_block(&mut self) -> Result<Block, String> {
        self.expect(Token::LBrace)?;
        let mut stmts = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RBrace { break; }
            stmts.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(Block { stmts })
    }

    fn parse_unsafe_block(&mut self) -> Result<UnsafeBlockStmt, String> {
        self.expect(Token::Unsafe)?;
        let kind = if let Some((Token::Verify, _)) = self.peek() {
            self.next();
            UnsafeKind::Verify
        } else {
            UnsafeKind::Normal
        };
        let body = self.parse_block()?;
        Ok(UnsafeBlockStmt { kind, body })
    }

    // ===== 语句（带调试打印） =====
    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        eprintln!("parse_stmt at pos {}: {:?}", self.pos, self.peek());
        match self.peek() {
            Some((Token::Persist, _)) => {
                self.next();
                return match self.peek() {
                    Some((Token::Let, _)) => {
                        self.next();
                        self.parse_binding(false, true)
                    }
                    Some((Token::Var, _)) => {
                        self.next();
                        self.parse_binding(true, true)
                    }
                    _ => Err("Expected 'let' or 'var' after 'persist'".to_string()),
                };
            }
            Some((Token::Let, _)) => {
                self.next();
                return self.parse_binding(false, false);
            }
            Some((Token::Var, _)) => {
                self.next();
                return self.parse_binding(true, false);
            }
            Some((Token::Break, _)) => return self.parse_break_stmt(),
            Some((Token::Return, _)) => return self.parse_return_stmt(),
            Some((Token::While, _)) => return self.parse_while_stmt(),
            Some((Token::For, _)) => return self.parse_for_stmt(),
            Some((Token::Loop, _)) => return self.parse_loop_stmt(),
            Some((Token::If, _)) | Some((Token::Lack, _)) => {
                let expr = self.parse_expr()?;
                if let Some((Token::Semicolon, _)) = self.peek() {
                    self.next();
                }
                return Ok(Stmt::ExprStmt(expr));
            }
            Some((Token::Unsafe, _)) => {
                let expr = self.parse_expr()?;
                if let Some((Token::Semicolon, _)) = self.peek() {
                    self.next();
                }
                return Ok(Stmt::ExprStmt(expr));
            }
            _ => {}
        }

        // 赋值语句（包括复合赋值 += -= *= /= %=）
        // 关键修改：target 不再局限于裸标识符——self.len = ...、arr[i] = ...
        // 这类写法，第一个 token 是 self/arr，不是 Ident，靠"peek 第一个
        // token 是不是 Ident"这种一次性前瞻的老办法完全判断不出来。
        // 现在统一先把 target 当一个完整表达式解析出来（parse_expr 天然
        // 会在碰到 = 时停下，因为 = 不属于任何运算符优先级链条），再看
        // 后面跟不跟赋值类 token；如果不跟，target 本身就是一条普通表达式
        // 语句，直接复用，不用再解析第二遍。
        let target = self.parse_expr()?;

        let compound_op = match self.peek() {
            Some((Token::Eq, _)) => None,
            Some((Token::PlusEq, _)) => Some(BinaryOp::Add),
            Some((Token::MinusEq, _)) => Some(BinaryOp::Sub),
            Some((Token::StarEq, _)) => Some(BinaryOp::Mul),
            Some((Token::SlashEq, _)) => Some(BinaryOp::Div),
            Some((Token::PercentEq, _)) => Some(BinaryOp::Mod),
            _ => None,
        };
        let is_assign = matches!(
            self.peek(),
            Some((Token::Eq, _))
                | Some((Token::PlusEq, _))
                | Some((Token::MinusEq, _))
                | Some((Token::StarEq, _))
                | Some((Token::SlashEq, _))
                | Some((Token::PercentEq, _))
        );

        if is_assign {
            self.next(); // 吃掉 = 或 += / -= / *= / /= / %=

            let rhs = self.parse_expr()?;
            self.expect(Token::Semicolon)?;

            let expr = match compound_op {
                None => rhs,
                Some(op) => Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::BinaryOp {
                        op,
                        left: Box::new(target.clone()),
                        right: Box::new(rhs),
                    },
                },
            };

            return Ok(Stmt::Assign(AssignStmt { target: Box::new(target), expr: Box::new(expr) }));
        }

        // 不是赋值——target 其实就是一条普通表达式语句
        if let Some((Token::Semicolon, _)) = self.peek() {
            self.next();
        }
        Ok(Stmt::ExprStmt(target))
    }

    fn parse_binding(&mut self, mutable: bool, persist: bool) -> Result<Stmt, String> {
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
            mutable,
            persist,
        }))
    }

    fn parse_break_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::Break)?;
        self.expect(Token::Semicolon)?;
        Ok(Stmt::Break(BreakStmt {}))
    }

    fn parse_loop_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::Loop)?;
        let body = self.parse_block()?;
        Ok(Stmt::Loop(LoopStmt { body }))
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::For)?;
        let var = self.parse_ident()?;
        self.expect(Token::In)?;
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let iterable = Box::new(self.parse_expr()?);
        self.no_struct_literal = prev;
        let body = self.parse_block()?;
        Ok(Stmt::For(ForStmt { var, iterable, body }))
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

    fn parse_while_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(Token::While)?;
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let cond = self.parse_expr()?;
        self.no_struct_literal = prev;
        let body = self.parse_block()?;
        Ok(Stmt::While(WhileStmt { cond: Box::new(cond), body }))
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt, String> {
        let expr = self.parse_expr()?;
        if let Some((Token::Semicolon, _)) = self.peek() {
            self.next();
        }
        Ok(Stmt::ExprStmt(expr))
    }

    // ===== 表达式解析（带调试打印） =====
    fn parse_expr(&mut self) -> Result<Expr, String> {
        eprintln!("parse_expr at pos {}: {:?}", self.pos, self.peek());
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

    // ===== 新增：lack &[T] 空切片字面量 =====
    fn parse_lack_slice(&mut self) -> Result<Expr, String> {
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

    fn parse_if_expr(&mut self) -> Result<Expr, String> {
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

    fn parse_match_expr(&mut self) -> Result<Expr, String> {
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

    // ===== 修改后的 parse_pattern（支持绑定） =====
    fn parse_pattern(&mut self) -> Result<Pattern, String> {
        if let Some((Token::Ident, name)) = self.peek() {
            if name == "_" {
                self.next();
                return Ok(Pattern::Wildcard);
            }
        }

        let name = self.parse_ident()?;
        match self.peek() {
            Some((Token::PathSep, _)) => {
                self.next();
                let variant_name = self.parse_ident()?;

                // ===== 检查是否带绑定：Enum::Variant(binding) =====
                if let Some((Token::LParen, _)) = self.peek() {
                    self.next(); // consume '('
                    let binding = self.parse_ident()?;
                    self.expect(Token::RParen)?;
                    return Ok(Pattern::EnumVariantWithBinding {
                        enum_name: name,
                        variant_name,
                        binding,
                    });
                }

                Ok(Pattern::EnumVariant {
                    enum_name: name,
                    variant_name,
                })
            }
            Some((Token::Colon, _)) => Err("Unexpected ':' in pattern, expected '::'".to_string()),
            _ => Err("expected '::' after enum name in pattern".to_string()),
        }
    }

    // ===== 运算符优先级 =====
    fn parse_range(&mut self) -> Result<Expr, String> {
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

    fn parse_or(&mut self) -> Result<Expr, String> {
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

    fn parse_and(&mut self) -> Result<Expr, String> {
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

    fn parse_comparison(&mut self) -> Result<Expr, String> {
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

    fn parse_add(&mut self) -> Result<Expr, String> {
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
    fn parse_mul(&mut self) -> Result<Expr, String> {
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

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            // 关键修复：!x 之前被脱糖成调用一个压根不存在的函数 "not"，
            // 是 test_option_inline.xiyi 那个 "undefined function or method: not"
            // 报错的真正源头。现在 ast.rs 已经有真正的 Unary 节点了，改用它。
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
            // 新增：一元负号 -x，之前这一层完全没处理
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

    // ===== 新增：后缀索引 bytes[i]，支持连续 arr[i][j] =====
    fn parse_postfix(&mut self) -> Result<Expr, String> {
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
    
    // ===== 新增：as 类型转换，优先级在一元运算符外面一层
    // （-x as i32 会解析成 (-x) as i32，跟 Rust 习惯一致）=====
    fn parse_cast(&mut self) -> Result<Expr, String> {
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
    fn parse_primary(&mut self) -> Result<Expr, String> {
        let peek_token = self.peek().cloned();
        match peek_token {
            Some((Token::Integer, value)) => {
                self.next();
                let num = value.parse::<i64>().unwrap();
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Literal(Literal::Int(num)),
                })
            }
            Some((Token::Float, value)) => {
                self.next();
                let num = value.parse::<f64>().unwrap();
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Literal(Literal::Float(num)),
                })
            }
            Some((Token::String, value)) => {
                self.next();
                let inner = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    value[1..value.len()-1].to_string()
                } else {
                    value
                };
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Literal(Literal::String(inner)),
                })
            }
            // 新增：bytes"..." 字节字符串字面量，类型是 &[u8]
            // bytes 后面必须紧跟字符串字面量——这是新加的全局关键字，
            // 在语法层面就要求它单独出现时后面立刻是 String token，
            // 不允许中间插别的 token（空格/换行在词法阶段已经被跳过，
            // 没法在这一层区分"有没有空格"，只能保证 token 序列上紧邻）。
            Some((Token::Bytes, _)) => {
                self.next();
                let value = match self.next() {
                    Some((Token::String, v)) => v,
                    other => {
                        return Err(format!(
                            "Expected string literal immediately after 'bytes', got {:?}",
                            other
                        ))
                    }
                };
                let inner = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    value[1..value.len() - 1].to_string()
                } else {
                    value
                };
                let bytes = Self::decode_byte_string(&inner)?;
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Literal(Literal::ByteString(bytes)),
                })
            }
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
            Some((Token::True, _)) => {
                self.next();
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Literal(Literal::Bool(true)),
                })
            }
            Some((Token::False, _)) => {
                self.next();
                Ok(Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Literal(Literal::Bool(false)),
                })
            }
            // ===== 处理 self（方法调用接收者） =====
            Some((Token::SelfLower, _)) => {
                self.next(); // consume 'self'
                let mut expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Ident("self".to_string()),
                };
                // 处理点链 .xxx 或 .xxx()
                while let Some((Token::Dot, _)) = self.peek() {
                    self.next();
                    let field_name = self.parse_ident()?;
                    if let Some((Token::LParen, _)) = self.peek() {
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
            // ===== SelfType 分支 =====
            Some((Token::SelfType, _)) => {
                self.next(); // consume 'Self'

                let mut expr = if let Some((Token::LBrace, _)) = self.peek() {
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

                // 处理点链
                while let Some((Token::Dot, _)) = self.peek() {
                    self.next();
                    let field_name = self.parse_ident()?;
                    if let Some((Token::LParen, _)) = self.peek() {
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
                if let Some((Token::PathSep, _)) = self.peek() {
                    self.next(); // consume '::'
                    let variant_name = self.parse_ident()?;

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
                    let mut expr = Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::StructInit {
                            struct_name: name.clone(),
                            fields,
                        },
                    };
                    // 处理点链
                    while let Some((Token::Dot, _)) = self.peek() {
                        self.next();
                        let field_name = self.parse_ident()?;
                        if let Some((Token::LParen, _)) = self.peek() {
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
                    return Ok(expr);
                    }
                }

                // 普通标识符 + 点链
                let mut expr = Expr {
                    id: self.next_expr_id(),
                    kind: ExprKind::Ident(name),
                };
                while let Some((Token::Dot, _)) = self.peek() {
                    self.next();
                    let field_name = self.parse_ident()?;
                    if let Some((Token::LParen, _)) = self.peek() {
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
            Some((Token::LParen, _)) => {
                self.next();
                // 检查是否是单元类型 ()
                if let Some((Token::RParen, _)) = self.peek() {
                    self.next();
                    return Ok(Expr {
                        id: self.next_expr_id(),
                        kind: ExprKind::Literal(Literal::Unit),
                    });
                }
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
                eprintln!("Unexpected token: {:?} at position {}", self.peek(), self.pos);
                eprintln!("Context (5 tokens before):");
                for i in 0..5 {
                    if self.pos > i {
                        let idx = self.pos - i - 1;
                        eprintln!("  {:?}", self.tokens.get(idx));
                    }
                }
                eprintln!("Current token: {:?}", self.tokens.get(self.pos));
                eprintln!("Next 5 tokens:");
                for i in 1..=5 {
                    eprintln!("  {:?}", self.tokens.get(self.pos + i));
                }
                Err("Expected expression".to_string())
            }
        }
    }

    // ===== 结构体初始化字段（支持简写） =====
    fn parse_struct_fields(&mut self) -> Result<Vec<(String, Expr)>, String> {
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
    fn parse_call_args(&mut self) -> Result<Vec<CallArg>, String> {
        let mut args = Vec::new();
        while let Some((token, _)) = self.peek() {
            if *token == Token::RParen { break; }
            let is_ident_like = match token {
                Token::Ident | Token::In => true,
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
    fn parse_closure(&mut self) -> Result<Expr, String> {
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