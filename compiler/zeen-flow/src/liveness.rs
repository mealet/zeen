use std::collections::{HashMap, HashSet};

use zeen_mir::{
    BlockId, CallTarget, LocalId, LocalKind, MirFunction, MirStatement, Operand, Place, Rvalue,
    Terminator,
};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::TypeInterner;

use crate::drop::{Taint, type_needs_drop};

fn successor_blocks(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Goto(target) => vec![*target],
        Terminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut out: Vec<BlockId> = targets.iter().map(|(_, block)| *block).collect();
            out.push(*otherwise);
            out
        }
        Terminator::Call {
            target: Some(t), ..
        }
        | Terminator::MacroCall {
            target: Some(t), ..
        } => vec![*t],
        _ => Vec::new(),
    }
}

fn operand_local(operand: &Operand) -> Option<LocalId> {
    match operand {
        Operand::Copy(place, _) | Operand::Move(place, _) => Some(place.local),
        Operand::Constant(_, _) => None,
    }
}

fn operand_is_complex(operand: &Operand) -> bool {
    match operand {
        Operand::Copy(place, _) | Operand::Move(place, _) => !place.projection.is_empty(),
        Operand::Constant(_, _) => false,
    }
}

fn stmt_uses(stmt: &MirStatement) -> Vec<LocalId> {
    let mut out = Vec::new();
    let mut push = |local: LocalId| {
        if !out.contains(&local) {
            out.push(local);
        };
    };
    let mut push_operand = |operand: &Operand| {
        if let Some(local) = operand_local(operand) {
            push(local);
        };
    };
    match stmt {
        MirStatement::Assign { rvalue, .. } => match rvalue {
            Rvalue::Use(operand)
            | Rvalue::UnaryOp { operand, .. }
            | Rvalue::Cast { operand, .. } => push_operand(operand),
            Rvalue::BinaryOp { lhs, rhs, .. } => {
                push_operand(lhs);
                push_operand(rhs);
            }
            Rvalue::Aggregate { operands, .. } => {
                for operand in operands {
                    push_operand(operand);
                }
            }
            Rvalue::Ref { place, .. } | Rvalue::Discriminant(place) => {
                push(place.local);
            }
            Rvalue::SizeOf(_) | Rvalue::AlignOf(_) => {}
        },
        MirStatement::Drop(place) => {
            push(place.local);
        }
        MirStatement::Discard(operand) => push_operand(operand),
        MirStatement::StorageLive(_) | MirStatement::StorageDead(_) | MirStatement::Nop => {}
    };
    out
}

fn stmt_def(stmt: &MirStatement) -> Option<LocalId> {
    match stmt {
        MirStatement::Assign { place, .. } if place.projection.is_empty() => Some(place.local),
        _ => None,
    }
}

fn operand_is_move(operand: &Operand) -> Option<LocalId> {
    match operand {
        Operand::Move(place, _) => Some(place.local),
        _ => None,
    }
}

fn collect_moves_stmt(stmt: &MirStatement, moved: &mut HashSet<LocalId>) {
    let mut push = |operand: &Operand| {
        if let Some(local) = operand_is_move(operand) {
            moved.insert(local);
        };
    };
    match stmt {
        MirStatement::Assign { rvalue, .. } => match rvalue {
            Rvalue::Use(operand)
            | Rvalue::UnaryOp { operand, .. }
            | Rvalue::Cast { operand, .. } => push(operand),
            Rvalue::BinaryOp { lhs, rhs, .. } => {
                push(lhs);
                push(rhs);
            }
            Rvalue::Aggregate { operands, .. } => {
                for operand in operands {
                    push(operand);
                }
            }
            Rvalue::Ref { .. }
            | Rvalue::Discriminant(_)
            | Rvalue::SizeOf(_)
            | Rvalue::AlignOf(_) => {}
        },
        MirStatement::Drop(_) | MirStatement::Discard(_) => {}
        MirStatement::StorageLive(_) | MirStatement::StorageDead(_) | MirStatement::Nop => {}
    };
}

