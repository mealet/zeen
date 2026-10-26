#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FormatChunk {
    Literal(String),
    Arg(FormatSpec),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FormatSpec {
    Display,
    Debug,
    Hex,
    Oct,
    Bin,
    Float { precision: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FormatParseError {
    UnclosedBrace { offset: usize },
    UnknownSpecifier { spec: String, offset: usize },
    InvalidPrecision { raw: String, offset: usize },
}

impl FormatParseError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::UnclosedBrace { .. } => "unclosed '{' in format string".to_string(),
            Self::UnknownSpecifier { spec, .. } => format!("unknown format specifier: '{spec}'"),
            Self::InvalidPrecision { raw, .. } => {
                format!("invalid precision '{raw}' in: `{{:.{raw}}}`")
            }
        }
    }
}

pub(crate) fn parse_format_string(input: &str) -> Result<Vec<FormatChunk>, FormatParseError> {
    let mut chunks = Vec::new();
    let mut literal = String::new();
    let mut chars = input.char_indices().peekable();

    while let Some((idx, chr)) = chars.next() {
        match chr {
            '{' => {
                if chars.peek().map(|(_, chr)| *chr) == Some('{') {
                    chars.next();
                    literal.push('{');
                    continue;
                }

                if !literal.is_empty() {
                    chunks.push(FormatChunk::Literal(std::mem::take(&mut literal)))
                }

                let mut spec_raw = String::new();
                let mut closed = false;

                for (_, chr) in chars.by_ref() {
                    if chr == '}' {
                        closed = true;
                        break;
                    }
                    spec_raw.push(chr);
                }

                if !closed {
                    return Err(FormatParseError::UnclosedBrace { offset: idx });
                }

                let spec = parse_spec(&spec_raw, idx)?;
                chunks.push(FormatChunk::Arg(spec));
            }

            '}' => {
                if chars.peek().map(|(_, c)| *c) == Some('}') {
                    chars.next();
                    literal.push('}');
                    continue;
                }

                literal.push('}');
            }

            _ => literal.push(chr),
        }
    }

    if !literal.is_empty() {
        chunks.push(FormatChunk::Literal(literal));
    }

    Ok(chunks)
}

fn parse_spec(inner: &str, offset: usize) -> Result<FormatSpec, FormatParseError> {
    match inner {
        "" => Ok(FormatSpec::Display),
        ":?" => Ok(FormatSpec::Debug),
        "hex" => Ok(FormatSpec::Hex),
        "oct" => Ok(FormatSpec::Oct),
        "bin" => Ok(FormatSpec::Bin),
        s if s.starts_with(":.") => {
            let digits = &s[2..];

            match digits.parse::<u32>() {
                Ok(n) => Ok(FormatSpec::Float { precision: n }),
                Err(_) => Err(FormatParseError::InvalidPrecision {
                    raw: digits.to_string(),
                    offset,
                }),
            }
        }
        other => Err(FormatParseError::UnknownSpecifier {
            spec: other.to_string(),
            offset,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_display_and_literals() {
        assert_eq!(
            parse_format_string("hello {}!").unwrap(),
            vec![
                FormatChunk::Literal("hello ".into()),
                FormatChunk::Arg(FormatSpec::Display),
                FormatChunk::Literal("!".into()),
            ]
        );
    }

    #[test]
    fn parses_debug() {
        assert_eq!(
            parse_format_string("{:?}").unwrap(),
            vec![FormatChunk::Arg(FormatSpec::Debug)]
        );
    }

    #[test]
    fn parses_integer_specs() {
        let chunks = parse_format_string("{hex} {oct} {bin}").unwrap();

        assert_eq!(
            chunks,
            vec![
                FormatChunk::Arg(FormatSpec::Hex),
                FormatChunk::Literal(" ".into()),
                FormatChunk::Arg(FormatSpec::Oct),
                FormatChunk::Literal(" ".into()),
                FormatChunk::Arg(FormatSpec::Bin),
            ]
        );
    }

    #[test]
    fn parses_float_precision() {
        assert_eq!(
            parse_format_string("{:.3}").unwrap(),
            vec![FormatChunk::Arg(FormatSpec::Float { precision: 3 })]
        );
    }

    #[test]
    fn parses_escaped_braces() {
        assert_eq!(
            parse_format_string("{{}}").unwrap(),
            vec![FormatChunk::Literal("{}".into())]
        );
    }

    #[test]
    fn rejects_unclosed_brace() {
        assert_eq!(
            parse_format_string("{"),
            Err(FormatParseError::UnclosedBrace { offset: 0 })
        );
    }

    #[test]
    fn rejects_unknown_specifier() {
        assert_eq!(
            parse_format_string("{:q}"),
            Err(FormatParseError::UnknownSpecifier {
                spec: ":q".into(),
                offset: 0,
            })
        );
    }

    #[test]
    fn rejects_invalid_precision() {
        assert_eq!(
            parse_format_string("{:.x}"),
            Err(FormatParseError::InvalidPrecision {
                raw: "x".into(),
                offset: 0,
            })
        );
    }
}
