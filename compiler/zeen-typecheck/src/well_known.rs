use std::collections::HashMap;

use zeen_ast::types::BuiltinType;
use zeen_resolve::{DefId, WellKnownInterface};

#[derive(Debug, Clone, Default)]
pub struct WellKnownInterfacesMap {
    pub interfaces: HashMap<String, DefId>,
    pub reverse: HashMap<DefId, WellKnownInterface>,
}

impl WellKnownInterfacesMap {
    pub fn from_resolution(resolution: &zeen_resolve::ResolutionResult) -> Self {
        let mut interfaces = HashMap::new();
        let mut reverse = HashMap::new();

        for (wk, def_id) in &resolution.well_known {
            interfaces.insert(wk.name(), *def_id);
            reverse.insert(*def_id, *wk);
        }

        Self {
            interfaces,
            reverse,
        }
    }

    pub fn get(&self, name: &str) -> Option<DefId> {
        self.interfaces.get(name).copied()
    }

    pub fn get_wk(&self, wk: WellKnownInterface) -> Option<DefId> {
        self.interfaces.get(&wk.name()).copied()
    }
}

pub fn builtin_well_known_interfaces(b: BuiltinType) -> &'static [WellKnownInterface] {
    use BuiltinType::*;
    use WellKnownInterface::*;

    match b {
        i8 | i16 | i32 | i64 | isize => &[
            Display, Debug, Copy, Eq, Add, Sub, Mul,
            Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr,
            BitNot, Neg
        ],

        u8 | u16 | u32 | u64 | usize => &[
            Display, Debug, Copy, Eq, Add, Sub, Mul,
            Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr,
            BitNot
        ],

        f32 | f64 => &[
            Display, Debug, Copy, Eq, Add, Sub, Mul, Div, Neg
        ],

        bool => &[
            Display, Debug, Copy, Eq, Not
        ],

        char => &[
            Display, Debug, Copy, Eq,
        ],

        void => &[Copy],
    }
}

pub fn binary_op_to_well_known(op: zeen_ast::expressions::BinaryOp) -> Option<WellKnownInterface> {
    use zeen_ast::expressions::BinaryOp::*;
 
    match op {
        Add => Some(WellKnownInterface::Add),
        Sub => Some(WellKnownInterface::Sub),
        Mul => Some(WellKnownInterface::Mul),
        Div => Some(WellKnownInterface::Div),
        Mod => Some(WellKnownInterface::Mod),
        BitAnd => Some(WellKnownInterface::BitAnd),
        BitOr => Some(WellKnownInterface::BitOr),
        BitXor => Some(WellKnownInterface::BitXor),
        Shl => Some(WellKnownInterface::Shl),
        Shr => Some(WellKnownInterface::Shr),
        Eq | Ne => Some(WellKnownInterface::Eq),

        // not covered by well-known, appliable only to builtins
        Lt | Gt | Le | Ge | LogicalAnd | LogicalOr => None,
    }
}

pub fn unary_op_to_well_known(op: zeen_ast::expressions::UnaryOp) -> Option<WellKnownInterface> {
    use zeen_ast::expressions::UnaryOp::*;
 
    match op {
        Neg => Some(WellKnownInterface::Neg),
        Not => Some(WellKnownInterface::Not),
        BitNot => Some(WellKnownInterface::BitNot),
        Deref => Some(WellKnownInterface::Deref),
        AddrOf => None,
    }
}
