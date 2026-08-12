use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use roller_parser::{
    Block, FunctionDeclaration, Lexer, LibraryDeclaration, LibraryItem, Parser, Program, Span,
    Statement, Token, TokenKind, TopLevelItem,
};
use roller_transpiler::{TranspileError, default_lib_source, emit_program_with_library_paths};
use serde_json::{Value, json};

const COMPLETION_METHOD: u32 = 2;
const COMPLETION_FUNCTION: u32 = 3;
const COMPLETION_FIELD: u32 = 5;
const COMPLETION_VARIABLE: u32 = 6;
const COMPLETION_CLASS: u32 = 7;
const COMPLETION_MODULE: u32 = 9;
const COMPLETION_VALUE: u32 = 12;
const COMPLETION_KEYWORD: u32 = 14;
const COMPLETION_SNIPPET: u32 = 15;
const COMPLETION_TYPE_PARAMETER: u32 = 25;

const SYMBOL_MODULE: u32 = 2;
const SYMBOL_CLASS: u32 = 5;
const SYMBOL_METHOD: u32 = 6;
const SYMBOL_FIELD: u32 = 8;
const SYMBOL_FUNCTION: u32 = 12;
const SYMBOL_CONSTANT: u32 = 14;

#[derive(Debug, Clone)]
struct Definition {
    uri: String,
    range: Value,
}

#[derive(Debug, Clone)]
struct Item {
    label: String,
    kind: u32,
    detail: String,
    documentation: String,
    insert_text: Option<String>,
    definition: Option<Definition>,
}

impl Item {
    fn completion(&self) -> Value {
        let mut value = json!({
            "label": self.label,
            "kind": self.kind,
            "detail": self.detail,
            "documentation": {
                "kind": "markdown",
                "value": self.documentation,
            },
        });
        if let Some(insert_text) = &self.insert_text {
            value["insertText"] = json!(insert_text);
            value["insertTextFormat"] = json!(2);
        }
        value
    }

    fn hover_markdown(&self) -> String {
        let mut markdown = format!("```roller\n{}\n```", self.detail);
        if !self.documentation.is_empty() {
            markdown.push_str("\n\n");
            markdown.push_str(&self.documentation);
        }
        markdown
    }
}

#[derive(Default)]
struct Catalog {
    globals: Vec<Item>,
    namespaces: BTreeMap<String, Vec<Item>>,
    members: Vec<Item>,
}

impl Catalog {
    fn add_namespace(&mut self, namespace: &str, item: Item) {
        self.namespaces
            .entry(namespace.to_string())
            .or_default()
            .push(item);
    }

    fn all_matching<'a>(&'a self, label: &str) -> Vec<&'a Item> {
        self.globals
            .iter()
            .chain(self.members.iter())
            .chain(self.namespaces.values().flatten())
            .filter(|item| item.label == label)
            .collect()
    }
}

pub struct AnalysisContext<'a> {
    pub uri: &'a str,
    pub source: &'a str,
    pub path: Option<&'a Path>,
    pub root: Option<&'a Path>,
}

pub fn diagnostics(context: &AnalysisContext<'_>) -> Vec<Value> {
    let tokens = match Lexer::new(context.source).tokenize() {
        Ok(tokens) => tokens,
        Err(error) => {
            return vec![diagnostic(
                context.source,
                error.span,
                &error.message,
                "lexer",
            )];
        }
    };
    let program = match Parser::new(tokens).parse_program() {
        Ok(program) => program,
        Err(error) => {
            return vec![diagnostic(
                context.source,
                error.span,
                &format!("expected {}, found {}", error.expected, error.actual),
                "parser",
            )];
        }
    };

    let script_name = context
        .path
        .map_or(context.uri.to_string(), |path| path.display().to_string());
    let library_paths = context
        .root
        .map(|root| vec![root.join("lib")])
        .unwrap_or_default();
    match emit_program_with_library_paths(&program, &script_name, &library_paths) {
        Ok(_) => Vec::new(),
        Err(error) => {
            let span = match &error {
                TranspileError::Type { span, .. }
                | TranspileError::Name { span, .. }
                | TranspileError::InvalidOperation { span, .. }
                | TranspileError::ModuleError { span, .. } => *span,
            };
            vec![diagnostic(
                context.source,
                span,
                &error.to_string(),
                "semantic",
            )]
        }
    }
}

pub fn completions(context: &AnalysisContext<'_>, line: u64, character: u64) -> Value {
    let offset = offset_at(context.source, line, character);
    let (word_start, _, prefix) = word_at(context.source, offset);
    let catalog = build_catalog(context);
    let mut items = if import_string_context(context.source, offset) {
        module_name_items(context)
    } else if let Some(namespace) = namespace_before(context.source, word_start) {
        catalog
            .namespaces
            .get(&namespace)
            .cloned()
            .unwrap_or_default()
    } else if dot_before(context.source, word_start) {
        catalog.members.clone()
    } else {
        let mut values = root_items();
        values.extend(catalog.globals.clone());
        values.extend(module_name_items(context));
        values
    };

    if !prefix.is_empty() {
        items.retain(|item| item.label.starts_with(&prefix));
    }
    items.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.detail.cmp(&right.detail))
    });
    items.dedup_by(|left, right| left.label == right.label && left.detail == right.detail);
    json!({
        "isIncomplete": false,
        "items": items.iter().map(Item::completion).collect::<Vec<_>>(),
    })
}

