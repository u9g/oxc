//! Linter with plugin

use std::{
    env,
    path::{Component, Path},
    rc::Rc,
};

use oxc_allocator::Allocator;
use oxc_linter::{LintContext, Linter};
use oxc_parser::Parser;
use oxc_semantic::{SemanticBuilder, SemanticBuilderReturn};
use oxc_span::SourceType;
use path_calculate::Calculate;

// Instruction:
// create a `test.js`,
// run `OXC_PLUGIN=./crates/oxc_linter/examples/queries cargo run -p oxc_linter --example plugin`
// or `OXC_PLUGIN=./crates/oxc_linter/examples/queries cargo watch -x "run -p oxc_linter --example plugin"`

fn main() {
    let name = env::args().nth(1).unwrap_or_else(|| "test.js".to_string());
    let path = Path::new(&name);
    let source_text = std::fs::read_to_string(path).unwrap_or_else(|_| panic!("{name} not found"));
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap();
    let ret = Parser::new(&allocator, &source_text, source_type).parse();

    // Handle parser errors
    if !ret.errors.is_empty() {
        print_errors(&source_text, ret.errors);
        return;
    }

    let program = allocator.alloc(ret.program);
    let SemanticBuilderReturn { semantic, errors } =
        SemanticBuilder::new(&source_text, source_type).with_trivias(&ret.trivias).build(program);
    assert!(errors.is_empty());

    let linter = Linter::new();
    let lint_ctx = LintContext::new(&Rc::new(semantic));

    let messages = linter.run(
        lint_ctx,
        path.related_to(Path::new("."))
            .unwrap()
            .components()
            .filter_map(|component| {
                let Component::Normal(s) = component else {
                    return None;
                };
                Some(s)
            })
            .map(|s| s.to_str().map(std::string::ToString::to_string))
            .collect::<Vec<_>>(),
    );
    let errors = messages.into_iter().map(|m| m.error).collect::<Vec<_>>();

    if !errors.is_empty() {
        print_errors(&source_text, errors);
        return;
    }

    println!("Success!");
}

fn print_errors(source_text: &str, errors: Vec<oxc_diagnostics::Error>) {
    for error in errors {
        let error = error.with_source_code(source_text.to_string());
        println!("{error:?}");
    }
}
