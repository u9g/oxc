use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use ignore::Walk;
use located_yaml::{YamlElt, YamlLoader};
use miette::{NamedSource, SourceSpan};
use oxc_allocator::Allocator;
use oxc_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
    Report,
};
use oxc_linter::LintContext;
use oxc_parser::Parser;
use oxc_query::{schema, Adapter};
use oxc_semantic::{SemanticBuilder, SemanticBuilderReturn};
use oxc_span::{SourceType, Span};
use serde::Deserialize;
use trustfall::{execute_query, FieldValue, Schema, TransparentValue, TryIntoStruct};

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
    pub summary: String,
    pub reason: String,
    #[serde(skip_deserializing)]
    pub path: PathBuf,
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

pub enum RulesToRun {
    All,
    Only(String),
}

#[derive(Debug, Error, Diagnostic)]
#[error("{0}")]
pub struct TrustfallError(String);

#[derive(Debug, Error, Diagnostic)]
#[error("{0}")]
pub struct LinterPluginError(String, String, #[label("{1}")] Span);

impl LinterPlugin {
    pub fn new(schema: &'static Schema, queries_path: PathBuf) -> Self {
        let rules = Walk::new(queries_path)
            .filter_map(Result::ok)
            .filter(|f| {
                Path::new(f.path().as_os_str().to_str().unwrap())
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("yml"))
            })
            .map(|f| (fs::read_to_string(f.path()), f.into_path()))
            .map(|(str, pathbuf)| (Result::unwrap(str), pathbuf))
            .map(|(rule, pathbuf)| {
                let mut deserialized = serde_yaml::from_str::<InputQuery>(rule.as_str())
                    .unwrap_or_else(|_| panic!("{rule}\n\nQuery above"));
                deserialized.path = pathbuf;
                deserialized
            })
            .collect::<Vec<_>>();

        Self { rules, schema }
    }

    pub fn run_plugin_rules(
        &self,
        ctx: &mut LintContext,
        plugin: &InputQuery,
        adapter: Arc<&Adapter<'_>>,
    ) -> oxc_diagnostics::Result<()> {
        for data_item in execute_query(
                self.schema,
                Arc::clone(&adapter),
                &plugin.query,
                plugin.args.clone(),
            ).map_err(|err| {
                TrustfallError(err.to_string())
            })?
            .map(|v| {
                if env::var("OXC_PRINT_TRUSTFALL_OUTPUTS").unwrap_or_else(|_| "false".to_owned())
                    == "true"
                {
                    println!("{v:#?}");
                }
                match (v.get("span_start"), v.get("span_end")) {
                    (Some(FieldValue::List(x)), Some(FieldValue::List(y))) if matches!(x[0], FieldValue::Int64(_)) && matches!(y[0], FieldValue::Int64(_)) => {
                        v.try_into_struct::<MultipleSpanInfo>().map(SpanInfo::MultipleSpanInfo).expect("to be able to convert into MultipleSpanInfo")
                    }
                    (Some(FieldValue::List(x)), Some(FieldValue::List(y))) if matches!(x[0], FieldValue::Uint64(_)) && matches!(y[0], FieldValue::Uint64(_)) => {
                        v.try_into_struct::<MultipleSpanInfo>().map(SpanInfo::MultipleSpanInfo).expect("to be able to convert into MultipleSpanInfo")
                    }
                    (Some(FieldValue::Int64(_)), Some(FieldValue::Int64(_))) | (Some(FieldValue::Uint64(_)), Some(FieldValue::Uint64(_))) => {
                        v.try_into_struct::<SingleSpanInfo>().map(SpanInfo::SingleSpanInfo).expect("to be able to convert into SingleSpanInfo")
                    },
                    (None, None) => panic!("No `span_start` and `span_end` were not `@output`'d from query '{}'", plugin.name),
                    (a, b) => panic!("Wrong type for `span_start` and `span_end` in query '{}'. Expected both to be Int or list of Int.\nInstead got:\nspan_start={a:?} & span_end={b:?}", plugin.name),
                }
            })
            .take(usize::MAX)
            {
                ctx.with_rule_name(""); // leave this empty as it's a static string so we can't make it at runtime, and it's not userfacing
                match data_item {
                    SpanInfo::SingleSpanInfo(SingleSpanInfo {
                        span_start: start,
                        span_end: end,
                    }) => {
                        ctx.diagnostic(LinterPluginError(plugin.summary.clone(), plugin.reason.clone(), Span{ start: start.try_into().unwrap(), end: end.try_into().unwrap() }));
                    }
                    SpanInfo::MultipleSpanInfo(MultipleSpanInfo {
                        span_start: start,
                        span_end: end,
                    }) => {
                        for i in 0..start.len() {
                            ctx.diagnostic(LinterPluginError(plugin.summary.clone(), plugin.reason.clone(), Span{ start: start[i].try_into().unwrap(), end: end[i].try_into().unwrap() }));
                        }
                    }
                }
            };
        Ok(())
    }

    pub fn run_tests(
        &self,
        ctx: &mut LintContext,
        relative_file_path_parts: Vec<Option<String>>,
        rules_to_run: RulesToRun,
    ) -> oxc_diagnostics::Result<()> {
        let inner = Adapter::new(Rc::clone(ctx.semantic()), relative_file_path_parts);
        let adapter = Arc::from(&inner);
        if let RulesToRun::Only(this_rule) = rules_to_run {
            for rule in self.rules.iter().filter(|x| x.name == this_rule) {
                self.run_plugin_rules(ctx, rule, Arc::clone(&adapter))?;
            }
        } else {
            for rule in &self.rules {
                self.run_plugin_rules(ctx, rule, Arc::clone(&adapter))?;
            }
        }
        Ok(())
    }
}