pub fn hover(context: &AnalysisContext<'_>, line: u64, character: u64) -> Value {
    let offset = offset_at(context.source, line, character);
    let (start, end, word) = word_at(context.source, offset);
    if word.is_empty() {
        return Value::Null;
    }
    let catalog = build_catalog(context);
    let matches = if let Some(namespace) = namespace_before(context.source, start) {
        catalog
            .namespaces
            .get(&namespace)
            .into_iter()
            .flatten()
            .filter(|item| item.label == word)
            .collect::<Vec<_>>()
    } else if dot_before(context.source, start) {
        catalog
            .members
            .iter()
            .filter(|item| item.label == word)
            .collect()
    } else {
        catalog.all_matching(&word)
    };

    let markdown = if matches.is_empty() {
        keyword_hover(&word)
    } else {
        Some(
            matches
                .iter()
                .map(|item| item.hover_markdown())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n"),
        )
    };
    let Some(markdown) = markdown else {
        return Value::Null;
    };
    json!({
        "contents": {"kind": "markdown", "value": markdown},
        "range": range_for_offsets(context.source, start, end),
    })
}

pub fn definitions(context: &AnalysisContext<'_>, line: u64, character: u64) -> Value {
    let offset = offset_at(context.source, line, character);
    let (start, _, word) = word_at(context.source, offset);
    if word.is_empty() {
        return Value::Null;
    }
    let catalog = build_catalog(context);
    let matches = if let Some(namespace) = namespace_before(context.source, start) {
        catalog
            .namespaces
            .get(&namespace)
            .into_iter()
            .flatten()
            .filter(|item| item.label == word)
            .collect::<Vec<_>>()
    } else if dot_before(context.source, start) {
        catalog
            .members
            .iter()
            .filter(|item| item.label == word)
            .collect()
    } else {
        catalog.all_matching(&word)
    };
    let mut seen = HashSet::new();
    let locations = matches
        .into_iter()
        .filter_map(|item| item.definition.as_ref())
        .filter(|definition| seen.insert(format!("{}:{}", definition.uri, definition.range)))
        .map(|definition| {
            json!({
                "uri": definition.uri,
                "range": definition.range,
            })
        })
        .collect::<Vec<_>>();
    if locations.is_empty() {
        Value::Null
    } else {
        json!(locations)
    }
}

pub fn document_symbols(context: &AnalysisContext<'_>) -> Value {
    let Ok(tokens) = Lexer::new(context.source).tokenize() else {
        return json!([]);
    };
    let Ok(program) = Parser::new(tokens).parse_program() else {
        return json!([]);
    };
    json!(
        program
            .items
            .iter()
            .filter_map(|item| top_level_symbol(context.source, item))
            .collect::<Vec<_>>()
    )
}

fn diagnostic(source: &str, span: Span, message: &str, code: &str) -> Value {
    json!({
        "range": range(source, span),
        "severity": 1,
        "code": code,
        "source": "roller",
        "message": message,
    })
}

fn build_catalog(context: &AnalysisContext<'_>) -> Catalog {
    let mut catalog = Catalog::default();
    add_builtins(&mut catalog);

    let Ok(tokens) = Lexer::new(context.source).tokenize() else {
        return catalog;
    };
    let imports = recovered_imports(&tokens);
    if let Ok(program) = Parser::new(tokens.clone()).parse_program() {
        collect_program(&mut catalog, &program, context.source, context.uri, true);
    } else {
        collect_recovered_bindings(&mut catalog, &tokens);
    }

    for import in imports {
        if let Some(module) = load_module(&import, context) {
            collect_program(
                &mut catalog,
                &module.program,
                &module.source,
                &module.uri,
                false,
            );
        }
    }
    catalog
}

fn recovered_imports(tokens: &[Token]) -> Vec<String> {
    tokens
        .windows(2)
        .filter_map(|tokens| {
            matches!(tokens[0].kind, TokenKind::Import)
                .then(|| match &tokens[1].kind {
                    TokenKind::String(module) => Some(module.clone()),
                    _ => None,
                })
                .flatten()
        })
        .collect()
}

fn collect_recovered_bindings(catalog: &mut Catalog, tokens: &[Token]) {
    for tokens in tokens.windows(2) {
        let detail = match tokens[0].kind {
            TokenKind::Let => Some("local binding"),
            TokenKind::ForParallel => Some("parallel iteration value"),
            _ => None,
        };
        let (Some(detail), TokenKind::Identifier(name)) = (detail, &tokens[1].kind) else {
            continue;
        };
        catalog.globals.push(Item {
            label: name.clone(),
            kind: COMPLETION_VARIABLE,
            detail: detail.into(),
            documentation: "Recovered from the incomplete Roller document.".into(),
            insert_text: None,
            definition: None,
        });
    }
}

