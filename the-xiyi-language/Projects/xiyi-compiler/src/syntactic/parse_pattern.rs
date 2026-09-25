// src/syntactic/parse_pattern.rs
use crate::ast::*;
use crate::token::Token;
use super::Parser;

impl Parser {
    // ===== parse_pattern（支持绑定） =====
    pub(crate) fn parse_pattern(&mut self) -> Result<Pattern, String> {
        if let Some((Token::Ident, name)) = self.peek() {
            if name == "_" {
                self.next();
                return Ok(Pattern::Wildcard);
            }
        }

        let name = self.parse_ident()?;

        // "peek 到 :: 就消费并读下一段"这条原语现在收在 parse_path.rs
        // 的 try_read_path_segment 里，跟 parse_expr.rs 读 Ident::Ident
        // 限定名时用的是同一个函数。但这里读不到 :: 时要报的错，跟
        // parse_expr.rs 不一样——parse_expr.rs 读不到 :: 就落到别的
        // 分支继续解析（合法情况，不是错误），而模式语法里"标识符后面
        // 不是 ::"本身就是错，还要进一步区分"根本没写 ::"和"把 ::
        // 写成了单个 :"这两种不同的提示。这条区分只对模式语法有意义，
        // 不属于路径原语该管的事，所以放在这里、原语返回 None 之后
        // 自己再 peek 一次决定说哪句话。
        let variant_name = match self.try_read_path_segment() {
            Some(result) => result?,
            None => {
                return match self.peek() {
                    Some((Token::Colon, _)) => Err("Unexpected ':' in pattern, expected '::'".to_string()),
                    _ => Err("expected '::' after enum name in pattern".to_string()),
                };
            }
        };

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
}
