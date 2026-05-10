use miette::SourceSpan;

#[derive(Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: SourceSpan,
}

impl Token {
    pub fn new(kind: TokenKind, span: SourceSpan) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, PartialEq)]
pub enum TokenKind {
    Ident,   // abcd
    Keyword, // `if`, `defer` and etc...
    Ref,     // &expr

    Literal { kind: LiteralKind },

    Underscore, // _

    Semicolon, // ;
    Colon,     // :
    Comma,     // ,
    Dot,       // .
    Tilde,     // ~
    Question,  // ?
    Eq,        // =
    Bang,      // !

    Lt,         // <
    Gt,         // >
    Leq,        // <=
    Geq,        // >=
    BooleanEq,  // ==
    BooleanAnd, // &&
    BooleanOr,  // ||

    LShift, // <<
    RShift, // >>

    Pipe,      // |
    Ampersand, // &
    Caret,     // ^

    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %

    OpenParen,  // (
    CloseParen, // )

    OpenBrace,  // {
    CloseBrace, // }

    OpenBracket,  // [
    CloseBracket, // ]

    Unknown,
    Eof,
}

#[derive(Debug, PartialEq)]
pub enum LiteralKind {
    Int { base: IntBase },
    Float,
    Char,
    ByteChar,
    Str,
    RawStr,
}

#[derive(Debug, PartialEq)]
pub enum IntBase {
    Binary = 2,
    Octal = 8,
    Decimal = 10,
    Hexadecimal = 16,
}
