pub mod token;
pub mod lexer;
pub mod ast;
pub mod parser;
pub mod intrinsic;
pub mod sema;
pub mod calc;
pub mod hir;
pub mod hir_builder;
pub mod mir;
pub mod mir_builder;
pub mod simplify;
pub mod elaborator;
pub mod codegen;

pub use ast::*;

#[cfg(test)]
mod tests {
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::sema::TypeChecker;

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

    #[test]
    fn test_type_check_ok() {
        let input = "fn main() -> i32 { let x: i32 = 10; x }";
        let mut parser = Parser::new(input);
        let program = parser.parse_program().unwrap();
        let mut checker = TypeChecker::new();
        // check_program 现在返回 Result<hir::HirProgram, String>，is_ok() 仍可用
        assert!(checker.check_program(&program).is_ok());
    }

    #[test]
    fn test_type_check_type_mismatch() {
        let input = "fn main() -> i32 { let x: bool = 10; x }";
        let mut parser = Parser::new(input);
        let program = parser.parse_program().unwrap();
        let mut checker = TypeChecker::new();
        assert!(checker.check_program(&program).is_err());
    }
}