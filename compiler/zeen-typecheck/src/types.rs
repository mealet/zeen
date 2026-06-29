use std::{cell::RefCell, collections::HashMap, rc::Rc};

use lasso::Spur;
use zeen_ast::types::BuiltinType;
use zeen_resolve::{DefId, DefInfo};

use crate::{DEFAULT_FLOAT_LITERAL, DEFAULT_INT_LITERAL};

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

impl Type {
    pub fn to_display(
        &self,
        interner: Rc<RefCell<lasso::Rodeo>>,
        type_interner: &TypeInterner,
        resolution_result: &zeen_resolve::ResolutionResult,
    ) -> String {
        match self {
            Type::Builtin(b) => b.to_string(),
            Type::IntLiteral => DEFAULT_INT_LITERAL.to_string(),
            Type::FloatLiteral => DEFAULT_FLOAT_LITERAL.to_string(),

            Type::Struct { def_id, .. }
            | Type::Interface { def_id }
            | Type::Enum { def_id }
            | Type::GenericParam(def_id) => resolution_result
                .defs
                .get(def_id)
                .map(|info| interner.borrow().resolve(&info.name).to_string())
                .unwrap_or("undefined".to_string()),

            Type::Pointer { inner, is_const } => format!(
                "*{}{}",
                if *is_const { "const " } else { "" },
                type_interner.display_type(*inner, interner, resolution_result)
            ),

            Type::Array { element, len } => format!(
                "[{}]{}",
                len.map(|val| val.to_string()).unwrap_or_default(),
                type_interner.display_type(*element, interner, resolution_result)
            ),

            Type::Slice { element } => format!(
                "[]{}",
                type_interner.display_type(*element, interner, resolution_result)
            ),

            Type::Fn { params, ret } => {
                let string_params = params
                    .iter()
                    .map(|param| {
                        type_interner.display_type(*param, Rc::clone(&interner), resolution_result)
                    })
                    .collect::<Vec<String>>();

                let string_ret = type_interner.display_type(*ret, interner, resolution_result);

                format!("fn({}) {}", string_params.join(", "), string_ret)
            }

            Type::Void => "void".into(),
            Type::Never => "never".into(),
            Type::Error => "error".into(),
        }
    }
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

    pub fn display_type(
        &self,
        id: TypeId,
        interner: Rc<RefCell<lasso::Rodeo>>,
        resolution_result: &zeen_resolve::ResolutionResult,
    ) -> String {
        let ty = self.get(id).clone();
        ty.to_display(interner, self, resolution_result)
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

#[derive(Debug, Clone, Default)]
#[allow(non_snake_case)]
pub struct WellKnownInterfaces {
    pub Display: Option<DefId>,
    pub Debug: Option<DefId>,

    pub Copy: Option<DefId>,
    pub Clone: Option<DefId>,
    pub Drop: Option<DefId>,

    pub Add: Option<DefId>,
    pub Sub: Option<DefId>,
    pub Mul: Option<DefId>,
    pub Div: Option<DefId>,

    pub Neg: Option<DefId>,
    pub Not: Option<DefId>,
    pub Cmp: Option<DefId>,

    pub Deref: Option<DefId>,
    pub DerefAssign: Option<DefId>,
    pub Slice: Option<DefId>,
    pub SliceAssign: Option<DefId>,
}

impl WellKnownInterfaces {
    pub fn resolve(defs: &HashMap<DefId, DefInfo>, interner: &lasso::Rodeo) -> Self {
        use zeen_resolve::DefKind;

        let mut out = WellKnownInterfaces::default();

        for (def_id, info) in defs {
            if !matches!(info.kind, DefKind::Interface) {
                continue;
            }

            match interner.resolve(&info.name) {
                "Display" => out.Display = Some(*def_id),
                "Debug" => out.Debug = Some(*def_id),

                "Copy" => out.Copy = Some(*def_id),
                "Drop" => out.Drop = Some(*def_id),

                "Add" => out.Add = Some(*def_id),
                "Sub" => out.Sub = Some(*def_id),
                "Mul" => out.Mul = Some(*def_id),
                "Div" => out.Div = Some(*def_id),

                "Neg" => out.Neg = Some(*def_id),
                "Not" => out.Not = Some(*def_id),

                "Deref" => out.Deref = Some(*def_id),
                "DerefAssign" => out.DerefAssign = Some(*def_id),
                "Slice" => out.Slice = Some(*def_id),
                "SliceAssign" => out.SliceAssign = Some(*def_id),

                _ => {}
            }
        }

        out
    }

    pub fn is_well_known(name: &str) -> bool {
        matches!(
            name,
            "Display"
                | "Debug"
                | "Copy"
                | "Drop"
                | "Add"
                | "Sub"
                | "Mul"
                | "Div"
                | "Neg"
                | "Not"
                | "Deref"
                | "DerefAssign"
                | "Slice"
                | "SliceAssign"
        )
    }
}
