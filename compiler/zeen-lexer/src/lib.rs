#![allow(unused)]

use std::str::Chars;

pub use token::{Token, TokenKind};

mod tests;
pub mod token;

pub fn tokenize(input: &str) -> impl Iterator<Item = Token> {
    let mut tokenizer = Tokenizer::new(input);

    std::iter::from_fn(move || {
        let token = tokenizer.advance_token();
        if token.kind != TokenKind::Eof {
            Some(token)
        } else {
            None
        }
    })
}

const EOF_CHAR: char = '\0';

pub struct Tokenizer<'inp> {
    src: &'inp str,

    chars: Chars<'inp>,
    prev: char,

    length: usize,
    remaining: usize,
}

impl<'inp> Tokenizer<'inp> {
    pub fn new(input: &'inp str) -> Self {
        Self {
            src: input,

            chars: input.chars(),
            prev: EOF_CHAR,

            length: input.len(),
            remaining: input.len(),
        }
    }

    pub fn as_str(&self) -> &'inp str {
        self.chars.as_str()
    }

    pub fn first(&self) -> char {
        self.chars.clone().next().unwrap_or(EOF_CHAR)
    }

    pub fn second(&self) -> char {
        let mut iter = self.chars.clone();
        iter.next();
        iter.next().unwrap_or(EOF_CHAR)
    }

    pub fn is_eof(&self) -> bool {
        self.chars.as_str().is_empty()
    }

    // -> Positioning

    fn pos_start(&self) -> usize {
        self.length - self.remaining
    }

    fn pos_len(&self) -> usize {
        self.remaining - self.chars.as_str().len()
    }

    fn reset_pos(&mut self) {
        self.remaining = self.chars.as_str().len();
    }

    // Helpers

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.prev = c;
        Some(c)
    }

    fn bump_n(&mut self, n: usize) {
        self.chars = self.as_str()[n..].chars();
    }

    fn eat_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while predicate(self.first()) && !self.is_eof() {
            let _ = self.bump();
        }
    }

    fn skip_whitespace(&mut self) {
        while (!self.is_eof()) {
            let chr = self.first();

            if (chr.is_whitespace()) {
                if self.bump().is_none() {
                    break;
                }
            } else if chr == '/' {
                match self.second() {
                    // line comment
                    '/' => {
                        while !self.is_eof() {
                            if self.bump() == Some('\n') {
                                break;
                            }
                        }
                    }

                    // block comment
                    '*' => {
                        while !self.is_eof() && !(self.first() == '*' && self.second() == '/') {
                            if self.bump() == None {
                                break;
                            }
                        }

                        self.bump_n(2);
                    }

                    _ => break,
                }
            } else {
                break;
            }
        }

        self.reset_pos();
    }

    // Tokenizers

    pub fn advance_token(&mut self) -> Token {
        self.skip_whitespace();

        let Some(first_char) = self.bump() else {
            return Token::new(TokenKind::Eof, (0, 0).into());
        };

        let token_kind = match first_char {
            // byte char literal
            'b' => self.byte_literal(),

            chr if is_ident_start(chr) => {
                let mut kind = self.ident();

                let pos_start = self.pos_start();
                let pos_len = self.pos_len();

                let slice = &self.src[pos_start..pos_start + pos_len];

                if let Some(compiler_type) = token::CompilerType::from_str(slice) {
                    kind = TokenKind::Type(compiler_type);
                }

                if let Some(compiler_keyword) = token::CompilerKeyword::from_str(slice) {
                    kind = TokenKind::Keyword(compiler_keyword);
                }

                if slice == "_" {
                    kind = TokenKind::Underscore;
                }

                kind
            }

            chr @ '0'..='9' => {
                let literal_kind = self.number(chr);
                TokenKind::Literal { kind: literal_kind }
            }

            '&' => {
                if matches!(self.first(), ' ' | '\0') {
                    TokenKind::Ampersand
                } else if self.first() == '&' {
                    let _ = self.bump();
                    TokenKind::BooleanAnd
                } else {
                    TokenKind::Ref
                }
            }

            '|' => {
                if self.first() == '|' {
                    let _ = self.bump();
                    TokenKind::BooleanOr
                } else {
                    TokenKind::Pipe
                }
            }

            ';' => TokenKind::Semicolon,
            ':' => TokenKind::Colon,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            '~' => TokenKind::Tilde,
            '?' => TokenKind::Question,
            '=' => TokenKind::Eq,
            '!' => TokenKind::Bang,
            '/' => TokenKind::Slash,

            '<' => TokenKind::Lt,
            '>' => TokenKind::Gt,

            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '%' => TokenKind::Percent,
            '^' => TokenKind::Caret,

            '(' => TokenKind::OpenParen,
            ')' => TokenKind::CloseParen,
            '[' => TokenKind::OpenBracket,
            ']' => TokenKind::CloseBracket,
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,

            _ => TokenKind::Unknown,
        };

        let token = Token::new(
            token_kind,
            miette::SourceSpan::new(self.pos_start().into(), self.pos_len()),
        );
        self.reset_pos();
        token
    }

    // ---------------

    fn ident(&mut self) -> TokenKind {
        self.eat_while(is_ident_continue);
        TokenKind::Ident
    }

    fn number(&mut self, first_digit: char) -> token::LiteralKind {
        let mut base = token::IntBase::Decimal;

        if first_digit == '0' {
            match self.first() {
                'b' => {
                    base = token::IntBase::Binary;

                    let _ = self.bump();
                    let _ = self.eat_decimal_digits();

                    return token::LiteralKind::Int { base };
                }

                'o' => {
                    base = token::IntBase::Octal;

                    let _ = self.bump();
                    let _ = self.eat_decimal_digits();

                    return token::LiteralKind::Int { base };
                }

                'x' => {
                    base = token::IntBase::Hexadecimal;

                    let _ = self.bump();
                    let _ = self.eat_hexadecimal_digits();

                    return token::LiteralKind::Int { base };
                }

                '0'..='9' | '_' => {
                    self.eat_decimal_digits();
                }

                '.' => {}

                _ => return token::LiteralKind::Int { base },
            }
        } else {
            self.eat_decimal_digits();
        }

        match self.first() {
            '.' if self.second() != '.' && !is_ident_start(self.second()) => {
                let _ = self.bump();

                if self.first().is_ascii_digit() {
                    self.eat_decimal_digits();
                }

                token::LiteralKind::Float
            }

            _ => token::LiteralKind::Int { base },
        }
    }

    fn eat_decimal_digits(&mut self) {
        loop {
            if matches!(self.first(), '_' | '0'..='9') {
                let _ = self.bump();
                continue;
            }

            break;
        }
    }

    fn eat_hexadecimal_digits(&mut self) {
        loop {
            if matches!(
                self.first(),
                '_' | '0'..='9' | 'a'..='f' | 'A'..='F'
            ) {
                let _ = self.bump();
                continue;
            }

            break;
        }
    }

    fn byte_literal(&mut self) -> TokenKind {
        match self.first() {
            '\'' => todo!(),
            chr if is_ident_continue(chr) => self.ident(),
            _ => TokenKind::Unknown,
        }
    }
}

fn is_ident_start(chr: char) -> bool {
    chr == '_' || chr.is_alphabetic()
}

fn is_ident_continue(chr: char) -> bool {
    chr == '_' || chr.is_alphanumeric()
}