fn collect_moves_term(term: &Terminator, moved: &mut HashSet<LocalId>) {
    let mut push = |operand: &Operand| {
        if let Some(local) = operand_is_move(operand) {
            moved.insert(local);
        };
    };
    match term {
        Terminator::Call { func, args, .. } => {
            if let CallTarget::Indirect(callee) = func {
                push(callee);
            };
            for arg in args {
                push(arg);
            }
        }
        Terminator::MacroCall { args, .. } => {
            for arg in args {
                push(arg);
            }
        }
        Terminator::SwitchInt { discriminant, .. } => {
            push(discriminant);
        }
        Terminator::Return(operand) => {
            push(operand);
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    };
}

pub fn insert_prompt_drops(
    function: &mut MirFunction,
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    tainted: &HashMap<LocalId, Taint>,
) {
    let count = function.blocks.len();
    let mut uses: Vec<HashSet<LocalId>> = vec![HashSet::new(); count];
    let mut defs: Vec<HashSet<LocalId>> = vec![HashSet::new(); count];
    let mut complex: HashSet<LocalId> = HashSet::new();
    let mut moved: HashSet<LocalId> = HashSet::new();

    for block in function.blocks.iter() {
        for stmt in &block.statements {
            collect_moves_stmt(stmt, &mut moved);
        }
        collect_moves_term(&block.terminator, &mut moved);
    }

    for (index, block) in function.blocks.iter().enumerate() {
        let mut defined: HashSet<LocalId> = HashSet::new();
        for stmt in &block.statements {
            for used in stmt_uses(stmt) {
                if !defined.contains(&used) {
                    uses[index].insert(used);
                };
            }
            match stmt {
                MirStatement::Assign { place, rvalue, .. } => {
                    if let Rvalue::Use(operand) | Rvalue::Cast { operand, .. } = rvalue
                        && let Some(local) = operand_local(operand)
                        && operand_is_complex(operand)
                    {
                        complex.insert(local);
                    };
                    if place.projection.is_empty() {
                        defined.insert(place.local);
                        defs[index].insert(place.local);
                    } else {
                        uses[index].insert(place.local);
                        complex.insert(place.local);
                    };
                }
                MirStatement::StorageLive(local) => {
                    defined.insert(*local);
                    defs[index].insert(*local);
                }
                _ => {}
            };
        }
        match &block.terminator {
            Terminator::Call { func, args, .. } => {
                if let CallTarget::Indirect(callee) = func
                    && let Some(local) = operand_local(callee)
                    && !defined.contains(&local)
                {
                    uses[index].insert(local);
                };
                for arg in args {
                    if let Some(local) = operand_local(arg) {
                        if !defined.contains(&local) {
                            uses[index].insert(local);
                        };
                        if operand_is_complex(arg) {
                            complex.insert(local);
                        };
                    };
                }
            }
            Terminator::MacroCall { args, .. } => {
                for arg in args {
                    if let Some(local) = operand_local(arg)
                        && !defined.contains(&local)
                    {
                        uses[index].insert(local);
                    };
                    if let Some(local) = operand_local(arg)
                        && operand_is_complex(arg)
                    {
                        complex.insert(local);
                    };
                }
            }
            Terminator::SwitchInt { discriminant, .. } => {
                if let Some(local) = operand_local(discriminant)
                    && !defined.contains(&local)
                {
                    uses[index].insert(local);
                };
            }
            Terminator::Return(operand) => {
                if let Some(local) = operand_local(operand)
                    && !defined.contains(&local)
                {
                    uses[index].insert(local);
                };
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        };
    }

    let successors: Vec<Vec<BlockId>> = function
        .blocks
        .iter()
        .map(|block| successor_blocks(&block.terminator))
        .collect();
    let mut predecessors: Vec<Vec<BlockId>> = vec![Vec::new(); count];
    for (index, succs) in successors.iter().enumerate() {
        for succ in succs {
            predecessors[succ.0 as usize].push(BlockId(index as u32));
        }
    }

    let mut live_in: Vec<HashSet<LocalId>> = vec![HashSet::new(); count];
    let mut live_out: Vec<HashSet<LocalId>> = vec![HashSet::new(); count];
    let mut worklist: Vec<BlockId> = (0..count as u32).map(BlockId).collect();
    while let Some(block) = worklist.pop() {
        let idx = block.0 as usize;
        let mut out: HashSet<LocalId> = HashSet::new();
        for succ in &successors[idx] {
            out.extend(live_in[succ.0 as usize].iter().copied());
        }
        let mut inn = uses[idx].clone();
        for local in out.difference(&defs[idx]) {
            inn.insert(*local);
        }
        if inn != live_in[idx] || out != live_out[idx] {
            live_in[idx] = inn;
            live_out[idx] = out;
            for pred in &predecessors[idx] {
                if !worklist.contains(pred) {
                    worklist.push(*pred);
                };
            }
        };
    }

    let mut addr_src: HashMap<LocalId, LocalId> = HashMap::new();
    for _ in 0..64 {
        let mut changed = false;
        for block in function.blocks.iter() {
            for stmt in &block.statements {
                if let MirStatement::Assign { place, rvalue, .. } = stmt
                    && place.projection.is_empty()
                {
                    match rvalue {
                        Rvalue::Ref { place: src, .. } => {
                            if addr_src.get(&place.local) != Some(&src.local) {
                                addr_src.insert(place.local, src.local);
                                changed = true;
                            };
                        }
                        Rvalue::Use(operand) | Rvalue::Cast { operand, .. } => {
                            match operand_local(operand)
                                .and_then(|inner| addr_src.get(&inner).copied())
                            {
                                Some(root) => {
                                    if addr_src.get(&place.local) != Some(&root) {
                                        addr_src.insert(place.local, root);
                                        changed = true;
                                    };
                                }
                                None => {
                                    if addr_src.remove(&place.local).is_some() {
                                        changed = true;
                                    };
                                }
                            };
                        }
                        _ => {
                            if addr_src.remove(&place.local).is_some() {
                                changed = true;
                            };
                        }
                    };
                };
            }
        }
        if !changed {
            break;
        };
    }
    fn borrow_source(operand: &Operand, addr_src: &HashMap<LocalId, LocalId>) -> Option<LocalId> {
        let mut current = match operand {
            Operand::Copy(place, _) | Operand::Move(place, _) => place.local,
            Operand::Constant(_, _) => return None,
        };
        let mut found = false;
        let mut seen = Vec::new();
        while let Some(next) = addr_src.get(&current) {
            if seen.contains(&current) {
                break;
            };
            seen.push(current);
            current = *next;
            found = true;
        }
        if found { Some(current) } else { None }
    }

    let eligible: HashSet<LocalId> = function
        .locals
        .iter()
        .enumerate()
        .filter_map(|(index, decl)| {
            let local = LocalId(index as u32);
            if decl.kind != LocalKind::Temporary {
                return None;
            };
            if complex.contains(&local) {
                return None;
            };
            if moved.contains(&local) {
                return None;
            };
            if tainted.contains_key(&local) {
                return None;
            };
            if !type_needs_drop(interner, typecheck, decl.ty) {
                return None;
            };
            Some(local)
        })
        .collect();
    if eligible.is_empty() {
        return;
    };

    fn returns_fresh(local: LocalId, function: &MirFunction, interner: &TypeInterner) -> bool {
        let ty = function.locals.get(local.0 as usize).map(|decl| decl.ty);
        matches!(
            ty.map(|ty| interner.get(ty).clone()),
            Some(
                zeen_types::Type::Builtin(_)
                    | zeen_types::Type::IntLiteral
                    | zeen_types::Type::FloatLiteral
                    | zeen_types::Type::Void
                    | zeen_types::Type::Never
                    | zeen_types::Type::Fn { .. },
            )
        )
    }

    let mut borrows: HashMap<LocalId, HashSet<LocalId>> = HashMap::new();
    for block in function.blocks.iter() {
        for stmt in &block.statements {
            if let MirStatement::Assign { place, rvalue, .. } = stmt
                && place.projection.is_empty()
            {
                let mut sources = HashSet::new();
                let mut check = |operand: &Operand| {
                    if let Some(source) = borrow_source(operand, &addr_src) {
                        sources.insert(source);
                    };
                };
                match rvalue {
                    Rvalue::Use(operand)
                    | Rvalue::Cast { operand, .. }
                    | Rvalue::UnaryOp { operand, .. } => check(operand),
                    Rvalue::BinaryOp { lhs, rhs, .. } => {
                        check(lhs);
                        check(rhs);
                    }
                    Rvalue::Aggregate { operands, .. } => {
                        for operand in operands {
                            check(operand);
                        }
                    }
                    _ => {}
                };
                if !sources.is_empty() && !returns_fresh(place.local, function, interner) {
                    borrows.insert(place.local, sources);
                };
            }
        }
        match &block.terminator {
            Terminator::Call {
                destination, args, ..
            }
            | Terminator::MacroCall {
                destination, args, ..
            } if destination.projection.is_empty() => {
                let mut sources = HashSet::new();
                for arg in args {
                    if let Some(source) = borrow_source(arg, &addr_src) {
                        sources.insert(source);
                    };
                }
                if !sources.is_empty() && !returns_fresh(destination.local, function, interner) {
                    borrows.insert(destination.local, sources);
                };
            }
            _ => {}
        };
    }

    let mut defpos: HashMap<LocalId, (BlockId, Option<usize>)> = HashMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        let bid = BlockId(index as u32);
        for (stmt_index, stmt) in block.statements.iter().enumerate() {
            if let Some(defined) = stmt_def(stmt) {
                defpos.entry(defined).or_insert((bid, Some(stmt_index)));
            };
        }
        if let Some(defined) = terminator_def(&block.terminator) {
            defpos.entry(defined).or_insert((bid, None));
        };
    }

    let entry = function.entry_block;
    let all_blocks: HashSet<BlockId> = (0..count as u32).map(BlockId).collect();
    let mut dominators: Vec<HashSet<BlockId>> = vec![HashSet::new(); count];
    for (index, dom) in dominators.iter_mut().enumerate() {
        if BlockId(index as u32) == entry {
            dom.insert(entry);
        } else {
            *dom = all_blocks.clone();
        };
    }
    let mut dom_worklist: Vec<BlockId> = (0..count as u32).map(BlockId).collect();
    while let Some(block) = dom_worklist.pop() {
        let idx = block.0 as usize;
        if block == entry {
            continue;
        };
        let mut dom: Option<HashSet<BlockId>> = None;
        for pred in &predecessors[idx] {
            match &dom {
                None => {
                    dom = Some(dominators[pred.0 as usize].clone());
                }
                Some(current) => {
                    let next: HashSet<BlockId> = current
                        .intersection(&dominators[pred.0 as usize])
                        .copied()
                        .collect();
                    dom = Some(next);
                }
            };
        }
        let mut dom = dom.unwrap_or_default();
        dom.insert(block);
        if dom != dominators[idx] {
            dominators[idx] = dom;
            for succ in &successors[idx] {
                if !dom_worklist.contains(succ) {
                    dom_worklist.push(*succ);
                };
            }
        };
    }

    let mut aft: Vec<Vec<HashSet<LocalId>>> = Vec::with_capacity(count);
    for (index, block) in function.blocks.iter().enumerate() {
        let mut live = live_out[index].clone();
        if let Some(defined) = terminator_def(&block.terminator) {
            live.remove(&defined);
        };
        for used in terminator_uses(&block.terminator) {
            live.insert(used);
        }
        let mut sets: Vec<HashSet<LocalId>> = vec![HashSet::new(); block.statements.len()];
        for (stmt_index, stmt) in block.statements.iter().enumerate().rev() {
            sets[stmt_index] = live.clone();
            for used in stmt_uses(stmt) {
                live.insert(used);
            }
            if let Some(defined) = stmt_def(stmt) {
                live.remove(&defined);
            };
            if let MirStatement::StorageLive(local) = stmt {
                live.remove(local);
            };
        }
        aft.push(sets);
    }

    let mut cross_inserts: Vec<(BlockId, usize, LocalId)> = Vec::new();
    for temp in eligible.iter().copied() {
        let Some((defblock, defidx)) = defpos.get(&temp).copied() else {
            continue;
        };
        let derived = derived_set(temp, &addr_src, &borrows);
        'scan: for (index, block) in function.blocks.iter().enumerate() {
            let bid = BlockId(index as u32);
            if !dominators[index].contains(&defblock) {
                continue;
            };
            if bid != defblock
                && !live_in[index].contains(&temp)
                && derived.iter().all(|local| !live_in[index].contains(local))
            {
                cross_inserts.push((bid, 0, temp));
                break 'scan;
            };
            let start = if bid == defblock {
                match defidx {
                    Some(i) => i + 1,
                    None => block.statements.len(),
                }
            } else {
                0
            };
            for (at, live) in aft[index].iter().enumerate().skip(start) {
                if !live.contains(&temp) && derived.iter().all(|local| !live.contains(local)) {
                    cross_inserts.push((bid, at + 1, temp));
                    break 'scan;
                };
            }
        }
    }
    cross_inserts.sort_by_key(|(block, at, _)| (block.0, std::cmp::Reverse(*at)));
    for (block, at, local) in cross_inserts {
        function.blocks[block.0 as usize]
            .statements
            .insert(at, MirStatement::Drop(Place::from_local(local)));
    }
}

