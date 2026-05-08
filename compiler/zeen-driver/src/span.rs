pub struct LocationSpan {
    start_line: usize,
    end_line: usize,

    start_column: usize,
    end_column: usize,
}

pub struct LineOffsets(Vec<usize>);

impl LocationSpan {
    pub fn single(line: usize, column: usize) -> Self {
        Self { start_line: line, start_column: column, end_line: line, end_column: column }
    }

    pub fn range(start_line: usize, start_column: usize, end_line: usize, end_column: usize) -> Self {
        Self { start_line, start_column, end_line, end_column }
    }

    pub fn merge(self, other: Self) -> Self {
        let start_before = self.start_line < other.start_line
            || (self.start_line == other.start_line && self.start_column <= other.start_column);

        let end_after = self.end_line > other.end_line
            || (self.end_line == other.end_line && self.end_column >= other.end_column);

        Self {
            start_line: if start_before { self.start_line } else { other.start_line },
            start_column: if start_before { self.start_column } else { other.start_column },
            end_line: if end_after { self.end_line } else { other.end_line },
            end_column: if end_after { self.end_column } else { other.end_column },
        }
    }

    pub fn to_miette_span(&self, offsets_table: &LineOffsets) -> miette::SourceSpan {
        let start = offsets_table.0[self.start_line - 1] + self.start_column - 1;
        let end = offsets_table.0[self.end_line - 1] + self.end_column - 1;

        (start, end - start).into()
    }
}

impl LineOffsets {
    pub fn build(src: &str) -> Self {
        let mut offsets = vec![0];
        let mut offset = 0;

        for ch in src.chars() {
            offset += ch.len_utf8();
            if ch == '\n' {
                offsets.push(offset);
            }
        }

        Self(offsets)
    }
}
