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
    const SOURCE: &str = "print!";

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
fn reference() {
    const SOURCE: &str = "&val";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(TokenKind::Ref, SourceSpan::new(0.into(), 4)))
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
    const SOURCE: &str = "'a'";

    let mut tokens = tokenize(SOURCE);

    assert_eq!(
        tokens.next(),
        Some(Token::new(
            TokenKind::Literal {
                kind: token::LiteralKind::Char
            },
            SourceSpan::new(0.into(), 3)
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
                kind: token::LiteralKind::Char
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
                kind: token::LiteralKind::Str
            },
            SourceSpan::new(0.into(), 6)
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
                kind: token::LiteralKind::RawStr
            },
            SourceSpan::new(0.into(), 10)
        ))
    );

    assert_eq!(tokens.next(), None);
}

#[test]
fn basic_symbols() {
    const SOURCE: &str = "_ ; : , . ~ ? = !";

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

    assert_eq!(tokens.next(), None);
}
