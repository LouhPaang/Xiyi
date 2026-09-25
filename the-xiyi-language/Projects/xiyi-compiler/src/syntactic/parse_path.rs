// src/syntactic/parse_path.rs
//
// 路径读取的原语集中地。之前这几件事分散在四个文件里各写一遍：
//   - parse_item.rs::parse_path_root      读路径第一段
//   - parse_item.rs::parse_use_item       读多段路径（含 :: 后跟 {...}
//                                          的花括号展开）
//   - parse_expr.rs::parse_primary        读 Ident::Ident 两段限定名，
//                                          构造 EnumVariantAccess /
//                                          EnumVariantConstruction
//   - parse_pattern.rs::parse_pattern     读 Ident::Ident 两段限定名，
//                                          构造 EnumVariant /
//                                          EnumVariantWithBinding
//
// 后两处手写了两遍逐字节相同的"peek 到 PathSep -> next -> parse_ident"
// 消费逻辑，改一次路径规则要跟着改两处，容易漏。这个文件只负责"怎么
// 读"，不负责"读完之后按什么上下文构造什么 AST 节点"：
//   - 不放 parse_use_item 本身——它是 use 语句级别的解析（花括号
//     展开、别名、结尾分号），路径只是其中一段，且它的"消费 :: 之后
//     可能是 {...} 也可能是普通标识符"这一步跟下面 try_read_path_segment
//     "消费 :: 之后必须紧跟一个标识符"的假设不一样，硬套上去反而要
//     多绕一层，大部分代码依然留在 parse_item.rs。
//   - 不放任何构造 AST 节点的代码——三个调用点构造的节点
//     （Item::Use / ExprKind::EnumVariantAccess·EnumVariantConstruction /
//     Pattern::EnumVariant·EnumVariantWithBinding）各不相同，硬凑成一个
//     通用构造函数只会引入一堆可选参数。
//   - 不放错误信息的措辞——parse_pattern 里"期望 ::"和"把 :: 写成 :"
//     是两条不同的错误，这条区分只对模式语法有意义，不该进路径原语，
//     留给调用方自己在读不到 `::` 时再 peek 一次决定说哪句话。

use crate::token::Token;
use super::Parser;

impl Parser {
    // ===== parse_path_root =====
    // 路径的第一段除了普通标识符，还可能是 crate/super/here 这三个路径
    // 关键字（crate:: 当前 crate 根、super:: 父模块、here:: 当前模块）。
    // 只有第一段会是这几个词，后续路径段（crate::iter::Iterator 里的
    // iter、Iterator）永远是普通标识符，不用特殊处理。
    pub(crate) fn parse_path_root(&mut self) -> Result<String, String> {
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

    // ===== try_read_path_segment =====
    // 限定名读取的核心原语：尝试把当前位置识别成 `::` 加紧随其后的
    // 一段标识符（`Ident::Ident` 里 `::` 之后的那一段）。
    //
    // - 命中 `::`：消费掉它和下一段，返回 `Some(Ok(下一段的名字))`。
    // - 不是 `::` 开头：完全不消费任何 token，返回 `None`——调用方
    //   据此决定是继续走别的分支（parse_expr.rs 就是这样：不是 `::`
    //   就落到普通函数调用/结构体初始化/裸标识符那几条路），还是
    //   自己再 peek 一次挑一句更具体的错误措辞（parse_pattern.rs 需要
    //   区分"根本没写 ::"和"把 :: 写成了单个 :"两种不同的提示）。
    // - `::` 已确认存在，但后面接的不是合法标识符：返回
    //   `Some(Err(..))`——这时 `::` 已经被消费掉了，不能再假装没
    //   识别出来退回去，只能把 parse_ident 的错误如实报出去。
    pub(crate) fn try_read_path_segment(&mut self) -> Option<Result<String, String>> {
        if let Some((Token::PathSep, _)) = self.peek() {
            self.next(); // consume '::'
            Some(self.parse_ident())
        } else {
            None
        }
    }
}
