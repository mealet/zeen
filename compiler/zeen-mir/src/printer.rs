// WARNING: Code below is fully written by AI, be careful with it!
// WARNING: Used only for MIR debug purposes, doesn't affect on compilation pipeline and context.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

use lasso::Rodeo;
use zeen_resolve::{DefId, ResolutionResult};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId, TypeInterner};

use crate::{
    AggregateKind, BasicBlock, BlockId, CallTarget, ConstValue, LocalDecl, LocalId, LocalKind,
    MirFunction, MirFunctionId, MirProgram, MirStatement, Mutability, Operand, Place, PlaceElem,
    Rvalue, Terminator,
};

pub fn print_mir_program(
    program: &MirProgram,
    typecheck: &TypeCheckResult,
    resolution: &ResolutionResult,
    rodeo: &Rc<RefCell<Rodeo>>,
) -> String {
    let printer = MirPrinter {
        program,
        typecheck,
        resolution,
        rodeo,
    };
    printer.print_program()
}

struct MirPrinter<'a> {
    program: &'a MirProgram,
    typecheck: &'a TypeCheckResult,
    resolution: &'a ResolutionResult,
    rodeo: &'a Rc<RefCell<Rodeo>>,
}

impl<'a> MirPrinter<'a> {
    fn print_program(&self) -> String {
        let mut out = String::new();

        for ext_var in &self.program.extern_vars {
            let _ = writeln!(
                out,
                "extern let {}: {};",
                ext_var.symbol_name,
                self.display_type(ext_var.ty)
            );
        }
        if !self.program.extern_vars.is_empty() {
            out.push('\n');
        }

        for ext_fn in &self.program.extern_fns {
            let param_strs: Vec<String> = ext_fn
                .param_types
                .iter()
                .map(|&t| self.display_type(t))
                .collect();
            let variadic = if ext_fn.is_variadic { ", ..." } else { "" };
            let _ = writeln!(
                out,
                "extern fn {}({}{}) {};",
                ext_fn.symbol_name,
                param_strs.join(", "),
                variadic,
                self.display_type(ext_fn.ret_ty)
            );
        }
        if !self.program.extern_fns.is_empty() {
            out.push('\n');
        }

        let mut ids: Vec<&MirFunctionId> = self.program.functions.keys().collect();
        ids.sort_by_key(|id| id.0);

        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let func = &self.program.functions[id];
            self.print_function(&mut out, **id, func);
        }

