pub mod token;
pub mod lexer;
pub mod ast;
pub mod parser;

pub use ast::*;

#[cfg(test)]
mod tests {
    use crate::lexer::Lexer;
    use crate::token::Token;
    use crate::parser::Parser;
    use logos::Logos;

    #[test]
    fn test_lexer_basic() {
        let input = "fn main() { let x: i32 = 10; }";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize();
        assert_eq!(tokens.len(), 13);
    }

    #[test]
    fn test_parse_basic() {
        let input = "fn main() -> i32 { let x: i32 = 10; x }";
        let mut parser = Parser::new(input);
        let program = parser.parse_program();
        assert!(program.is_ok());
    }
}
