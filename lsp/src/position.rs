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

    Position::new(line, (offset - line_start) as u32)
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

    line_start
        .saturating_add(position.character as usize)
        .min(line_end)
}

pub fn span_to_range(text: &str, offset: usize, len: usize) -> Range {
    let start = offset_to_position(text, offset);
    let end = offset_to_position(text, offset.saturating_add(len));

    Range::new(start, end)
}
