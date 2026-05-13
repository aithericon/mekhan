//! Rust-to-Rhai expression converter.
//!
//! Converts a limited subset of Rust expressions to Rhai script syntax.
//! Supports:
//! - Struct construction (primary use case)
//! - Field access (e.g., `task.id`)
//! - Simple literals (integers, strings, booleans)
//! - Binary operations (+, -, *, /, %, ==, !=, <, >, <=, >=, &&, ||)
//! - Raw Rhai strings (pass-through when body is a string literal)

use heck::ToSnakeCase;
use syn::{
    BinOp, Block, Expr, ExprBinary, ExprField, ExprIf, ExprLit, ExprPath, ExprStruct, ExprTuple,
    Lit, Member, Stmt,
};

/// Convert a Rust block (function body) to a Rhai script string.
///
/// The block should contain a single expression that returns a struct,
/// OR a raw string literal containing Rhai code directly.
///
/// When a raw string is detected, it's passed through without transformation.
pub fn rust_block_to_rhai(block: &Block) -> Result<String, String> {
    let expr = extract_single_expr(block)?;

    // Check for raw Rhai: if the body is a string literal, use it directly
    if let Some(raw_rhai) = try_extract_raw_rhai(expr) {
        return Ok(raw_rhai);
    }

    expr_to_rhai(expr)
}

/// Try to extract raw Rhai from a string literal expression.
///
/// Returns `Some(script)` if the expression is a string literal (including raw strings),
/// otherwise returns `None` to fall back to Rust→Rhai conversion.
///
/// # Example
/// ```ignore
/// #[step("branch", "Branch")]
/// fn branch(input: Input) -> (Approved, Rejected) {
///     r#"
///         let score = input.value * 2;
///         if score > 100 {
///             #{ approved: #{ id: input.id, score: score } }
///         } else {
///             #{ rejected: #{ id: input.id, reason: "Low score" } }
///         }
///     "#
/// }
/// ```
fn try_extract_raw_rhai(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Some(s.value().trim().to_string()),
        _ => None,
    }
}

/// Convert a Rust block to Rhai for steps with Target<T> outputs.
///
/// Unlike `rust_block_to_rhai`, this uses the provided target parameter name
/// as the output key instead of deriving it from the struct type name.
///
/// Example:
/// ```ignore
/// fn rollback_retry(..., job: Target<Job>) {
///     Job { id: reservation.job_id, ... }
/// }
/// ```
/// becomes:
/// ```rhai
/// #{ job: #{ id: reservation.job_id, ... } }
/// ```
///
/// Also supports raw Rhai string literals that bypass conversion entirely.
pub fn rust_block_to_rhai_with_targets(
    block: &Block,
    target_names: &[String],
) -> Result<String, String> {
    let expr = extract_single_expr(block)?;

    // Check for raw Rhai: if the body is a string literal, use it directly
    if let Some(raw_rhai) = try_extract_raw_rhai(expr) {
        return Ok(raw_rhai);
    }

    expr_to_rhai_with_target(expr, target_names)
}

/// Convert an expression to Rhai with explicit target names.
fn expr_to_rhai_with_target(expr: &Expr, target_names: &[String]) -> Result<String, String> {
    match expr {
        // Single struct → use first target name
        Expr::Struct(s) => {
            if target_names.is_empty() {
                return Err("No target names provided for struct expression".to_string());
            }
            struct_to_rhai_with_name(s, &target_names[0])
        }
        // Tuple of structs → use target names in order
        Expr::Tuple(tuple) => {
            if tuple.elems.len() != target_names.len() {
                return Err(format!(
                    "Tuple has {} elements but {} target names provided",
                    tuple.elems.len(),
                    target_names.len()
                ));
            }
            let mut entries: Vec<String> = Vec::new();
            for (elem, target_name) in tuple.elems.iter().zip(target_names.iter()) {
                match elem {
                    Expr::Struct(s) => {
                        // Convert fields only (without wrapper)
                        let fields = struct_fields_to_rhai(s)?;
                        entries.push(format!("{}: #{{ {} }}", target_name, fields));
                    }
                    _ => {
                        return Err(
                            "Tuple elements for target outputs must be struct constructions"
                                .to_string(),
                        );
                    }
                }
            }
            Ok(format!("#{{ {} }}", entries.join(", ")))
        }
        // If-else branching - keep type names from branches but use target context
        Expr::If(if_expr) => if_to_rhai_with_targets(if_expr, target_names),
        // Other expressions - fallback to normal conversion
        _ => expr_to_rhai(expr),
    }
}

