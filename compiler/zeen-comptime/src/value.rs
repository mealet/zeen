use std::rc::Rc;

use zeen_ast::TypeExpr;

#[derive(Clone, Debug, PartialEq)]
pub struct Record<'arena> {
    pub kind: &'static str,
    pub fields: Vec<(&'static str, Value<'arena>)>,
}

impl<'arena> Record<'arena> {
    pub fn get(&self, name: &str) -> Option<&Value<'arena>> {
        self.fields
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value<'arena> {
    Int(i128),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(Rc<str>),
    Array(Vec<Value<'arena>>),
    Record(Rc<Record<'arena>>),

    Type(&'arena TypeExpr<'arena>),
}

impl Value<'_> {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Int(_) => "comptime_int",
            Self::Float(_) => "comptime_float",
            Self::Bool(_) => "comptime_bool",
            Self::Char(_) => "comptime_char",
            Self::Str(_) => "comptime_str",
            Self::Array(_) => "comptime_array",
            Self::Record(record) => record.kind,
            Self::Type(_) => "zeen_type",
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Int(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Bool(v) => v.to_string(),
            Self::Char(v) => v.to_string(),
            Self::Str(v) => v.to_string(),
            Self::Array(items) => {
                let mut s = String::from("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&item.display());
                }
                s.push(']');
                s
            }
            Self::Record(record) => {
                let mut s = String::from(record.kind);
                s.push_str(" { ");
                for (i, (name, value)) in record.fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(name);
                    s.push_str(": ");
                    s.push_str(&value.display());
                }
                s.push_str(" }");
                s
            }
            Self::Type(_) => "type".to_string(),
        }
    }
}