        out
    }

    fn print_function(&self, out: &mut String, id: MirFunctionId, func: &MirFunction) {
        let name = self
            .program
            .function_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("fn#{}", id.0));

        if !func.mono_args.is_empty() {
            let arg_names: Vec<String> = func
                .mono_args
                .iter()
                .map(|&t| self.display_type(t))
                .collect();
            let _ = writeln!(
                out,
                "// monomorphized from {:?} with [{}]",
                func.source_def,
                arg_names.join(", ")
            );
        }

        let param_strs: Vec<String> = func
            .params
            .iter()
            .map(|&local| {
                let decl = func.local(local);
                format!("{}: {}", self.local_ref(local), self.display_type(decl.ty))
            })
            .collect();

        let ret_ty_str = self.display_type(func.ret_ty);

        let _ = writeln!(
            out,
            "fn {}({}) {} {{",
            name,
            param_strs.join(", "),
            ret_ty_str
        );

        for (idx, decl) in func.locals.iter().enumerate() {
            let local = LocalId(idx as u32);
            if matches!(decl.kind, LocalKind::Param) {
                continue;
            }
            self.print_local_decl(out, local, decl);
        }

        // out.push('\n');

        for (idx, block) in func.blocks.iter().enumerate() {
            let block_id = BlockId(idx as u32);
            self.print_block(out, block_id, block, func);
        }

        out.push_str("}\n");
    }

    fn print_local_decl(&self, out: &mut String, local: LocalId, decl: &LocalDecl) {
        let mutability = match decl.mutability {
            Mutability::Mut => "",
            Mutability::Const => "const ",
        };

        let comment = match decl.kind {
            LocalKind::UserVariable => decl
                .name
                .map(|n| self.resolve_spur(n))
                .unwrap_or_else(|| "user var".to_string()),
            LocalKind::Param => "param".to_string(),
            LocalKind::Temporary => "temp".to_string(),
            LocalKind::ReturnSlot => "return slot".to_string(),
        };

        let _ = writeln!(
            out,
            "    let {}{}: {}; // {}",
            mutability,
            self.local_ref(local),
            self.display_type(decl.ty),
            comment
        );
    }

    fn print_block(&self, out: &mut String, id: BlockId, block: &BasicBlock, func: &MirFunction) {
        let _ = writeln!(out, "\n    bb{}: {{", id.0);

        for stmt in &block.statements {
            self.print_statement(out, stmt, func);
        }

        self.print_terminator(out, &block.terminator, func);

        out.push_str("    }\n");
    }

    fn print_statement(&self, out: &mut String, stmt: &MirStatement, func: &MirFunction) {
        match stmt {
            MirStatement::Assign { place, rvalue } => {
                let _ = writeln!(
                    out,
                    "        {} = {};",
                    self.place_ref(place, func),
                    self.rvalue_ref(rvalue, func)
                );
            }
            MirStatement::Drop(place) => {
                let _ = writeln!(out, "        drop({});", self.place_ref(place, func));
            }
            MirStatement::StorageLive(local) => {
                let _ = writeln!(out, "        StorageLive({});", self.local_ref(*local));
            }
            MirStatement::StorageDead(local) => {
                let _ = writeln!(out, "        StorageDead({});", self.local_ref(*local));
            }
            MirStatement::Nop => {
                let _ = writeln!(out, "        nop;");
            }
        }
    }

    fn print_terminator(&self, out: &mut String, term: &Terminator, func: &MirFunction) {
        match term {
            Terminator::Goto(target) => {
                let _ = writeln!(out, "        goto -> bb{};", target.0);
            }

            Terminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let arms: Vec<String> = targets
                    .iter()
                    .map(|(v, b)| format!("{} -> bb{}", v, b.0))
                    .collect();
                let _ = writeln!(
                    out,
                    "        switchInt({}) -> [{}, otherwise: bb{}];",
                    self.operand_ref(discriminant, func),
                    arms.join(", "),
                    otherwise.0
                );
            }

            Terminator::Call {
                func: call_target,
                args,
                destination,
                target,
            } => {
                let arg_strs: Vec<String> =
                    args.iter().map(|a| self.operand_ref(a, func)).collect();
                let target_str = match target {
                    Some(b) => format!(" -> bb{}", b.0),
                    None => String::new(),
                };
                let _ = writeln!(
                    out,
                    "        {} = {}({}){};",
                    self.place_ref(destination, func),
                    self.call_target_ref(call_target),
                    arg_strs.join(", "),
                    target_str
                );
            }

            Terminator::MacroCall {
                kind,
                format_chunks,
                args,
                destination,
                target,
            } => {
                let arg_strs: Vec<String> =
                    args.iter().map(|a| self.operand_ref(a, func)).collect();
                let target_str = match target {
                    Some(b) => format!(" -> bb{}", b.0),
                    None => String::new(),
                };
                let fmt_note = if format_chunks.is_some() {
                    " [fmt]"
                } else {
                    ""
                };
                let _ = writeln!(
                    out,
                    "        {} = @{:?}{}({}){};",
                    self.place_ref(destination, func),
                    kind,
                    fmt_note,
                    arg_strs.join(", "),
                    target_str
                );
            }

            Terminator::Return(operand) => {
                let _ = writeln!(out, "        return {};", self.operand_ref(operand, func));
            }

            Terminator::Unreachable => {
                let _ = writeln!(out, "        unreachable;");
            }
        }
    }

    fn call_target_ref(&self, target: &CallTarget) -> String {
        match target {
            CallTarget::Direct(id) => self
                .program
                .function_names
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("fn#{}", id.0)),
            CallTarget::Indirect(operand) => {
                let _ = operand;
                "<indirect>".to_string()
            }
            CallTarget::Extern(idx) => {
                format!("extern \"{}\"", self.program.extern_fns[*idx].symbol_name)
            }
        }
    }

    fn rvalue_ref(&self, rvalue: &Rvalue, func: &MirFunction) -> String {
        match rvalue {
            Rvalue::Use(op) => self.operand_ref(op, func),

            Rvalue::BinaryOp { op, lhs, rhs } => {
                format!(
                    "{:?}({}, {})",
                    op,
                    self.operand_ref(lhs, func),
                    self.operand_ref(rhs, func)
                )
            }

            Rvalue::UnaryOp { op, operand } => {
                format!("{:?}({})", op, self.operand_ref(operand, func))
            }

            Rvalue::Ref { place, is_const } => {
                let prefix = if *is_const { "&const " } else { "&" };
                format!("{}{}", prefix, self.place_ref(place, func))
            }

            Rvalue::Cast { operand, target } => {
                format!(
                    "{} as {}",
                    self.operand_ref(operand, func),
                    self.display_type(*target)
                )
            }

            Rvalue::SizeOf(ty) => format!("@sizeof({})", self.display_type(*ty)),
            Rvalue::AlignOf(ty) => format!("@alignof({})", self.display_type(*ty)),

            Rvalue::Aggregate { kind, operands } => {
                let operand_strs: Vec<String> =
                    operands.iter().map(|o| self.operand_ref(o, func)).collect();
                let kind_str = match kind {
                    AggregateKind::Struct(def_id) => self.resolve_def_name(*def_id),
                    AggregateKind::Array => "array".to_string(),
                    AggregateKind::Slice => "slice".to_string(),
                };
                format!("{} {{ {} }}", kind_str, operand_strs.join(", "))
            }

            Rvalue::Discriminant(place) => {
                format!("discriminant({})", self.place_ref(place, func))
            }
        }
    }

    fn operand_ref(&self, operand: &Operand, func: &MirFunction) -> String {
        match operand {
            Operand::Copy(place) => self.place_ref(place, func),
            Operand::Move(place) => format!("move {}", self.place_ref(place, func)),
            Operand::Constant(c) => self.const_ref(c),
        }
    }

    fn const_ref(&self, c: &ConstValue) -> String {
        match c {
            ConstValue::Int(n) => n.to_string(),
            ConstValue::Float(f) => f.to_string(),
            ConstValue::Bool(b) => b.to_string(),
            ConstValue::Char(c) => format!("'{}'", c),
            ConstValue::Str(s) => format!("{:?}", self.resolve_spur(*s)),
            ConstValue::NullPtr => "null".to_string(),
            ConstValue::Void => "void".to_string(),
        }
    }

    fn place_ref(&self, place: &Place, func: &MirFunction) -> String {
        let mut s = self.local_ref(place.local);
        let mut deref_prefix = false;

        for elem in &place.projection {
            match elem {
                PlaceElem::Field(def_id) => {
                    let _ = write!(s, ".{}", self.resolve_def_name(*def_id));
                }
                PlaceElem::Index(idx_local) => {
                    let _ = write!(s, "[{}]", self.local_ref(*idx_local));
                }
                PlaceElem::Deref => {
                    s = format!("(*{})", s);
                    deref_prefix = true;
                }
                PlaceElem::SliceLen => {
                    let _ = write!(s, ".len");
                }
                PlaceElem::SlicePtr => {
                    let _ = write!(s, ".ptr");
                }
            }
        }

        let _ = deref_prefix;
        let _ = func;
        s
    }

    fn local_ref(&self, local: LocalId) -> String {
        format!("%{}", local.0)
    }

    fn display_type(&self, ty: TypeId) -> String {
        match self.typecheck.interner.get(ty).clone() {
            Type::Builtin(b) => format!("{:?}", b),
            Type::IntLiteral => "{integer}".to_string(),
            Type::FloatLiteral => "{float}".to_string(),

            Type::Struct {
                def_id,
                generic_args,
            } => {
                let name = self.resolve_def_name(def_id);
                if generic_args.is_empty() {
                    name
                } else {
                    let args: Vec<String> =
                        generic_args.iter().map(|&a| self.display_type(a)).collect();
                    format!("{}[{}]", name, args.join(", "))
                }
            }

            Type::Interface { def_id } => self.resolve_def_name(def_id),
            Type::Enum { def_id } => self.resolve_def_name(def_id),

            Type::Pointer { inner, is_const } => {
                let inner_s = self.display_type(inner);
                if is_const {
                    format!("*const {}", inner_s)
                } else {
                    format!("*{}", inner_s)
                }
            }
            Type::ManyPointer { inner, is_const } => {
                let inner_s = self.display_type(inner);
                if is_const {
                    format!("[*]const {}", inner_s)
                } else {
                    format!("[*]{}", inner_s)
                }
            }
            Type::Slice { element, is_const } => {
                let elem_s = self.display_type(element);
                if is_const {
                    format!("[]const {}", elem_s)
                } else {
                    format!("[]{}", elem_s)
                }
            }
            Type::Array { element, len } => {
                let elem_s = self.display_type(element);
                match len {
                    Some(n) => format!("[{}]{}", n, elem_s),
                    None => format!("[?]{}", elem_s),
                }
            }

            Type::Fn { params, ret } => {
                let param_strs: Vec<String> =
                    params.iter().map(|&p| self.display_type(p)).collect();
                format!("fn({}) {}", param_strs.join(", "), self.display_type(ret))
            }

            Type::GenericParam(def_id) => self.resolve_def_name(def_id),
            Type::InterfaceSelfPlaceholder(_) => "Self".to_string(),

            Type::Void => "void".to_string(),
            Type::Never => "never".to_string(),
            Type::Error => "<error>".to_string(),
        }
    }

    fn resolve_def_name(&self, def_id: DefId) -> String {
        self.resolution
            .defs
            .get(&def_id)
            .map(|info| self.resolve_spur(info.name))
            .unwrap_or_else(|| format!("<def#{:?}>", def_id))
    }

    fn resolve_spur(&self, spur: lasso::Spur) -> String {
        self.rodeo.borrow().resolve(&spur).to_string()
    }
}
