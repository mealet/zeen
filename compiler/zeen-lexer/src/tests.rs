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