/// Convert struct to Rhai using explicit output name instead of type name.
fn struct_to_rhai_with_name(s: &ExprStruct, output_name: &str) -> Result<String, String> {
    let fields_str = struct_fields_to_rhai(s)?;
    Ok(format!("#{{ {}: #{{ {} }} }}", output_name, fields_str))
}

/// Convert just the fields of a struct (without wrapper).
fn struct_fields_to_rhai(s: &ExprStruct) -> Result<String, String> {
    let fields: Result<Vec<String>, String> = s
        .fields
        .iter()
        .map(|field| {
            let name = match &field.member {
                Member::Named(ident) => ident.to_string(),
                Member::Unnamed(idx) => idx.index.to_string(),
            };
            let value = expr_to_rhai(&field.expr)?;
            Ok(format!("{}: {}", name, value))
        })
        .collect();
    Ok(fields?.join(", "))
}

/// Convert if-else with target names context.
fn if_to_rhai_with_targets(if_expr: &ExprIf, target_names: &[String]) -> Result<String, String> {
    let condition = expr_to_rhai(&if_expr.cond)?;
    let then_branch = block_to_rhai_with_targets(&if_expr.then_branch, target_names)?;

    let else_part = match &if_expr.else_branch {
        Some((_, else_expr)) => match else_expr.as_ref() {
            Expr::Block(block) => {
                let else_content = block_to_rhai_with_targets(&block.block, target_names)?;
                format!("else {{ {} }}", else_content)
            }
            Expr::If(nested_if) => {
                let nested = if_to_rhai_with_targets(nested_if, target_names)?;
                format!("else {}", nested)
            }
            _ => return Err("Unsupported else branch type".to_string()),
        },
        None => return Err("Branching steps require an else branch".to_string()),
    };

    Ok(format!(
        "if {} {{ {} }} {}",
        condition, then_branch, else_part
    ))
}

/// Convert block to Rhai with target names context.
fn block_to_rhai_with_targets(block: &Block, target_names: &[String]) -> Result<String, String> {
    let expr = extract_single_expr(block)?;
    expr_to_rhai_with_target(expr, target_names)
}

/// Extract the single return expression from a block.
fn extract_single_expr(block: &Block) -> Result<&Expr, String> {
    // Handle blocks with a single expression (no semicolon)
    if block.stmts.len() == 1 {
        match &block.stmts[0] {
            Stmt::Expr(expr, None) => return Ok(expr),
            Stmt::Expr(expr, Some(_)) => {
                return Err(format!(
                    "Step function body should not end with semicolon. Found: {:?}",
                    expr
                ))
            }
            _ => {}
        }
    }

    Err(format!(
        "Step function body must contain a single struct construction expression. Found {} statements.",
        block.stmts.len()
    ))
}