struct LoadedModule {
    source: String,
    uri: String,
    program: Program,
}

fn load_module(name: &str, context: &AnalysisContext<'_>) -> Option<LoadedModule> {
    let mut candidates = Vec::new();
    if let Some(path) = context.path.and_then(Path::parent) {
        candidates.push(path.join("lib").join(format!("{name}.roller")));
    }
    if let Some(root) = context.root {
        candidates.push(root.join("lib").join(format!("{name}.roller")));
    }
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join("lib").join(format!("{name}.roller")));
    }
    for path in candidates {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(tokens) = Lexer::new(&source).tokenize() else {
            continue;
        };
        let Ok(program) = Parser::new(tokens).parse_program() else {
            continue;
        };
        return Some(LoadedModule {
            source,
            uri: path_to_uri(&path),
            program,
        });
    }

    let source = default_lib_source(name)?.to_string();
    let tokens = Lexer::new(&source).tokenize().ok()?;
    let program = Parser::new(tokens).parse_program().ok()?;
    Some(LoadedModule {
        source,
        uri: format!("roller-stdlib:/{name}.roller"),
        program,
    })
}

fn collect_program(
    catalog: &mut Catalog,
    program: &Program,
    source: &str,
    uri: &str,
    include_executable_symbols: bool,
) {
    for item in &program.items {
        match item {
            TopLevelItem::Import(import) if include_executable_symbols => {
                catalog.globals.push(Item {
                    label: import.module.clone(),
                    kind: COMPLETION_MODULE,
                    detail: format!("import \"{}\"", import.module),
                    documentation: "Imported Roller library.".into(),
                    insert_text: None,
                    definition: None,
                });
            }
            TopLevelItem::Constant(constant) if include_executable_symbols => {
                catalog.globals.push(declared_item(
                    &constant.name,
                    COMPLETION_VALUE,
                    format!("#define {}", constant.name),
                    "Roller constant.",
                    source,
                    uri,
                    constant.span,
                ));
            }
            TopLevelItem::Section(section) if include_executable_symbols => {
                catalog.globals.push(declared_item(
                    &section.name,
                    COMPLETION_FUNCTION,
                    format!(
                        "section {}({})",
                        section.name,
                        parameters(&section.parameters)
                    ),
                    "CLI-callable Roller section.",
                    source,
                    uri,
                    section.span,
                ));
                for parameter in &section.parameters {
                    catalog.globals.push(declared_item(
                        &parameter.name,
                        COMPLETION_VARIABLE,
                        format!("{}: {}", parameter.name, parameter.type_name),
                        "Section parameter.",
                        source,
                        uri,
                        parameter.span,
                    ));
                }
                collect_block_bindings(catalog, &section.body, source, uri);
            }
            TopLevelItem::Library(library) => {
                if include_executable_symbols {
                    catalog.globals.push(declared_item(
                        &library.name,
                        COMPLETION_MODULE,
                        format!("library \"{}\"", library.name),
                        "Inline Roller library.",
                        source,
                        uri,
                        library.span,
                    ));
                }
                collect_library(catalog, library, source, uri);
            }
            _ => {}
        }
    }
}

fn collect_library(catalog: &mut Catalog, library: &LibraryDeclaration, source: &str, uri: &str) {
    for item in &library.items {
        match item {
            LibraryItem::Function(function) => {
                catalog.add_namespace(&library.name, function_item(function, source, uri, false));
            }
            LibraryItem::Compiler(compiler) => {
                catalog.add_namespace(
                    &library.name,
                    declared_item(
                        &compiler.name,
                        COMPLETION_CLASS,
                        format!("compiler {}", compiler.name),
                        "Concrete Compiler implementation.",
                        source,
                        uri,
                        compiler.span,
                    ),
                );
                for field in &compiler.fields {
                    catalog.members.push(declared_item(
                        &field.name,
                        COMPLETION_FIELD,
                        format!("{}: {}", field.name, field.type_name),
                        &format!("Field of `{}::{}`.", library.name, compiler.name),
                        source,
                        uri,
                        field.span,
                    ));
                }
            }
            LibraryItem::Implement(implementation) => {
                for method in &implementation.methods {
                    let mut item = function_item(method, source, uri, true);
                    item.documentation = format!(
                        "Method implemented by `{}::{}`.{}",
                        library.name,
                        implementation.compiler_name,
                        if method.is_parallelable {
                            " May be scheduled with `parallel`."
                        } else {
                            ""
                        }
                    );
                    catalog.members.push(item);
                }
            }
        }
    }
}

fn collect_block_bindings(catalog: &mut Catalog, block: &Block, source: &str, uri: &str) {
    for statement in &block.statements {
        match statement {
            Statement::Let { name, span, .. } => catalog.globals.push(declared_item(
                name,
                COMPLETION_VARIABLE,
                format!("let {name}"),
                "Local binding.",
                source,
                uri,
                *span,
            )),
            Statement::ForParallel {
                binding,
                body,
                span,
                ..
            } => {
                catalog.globals.push(declared_item(
                    binding,
                    COMPLETION_VARIABLE,
                    format!("for-parallel {binding}"),
                    "Dynamically typed parallel iteration value.",
                    source,
                    uri,
                    *span,
                ));
                collect_block_bindings(catalog, body, source, uri);
            }
            Statement::If {
                then_block,
                else_block,
                ..
            } => {
                collect_block_bindings(catalog, then_block, source, uri);
                if let Some(block) = else_block {
                    collect_block_bindings(catalog, block, source, uri);
                }
            }
            _ => {}
        }
    }
}

