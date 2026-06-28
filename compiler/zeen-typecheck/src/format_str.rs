/// Parsed format string chunk
#[derive(Debug, Clone, PartialEq)]
pub enum FormatChunk {
    /// Text segment without format
    Literal(String),
    /// Argument being formatted
    Arg(FormatSpec),
}

/// Formatting argument specifier
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatSpec {
    /// `{}` - Basic display. Argument must implement `Display` interface.
    Display,
    /// `{:?}` - Debug display. Argument must implement `Debug` interface.
    Debug,
    /// `{hex}` - Hexadecimal format. Argument must be integer.
    Hex,
    /// `{oct}` - Octal format. Argument must be integer.
    Oct,
    /// `{bin}` - Binary format. Argument must be integer.
    Bin,
    /// `{:.N}` - Float with specified N numbers after dot. Argument must be float.
    Float { precision: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormatParseError {
    UnclosedBrace { offset: usize },
    UnknownSpecifier { spec: String, offset: usize },
    InvalidPrecision { raw: String, offset: usize },
}

/// Parses format string into vec of `FormatChunk`s
///
/// Possible format specifiers:
/// * `{}` - Display
/// * `{:?}` - Debug
/// * `{hex} - Hex (integer)
/// * `{oct} - Oct (integer)
/// * `{bin} - Bin (integer)
/// * `{:.N} - Float with N decimal places
pub fn parse_format_string(input: &str) -> Result<Vec<FormatChunk>, FormatParseError> {
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
        // `{}`
        "" => Ok(FormatSpec::Display),

        // `{:?}`
        ":?" => Ok(FormatSpec::Debug),

        // `{hex}`, `{oct}`, `{bin}`
        "hex" => Ok(FormatSpec::Hex),
        "oct" => Ok(FormatSpec::Oct),
        "bin" => Ok(FormatSpec::Bin),

        // `{:.N}`
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

pub fn arg_specs(chunks: &[FormatChunk]) -> Vec<FormatSpec> {
    chunks
        .iter()
        .filter_map(|c| match c {
            FormatChunk::Arg(spec) => Some(*spec),
            FormatChunk::Literal(_) => None,
        })
        .collect()
}

// WARNING: Don't blame me, but these tests are written by AI, just to save time, I'm so sorry!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display() {
        let chunks = parse_format_string("hello {}!").unwrap();
        assert_eq!(
            chunks,
            vec![
                FormatChunk::Literal("hello ".into()),
                FormatChunk::Arg(FormatSpec::Display),
                FormatChunk::Literal("!".into()),
            ]
        );
    }

    #[test]
    fn test_debug() {
        let chunks = parse_format_string("{:?}").unwrap();
        assert_eq!(chunks, vec![FormatChunk::Arg(FormatSpec::Debug)]);
    }

    #[test]
    fn test_hex_oct_bin() {
        let chunks = parse_format_string("{hex} {oct} {bin}").unwrap();
        assert_eq!(
            arg_specs(&chunks),
            vec![FormatSpec::Hex, FormatSpec::Oct, FormatSpec::Bin]
        );
    }

    #[test]
    fn test_float_precision() {
        let chunks = parse_format_string("{:.3}").unwrap();
        assert_eq!(
            chunks,
            vec![FormatChunk::Arg(FormatSpec::Float { precision: 3 })]
        );
    }

    #[test]
    fn test_escape_braces() {
        let chunks = parse_format_string("{{}}").unwrap();
        assert_eq!(
            chunks,
            vec![
                FormatChunk::Literal("{".into()),
                FormatChunk::Literal("}".into()),
            ]
        );
    }

    #[test]
    fn test_unclosed() {
        assert!(matches!(
            parse_format_string("{"),
            Err(FormatParseError::UnclosedBrace { .. })
        ));
    }

    #[test]
    fn test_unknown_spec() {
        assert!(matches!(
            parse_format_string("{:q}"),
            Err(FormatParseError::UnknownSpecifier { .. })
        ));
    }
}
