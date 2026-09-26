#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Function,
    Method,
    Type,
    Variable,
    Parameter,
    Property,
    EnumMember,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub offset: usize,
    pub len: usize,
    pub role: Role,
    pub target_offset: Option<usize>,
    pub target_len: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub occurrences: Vec<Occurrence>,
}

impl Analysis {
    pub fn at(&self, offset: usize) -> Option<&Occurrence> {
        let index = self
            .occurrences
            .partition_point(|item| item.offset <= offset)
            .checked_sub(1)?;

        let item = &self.occurrences[index];

        if offset < item.offset + item.len {
            Some(item)
        } else {
            None
        }
    }
}