/// Convert an expression to Rhai syntax.
pub fn expr_to_rhai(expr: &Expr) -> Result<String, String> {
    match expr {
        Expr::Struct(s) => struct_to_rhai(s),
        Expr::If(if_expr) => if_to_rhai(if_expr),
        Expr::Tuple(tuple) => tuple_to_rhai(tuple),
        Expr::Field(f) => field_to_rhai(f),
        Expr::Path(p) => path_to_rhai(p),
        Expr::Lit(lit) => lit_to_rhai(lit),
        Expr::Binary(bin) => binary_to_rhai(bin),
        Expr::Paren(p) => {
            let inner = expr_to_rhai(&p.expr)?;
            Ok(format!("({})", inner))
        }
        Expr::Unary(u) => {
            let operand = expr_to_rhai(&u.expr)?;
            match u.op {
                syn::UnOp::Neg(_) => Ok(format!("-{}", operand)),
                syn::UnOp::Not(_) => Ok(format!("!{}", operand)),
                _ => Err(format!("Unsupported unary operator: {:?}", u.op)),
            }
        }
        _ => Err(format!(
            "Unsupported expression type in #[step] body: {:?}",
            expr
        )),
    }
}

/// Convert a tuple expression to Rhai syntax for FORKING (sending to ALL outputs).
///
/// ```ignore
/// (Notification { msg: task.name }, AuditLog { action: "created" })
/// ```
/// becomes:
/// ```rhai
/// #{ notification: #{ msg: task.name }, audit_log: #{ action: "created" } }
/// ```
fn tuple_to_rhai(tuple: &ExprTuple) -> Result<String, String> {
    if tuple.elems.is_empty() {
        return Err("Empty tuple not supported for forking".to_string());
    }

    // Each element must be a struct construction
    let mut entries: Vec<String> = Vec::new();

    for elem in &tuple.elems {
        match elem {
            Expr::Struct(s) => {
                // Get the struct type name and convert to snake_case for the output port name
                let type_name = s
                    .path
                    .segments
                    .last()
                    .ok_or("Empty struct path")?
                    .ident
                    .to_string();
                let output_name = type_name.to_snake_case();

                // Convert fields
                let fields: Result<Vec<String>, String> = s
                    .fields
                    .iter()
                    .map(|field| {
                        let name = match &field.member {
                            Member::Named(ident) => ident.to_string(),
                            Member::Unnamed(idx) => idx.index.to_string(),
                        };
                        let value = expr_to_rhai(&field.expr)?;
                        Ok(format!("{}: {}", name, value))
                    })
                    .collect();

                let fields_str = fields?.join(", ");
                entries.push(format!("{}: #{{ {} }}", output_name, fields_str));
            }
            _ => {
                return Err("Tuple elements for forking must be struct constructions".to_string());
            }
        }
    }

    Ok(format!("#{{ {} }}", entries.join(", ")))
}

/// Convert an if-else expression to Rhai syntax.
///
/// Supports branching steps where different outputs are returned conditionally.
///
/// ```ignore
/// if task.priority > 80 {
///     Approved { task_id: task.id }
/// } else {
///     Rejected { task_id: task.id }
/// }
/// ```
/// becomes:
/// ```rhai
/// if task.priority > 80 { #{ approved: #{ task_id: task.id } } } else { #{ rejected: #{ task_id: task.id } } }
/// ```
fn if_to_rhai(if_expr: &ExprIf) -> Result<String, String> {
    // Convert condition
    let condition = expr_to_rhai(&if_expr.cond)?;

    // Convert then branch (block containing struct or nested if)
    let then_branch = block_to_rhai(&if_expr.then_branch)?;

    // Convert else branch (required for branching)
    let else_part = match &if_expr.else_branch {
        Some((_, else_expr)) => match else_expr.as_ref() {
            Expr::Block(block) => {
                let else_content = block_to_rhai(&block.block)?;
                format!("else {{ {} }}", else_content)
            }
            Expr::If(nested_if) => {
                // else if chain - render as "else if" without extra braces
                let nested = if_to_rhai(nested_if)?;
                format!("else {}", nested)
            }
            _ => return Err("Unsupported else branch type".to_string()),
        },
        None => return Err("Branching steps require an else branch".to_string()),
    };

    Ok(format!(
        "if {} {{ {} }} {}",
        condition, then_branch, else_part
    ))
}