fn function_item(function: &FunctionDeclaration, source: &str, uri: &str, method: bool) -> Item {
    let visible_parameters = if method && !function.parameters.is_empty() {
        &function.parameters[1..]
    } else {
        &function.parameters
    };
    let result = function
        .return_type
        .as_deref()
        .map_or(String::new(), |value| format!(" -> {value}"));
    let prefix = if function.is_parallelable {
        "paralleable function"
    } else {
        "function"
    };
    let signature = format!(
        "{prefix} {}({}){result}",
        function.name,
        parameters(visible_parameters)
    );
    let placeholders = visible_parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| format!("${{{}:{}}}", index + 1, parameter.name))
        .collect::<Vec<_>>()
        .join(", ");
    Item {
        label: function.name.clone(),
        kind: if method {
            COMPLETION_METHOD
        } else {
            COMPLETION_FUNCTION
        },
        detail: signature,
        documentation: if method {
            "Compiler implementation method.".into()
        } else {
            "Roller library function.".into()
        },
        insert_text: Some(format!("{}({placeholders})", function.name)),
        definition: Some(definition(source, uri, function.span, &function.name)),
    }
}

fn declared_item(
    name: &str,
    kind: u32,
    detail: String,
    documentation: &str,
    source: &str,
    uri: &str,
    span: Span,
) -> Item {
    Item {
        label: name.to_string(),
        kind,
        detail,
        documentation: documentation.to_string(),
        insert_text: None,
        definition: Some(definition(source, uri, span, name)),
    }
}

fn definition(source: &str, uri: &str, span: Span, name: &str) -> Definition {
    let (start, end) = find_name_offsets(source, span, name).unwrap_or((
        span.start.offset.min(source.len()),
        span.end.offset.min(source.len()),
    ));
    Definition {
        uri: uri.to_string(),
        range: range_for_offsets(source, start, end),
    }
}