fn run_test(
    test: &SingleTest,
    rule_name: &str,
    plugin: &LinterPlugin,
) -> std::result::Result<Vec<Report>, Vec<Report>> {
    let file_path = &test.relative_path.last().expect("there to be atleast 1 path part");
    let source_text = &test.code;

    let allocator = Allocator::default();
    let source_type = SourceType::from_path(file_path).unwrap();
    let ret = Parser::new(&allocator, source_text, source_type).parse();

    // Handle parser errors

    if !ret.errors.is_empty() {
        return Err(ret.errors);
    }

    let program = allocator.alloc(ret.program);
    let SemanticBuilderReturn { semantic, errors } =
        SemanticBuilder::new(source_text, source_type).with_trivias(ret.trivias).build(program);

    assert!(
        errors.is_empty(),
        "In test {rule_name}: Semantic errors: {:?} Code:\n\n{:?}",
        errors,
        test.code
    );

    let semantic = Rc::new(semantic);

    let mut lint_ctx = LintContext::new(&Rc::clone(&semantic));

    let result = plugin.run_tests(
        &mut lint_ctx,
        test.relative_path.iter().map(|el| Some(el.clone())).collect::<Vec<_>>(),
        RulesToRun::Only(rule_name.to_string()),
    );

    if let Some(err) = result.err() {
        return Err(vec![err]);
    }

    Ok(lint_ctx.into_message().into_iter().map(|m| m.error).collect::<Vec<_>>())
}

#[derive(Debug, Error, Diagnostic)]
#[error("Test expected to pass, but failed.")]
struct ExpectedTestToPassButFailed {
    #[source_code]
    query: NamedSource,
    #[label = "This test failed."]
    err_span: SourceSpan,
    #[related]
    errors: Vec<Report>,
}

#[derive(Debug, Error, Diagnostic)]
#[error("Test expected to fail, but passed.")]
struct ExpectedTestToFailButPassed {
    #[source_code]
    query: NamedSource,
    #[label = "This test should have failed but it passed."]
    err_span: SourceSpan,
}

fn span_of_test_n(query_text: &str, test_ix: usize) -> SourceSpan {
    let yaml = YamlLoader::load_from_str(
        // TODO: Should we just save the string after we read it originally?
        query_text,
    )
    .expect("to be able to parse yaml for error reporting");
    let YamlElt::Hash(hash) = &yaml.docs[0].yaml else {unreachable!("must be a top level hashmap in the yaml")};
    let tests_hash_key = hash
        .keys()
        .find(|x| {
            let YamlElt::String(str) = &x.yaml else {return false};
            str == "tests"
        })
        .expect("to be able to find tests hash in yaml file");
    let YamlElt::Hash(tests_hash) = &hash[tests_hash_key].yaml else {unreachable!("there must be a tests hashmap in the yaml")};
    let pass_hash_key = tests_hash
        .keys()
        .find(|x| {
            let YamlElt::String(str) = &x.yaml else {return false};
            str == "pass"
        })
        .expect("to be able to find pass hash in yaml file");
    let YamlElt::Array(passing_test_arr) = &tests_hash[pass_hash_key].yaml else {unreachable!("there must be a pass array in the yaml")};
    let test_hash_span = &passing_test_arr[test_ix].lines_range();
    let start = query_text
        .char_indices()
        .filter(|a| a.1 == '\n')
        .nth(test_hash_span.0 - 1) // subtract one because span is 1-based
        .map(|a| a.0)
        .expect("to find start of span of test");
    let end = query_text
        .char_indices()
        .filter(|a| a.1 == '\n')
        .nth(test_hash_span.1 - 1) // subtract one because span is 1-based
        .map(|a| a.0)
        .expect("to find start of span of test");
    SourceSpan::new(start.into(), (end - start).into())
}

pub fn test_queries(queries_to_test: PathBuf) -> oxc_diagnostics::Result<()> {
    let plugin = LinterPlugin::new(schema(), queries_to_test);

    for rule in &plugin.rules {
        for (ix, test) in rule.tests.pass.iter().enumerate() {
            let diagnostics_collected = run_test(test, &rule.name, &plugin);
            let source = Arc::new(NamedSource::new(
                format!("./{}", test.relative_path.join("/")),
                test.code.clone(),
            ));
            if let Err(errs) = diagnostics_collected {
                let query_text =
                    fs::read_to_string(&rule.path).expect("to be able to get text of rule");

                return Err(ExpectedTestToPassButFailed {
                    errors: errs
                        .into_iter()
                        .map(|e| e.with_source_code(Arc::clone(&source)))
                        .collect(),
                    err_span: span_of_test_n(&query_text, ix),
                    query: NamedSource::new(rule.path.to_string_lossy(), query_text),
                }
                .into());
            }
        }

        for (i, test) in rule.tests.fail.iter().enumerate() {
            let messages = run_test(test, &rule.name, &plugin);
            if messages.is_ok_and(|x| x.is_empty()) {
                let query_text =
                    fs::read_to_string(&rule.path).expect("to be able to get text of rule");

                return Err(ExpectedTestToFailButPassed {
                    err_span: span_of_test_n(&query_text, i),
                    query: NamedSource::new(rule.path.to_string_lossy(), query_text),
                }
                .into());
            }
        }

        if rule.tests.pass.len() + rule.tests.fail.len() > 0 {
            println!(
                "{} passed {} tests successfully.\n",
                rule.name,
                rule.tests.pass.len() + rule.tests.fail.len()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use miette::Result;

    use super::test_queries;

    #[test]
    fn query_tests() -> Result<()> {
        test_queries(Path::new("examples/queries").to_path_buf())?;
        Ok(())
    }
}
