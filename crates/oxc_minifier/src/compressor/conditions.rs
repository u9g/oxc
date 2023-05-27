//! Minimize Conditions
//!
//! <https://github.com/google/closure-compiler/blob/master/src/com/google/javascript/jscomp/PeepholeMinimizeConditions.java>

#[allow(clippy::wildcard_imports)]
use oxc_hir::hir::*;

use super::Compressor;

impl<'a> Compressor<'a> {
    pub(crate) fn try_replace_if<'b>(&mut self, stmt: &'b mut Statement<'a>) {
        let Statement::IfStatement(if_stmt) = stmt else {
            return
        };

        // `if (x) { return 1 } else { return 2 }` -> `return x ? 1 : 2
        // `if (x) return 1 else return 2` -> `return x ? 1 : 2
        if matches!(
            if_stmt.alternate,
            Some(Statement::BlockStatement(_) | Statement::ReturnStatement(_))
        ) {
            let consequent = get_single_return_argument(if_stmt.consequent.as_mut());
            let alternate = get_single_return_argument(if_stmt.alternate.as_mut());
            if let (Some(consequent), Some(alternate)) = (consequent, alternate) {
                let dummy_expr = self.dummy_expr();
                let test = std::mem::replace(&mut if_stmt.test, dummy_expr);
                let argument =
                    self.hir.conditional_expression(if_stmt.span, test, consequent, alternate);
                *stmt = self.hir.return_statement(if_stmt.span, Some(argument));
                return;
            }
        }
    }
}

/// Gets the sinle return argument from a statement
///
/// `{ return argument }` -> `argument`
/// `return argument` -> `argument`
fn get_single_return_argument<'a>(stmt: Option<&mut Statement<'a>>) -> Option<Expression<'a>> {
    match stmt {
        Some(Statement::ReturnStatement(return_stmt)) => return_stmt.argument.take(),
        Some(Statement::BlockStatement(block_stmt)) if block_stmt.body.len() == 1 => {
            return if let Statement::ReturnStatement(return_stmt) = &mut block_stmt.body[0] {
                return_stmt.argument.take()
            } else {
                None
            };
        }
        _ => None,
    }
}
