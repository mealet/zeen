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