fn parameters(parameters: &[roller_parser::Parameter]) -> String {
    parameters
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, parameter.type_name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn add_builtins(catalog: &mut Catalog) {
    for (name, documentation) in [
        ("sys", "Generic operating-system primitives."),
        ("log", "Build logging."),
        ("roller", "Roller execution control."),
        ("process", "Process compatibility namespace."),
        ("dir", "Directory compatibility namespace."),
        ("Compiler", "Core dynamically selected compiler contract."),
    ] {
        catalog.globals.push(Item {
            label: name.into(),
            kind: COMPLETION_MODULE,
            detail: format!("namespace {name}"),
            documentation: documentation.into(),
            insert_text: None,
            definition: None,
        });
    }

    for namespace in ["cmd", "process", "fs", "path", "str", "env", "io", "time"] {
        catalog.add_namespace(
            "sys",
            Item {
                label: namespace.into(),
                kind: COMPLETION_MODULE,
                detail: format!("namespace sys::{namespace}"),
                documentation: "Generic system API namespace.".into(),
                insert_text: None,
                definition: None,
            },
        );
    }

    for &(namespace, name, signature, documentation) in builtin_functions() {
        catalog.add_namespace(
            namespace,
            Item {
                label: name.into(),
                kind: COMPLETION_FUNCTION,
                detail: signature.into(),
                documentation: documentation.into(),
                insert_text: Some(snippet_for_signature(name, signature)),
                definition: None,
            },
        );
    }
    for (name, signature, documentation) in [
        (
            "push",
            "push(value)",
            "Append one dynamic value to an array.",
        ),
        (
            "push_str",
            "push_str(value)",
            "Append one value to an array.",
        ),
        (
            "push_vec",
            "push_vec(values)",
            "Append all values from another array.",
        ),
        (
            "copy",
            "copy() -> Vec<any>",
            "Create an independent array container.",
        ),
        (
            "is_empty",
            "is_empty() -> bool",
            "Test whether a string or array is empty.",
        ),
        (
            "join",
            "join(separator: String) -> String",
            "Join an array of strings.",
        ),
    ] {
        catalog.members.push(Item {
            label: name.into(),
            kind: COMPLETION_METHOD,
            detail: signature.into(),
            documentation: documentation.into(),
            insert_text: Some(snippet_for_signature(name, signature)),
            definition: None,
        });
    }
}

fn builtin_functions() -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
    &[
        (
            "Compiler",
            "new",
            "Compiler::new() -> Compiler",
            "Create an unselected Compiler value.",
        ),
        (
            "Compiler",
            "AVAILABLE",
            "Compiler::AVAILABLE",
            "Successful compiler discovery status.",
        ),
        (
            "Compiler",
            "UNAVAILABLE",
            "Compiler::UNAVAILABLE",
            "Unavailable compiler status.",
        ),
        (
            "Compiler",
            "NOTFOUND",
            "Compiler::NOTFOUND",
            "Alias of the unavailable status.",
        ),
        (
            "log",
            "info",
            "log::info(message: String)",
            "Write an informational message.",
        ),
        (
            "log",
            "error",
            "log::error(message: String)",
            "Write an error message.",
        ),
        (
            "log",
            "err",
            "log::err(message: String)",
            "Alias of `log::error`.",
        ),
        (
            "roller",
            "exit",
            "roller::exit(code: integer)",
            "Stop the requested section with an exit code.",
        ),
        (
            "roller",
            "set_parallel_jobs",
            "roller::set_parallel_jobs(jobs: integer)",
            "Set bounded parallelism.",
        ),
        (
            "dir",
            "recursive",
            "dir::recursive(path: String) -> Vec<String>",
            "List all regular files recursively in deterministic path order.",
        ),
        (
            "process",
            "run",
            "process::run(program: String, args?: Vec<String>)",
            "Run a process without using a shell.",
        ),
        (
            "sys::cmd",
            "which",
            "sys::cmd::which(name: String) -> String",
            "Resolve an executable through PATH.",
        ),
        (
            "sys::cmd",
            "is_exists",
            "sys::cmd::is_exists(name: String) -> bool",
            "Check whether an executable exists.",
        ),
        (
            "sys::process",
            "run",
            "sys::process::run(program: String, args: Vec<String>)",
            "Run a process.",
        ),
        (
            "sys::process",
            "output",
            "sys::process::output(program: String, args: Vec<String>) -> Vec<any>",
            "Run and return `[status, stdout, stderr]`.",
        ),
        (
            "sys::process",
            "status",
            "sys::process::status(program: String, args: Vec<String>) -> integer",
            "Run and return the exit status.",
        ),
        (
            "sys::process",
            "spawn",
            "sys::process::spawn(program: String, args: Vec<String>) -> integer",
            "Spawn a background process and return its PID.",
        ),
        (
            "sys::process",
            "wait",
            "sys::process::wait(pid: integer) -> integer",
            "Wait for a process.",
        ),
        (
            "sys::process",
            "kill",
            "sys::process::kill(pid: integer, signal: integer) -> integer",
            "Send a signal to a process.",
        ),
        (
            "sys::path",
            "join",
            "sys::path::join(base: String, child: String) -> String",
            "Join paths.",
        ),
        (
            "sys::path",
            "replace_extension",
            "sys::path::replace_extension(path: String, extension: String) -> String",
            "Replace a path extension.",
        ),
        (
            "sys::path",
            "extension",
            "sys::path::extension(path: String) -> String",
            "Return an extension without the dot.",
        ),
        (
            "sys::str",
            "concat",
            "sys::str::concat(left: String, right: String) -> String",
            "Concatenate strings.",
        ),
        (
            "sys::str",
            "contains",
            "sys::str::contains(value: String, needle: String) -> bool",
            "Test substring containment.",
        ),
        (
            "sys::fs",
            "read",
            "sys::fs::read(path: String) -> String",
            "Read a complete file.",
        ),
        (
            "sys::fs",
            "write",
            "sys::fs::write(path: String, contents: String)",
            "Write a complete file.",
        ),
        (
            "sys::fs",
            "exists",
            "sys::fs::exists(path: String) -> bool",
            "Check path existence.",
        ),
        (
            "sys::fs",
            "is_file",
            "sys::fs::is_file(path: String) -> bool",
            "Check for a regular file.",
        ),
        (
            "sys::fs",
            "is_dir",
            "sys::fs::is_dir(path: String) -> bool",
            "Check for a directory.",
        ),
        (
            "sys::fs",
            "size",
            "sys::fs::size(path: String) -> integer",
            "Return file size.",
        ),
        (
            "sys::fs",
            "mtime",
            "sys::fs::mtime(path: String) -> integer",
            "Return modification time.",
        ),
        (
            "sys::fs",
            "mkdir",
            "sys::fs::mkdir(path: String)",
            "Create one directory.",
        ),
        (
            "sys::fs",
            "mkdir_all",
            "sys::fs::mkdir_all(path: String)",
            "Create a directory tree.",
        ),
        (
            "sys::fs",
            "mkdir_parent",
            "sys::fs::mkdir_parent(path: String)",
            "Create a path's parent directories.",
        ),
        (
            "sys::fs",
            "remove_file",
            "sys::fs::remove_file(path: String)",
            "Remove one file.",
        ),
        (
            "sys::fs",
            "remove_dir",
            "sys::fs::remove_dir(path: String)",
            "Remove a directory tree.",
        ),
        (
            "sys::fs",
            "rename",
            "sys::fs::rename(from: String, to: String)",
            "Rename a path.",
        ),
        (
            "sys::fs",
            "copy",
            "sys::fs::copy(from: String, to: String)",
            "Copy a file.",
        ),
        (
            "sys::fs",
            "read_dir",
            "sys::fs::read_dir(path: String) -> Vec<String>",
            "List a directory.",
        ),
        (
            "sys::fs",
            "find_recursive",
            "sys::fs::find_recursive(path: String) -> Vec<String>",
            "List all regular files recursively.",
        ),
        (
            "sys::env",
            "get",
            "sys::env::get(name: String) -> String",
            "Read an environment variable.",
        ),
        (
            "sys::env",
            "set",
            "sys::env::set(name: String, value: String)",
            "Set an environment variable.",
        ),
        (
            "sys::env",
            "cwd",
            "sys::env::cwd() -> String",
            "Return the current directory.",
        ),
        (
            "sys::env",
            "chdir",
            "sys::env::chdir(path: String)",
            "Change the current directory.",
        ),
        (
            "sys::env",
            "args",
            "sys::env::args() -> Vec<String>",
            "Return process arguments.",
        ),
        (
            "sys::io",
            "read_line",
            "sys::io::read_line() -> String",
            "Read one input line.",
        ),
        (
            "sys::io",
            "print",
            "sys::io::print(value: String)",
            "Write to stdout.",
        ),
        (
            "sys::io",
            "eprint",
            "sys::io::eprint(value: String)",
            "Write to stderr.",
        ),
        ("sys::io", "flush", "sys::io::flush()", "Flush stdout."),
        (
            "sys::time",
            "sleep",
            "sys::time::sleep(milliseconds: integer)",
            "Sleep for a duration.",
        ),
        (
            "sys::time",
            "now_ms",
            "sys::time::now_ms() -> integer",
            "Return the current Unix time in milliseconds.",
        ),
    ]
}