/// Convert a block to Rhai by extracting and converting its single expression.
fn block_to_rhai(block: &Block) -> Result<String, String> {
    let expr = extract_single_expr(block)?;
    expr_to_rhai(expr)
}

/// Convert a struct construction expression to Rhai map syntax.
///
/// `Assignment { task_id: task.id, worker_id: worker.id }`
/// becomes:
/// `#{ assignment: #{ task_id: task.id, worker_id: worker.id } }`
fn struct_to_rhai(s: &ExprStruct) -> Result<String, String> {
    // Get the struct type name and convert to snake_case for the output port name
    let type_name = s
        .path
        .segments
        .last()
        .ok_or("Empty struct path")?
        .ident
        .to_string();
    let output_name = type_name.to_snake_case();

    // Convert each field
    let fields: Result<Vec<String>, String> = s
        .fields
        .iter()
        .map(|field| {
            let name = match &field.member {
                Member::Named(ident) => ident.to_string(),
                Member::Unnamed(idx) => idx.index.to_string(),
            };
            let value = expr_to_rhai(&field.expr)?;
            Ok(format!("{}: {}", name, value))
        })
        .collect();

    let fields_str = fields?.join(", ");
    Ok(format!("#{{ {}: #{{ {} }} }}", output_name, fields_str))
}

/// Convert field access expression to Rhai syntax.
/// `task.id` stays as `task.id`
fn field_to_rhai(f: &ExprField) -> Result<String, String> {
    let base = expr_to_rhai(&f.base)?;
    let member = match &f.member {
        Member::Named(ident) => ident.to_string(),
        Member::Unnamed(idx) => idx.index.to_string(),
    };
    Ok(format!("{}.{}", base, member))
}

/// Convert a path expression (variable reference) to Rhai.
/// `task` stays as `task`
fn path_to_rhai(p: &ExprPath) -> Result<String, String> {
    if p.path.segments.len() == 1 {
        Ok(p.path.segments[0].ident.to_string())
    } else {
        // Multi-segment paths like `module::Type` are not supported
        Err(format!(
            "Multi-segment paths not supported in #[step] body: {:?}",
            p.path
        ))
    }
}

/// Convert a literal expression to Rhai.
fn lit_to_rhai(lit: &ExprLit) -> Result<String, String> {
    match &lit.lit {
        Lit::Int(i) => Ok(i.base10_digits().to_string()),
        Lit::Float(f) => Ok(f.base10_digits().to_string()),
        Lit::Str(s) => Ok(format!("\"{}\"", s.value())),
        Lit::Bool(b) => Ok(if b.value { "true" } else { "false" }.to_string()),
        Lit::Char(c) => Ok(format!("'{}'", c.value())),
        _ => Err(format!("Unsupported literal type: {:?}", lit.lit)),
    }
}

