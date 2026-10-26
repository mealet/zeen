use zeen_driver::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

impl Layout {
    pub const fn new(size: u64, align: u64) -> Self {
        Self { size, align }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    I8,
    I16,
    I32,
    I64,
    Isize,
    F32,
    F64,
    Bool,
    Char,
    Void,
    Never,
    Pointer,
    Array(Box<Shape>, u64),
    Struct(Vec<Shape>),
    Enum(Vec<Option<Shape>>),
}

impl Shape {
    pub fn slice() -> Self {
        Self::Struct(vec![Self::Pointer, Self::Isize])
    }

    pub fn fat_fn() -> Self {
        Self::Struct(vec![Self::Pointer, Self::Pointer, Self::Pointer])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutEngine {
    pointer_size: u64,
    pointer_align: u64,
}

impl LayoutEngine {
    pub fn for_target(target: &Target) -> Self {
        let pointer_size = match target.arch.as_str() {
            "i686" | "x86" | "wasm32" => 4,
            _ => 8,
        };

        Self::with_pointer_size(pointer_size)
    }

    pub const fn with_pointer_size(pointer_size: u64) -> Self {
        Self {
            pointer_size,
            pointer_align: pointer_size,
        }
    }

    pub const fn pointer_size(&self) -> u64 {
        self.pointer_size
    }

    pub fn size_of(&self, shape: &Shape) -> u64 {
        self.layout(shape).size
    }

    pub fn align_of(&self, shape: &Shape) -> u64 {
        self.layout(shape).align
    }

    pub fn layout(&self, shape: &Shape) -> Layout {
        match shape {
            Shape::I8 | Shape::Bool | Shape::Char => Layout::new(1, 1),
            Shape::I16 => Layout::new(2, 2),
            Shape::I32 | Shape::F32 => Layout::new(4, 4),
            Shape::I64 | Shape::F64 => Layout::new(8, 8),
            Shape::Isize | Shape::Pointer => Layout::new(self.pointer_size, self.pointer_align),
            Shape::Void | Shape::Never => Layout::new(0, 1),
            Shape::Array(element, len) => {
                let element_layout = self.layout(element);
                Layout::new(element_layout.size * len, element_layout.align)
            }
            Shape::Struct(fields) => {
                let layouts: Vec<Layout> = fields.iter().map(|field| self.layout(field)).collect();
                self.struct_layout(&layouts)
            }
            Shape::Enum(variants) => self.enum_layout(variants),
        }
    }

    pub fn struct_layout(&self, fields: &[Layout]) -> Layout {
        if fields.is_empty() {
            return Layout::new(0, 1);
        }

        let mut size = 0;
        let mut align = 1;

        for field in fields {
            size = round_up(size, field.align) + field.size;
            align = align.max(field.align);
        }

        Layout::new(round_up(size, align), align)
    }

    pub fn enum_layout(&self, variants: &[Option<Shape>]) -> Layout {
        let payloads: Vec<Layout> = variants
            .iter()
            .flatten()
            .map(|shape| self.layout(shape))
            .collect();

        if payloads.is_empty() {
            return Layout::new(1, 1);
        }

        let union = if payloads.len() == 1 {
            let void_payload = matches!(
                variants.iter().flatten().next(),
                Some(Shape::Void | Shape::Never)
            );

            if void_payload {
                Layout::new(1, 1)
            } else {
                payloads[0]
            }
        } else {
            let size = payloads.iter().map(|layout| layout.size).max().unwrap_or(0);
            let align = payloads
                .iter()
                .map(|layout| layout.align)
                .max()
                .unwrap_or(1);
            self.union_layout(size, align)
        };

        self.struct_layout(&[Layout::new(1, 1), union])
    }

    fn union_layout(&self, size: u64, align: u64) -> Layout {
        if size == 0 {
            return Layout::new(1, 1);
        }

        let word_size = if align >= 8 {
            8
        } else if align >= 4 {
            4
        } else if align >= 2 {
            2
        } else {
            1
        };

        let rest = size % word_size;
        if rest == 0 {
            return Layout::new(size, word_size);
        }

        let words = Layout::new(size - rest, word_size);
        let tail = Layout::new(rest, 1);

        self.struct_layout(&[words, tail])
    }
}

fn round_up(value: u64, align: u64) -> u64 {
    if align == 0 {
        return value;
    }

    value.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(pointer_size: u64) -> LayoutEngine {
        LayoutEngine::with_pointer_size(pointer_size)
    }

    #[test]
    fn primitives_have_natural_layouts() {
        let engine = engine(8);

        assert_eq!(engine.layout(&Shape::I8), Layout::new(1, 1));
        assert_eq!(engine.layout(&Shape::I16), Layout::new(2, 2));
        assert_eq!(engine.layout(&Shape::I32), Layout::new(4, 4));
        assert_eq!(engine.layout(&Shape::I64), Layout::new(8, 8));
        assert_eq!(engine.layout(&Shape::F32), Layout::new(4, 4));
        assert_eq!(engine.layout(&Shape::F64), Layout::new(8, 8));
        assert_eq!(engine.layout(&Shape::Bool), Layout::new(1, 1));
        assert_eq!(engine.layout(&Shape::Char), Layout::new(1, 1));
        assert_eq!(engine.layout(&Shape::Void), Layout::new(0, 1));
    }

    #[test]
    fn pointers_follow_the_target_width() {
        assert_eq!(engine(8).layout(&Shape::Pointer), Layout::new(8, 8));
        assert_eq!(engine(8).layout(&Shape::Isize), Layout::new(8, 8));
        assert_eq!(engine(4).layout(&Shape::Pointer), Layout::new(4, 4));
        assert_eq!(engine(4).layout(&Shape::Isize), Layout::new(4, 4));
    }

    #[test]
    fn arrays_multiply_the_element_size() {
        let engine = engine(8);

        assert_eq!(
            engine.layout(&Shape::Array(Box::new(Shape::I32), 4)),
            Layout::new(16, 4)
        );

        let pointer_array = Shape::Array(Box::new(Shape::Pointer), 3);
        assert_eq!(engine.layout(&pointer_array), Layout::new(24, 8));
    }

    #[test]
    fn struct_fields_are_aligned_and_padded() {
        let engine = engine(8);

        let fields = vec![Shape::I8, Shape::I64, Shape::I8];
        assert_eq!(engine.layout(&Shape::Struct(fields)), Layout::new(24, 8));

        assert_eq!(engine.layout(&Shape::Struct(Vec::new())), Layout::new(0, 1));
    }

    #[test]
    fn slices_and_fat_fns_are_pointer_pairs() {
        let wide = engine(8);

        assert_eq!(wide.layout(&Shape::slice()), Layout::new(16, 8));
        assert_eq!(wide.layout(&Shape::fat_fn()), Layout::new(24, 8));

        let narrow = engine(4);
        assert_eq!(narrow.layout(&Shape::slice()), Layout::new(8, 4));
        assert_eq!(narrow.layout(&Shape::fat_fn()), Layout::new(12, 4));
    }

    #[test]
    fn empty_enum_is_a_tag() {
        let engine = engine(8);

        assert_eq!(
            engine.layout(&Shape::Enum(vec![None, None])),
            Layout::new(1, 1)
        );
        assert_eq!(engine.layout(&Shape::Enum(Vec::new())), Layout::new(1, 1));
    }

    #[test]
    fn enum_with_one_payload_wraps_it() {
        let engine = engine(8);

        assert_eq!(
            engine.layout(&Shape::Enum(vec![None, Some(Shape::I64)])),
            Layout::new(16, 8)
        );
        assert_eq!(
            engine.layout(&Shape::Enum(vec![None, Some(Shape::Void)])),
            Layout::new(2, 1)
        );
        assert_eq!(
            engine.layout(&Shape::Enum(vec![None, Some(Shape::Struct(Vec::new()))])),
            Layout::new(1, 1)
        );
    }

    #[test]
    fn enum_with_several_payloads_uses_a_union() {
        let engine = engine(8);

        assert_eq!(
            engine.layout(&Shape::Enum(vec![Some(Shape::I32), Some(Shape::I64)])),
            Layout::new(16, 8)
        );
        assert_eq!(
            engine.layout(&Shape::Enum(vec![Some(Shape::I8), Some(Shape::I16)])),
            Layout::new(4, 2)
        );
        assert_eq!(
            engine.layout(&Shape::Enum(vec![Some(Shape::Void), Some(Shape::I8)])),
            Layout::new(2, 1)
        );
    }

    #[test]
    fn nested_structs_keep_their_padding() {
        let engine = engine(8);

        let inner = Shape::Struct(vec![Shape::I8, Shape::I32]);
        assert_eq!(engine.size_of(&inner), 8);

        let outer = Shape::Struct(vec![Shape::I8, inner, Shape::I8]);
        assert_eq!(engine.layout(&outer), Layout::new(16, 4));
    }
}