fn root_items() -> Vec<Item> {
    let mut items = Vec::new();
    for (label, insert_text, documentation) in [
        (
            "import",
            "import \"${1:library}\"",
            "Import a Roller library.",
        ),
        (
            "section",
            "section ${1:name}(${2}) {\n    ${0}\n}",
            "Declare a CLI-callable section.",
        ),
        (
            "library",
            "library \"${1:name}\" {\n    ${0}\n}",
            "Declare an inline library.",
        ),
        (
            "compiler",
            "compiler ${1:name} {\n    ${0}\n}",
            "Declare implementation-local Compiler state.",
        ),
        (
            "implement",
            "implement Self::${1:compiler} {\n    ${0}\n}",
            "Implement Compiler methods.",
        ),
        (
            "function",
            "function ${1:name}(${2}) {\n    ${0}\n}",
            "Declare a function.",
        ),
        (
            "paralleable",
            "paralleable function ${1:name}(${2}) {\n    ${0}\n}",
            "Declare a schedulable Compiler method.",
        ),
        ("let", "let ${1:name} = ${0};", "Create a local binding."),
        (
            "if",
            "if ${1:condition} {\n    ${0}\n}",
            "Conditional execution.",
        ),
        (
            "else",
            "else {\n    ${0}\n}",
            "Alternative conditional branch.",
        ),
        (
            "for-parallel",
            "for-parallel ${1:item} in ${2:values} {\n    ${0}\n}",
            "Iterate dynamic values with a bounded job queue.",
        ),
        (
            "parallel",
            "parallel ${1:compiler}.${2:method}(${0});",
            "Schedule a `paralleable` Compiler method.",
        ),
        ("return", "return ${0};", "Return a value."),
        (
            "#define",
            "#define ${1:NAME} ${0}",
            "Declare a Roller constant.",
        ),
    ] {
        items.push(Item {
            label: label.into(),
            kind: COMPLETION_SNIPPET,
            detail: format!("keyword {label}"),
            documentation: documentation.into(),
            insert_text: Some(insert_text.into()),
            definition: None,
        });
    }
    for type_name in [
        "String",
        "integer",
        "bool",
        "Vec<String>",
        "Compiler",
        "CompilerStatus",
    ] {
        items.push(Item {
            label: type_name.into(),
            kind: COMPLETION_TYPE_PARAMETER,
            detail: format!("type {type_name}"),
            documentation: "Roller type annotation.".into(),
            insert_text: None,
            definition: None,
        });
    }
    for value in ["true", "false"] {
        items.push(Item {
            label: value.into(),
            kind: COMPLETION_KEYWORD,
            detail: format!("boolean {value}"),
            documentation: "Boolean literal.".into(),
            insert_text: None,
            definition: None,
        });
    }
    items
}

fn module_name_items(context: &AnalysisContext<'_>) -> Vec<Item> {
    let mut names = vec!["gcc".to_string(), "clang".to_string(), "zig".to_string()];
    let mut directories = Vec::new();
    if let Some(parent) = context.path.and_then(Path::parent) {
        directories.push(parent.join("lib"));
    }
    if let Some(root) = context.root {
        directories.push(root.join("lib"));
    }
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("roller")
                && let Some(stem) = path.file_stem().and_then(|value| value.to_str())
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| Item {
            label: name.clone(),
            kind: COMPLETION_MODULE,
            detail: format!("Roller library {name}"),
            documentation: "Available `.roller` library.".into(),
            insert_text: None,
            definition: None,
        })
        .collect()
}

