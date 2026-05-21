use miette::SourceSpan;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: SourceSpan,
}

impl Token {
    pub fn new(kind: TokenKind, span: SourceSpan) -> Self {
        Self { kind, span }
    }

    pub fn merge_span(&self, other: SourceSpan) -> SourceSpan {
        let start = self.span.offset().min(other.offset());
        let end = (self.span.offset() + self.span.len()).max(other.offset() + other.len());

        SourceSpan::new(start.into(), end - start)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum TokenKind {
    Ident,      // abcd
    MacroIdent, // print!
    Ref,        // &expr

    Keyword(CompilerKeyword),
    Type(CompilerType),
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
    BooleanNe,  // !=
    BooleanAnd, // &&
    BooleanOr,  // ||

    LShift, // <<
    RShift, // >>

    FatArrow, // =>

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

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LiteralKind {
    Int { base: IntBase },
    Float,
    Char { terminated: bool, empty: bool },
    ByteChar { terminated: bool, empty: bool },
    Str { terminated: bool },
    RawStr { terminated: bool },
    InvalidRawStr,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum IntBase {
    Binary = 2,
    Octal = 8,
    Decimal = 10,
    Hexadecimal = 16,
}

#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Clone)]
pub enum CompilerType {
    // signed integers
    i8,
    i16,
    i32,
    i64,
    isize,

    // unsigned integers
    u8,
    u16,
    u32,
    u64,
    usize,

    // float types
    f32,
    f64,

    // others
    bool,
    char,
    void,
}

impl CompilerType {
    pub fn try_str(str: impl AsRef<str>) -> Option<Self> {
        match str.as_ref() {
            "i8" => Some(CompilerType::i8),
            "i16" => Some(CompilerType::i16),
            "i32" => Some(CompilerType::i32),
            "i64" => Some(CompilerType::i64),
            "isize" => Some(CompilerType::isize),

            "u8" => Some(CompilerType::u8),
            "u16" => Some(CompilerType::u16),
            "u32" => Some(CompilerType::u32),
            "u64" => Some(CompilerType::u64),
            "usize" => Some(CompilerType::usize),

            "f32" => Some(CompilerType::f32),
            "f64" => Some(CompilerType::f64),

            "bool" => Some(CompilerType::bool),
            "char" => Some(CompilerType::char),
            "void" => Some(CompilerType::void),

            _ => None,
        }
    }
}

impl std::fmt::Display for CompilerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum CompilerKeyword {
    If,
    Else,
    While,
    For,
    Break,

    Let,
    Const,
    Defer,
    Switch,
    Return,

    Pub,
    Fn,
    Extern,
    Include,
    Link,
    Import,
    Struct,
    Enum,

    Interface,
    Implement,
    Type,

    True,
    False,
    Null,

    SelfLower,
    SelfUpper,
}

impl CompilerKeyword {
    pub fn try_str(str: impl AsRef<str>) -> Option<Self> {
        match str.as_ref() {
            "if" => Some(CompilerKeyword::If),
            "else" => Some(CompilerKeyword::Else),
            "while" => Some(CompilerKeyword::While),
            "for" => Some(CompilerKeyword::For),
            "break" => Some(CompilerKeyword::Break),

            "let" => Some(CompilerKeyword::Let),
            "const" => Some(CompilerKeyword::Const),
            "defer" => Some(CompilerKeyword::Defer),
            "switch" => Some(CompilerKeyword::Switch),
            "return" => Some(CompilerKeyword::Return),

            "public" => Some(CompilerKeyword::Pub),
            "fn" => Some(CompilerKeyword::Fn),
            "extern" => Some(CompilerKeyword::Extern),
            "include" => Some(CompilerKeyword::Include),
            "link" => Some(CompilerKeyword::Link),
            "import" => Some(CompilerKeyword::Import),
            "struct" => Some(CompilerKeyword::Struct),
            "enum" => Some(CompilerKeyword::Enum),

            "interface" => Some(CompilerKeyword::Interface),
            "implement" => Some(CompilerKeyword::Implement),
            "type" => Some(CompilerKeyword::Type),

            "true" => Some(CompilerKeyword::True),
            "false" => Some(CompilerKeyword::False),
            "null" => Some(CompilerKeyword::Null),

            "self" => Some(CompilerKeyword::SelfLower),
            "Self" => Some(CompilerKeyword::SelfUpper),

            _ => None,
        }
    }
}
