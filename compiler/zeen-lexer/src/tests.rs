#![cfg(test)]

use miette::SourceSpan;

use super::*;

#[test]
fn ident() {
    const SOURCE: &str = "abc ident hello";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(0.into(), 3)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(4.into(), 5)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(10.into(), 5)))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn ident_after_block_comment() {
    const SOURCE: &str = "/* */ abc";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(6.into(), 3)))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn ident_after_line_comment() {
    const SOURCE: &str = "// \n abc";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(5.into(), 3)))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn macro_ident() {
    const SOURCE: &str = "@print";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::MacroIdent,
            SourceSpan::new(0.into(), 6)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn preprocessor_ident() {
    const SOURCE: &str = "@os[linux]";

    let mut tokens = tokenize(SOURCE);
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::PreprocessorIdent,
            SourceSpan::new(0.into(), 3)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::OpenBracket,
            SourceSpan::new(3.into(), 1)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(4.into(), 5)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::CloseBracket,
            SourceSpan::new(9.into(), 1)
        ))
    );
    assert_eq!(tokens.next(), None);
}

#[test]
fn preprocessor_var() {
    const SOURCE: &str = "@var[arch]";

    let mut tokens = tokenize(SOURCE);
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::PreprocessorVar,
            SourceSpan::new(0.into(), 4)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::OpenBracket,
            SourceSpan::new(4.into(), 1)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(5.into(), 4)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::CloseBracket,
            SourceSpan::new(9.into(), 1)
        ))
    );
    assert_eq!(tokens.next(), None);
}

#[test]
fn preprocessor_debug_and_release() {
    let mut tokens = tokenize("@debug @release");
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::PreprocessorDebug,
            SourceSpan::new(0.into(), 6)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::PreprocessorRelease,
            SourceSpan::new(7.into(), 8)
        ))
    );
    assert_eq!(tokens.next(), None);
}

#[test]
fn reference() {
    const SOURCE: &str = "&val";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ref, SourceSpan::new(0.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(1.into(), 3)))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn ampersand_whitespace_disambiguation() {
    const SOURCE: &str = "a && b a&&b a &&b a & b a &b & x &x";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(0.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::BooleanAnd,
            SourceSpan::new(2.into(), 2)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(5.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(7.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::BooleanAnd,
            SourceSpan::new(8.into(), 2)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(10.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(12.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::BooleanAnd,
            SourceSpan::new(14.into(), 2)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(16.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(18.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Ampersand,
            SourceSpan::new(20.into(), 1)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(22.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(24.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ref, SourceSpan::new(26.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(27.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Ampersand,
            SourceSpan::new(29.into(), 1)
        ))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(31.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ref, SourceSpan::new(33.into(), 1)))
    );
    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ident, SourceSpan::new(34.into(), 1)))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn integers() {
    const SOURCE: &str = "123 0b101 0x1aF 0o14";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Int {
                    base: token::IntBase::Decimal
                }
            },
            SourceSpan::new(0.into(), 3)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Int {
                    base: token::IntBase::Binary
                }
            },
            SourceSpan::new(4.into(), 5)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Int {
                    base: token::IntBase::Hexadecimal
                }
            },
            SourceSpan::new(10.into(), 5)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Int {
                    base: token::IntBase::Octal
                }
            },
            SourceSpan::new(16.into(), 4)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn floats() {
    const SOURCE: &str = "3.14 0.0001";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Float
            },
            SourceSpan::new(0.into(), 4)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Float
            },
            SourceSpan::new(5.into(), 6)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn char_literal() {
    const SOURCE: &str = "'a' '\\0'";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Char {
                    terminated: true,
                    empty: false
                }
            },
            SourceSpan::new(0.into(), 3)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Char {
                    terminated: true,
                    empty: false
                }
            },
            SourceSpan::new(4.into(), 4)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn bytechar_literal() {
    const SOURCE: &str = "b'a'";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::ByteChar {
                    terminated: true,
                    empty: false,
                }
            },
            SourceSpan::new(0.into(), 4)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn str_literal() {
    const SOURCE: &str = "\"hello\"";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Str { terminated: true }
            },
            SourceSpan::new(0.into(), 7)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn raw_str_literal() {
    const SOURCE: &str = "r#\"hello\"#";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::RawStr { terminated: true }
            },
            SourceSpan::new(0.into(), 10)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn basic_symbols() {
    const SOURCE: &str = "_ ; : , . ~ ? = ! > < & | ^ + - * / % #";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Underscore,
            SourceSpan::new(0.into(), 1)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Semicolon,
            SourceSpan::new(2.into(), 1)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Colon, SourceSpan::new(4.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Comma, SourceSpan::new(6.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Dot, SourceSpan::new(8.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Tilde, SourceSpan::new(10.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Question,
            SourceSpan::new(12.into(), 1)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Eq, SourceSpan::new(14.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Bang, SourceSpan::new(16.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Gt, SourceSpan::new(18.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Lt, SourceSpan::new(20.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Ampersand,
            SourceSpan::new(22.into(), 1)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Pipe, SourceSpan::new(24.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Caret, SourceSpan::new(26.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Plus, SourceSpan::new(28.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Minus, SourceSpan::new(30.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Star, SourceSpan::new(32.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Slash, SourceSpan::new(34.into(), 1)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Percent,
            SourceSpan::new(36.into(), 1)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Hashtag,
            SourceSpan::new(38.into(), 1)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn complex_symbols() {
    const SOURCE: &str = "<= >= == && || => << >>";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Leq, SourceSpan::new(0.into(), 2)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Geq, SourceSpan::new(3.into(), 2)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::BooleanEq,
            SourceSpan::new(6.into(), 2)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::BooleanAnd,
            SourceSpan::new(9.into(), 2)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::BooleanOr,
            SourceSpan::new(12.into(), 2)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::FatArrow,
            SourceSpan::new(15.into(), 2)
        ))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::LShift, SourceSpan::new(18.into(), 2)))
    );

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::RShift, SourceSpan::new(21.into(), 2)))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn unterminated_block_comment() {
    const SOURCE: &str = "/*";

    let mut tokens = tokenize(SOURCE);

    assert!(matches!(
        tokens.next(),
        Some(Token {
            kind: TokenKind::LexError,
            ..
        })
    ));
    assert_eq!(tokens.next(), None);
}

#[test]
fn unterminated_block_comment_with_body() {
    const SOURCE: &str = "/* abc with * and / chars but no close";

    let mut tokens = tokenize(SOURCE);

    assert!(matches!(
        tokens.next(),
        Some(Token {
            kind: TokenKind::LexError,
            ..
        })
    ));
    assert_eq!(tokens.next(), None);
}

#[test]
fn unterminated_block_comment_then_code() {
    const SOURCE: &str = "/* unterminated\nlet a = 1;";

    let mut tokens = tokenize(SOURCE);

    assert!(matches!(
        tokens.next(),
        Some(Token {
            kind: TokenKind::LexError,
            ..
        })
    ));
    assert_eq!(tokens.next(), None);
}
