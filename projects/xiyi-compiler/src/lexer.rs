use crate::token::Token;
use logos::Logos;
use std::fmt;

pub struct Lexer<'a> {
    inner: logos::Lexer<'a, Token>,
    source: &'a str,
}

#[derive(Debug)]
pub struct LexError {
    pub message: String,
    pub byte_offset: usize,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (line {}, column {})", self.message, self.line, self.column)
    }
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            inner: Token::lexer(input),
            source: input,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<(Token, String)>, LexError> {
        let trace = std::env::var("XIYI_LEXER_TRACE").is_ok();
        let mut tokens = Vec::new();

        while let Some(token_result) = self.inner.next() {
            match token_result {
                Ok(token) => {
                    let slice = self.inner.slice().to_string();
                    let value = if token == Token::String {
                        let inner = &slice[1..slice.len()-1];
                        unescape_string(inner)
                    } else {
                        slice
                    };
                    if trace {
                        eprintln!(
                            "[LEXER] pos {}: {:?} = {:?}",
                            self.inner.span().start,
                            token,
                            value
                        );
                    }
                    tokens.push((token, value));
                }
                Err(_) => {
                    return Err(self.build_error(self.inner.span().start));
                }
            }
        }
        Ok(tokens)
    }

    fn build_error(&self, byte_offset: usize) -> LexError {
        let bad_char = self.source[byte_offset..].chars().next();
        let (line, column) = Self::line_col(self.source, byte_offset);

        let message = match bad_char {
            Some(ch) => format!(
                "unrecognized character {:?} (U+{:04X})",
                ch, ch as u32
            ),
            None => "unexpected end of input".to_string(),
        };

        LexError { message, byte_offset, line, column }
    }

    fn line_col(source: &str, byte_offset: usize) -> (usize, usize) {
        let mut line = 1;
        let mut col = 1;
        for (i, ch) in source.char_indices() {
            if i >= byte_offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

// 对字符串字面量内容进行转义处理，支持常见的转义序列和 Unicode 转义。
fn unescape_string(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some('\'') => result.push('\''),
                Some('0') => result.push('\0'),
                Some('x') => {
                    let mut hex = String::new();
                    for _ in 0..2 {
                        if let Some(c) = chars.next() {
                            hex.push(c);
                        } else {
                            break;
                        }
                    }
                    if hex.len() == 2 {
                        if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                            result.push(byte as char);
                        } else {
                            result.push_str(&format!("\\x{}", hex));
                        }
                    } else {
                        result.push_str("\\x");
                    }
                }
                Some('u') => {
                    if chars.next() == Some('{') {
                        let mut hex = String::new();
                        while let Some(c) = chars.next() {
                            if c == '}' { break; }
                            hex.push(c);
                        }
                        if !hex.is_empty() && hex.len() <= 6 {
                            if let Ok(codepoint) = u32::from_str_radix(&hex, 16) {
                                if let Some(c) = char::from_u32(codepoint) {
                                    result.push(c);
                                } else {
                                    result.push_str(&format!("\\u{{{}}}", hex));
                                }
                            } else {
                                result.push_str(&format!("\\u{{{}}}", hex));
                            }
                        } else {
                            result.push_str(&format!("\\u{{{}}}", hex));
                        }
                    } else {
                        result.push_str("\\u");
                    }
                }
                Some(c) => {
                    result.push_str(&format!("\\{}", c));
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}