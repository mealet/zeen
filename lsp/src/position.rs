use tower_lsp_server::ls_types::{Position, Range};

pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());

    let mut line = 0;
    let mut line_start = 0;

    for (index, byte) in text.bytes().enumerate() {
        if index >= offset {
            break;
        }

        if byte == b'\n' {
            line += 1;
            line_start = index + 1;
        }
    }

    let mut character: u32 = 0;

    for (index, ch) in text[line_start..].char_indices() {
        if line_start + index >= offset {
            break;
        }

        character += ch.len_utf16() as u32;
    }

    Position::new(line, character)
}

pub fn position_to_offset(text: &str, position: Position) -> usize {
    let mut line = 0;
    let mut line_start = 0;
    let mut line_end = text.len();

    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            if line == position.line {
                line_end = index;

                break;
            }

            line += 1;
            line_start = index + 1;
        }
    }

    if line != position.line {
        return text.len();
    }

    let line = text.get(line_start..line_end).unwrap_or("");
    let mut taken: u32 = 0;
    let mut offset = line_start;

    for ch in line.chars() {
        if taken >= position.character {
            break;
        }

        taken += ch.len_utf16() as u32;
        offset += ch.len_utf8();
    }

    offset.min(line_end)
}

pub fn span_to_range(text: &str, offset: usize, len: usize) -> Range {
    let start = offset_to_position(text, offset);
    let end = offset_to_position(text, offset.saturating_add(len));

    Range::new(start, end)
}

pub fn line_text(text: &str, line: u32) -> &str {
    let mut current = 0;
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if current == line {
            let end = text[index..]
                .find('\n')
                .map(|position| index + position)
                .unwrap_or(text.len());
            return text.get(start..end).unwrap_or("");
        }
        if byte == b'\n' {
            current += 1;
            start = index + 1;
        }
    }
    if current == line {
        return text.get(start..).unwrap_or("");
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip() {
        let text = "fn main() {\n    let x = 1;\n}\n";
        let offset = text.find("x =").unwrap();
        let position = offset_to_position(text, offset);

        assert_eq!(position_to_offset(text, position), offset);
    }

    #[test]
    fn unicode_counts_utf16() {
        let text = "// привет\nlet x = 1;\n";
        let offset = text.find("x =").unwrap();
        let position = offset_to_position(text, offset);

        assert_eq!(position.line, 1);
        assert_eq!(position.character, "let ".chars().count() as u32);
        assert_eq!(position_to_offset(text, position), offset);
    }

    #[test]
    fn multibyte_roundtrip() {
        let text = "let s = \"жж\";\nlet x = s;\n";

        for (index, _) in text.char_indices() {
            let position = offset_to_position(text, index);
            let back = position_to_offset(text, position);

            assert!(back <= index);
            assert!(text.is_char_boundary(back));
        }
    }
}
