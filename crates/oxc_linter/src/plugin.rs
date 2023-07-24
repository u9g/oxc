use std::{collections::BTreeMap, env, fs, rc::Rc, sync::Arc};

use oxc_diagnostics::miette::{miette, LabeledSpan};
use oxc_query::Adapter;
use oxc_semantic::Semantic;
use serde::Deserialize;
use trustfall::{execute_query, Schema, TransparentValue, TryIntoStruct};

use crate::context::LintContext;

enum SpanInfo {
    SingleSpanInfo(SingleSpanInfo),
    MultipleSpanInfo(MultipleSpanInfo),
}

#[derive(Debug, Deserialize)]
struct SingleSpanInfo {
    span_start: u64,
    span_end: u64,
}

#[derive(Debug, Deserialize)]
struct MultipleSpanInfo {
    span_start: Box<[u64]>,
    span_end: Box<[u64]>,
}

#[derive(Deserialize, Clone)]
pub struct InputQuery {
    pub name: String,
    pub query: String,
    pub args: BTreeMap<Arc<str>, TransparentValue>,
    pub reason: String,
    #[serde(default)]
    pub tests: QueryTests,
}

#[derive(Deserialize, Default, Clone)]
pub struct QueryTests {
    pub pass: Vec<SingleTest>,
    pub fail: Vec<SingleTest>,
}

#[derive(Deserialize, Clone)]
pub struct SingleTest {
    pub relative_path: Vec<String>,
    pub code: String,
}

pub struct LinterPlugin {
    pub(super) rules: Vec<InputQuery>,
    schema: &'static Schema,
}

impl LinterPlugin {
    pub fn new(schema: &'static Schema) -> Self {
        let queries_path = env::var("OXC_PLUGIN").unwrap();
        let rules = fs::read_dir(queries_path)
            .expect("to readdir queries_path folder")
            .filter_map(std::result::Result::ok)
            .filter(|dir_entry| dir_entry.path().is_dir())
            .filter(|dir| !dir.path().to_str().map_or_else(|| true, |f| f.contains("ignore_")))
            .filter_map(|folder| fs::read_dir(folder.path()).ok())
            .flat_map(std::iter::IntoIterator::into_iter)
            .filter_map(std::result::Result::ok)
            .filter(|dir_entry| dir_entry.path().is_file())
            .filter(|f| {
                std::path::Path::new(f.path().as_os_str().to_str().unwrap())
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("yml"))
            })
            .map(|f| fs::read_to_string(f.path()))
            .map(std::result::Result::unwrap)
            .map(|rule| {
                serde_yaml::from_str::<InputQuery>(rule.as_str())
                    .expect(&format!("{rule}\n\nQuery above"))
            })
            .collect::<Vec<_>>();

        Self { rules, schema }
    }

    pub fn run(
        &self,
        ctx: &mut LintContext,
        semantic: Rc<Semantic<'_>>,
        relative_file_path_parts: Vec<Option<String>>,
    ) {
        let inner = Adapter { path_components: relative_file_path_parts, semantic };
        let adapter = Arc::from(&inner);
        for input_query in &self.rules {
            for data_item in execute_query(
                self.schema,
                Arc::clone(&adapter),
                &input_query.query,
                input_query.args.clone(),
            )
            .expect(
                format!("not a legal query in query: \n\n\n{}", input_query.query.as_str())
                    .as_str(),
            )
            .map(|v| {
                if env::var("OXC_PRINT_TFALL_OUTPUTS").unwrap_or_else(|_| "false".to_owned())
                    == "true"
                {
                    println!("{v:#?}");
                }
                let multi = v.clone().try_into_struct::<MultipleSpanInfo>();
                let single = v.try_into_struct::<SingleSpanInfo>();
                single.map_or_else(
                    |_| SpanInfo::MultipleSpanInfo(multi.unwrap()),
                    SpanInfo::SingleSpanInfo,
                )
            })
            .take(usize::MAX)
            {
                ctx.with_rule_name("a rule");
                // TODO: this isn't how we do this at all, need to make this consistent with the project's miette style
                match data_item {
                    SpanInfo::SingleSpanInfo(SingleSpanInfo {
                        span_start: start,
                        span_end: end,
                    }) => {
                        ctx.diagnostic(miette!(
                            labels = vec![LabeledSpan::at(
                                (start as usize, (end - start) as usize),
                                input_query.reason.as_str()
                            )],
                            "Unexpected error"
                        ));
                    }
                    SpanInfo::MultipleSpanInfo(MultipleSpanInfo {
                        span_start: start,
                        span_end: end,
                    }) => {
                        for i in 0..start.len() {
                            ctx.diagnostic(miette!(
                                labels = vec![LabeledSpan::at(
                                    (start[i] as usize, (end[i] - start[i]) as usize),
                                    input_query.reason.as_str()
                                )],
                                "Unexpected error"
                            ));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{env, rc::Rc};

    use oxc_allocator::Allocator;
    use oxc_diagnostics::Report;
    use oxc_parser::Parser;
    use oxc_query::schema;
    use oxc_semantic::SemanticBuilder;
    use oxc_span::SourceType;

    use super::{LinterPlugin, SingleTest};
    use crate::{LintContext, Linter};

    #[test]
    fn test_queries() {
        env::set_var("OXC_PLUGIN", "./examples/queries");
        let plugin = LinterPlugin::new(schema());
        for rule in plugin.rules {
            for (i, test) in rule.tests.pass.iter().enumerate() {
                let messages = run_test(test, &rule.name);
                assert!(
                    messages.is_empty(),
                    "{}'s test {} is failing when it should pass.\nErrors: {:#?}\nPath: {:?}\nCode:\n\n{}\n\n",
                    rule.name,
                    i + 1,
                    messages,
                    test.relative_path,
                    test.code
                );
            }

            for (i, test) in rule.tests.fail.iter().enumerate() {
                let messages = run_test(test, &rule.name);
                assert!(
                    !messages.is_empty(),
                    "{}'s test {} is passing when it should fail.\nPath: {:?}\nCode:\n\n{}\n\n",
                    rule.name,
                    i + 1,
                    test.relative_path,
                    test.code
                );
            }

            if rule.tests.pass.len() + rule.tests.fail.len() > 0 {
                println!(
                    "{} passed {} tests successfully.\n",
                    rule.name,
                    rule.tests.pass.len() + rule.tests.fail.len()
                );
            }
        }
    }

    fn run_test(test: &SingleTest, rule_name: &str) -> Vec<Report> {
        let file_path = &test.relative_path.last().expect("there to be atleast 1 path part");
        let source_text = &test.code;

        let allocator = Allocator::default();
        let source_type = SourceType::from_path(file_path).unwrap();
        let ret = Parser::new(&allocator, source_text, source_type).parse();

        // Handle parser errors
        assert!(ret.errors.is_empty(), "Parser errors: {:?} Code:\n\n{:?}", ret.errors, test.code);

        let program = allocator.alloc(ret.program);
        let semantic_ret = SemanticBuilder::new(source_text, source_type)
            .with_trivias(&ret.trivias)
            .build(program);

        let linter = Linter::new().with_fix(false).only_use_query_rule(rule_name);

        let lint_ctx = LintContext::new(&Rc::new(semantic_ret.semantic));

        let messages = linter.run(
            lint_ctx,
            test.relative_path.iter().map(|el| Some(el.clone())).collect::<Vec<_>>(),
        );

        messages.into_iter().map(|m| m.error).collect::<Vec<_>>()
    }
}
