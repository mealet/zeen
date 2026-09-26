use tower_lsp_server::ls_types::{SemanticToken, SemanticTokenType, SemanticTokensLegend};
use zeen_lexer::TokenKind;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenType {
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Macro,
    Decorator,
    Variable,
    Operator,
}

struct RawToken {
    line: u32,
    start: u32,
    length: u32,
    ty: TokenType,
}

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        // Order must follow `TokenType` enum order (if you don't want to get fucked up)
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::MACRO,
            SemanticTokenType::DECORATOR,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: Vec::new(),
    }
}

pub fn tokens_for(text: &str) -> Vec<SemanticToken> {
    encode(collect(text))
}

pub fn tokens_in_range(text: &str, start_line: u32, end_line: u32) -> Vec<SemanticToken> {
    encode(
        collect(text)
            .into_iter()
            .filter(|item| item.line >= start_line && item.line <= end_line)
            .collect(),
    )
}

fn collect(text: &str) -> Vec<RawToken> {
    let starts = line_starts(text);
    let mut items: Vec<RawToken> = Vec::new();

    for token in zeen_lexer::tokenize_with_comments(text) {
        let Some(token_type) = token_kind_index(token.kind) else {
            continue;
        };

        let start = token.span.offset().min(text.len());
        let end = start.saturating_add(token.span.len()).min(text.len());

        if start >= end {
            continue;
        }

        let (start_line, _) = offset_to_line_col(&starts, start);
        let (end_line, _) = offset_to_line_col(&starts, end.saturating_sub(1));

        let mut line = start_line;

        while line <= end_line {
            let line_end = line_end_offset(text, &starts, line);
            let seg_start = if line == start_line {
                start
            } else {
                starts[line as usize]
            };

            let seg_end = if line == end_line { end } else { line_end };

            if seg_start < seg_end {
                let col = (seg_start - starts[line as usize]) as u32;
                items.push(RawToken {
                    line,
                    start: col,
                    length: (seg_end - seg_start) as u32,
                    ty: token_type,
                });
            }

            line += 1;
        }
    }

    items
}

fn encode(items: Vec<RawToken>) -> Vec<SemanticToken> {
    let mut out = Vec::with_capacity(items.len());
    let mut prev_line = 0;
    let mut prev_start = 0;

    for (position, item) in items.into_iter().enumerate() {
        let delta_line = item.line - prev_line;
        let delta_start = if position == 0 || delta_line != 0 {
            item.start
        } else {
            item.start - prev_start
        };

        prev_line = item.line;
        prev_start = item.start;

        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: item.length,
            token_type: item.ty as u32,
            token_modifiers_bitset: 0,
        });
    }

    out
}

fn token_kind_index(kind: TokenKind) -> Option<TokenType> {
    match kind {
        TokenKind::Keyword(_) => Some(TokenType::Keyword),
        TokenKind::Type(_) => Some(TokenType::Type),
        TokenKind::Literal { kind } => match kind {
            zeen_lexer::token::LiteralKind::Int { .. } | zeen_lexer::token::LiteralKind::Float => {
                Some(TokenType::Number)
            }

            _ => Some(TokenType::String),
        },
        TokenKind::Ident => Some(TokenType::Variable),
        TokenKind::MacroIdent => Some(TokenType::Macro),

        TokenKind::PreprocessorIdent
        | TokenKind::PreprocessorVar
        | TokenKind::PreprocessorDebug
        | TokenKind::PreprocessorRelease => Some(TokenType::Decorator),

        TokenKind::Comment => Some(TokenType::Comment),
        TokenKind::Unknown | TokenKind::LexError | TokenKind::Eof => None,

        _ => Some(TokenType::Operator),
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];

    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }

    starts
}

fn offset_to_line_col(line_starts: &[usize], offset: usize) -> (u32, u32) {
    let mut line = 0;

    for (index, start) in line_starts.iter().enumerate() {
        if *start <= offset {
            line = index;
        } else {
            break;
        }
    }

    (line as u32, (offset - line_starts[line]) as u32)
}

fn line_end_offset(text: &str, line_starts: &[usize], line: u32) -> usize {
    let next = line as usize + 1;

    if next < line_starts.len() {
        line_starts[next] - 1
    } else {
        text.len()
    }
}
