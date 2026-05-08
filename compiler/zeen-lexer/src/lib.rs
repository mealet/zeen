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
    chars: Chars<'inp>,
    prev: char,

    length: usize,
    remaining: usize,
}

impl<'inp> Tokenizer<'inp> {
    pub fn new(input: &'inp str) -> Self {
        Self {
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

    fn pos_len(&self) -> u32 {
        (self.remaining - self.chars.as_str().len()) as u32
    }

    fn reset_pos_len(&mut self) {
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
    }

    // Tokenizers

    pub fn advance_token(&mut self) -> Token {
        self.skip_whitespace();

        let Some(first_char) = self.bump() else {
            return Token::new(TokenKind::Eof, (0, 0).into());
        };

        todo!();
    }
}
