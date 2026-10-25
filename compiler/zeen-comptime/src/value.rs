use std::path::PathBuf;
use std::rc::Rc;

use zeen_ast::{Declaration, TypeExpr};

#[derive(Clone, Debug, PartialEq)]
pub struct StructValue<'arena> {
    pub decl: &'arena Declaration<'arena>,
    pub module: PathBuf,
    pub name: Rc<str>,
    pub fields: Vec<(Rc<str>, Value<'arena>)>,
}

impl<'arena> StructValue<'arena> {
    pub fn get(&self, name: &str) -> Option<&Value<'arena>> {
        self.fields
            .iter()
            .find(|(field, _)| field.as_ref() == name)
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
    Struct(Rc<StructValue<'arena>>),

    Type(&'arena TypeExpr<'arena>),
}

impl Value<'_> {
    pub fn type_name(&self) -> String {
        match self {
            Self::Int(_) => "comptime_int".to_string(),
            Self::Float(_) => "comptime_float".to_string(),
            Self::Bool(_) => "comptime_bool".to_string(),
            Self::Char(_) => "comptime_char".to_string(),
            Self::Str(_) => "comptime_str".to_string(),
            Self::Array(_) => "comptime_array".to_string(),
            Self::Struct(value) => value.name.to_string(),
            Self::Type(_) => "zeen_type".to_string(),
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
            Self::Struct(value) => {
                let mut s = String::from(value.name.as_ref());
                s.push_str(" { ");
                for (i, (name, field)) in value.fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(name);
                    s.push_str(": ");
                    s.push_str(&field.display());
                }
                s.push_str(" }");
                s
            }
            Self::Type(_) => "type".to_string(),
        }
    }
}