fn derived_set(
    temp: LocalId,
    addr_src: &HashMap<LocalId, LocalId>,
    borrows: &HashMap<LocalId, HashSet<LocalId>>,
) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    let mut stack = vec![temp];
    let mut seen = HashSet::new();
    while let Some(current) = stack.pop() {
        if !seen.insert(current) {
            continue;
        };
        for (local, _) in addr_src.iter().filter(|(_, source)| **source == current) {
            if !seen.contains(local) {
                out.insert(*local);
                stack.push(*local);
            };
        }
        for (result, _) in borrows
            .iter()
            .filter(|(_, sources)| sources.contains(&current))
        {
            if !seen.contains(result) {
                out.insert(*result);
                stack.push(*result);
            };
        }
    }
    out
}

fn terminator_uses(term: &Terminator) -> Vec<LocalId> {
    let mut out = Vec::new();
    let mut push_operand = |operand: &Operand| {
        if let Some(local) = operand_local(operand)
            && !out.contains(&local)
        {
            out.push(local);
        };
    };
    match term {
        Terminator::Call { func, args, .. } => {
            if let CallTarget::Indirect(callee) = func {
                push_operand(callee);
            };
            for arg in args {
                push_operand(arg);
            }
        }
        Terminator::MacroCall { args, .. } => {
            for arg in args {
                push_operand(arg);
            }
        }
        Terminator::SwitchInt { discriminant, .. } => {
            push_operand(discriminant);
        }
        Terminator::Return(operand) => {
            push_operand(operand);
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    };
    out
}

fn terminator_def(term: &Terminator) -> Option<LocalId> {
    match term {
        Terminator::Call { destination, .. } | Terminator::MacroCall { destination, .. } => {
            if destination.projection.is_empty() {
                Some(destination.local)
            } else {
                None
            }
        }
        _ => None,
    }
}