/// Convert a binary expression to Rhai.
fn binary_to_rhai(bin: &ExprBinary) -> Result<String, String> {
    let left = expr_to_rhai(&bin.left)?;
    let right = expr_to_rhai(&bin.right)?;

    let op = match bin.op {
        BinOp::Add(_) => "+",
        BinOp::Sub(_) => "-",
        BinOp::Mul(_) => "*",
        BinOp::Div(_) => "/",
        BinOp::Rem(_) => "%",
        BinOp::Eq(_) => "==",
        BinOp::Ne(_) => "!=",
        BinOp::Lt(_) => "<",
        BinOp::Gt(_) => ">",
        BinOp::Le(_) => "<=",
        BinOp::Ge(_) => ">=",
        BinOp::And(_) => "&&",
        BinOp::Or(_) => "||",
        _ => return Err(format!("Unsupported binary operator: {:?}", bin.op)),
    };

    Ok(format!("{} {} {}", left, op, right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_simple_struct() {
        let block: Block = parse_quote! {
            {
                Output { value: 42 }
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert_eq!(result, "#{ output: #{ value: 42 } }");
    }

    #[test]
    fn test_field_access() {
        let block: Block = parse_quote! {
            {
                Assignment { task_id: task.id }
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert_eq!(result, "#{ assignment: #{ task_id: task.id } }");
    }

    #[test]
    fn test_multiple_fields() {
        let block: Block = parse_quote! {
            {
                Result { a: x.a, b: y.b, c: 10 }
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert_eq!(result, "#{ result: #{ a: x.a, b: y.b, c: 10 } }");
    }

    #[test]
    fn test_binary_expression() {
        let block: Block = parse_quote! {
            {
                Sum { total: a.x + b.y }
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert_eq!(result, "#{ sum: #{ total: a.x + b.y } }");
    }

    #[test]
    fn test_if_else_branching() {
        let block: Block = parse_quote! {
            {
                if x.value > 10 {
                    Approved { id: x.id }
                } else {
                    Rejected { id: x.id }
                }
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert!(result.contains("if x.value > 10"));
        assert!(result.contains("#{ approved:"));
        assert!(result.contains("#{ rejected:"));
    }

    #[test]
    fn test_else_if_chain() {
        let block: Block = parse_quote! {
            {
                if x.priority >= 10 {
                    VipResult { level: "gold" }
                } else if x.priority >= 5 {
                    StandardResult { level: "silver" }
                } else {
                    LowResult { level: "bronze" }
                }
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert!(result.contains("if x.priority >= 10"));
        assert!(result.contains("else if x.priority >= 5"));
        assert!(result.contains("#{ vip_result:"));
        assert!(result.contains("#{ standard_result:"));
        assert!(result.contains("#{ low_result:"));
    }

    #[test]
    fn test_if_without_else_fails() {
        let block: Block = parse_quote! {
            {
                if x.value > 10 {
                    Approved { id: x.id }
                }
            }
        };
        let result = rust_block_to_rhai(&block);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("else branch"));
    }

    #[test]
    fn test_tuple_forking() {
        // Forking: send to ALL outputs simultaneously
        let block: Block = parse_quote! {
            {
                (
                    Notification { msg: task.name },
                    AuditLog { action: "created", task_id: task.id }
                )
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        // Should contain both outputs in a single map
        assert!(result.contains("notification:"));
        assert!(result.contains("audit_log:"));
        assert!(result.contains("msg: task.name"));
        assert!(result.contains("action: \"created\""));
    }

    #[test]
    fn test_tuple_forking_three_outputs() {
        let block: Block = parse_quote! {
            {
                (
                    OutputA { x: 1 },
                    OutputB { y: 2 },
                    OutputC { z: 3 }
                )
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert!(result.contains("output_a:"));
        assert!(result.contains("output_b:"));
        assert!(result.contains("output_c:"));
    }

    #[test]
    fn test_raw_rhai_passthrough() {
        let block: Block = parse_quote! {
            {
                r#"
                    let score = input.value * 2;
                    if score > 100 {
                        #{ approved: #{ id: input.id } }
                    } else {
                        #{ rejected: #{ id: input.id } }
                    }
                "#
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        // Raw Rhai should be passed through as-is (trimmed)
        assert!(result.contains("let score = input.value * 2"));
        assert!(result.contains("if score > 100"));
        assert!(result.contains("#{ approved:"));
    }

    #[test]
    fn test_raw_rhai_simple_string() {
        let block: Block = parse_quote! {
            {
                "#{ output: #{ value: input.x + input.y } }"
            }
        };
        let result = rust_block_to_rhai(&block).unwrap();
        assert_eq!(result, "#{ output: #{ value: input.x + input.y } }");
    }

    #[test]
    fn test_raw_rhai_with_targets() {
        let block: Block = parse_quote! {
            {
                r#"#{ job: #{ id: task.id, retries: task.retries + 1 } }"#
            }
        };
        let result = rust_block_to_rhai_with_targets(&block, &["job".to_string()]).unwrap();
        assert!(result.contains("#{ job:"));
        assert!(result.contains("retries: task.retries + 1"));
    }
}
