use zeen_ast::types::BuiltinShape;

pub(crate) fn strip_pointer_wrappers(ty: &str) -> &str {
    let mut name = ty.trim();

    loop {
        if name.starts_with('[') {
            return name;
        }

        if let Some(rest) = name.strip_prefix("*const ") {
            name = rest.trim_start();
            continue;
        }

        if let Some(rest) = name.strip_prefix('*') {
            name = rest.trim_start();
            continue;
        }

        return name;
    }
}

pub(crate) fn shape_matches_type(shape: &BuiltinShape, ty: &str) -> bool {
    let mut bindings = Vec::new();

    shape_matches_text(shape, strip_pointer_wrappers(ty), &mut bindings)
}

fn shape_matches_text(shape: &BuiltinShape, ty: &str, bindings: &mut Vec<(u32, String)>) -> bool {
    let ty = ty.trim();

    match shape {
        BuiltinShape::Scalar(builtin) => ty == builtin.to_string(),

        BuiltinShape::Slice { element, is_const } => {
            let Some(rest) = ty.strip_prefix("[]") else {
                return false;
            };

            let (found_const, rest) = strip_const(rest);

            found_const == *is_const && shape_matches_text(element, rest, bindings)
        }

        BuiltinShape::ManyPointer { element, is_const } => {
            let Some(rest) = ty.strip_prefix("[*]") else {
                return false;
            };

            let (found_const, rest) = strip_const(rest);

            found_const == *is_const && shape_matches_text(element, rest, bindings)
        }

        BuiltinShape::Array { element, len } => {
            let Some(rest) = ty.strip_prefix('[') else {
                return false;
            };

            let Some(close) = rest.find(']') else {
                return false;
            };

            let digits = &rest[..close];

            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }

            if digits.parse::<u64>() != Ok(*len) {
                return false;
            }

            shape_matches_text(element, &rest[close + 1..], bindings)
        }

        BuiltinShape::Param(index) => match bindings.iter().find(|(slot, _)| slot == index) {
            Some((_, bound)) => bound == ty,
            None => {
                bindings.push((*index, ty.to_string()));

                true
            }
        },
    }
}

fn strip_const(ty: &str) -> (bool, &str) {
    match ty.trim_start().strip_prefix("const ") {
        Some(rest) => (true, rest.trim_start()),
        None => (false, ty.trim_start()),
    }
}
