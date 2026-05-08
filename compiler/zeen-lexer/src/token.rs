use zeen_driver::LocationSpan;

pub struct Token {
    pub kind: TokenKind,
    pub span: LocationSpan,
}

impl Token {
    pub fn new(kind: TokenKind, span: LocationSpan) -> Self {
        Self { kind, span }
    }
}

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

    Eof,
}

pub enum LiteralKind {
    Int { base: IntBase },
    Float,
    Char,
    Byte,
    Str,
}

pub enum IntBase {
    Binary = 2,
    Octal = 8,
    Decimal = 10,
    Hexadecimal = 16,
}