fn snippet_for_signature(name: &str, signature: &str) -> String {
    let Some(open) = signature.find('(') else {
        return name.to_string();
    };
    let Some(close_relative) = signature[open + 1..].find(')') else {
        return name.to_string();
    };
    let arguments = &signature[open + 1..open + 1 + close_relative];
    if arguments.trim().is_empty() {
        return format!("{name}()");
    }
    let placeholders = arguments
        .split(',')
        .enumerate()
        .map(|(index, argument)| {
            let name = argument
                .trim()
                .split_once(':')
                .map_or(argument.trim(), |(name, _)| name)
                .trim_end_matches('?');
            format!("${{{}:{name}}}", index + 1)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({placeholders})")
}

fn keyword_hover(word: &str) -> Option<String> {
    root_items()
        .into_iter()
        .find(|item| item.label == word)
        .map(|item| item.hover_markdown())
}

fn top_level_symbol(source: &str, item: &TopLevelItem) -> Option<Value> {
    match item {
        TopLevelItem::Import(_) => None,
        TopLevelItem::Constant(constant) => Some(document_symbol(
            source,
            &constant.name,
            Some("constant"),
            SYMBOL_CONSTANT,
            constant.span,
            Vec::new(),
        )),
        TopLevelItem::Section(section) => Some(document_symbol(
            source,
            &section.name,
            Some("section"),
            SYMBOL_FUNCTION,
            section.span,
            Vec::new(),
        )),
        TopLevelItem::Library(library) => {
            let children = library
                .items
                .iter()
                .map(|item| match item {
                    LibraryItem::Function(function) => document_symbol(
                        source,
                        &function.name,
                        function.return_type.as_deref(),
                        SYMBOL_FUNCTION,
                        function.span,
                        Vec::new(),
                    ),
                    LibraryItem::Compiler(compiler) => document_symbol(
                        source,
                        &compiler.name,
                        Some("compiler"),
                        SYMBOL_CLASS,
                        compiler.span,
                        compiler
                            .fields
                            .iter()
                            .map(|field| {
                                document_symbol(
                                    source,
                                    &field.name,
                                    Some(&field.type_name),
                                    SYMBOL_FIELD,
                                    field.span,
                                    Vec::new(),
                                )
                            })
                            .collect(),
                    ),
                    LibraryItem::Implement(implementation) => document_symbol(
                        source,
                        &format!("Self::{}", implementation.compiler_name),
                        Some("implementation"),
                        SYMBOL_CLASS,
                        implementation.span,
                        implementation
                            .methods
                            .iter()
                            .map(|method| {
                                document_symbol(
                                    source,
                                    &method.name,
                                    method.return_type.as_deref(),
                                    SYMBOL_METHOD,
                                    method.span,
                                    Vec::new(),
                                )
                            })
                            .collect(),
                    ),
                })
                .collect();
            Some(document_symbol(
                source,
                &library.name,
                Some("library"),
                SYMBOL_MODULE,
                library.span,
                children,
            ))
        }
    }
}

fn document_symbol(
    source: &str,
    name: &str,
    detail: Option<&str>,
    kind: u32,
    span: Span,
    children: Vec<Value>,
) -> Value {
    let selection_range = find_name_offsets(source, span, name).map_or_else(
        || range(source, span),
        |(start, end)| range_for_offsets(source, start, end),
    );
    let mut symbol = json!({
        "name": name,
        "kind": kind,
        "range": range(source, span),
        "selectionRange": selection_range,
    });
    if let Some(detail) = detail {
        symbol["detail"] = json!(detail);
    }
    if !children.is_empty() {
        symbol["children"] = json!(children);
    }
    symbol
}

fn find_name_offsets(source: &str, span: Span, name: &str) -> Option<(usize, usize)> {
    let start = span.start.offset.min(source.len());
    let end = span.end.offset.min(source.len());
    let segment = source.get(start..end)?;
    let needle = name.rsplit("::").next().unwrap_or(name);
    let relative = segment.find(needle)?;
    Some((start + relative, start + relative + needle.len()))
}

fn import_string_context(source: &str, offset: usize) -> bool {
    let line_start = source[..offset.min(source.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = source[line_start..offset.min(source.len())].trim_start();
    let Some(rest) = line.strip_prefix("import") else {
        return false;
    };
    let rest = rest.trim_start();
    rest.starts_with('"') && rest[1..].find('"').is_none()
}

fn namespace_before(source: &str, word_start: usize) -> Option<String> {
    let before = source.get(..word_start)?.trim_end();
    let namespace = before.strip_suffix("::")?;
    let start = namespace
        .char_indices()
        .rev()
        .find(|(_, character)| {
            !(character.is_ascii_alphanumeric()
                || *character == '_'
                || *character == '-'
                || *character == ':')
        })
        .map_or(0, |(index, character)| index + character.len_utf8());
    let value = namespace[start..].trim_matches(':');
    (!value.is_empty()).then(|| value.to_string())
}

fn dot_before(source: &str, word_start: usize) -> bool {
    source
        .get(..word_start)
        .is_some_and(|before| before.trim_end().ends_with('.'))
}

fn word_at(source: &str, offset: usize) -> (usize, usize, String) {
    let offset = floor_char_boundary(source, offset.min(source.len()));
    let mut start = offset;
    while start > 0 {
        let Some(character) = source[..start].chars().next_back() else {
            break;
        };
        if !(character.is_ascii_alphanumeric() || character == '_' || character == '-') {
            break;
        }
        start -= character.len_utf8();
    }
    let mut end = offset;
    while end < source.len() {
        let Some(character) = source[end..].chars().next() else {
            break;
        };
        if !(character.is_ascii_alphanumeric() || character == '_' || character == '-') {
            break;
        }
        end += character.len_utf8();
    }
    (start, end, source[start..end].to_string())
}

pub fn offset_at(source: &str, line: u64, utf16_character: u64) -> usize {
    let mut offset = 0;
    for _ in 0..line {
        let Some(relative) = source[offset..].find('\n') else {
            return source.len();
        };
        offset += relative + 1;
    }
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |relative| offset + relative);
    let mut utf16 = 0_u64;
    for character in source[offset..line_end].chars() {
        let width = u64::from(character.len_utf16() as u16);
        if utf16 + width > utf16_character {
            break;
        }
        utf16 += width;
        offset += character.len_utf8();
        if utf16 == utf16_character {
            break;
        }
    }
    offset
}

fn range(source: &str, span: Span) -> Value {
    range_for_offsets(
        source,
        span.start.offset.min(source.len()),
        span.end.offset.min(source.len()),
    )
}

fn range_for_offsets(source: &str, start: usize, end: usize) -> Value {
    json!({
        "start": lsp_position(source, start),
        "end": lsp_position(source, end),
    })
}

fn lsp_position(source: &str, offset: usize) -> Value {
    let offset = floor_char_boundary(source, offset.min(source.len()));
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let character = source[line_start..offset].encode_utf16().count();
    json!({"line": line, "character": character})
}

fn floor_char_boundary(source: &str, mut offset: usize) -> usize {
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn path_to_uri(path: &Path) -> String {
    let absolute = path.canonicalize().unwrap_or_else(|_| PathBuf::from(path));
    let path = absolute.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn context(source: &str) -> AnalysisContext<'_> {
        AnalysisContext {
            uri: "file:///tmp/build.roller",
            source,
            path: Some(Path::new("/tmp/build.roller")),
            root: None,
        }
    }

    #[test]
    fn syntax_and_semantic_errors_become_diagnostics() {
        let syntax = diagnostics(&context("section build() { let x = 1 }"));
        assert_eq!(syntax[0]["code"], "parser");

        let semantic = diagnostics(&context("section build() { missing::call(); }"));
        assert_eq!(semantic[0]["code"], "semantic");
    }

    #[test]
    fn imported_compiler_methods_drive_member_completion() {
        let source = "import \"zig\"\nsection build() { let z = Compiler::new(); z. }";
        let line_start = source.find('\n').unwrap() + 1;
        let position = source.find("z. }").unwrap() + 2 - line_start;
        let result = completions(&context(source), 1, position as u64);
        let labels = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"compile"));
        assert!(labels.contains(&"optimize"));
        assert!(labels.contains(&"objects"));
    }

    #[test]
    fn namespace_completion_uses_library_declarations() {
        let source = "import \"zig\"\nsection build() { zig:: }";
        let line_start = source.find('\n').unwrap() + 1;
        let position = source.find("zig:: }").unwrap() + 5 - line_start;
        let result = completions(&context(source), 1, position as u64);
        let labels = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"get_compiler"));
        assert!(labels.contains(&"version"));
    }

    #[test]
    fn deep_sys_namespace_completion_is_available() {
        let source = "section build() { let ext = sys::path:: }";
        let position = source.find("sys::path::").unwrap() + "sys::path::".len();
        let result = completions(&context(source), 0, position as u64);
        let labels = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"extension"));
        assert!(labels.contains(&"replace_extension"));
    }

    #[test]
    fn hover_and_definition_use_declared_library_signatures() {
        let source = "library \"tool\" { function version() -> String { return \"1\"; } }\nsection build() { tool::version(); }";
        let line = "section build() { tool::version(); }";
        let character = line.find("version").unwrap() + 2;

        let hovered = hover(&context(source), 1, character as u64);
        assert!(
            hovered["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("function version() -> String")
        );

        let locations = definitions(&context(source), 1, character as u64);
        assert_eq!(locations[0]["uri"], "file:///tmp/build.roller");
        assert_eq!(locations[0]["range"]["start"]["line"], 0);
    }

    #[test]
    fn utf16_positions_are_converted_without_splitting_characters() {
        let source = "😀x\nvalue";
        assert_eq!(offset_at(source, 0, 2), "😀".len());
        assert_eq!(offset_at(source, 1, 3), "😀x\nval".len());
    }

    #[test]
    fn document_symbols_include_compiler_children() {
        let source = r#"library "x" {
            compiler tool { mode: String }
            implement Self::tool { function compile(c: self, input: String) {} }
        }"#;
        let symbols = document_symbols(&context(source));
        let library = &symbols.as_array().unwrap()[0];
        assert_eq!(library["name"], "x");
        assert!(library["children"].as_array().unwrap().len() >= 2);
    }
}
