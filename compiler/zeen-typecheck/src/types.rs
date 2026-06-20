use std::collections::HashMap;

use lasso::Spur;
use zeen_ast::types::BuiltinType;
use zeen_resolve::DefId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Builtin(BuiltinType),

    IntLiteral,   // not typed int literal, defaults to `i32`
    FloatLiteral, // same, defaults to `f64`

    Struct {
        def_id: DefId,
        generic_args: Vec<TypeId>,
    },

    Interface {
        def_id: DefId,
    },

    Enum {
        def_id: DefId,
    },

    Pointer {
        inner: TypeId,
        is_const: bool,
    },

    Array {
        element: TypeId,
        len: Option<u64>,
    },

    Slice {
        element: TypeId,
    },

    Fn {
        params: Vec<TypeId>,
        ret: TypeId,
    },

    GenericParam(DefId),

    Void,
    Never,
    Error,
}

#[derive(Debug, Default)]
pub struct TypeInterner {
    types: Vec<Type>,
    lookup: HashMap<Type, TypeId>,
}

impl TypeInterner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(id) = self.lookup.get(&ty) {
            return *id;
        }

        let id = TypeId(self.types.len() as u32);
        self.types.push(ty.clone());
        self.lookup.insert(ty, id);
        id
    }

    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.0 as usize]
    }

    pub fn builtin(&mut self, b: BuiltinType) -> TypeId {
        self.intern(Type::Builtin(b))
    }

    pub fn void(&mut self) -> TypeId {
        self.intern(Type::Void)
    }

    pub fn error(&mut self) -> TypeId {
        self.intern(Type::Error)
    }

    pub fn int_literal(&mut self) -> TypeId {
        self.intern(Type::IntLiteral)
    }

    pub fn float_literal(&mut self) -> TypeId {
        self.intern(Type::FloatLiteral)
    }

    pub fn never(&mut self) -> TypeId {
        self.intern(Type::Never)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub is_copy: bool,
    pub needs_drop: bool,
}

impl Capabilities {
    pub const COPY: Capabilities = Capabilities {
        is_copy: true,
        needs_drop: false,
    };

    pub const MOVE_ONLY: Capabilities = Capabilities {
        is_copy: false,
        needs_drop: false,
    };
}

#[derive(Debug, Clone)]
pub struct StructTypeInfo {
    pub def_id: DefId,
    /// (name, field DefId, field TypeId)
    pub fields: Vec<(Spur, DefId, TypeId)>,
    pub capabalities: Capabilities,
}
