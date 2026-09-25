// src/syntactic/parse_item.rs
use crate::ast::*;
use crate::token::Token;
use super::Parser;

impl Parser {
    // ===== parse_item：支持所有顶层项 =====
    //
    // 返回类型是 Result<Vec<Item>, String>，不是单个 Item——因为：
    // 1. `mod name;` 声明解析完之后没有对应的 Item 可以产出（返回空 Vec）。
    // 2. `use a::b::{X, Y};` 这种花括号多导入，一次要产出好几个 Item::Use
    //    （每个导入名一个），单个 Option<Item> 装不下。
    pub(crate) fn parse_item(&mut self) -> Result<Vec<Item>, String> {
        let attrs = self.parse_attributes()?;

        // ===== 顶层 pub/priv 前缀 =====
        // 之前只有 parse_func_def/parse_func_sig 内部会消费一个前置的
        // pub/priv（针对 implement/interface 块里的方法），但顶层项
        // （`pub use ...;`、`pub mod ...;`）从来没有被消费过——parse_item
        // 的大 match 直接拿到 Token::Pub，哪个分支都对不上，一路落到
        // 最后那句 "Expected function, model, implement, interface,
        // use, struct, enum, or const definition"。这里统一吃掉，不区分
        // pub/priv（这两个修饰符目前对顶层项没有实际的可见性检查，跟
        // `Item::Use` 现在也只是被解析出来、不真正生效是一回事）。
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
            Some((Token::Fn, _)) => Ok(vec![Item::FnDef(self.parse_func_def(attrs)?)]),
            Some((Token::Model, _)) => Ok(vec![Item::ModelDef(self.parse_model(attrs)?)]),
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
    // 路径读取的原语（读第一段、判断/消费 ::）挪进了 parse_path.rs，
    // 这里只保留 use 语句自己特有的部分：花括号多导入展开、别名、
    // 结尾分号。parse_path_root() 仍然是 self 上的方法，只是定义
    // 挪了个文件，调用方式不用变。
    pub(crate) fn parse_use_item(&mut self) -> Result<Vec<Item>, String> {
        let mut parts = Vec::new();
        let first = self.parse_path_root()?;
        parts.push(first);
        while let Some((Token::PathSep, _)) = self.peek() {
            self.next();

            // ===== 花括号多导入：use a::b::{X, Y, Z}; =====
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
    pub(crate) fn parse_implement_def(&mut self, attributes: Vec<Attribute>) -> Result<Item, String> {
        self.expect(Token::Implement)?;

        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        // 从这里开始，target_type/interface_name/where_clause/函数体全都可能
        // 引用到自己声明的泛型参数（比如 `implement<T> Container<T>`）。原来
        // 这里手写 push_generic_scope 开头、pop_generic_scope 结尾，中间一整
        // 段全是会用 `?` 直接甩错误的解析代码——任何一步失败都会跳过结尾的
        // pop，让作用域永久残留，污染后续所有解析。改成 with_generic_scope：
        // 闭包内部想失败就正常 Err、想成功就正常 Ok，pop 由它自己无条件执行
        // 一次，不用在每条错误路径上都惦记着手动收尾（之前给"interface 名字
        // 缺失"和"游离 '('"这两条错误路径手写的 pop_generic_scope() 调用，
        // 现在也不再需要，一并去掉）。
        //
        // 闭包只把中间这段可能失败的解析结果（target_type/interface_name/
        // where_clause/functions）传出来，attributes 和 generic_params 留在
        // 外层、等 with_generic_scope 返回之后再用来拼最终的 ImplementDef——
        // 这样不需要为了"闭包内外都要用一份 generic_params"而去 clone 它
        // （没法确定 GenericParam 有没有派生 Clone，没必要冒这个风险）。
        let (target_type, interface_name, where_clause, functions) =
            self.with_generic_scope(&generic_params, |this| {
                // ===== implement X for Y 里 X/Y 谁是谁 =====
                // 规范（docx §6.2）写得很明确：
                //     implement Drawable for Point { ... }
                // Drawable 是接口名，Point 才是被实现的目标类型——"for" 前面是
                // 接口，后面是目标。但这里没法在看到 "for" 之前就知道第一段该
                // 按哪种身份解析（`implement<T> Container<T> { ... }` 完全没有
                // "for"，第一段直接就是目标类型），只能先按"类型"解析出第一
                // 段，再看后面有没有 "for" 来决定：没有 "for"，第一段就是目标
                // 类型（固有实现）；有 "for"，说明第一段其实是接口名，"for"
                // 后面那个才是真正的目标类型。
                let first_type = this.parse_type()?;

                let (target_type, interface_name) = if let Some((Token::For, _)) = this.peek() {
                    this.next(); // consume 'for'
                    let interface_name = match &first_type {
                        Type::Struct(name) => name.clone(),
                        Type::Generic(name, _) => name.clone(),
                        _ => return Err("Expected interface name before 'for'".to_string()),
                    };
                    let target_type = this.parse_type()?;
                    (target_type, Some(interface_name))
                } else {
                    (first_type, None)
                };

                let where_clause = if let Some((Token::Where, _)) = this.peek() {
                    this.next();
                    this.parse_where_clause()?
                } else {
                    Vec::new()
                };

                // implement 头部到这里只应该紧跟 '{'，任何 '(' 都意味着用户
                // 写错了（比如多打了一个括号），必须直接报错，不能悄悄吃掉
                // 继续解析（那样只会得到一棵语义已经跑偏的 AST）。
                if let Some((Token::LParen, _)) = this.peek() {
                    return Err(format!(
                        "Unexpected '(' after implement header at pos {}",
                        this.pos
                    ));
                }

                this.expect(Token::LBrace)?;
                let mut functions = Vec::new();
                while let Some((token, _)) = this.peek() {
                    if *token == Token::RBrace { break; }
                    let fn_attrs = this.parse_attributes()?;
                    functions.push(this.parse_func_def(fn_attrs)?);
                }
                this.expect(Token::RBrace)?;

                Ok((target_type, interface_name, where_clause, functions))
            })?;

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
    pub(crate) fn parse_interface_def(&mut self, attributes: Vec<Attribute>) -> Result<Item, String> {
        self.expect(Token::Interface)?;
        let name = self.parse_ident()?;

        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };

        // 跟 parse_implement_def 一样的问题：原来 push_generic_scope 开头、
        // pop_generic_scope 结尾，中间 expect(LBrace)/methods 循环/
        // expect(RBrace) 任何一步失败都会漏掉结尾的 pop。改用
        // with_generic_scope，收尾保证执行；闭包只返回 methods，
        // attributes/name/generic_params 留在外层，出了闭包再拼
        // InterfaceDef，不需要 clone generic_params。
        let methods = self.with_generic_scope(&generic_params, |this| {
            this.expect(Token::LBrace)?;
            let mut methods = Vec::new();
            while let Some((token, _)) = this.peek() {
                if *token == Token::RBrace { break; }
                methods.push(this.parse_func_sig()?);
            }
            this.expect(Token::RBrace)?;
            Ok(methods)
        })?;

        Ok(Item::Interface(InterfaceDef {
            attributes,
            name,
            generic_params,
            methods,
        }))
    }

    // ===== parse_struct_def（支持泛型） =====
    pub(crate) fn parse_struct_def(&mut self) -> Result<StructDef, String> {
        self.expect(Token::Struct)?;
        let name = self.parse_ident()?;

        // ===== 解析泛型参数：struct Vec<T> =====
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };

        // 同上：字段类型可能引用到自己声明的泛型参数，中间任何一步
        // 失败都不能漏掉 pop，改用 with_generic_scope；闭包只返回
        // fields，name/generic_params 留在外层再拼 StructDef。
        let fields = self.with_generic_scope(&generic_params, |this| {
            this.expect(Token::LBrace)?;
            let mut fields = Vec::new();

            while let Some((token, _)) = this.peek() {
                if *token == Token::RBrace { break; }
                let field_name = this.parse_ident()?;
                this.expect(Token::Colon)?;
                let ty = this.parse_type()?;
                fields.push(StructField { name: field_name, ty });
                match this.peek() {
                    Some((Token::Comma, _)) => {
                        this.next();
                        if let Some((Token::RBrace, _)) = this.peek() { break; }
                    }
                    Some((Token::RBrace, _)) => break,
                    _ => return Err("Expected comma or closing brace".to_string()),
                }
            }

            this.expect(Token::RBrace)?;
            Ok(fields)
        })?;

        Ok(StructDef {
            name,
            generic_params,
            fields,
        })
    }

    // ===== parse_enum_def（支持泛型） =====
    pub(crate) fn parse_enum_def(&mut self) -> Result<EnumDef, String> {
        self.expect(Token::Enum)?;
        let name = self.parse_ident()?;

        // ===== 解析泛型参数：enum Option<T> =====
        let generic_params = if let Some((Token::Lt, _)) = self.peek() {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };

        // 同上：变体携带的类型可能引用到自己声明的泛型参数，改用
        // with_generic_scope 保证收尾；闭包只返回 variants。
        let variants = self.with_generic_scope(&generic_params, |this| {
            this.expect(Token::LBrace)?;
            let mut variants = Vec::new();

            while let Some((token, _)) = this.peek() {
                if *token == Token::RBrace { break; }
                let variant_name = this.parse_ident()?;

                let ty = if let Some((Token::LParen, _)) = this.peek() {
                    this.next();
                    let param_ty = this.parse_type()?;
                    this.expect(Token::RParen)?;
                    Some(param_ty)
                } else {
                    None
                };

                variants.push(EnumVariant { name: variant_name, ty });

                match this.peek() {
                    Some((Token::Comma, _)) => {
                        this.next();
                        if let Some((Token::RBrace, _)) = this.peek() { break; }
                    }
                    Some((Token::RBrace, _)) => break,
                    _ => return Err("Expected comma or closing brace".to_string()),
                }
            }

            this.expect(Token::RBrace)?;
            Ok(variants)
        })?;

        Ok(EnumDef {
            name,
            generic_params,
            variants,
        })
    }

    pub(crate) fn parse_const_def(&mut self) -> Result<ConstDef, String> {
        self.expect(Token::Const)?;
        let name = self.parse_ident()?;
        self.expect(Token::Colon)?;
        let ty = self.parse_type()?;
        self.expect(Token::Eq)?;
        let value = Box::new(self.parse_expr()?);
        self.expect(Token::Semicolon)?;
        Ok(ConstDef { name, ty, value })
    }
}
