//! AST → C code emission with module loading and sys:: primitives.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use roller_parser::{
    BinaryOperator, Block, CompilerDeclaration, Expression, ExpressionKind, FunctionDeclaration,
    Lexer, LibraryItem, Parser as RollerParser, Program, Span, Statement, TopLevelItem,
};

use crate::default_lib_source;

/// Errors during transpilation.
#[derive(Debug, thiserror::Error)]
pub enum TranspileError {
    #[error("{message}")]
    Type { message: String, span: Span },
    #[error("{message}")]
    Name { message: String, span: Span },
    #[error("{message}")]
    InvalidOperation { message: String, span: Span },
    #[error("{message}")]
    ModuleError { message: String, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarType {
    Unknown,
    Compiler,
    Integer,
    Boolean,
    CString,
    Array,
    CompilerStatus,
    Unit,
}

#[derive(Debug, Clone)]
struct MethodBinding {
    implementation: String,
    function_name: String,
    argument_count: usize,
    return_type: VarType,
    parallelable: bool,
}

/// Module registry for loaded Roller library files.
struct ModuleRegistry {
    modules: HashMap<String, Program>,
    loading: HashSet<String>,
    lib_dirs: Vec<PathBuf>,
    inline_modules: HashSet<String>,
    /// Dynamically dispatched methods supplied by concrete compilers.
    compiler_methods: HashMap<String, Vec<MethodBinding>>,
    /// Return types of module functions.
    function_returns: HashMap<(String, String), VarType>,
    /// Concrete compilers declared by each library.
    compilers: HashSet<(String, String)>,
}

impl ModuleRegistry {
    fn new(lib_dirs: Vec<PathBuf>) -> Self {
        Self {
            modules: HashMap::new(),
            loading: HashSet::new(),
            lib_dirs,
            inline_modules: HashSet::new(),
            compiler_methods: HashMap::new(),
            function_returns: HashMap::new(),
            compilers: HashSet::new(),
        }
    }

    fn register_inline(&mut self, name: &str) {
        self.inline_modules.insert(name.to_string());
    }

    fn register_program_metadata(&mut self, program: &Program) -> Result<(), TranspileError> {
        for item in &program.items {
            let TopLevelItem::Library(library) = item else {
                continue;
            };
            for item in &library.items {
                match item {
                    LibraryItem::Compiler(compiler) => {
                        self.compilers
                            .insert((library.name.clone(), compiler.name.clone()));
                    }
                    LibraryItem::Implement(implementation) => {
                        let implementation_name =
                            format!("{}::{}", library.name, implementation.compiler_name);
                        for method in &implementation.methods {
                            let Some(argument_count) = method.parameters.len().checked_sub(1)
                            else {
                                return Err(TranspileError::InvalidOperation {
                                    message: format!(
                                        "compiler method `{}` requires a receiver parameter",
                                        method.name
                                    ),
                                    span: method.span,
                                });
                            };
                            self.compiler_methods
                                .entry(method.name.clone())
                                .or_default()
                                .push(MethodBinding {
                                    implementation: implementation_name.clone(),
                                    function_name: format!(
                                        "r_lib_impl_{}_{}_{}",
                                        sid(&library.name),
                                        sid(&implementation.compiler_name),
                                        sid(&method.name)
                                    ),
                                    argument_count,
                                    return_type: method
                                        .return_type
                                        .as_deref()
                                        .map_or(VarType::Unit, type_from_name),
                                    parallelable: method.is_parallelable,
                                });
                        }
                    }
                    LibraryItem::Function(function) => {
                        self.function_returns.insert(
                            (library.name.clone(), function.name.clone()),
                            function
                                .return_type
                                .as_deref()
                                .map_or(VarType::Unit, type_from_name),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn method_bindings(&self, name: &str) -> Option<&[MethodBinding]> {
        self.compiler_methods.get(name).map(Vec::as_slice)
    }

    fn method_return_type(&self, name: &str) -> VarType {
        let Some(methods) = self.compiler_methods.get(name) else {
            return VarType::Unknown;
        };
        let Some(first) = methods.first() else {
            return VarType::Unknown;
        };
        if methods
            .iter()
            .all(|method| method.return_type == first.return_type)
        {
            first.return_type
        } else {
            VarType::Unknown
        }
    }

    fn has_parallelable_method(&self, name: &str) -> bool {
        self.compiler_methods
            .get(name)
            .is_some_and(|methods| methods.iter().any(|method| method.parallelable))
    }

    fn function_return_type(&self, module: &str, name: &str) -> VarType {
        self.function_returns
            .get(&(module.to_string(), name.to_string()))
            .copied()
            .unwrap_or(VarType::Unknown)
    }

    fn has_compiler(&self, module: &str, name: &str) -> bool {
        self.compilers
            .contains(&(module.to_string(), name.to_string()))
    }

    fn load_module(&mut self, name: &str) -> Result<(), TranspileError> {
        if self.modules.contains_key(name) {
            return Ok(());
        }
        if self.loading.contains(name) {
            return Err(TranspileError::ModuleError {
                message: format!("circular import: {}", name),
                span: dummy_span(),
            });
        }
        self.loading.insert(name.to_string());
        let source = if let Some(src) = default_lib_source(name) {
            src.to_string()
        } else {
            let mut found = None;
            for dir in &self.lib_dirs {
                let path = dir.join(format!("{}.roller", name));
                if path.exists() {
                    found = Some(std::fs::read_to_string(&path).map_err(|e| {
                        TranspileError::ModuleError {
                            message: format!("cannot read {}: {}", path.display(), e),
                            span: dummy_span(),
                        }
                    })?);
                    break;
                }
            }
            found.ok_or_else(|| TranspileError::ModuleError {
                message: format!("module not found: {}", name),
                span: dummy_span(),
            })?
        };
        let tokens = Lexer::new(&source)
            .tokenize()
            .map_err(|e| TranspileError::ModuleError {
                message: format!("lexer error in {}: {}", name, e),
                span: e.span,
            })?;
        let program =
            RollerParser::new(tokens)
                .parse_program()
                .map_err(|e| TranspileError::ModuleError {
                    message: format!("parse error in {}: {}", name, e),
                    span: e.span,
                })?;
        for item in &program.items {
            if let TopLevelItem::Import(imp) = item {
                self.load_module(&imp.module)?;
            }
        }
        self.modules.insert(name.to_string(), program);
        self.loading.remove(name);
        Ok(())
    }

    fn has_module(&self, name: &str) -> bool {
        self.modules.contains_key(name) || self.inline_modules.contains(name)
    }
}

fn dummy_span() -> Span {
    Span {
        start: Default::default(),
        end: Default::default(),
    }
}

struct Scope {
    bindings: Vec<(String, VarType, bool)>,
}
struct ScopeStack {
    scopes: Vec<Scope>,
    current_module: Option<String>,
}

impl ScopeStack {
    fn new() -> Self {
        Self {
            scopes: vec![Scope {
                bindings: Vec::new(),
            }],
            current_module: None,
        }
    }
    fn push(&mut self) {
        self.scopes.push(Scope {
            bindings: Vec::new(),
        });
    }
    fn pop(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }
    fn define(&mut self, n: &str, t: VarType) {
        if let Some(s) = self.scopes.last_mut() {
            s.bindings.push((n.into(), t, false));
        }
    }
    fn define_raw(&mut self, n: &str, t: VarType) {
        if let Some(s) = self.scopes.last_mut() {
            s.bindings.push((n.into(), t, true));
        }
    }
    fn get_type(&self, n: &str) -> VarType {
        for s in self.scopes.iter().rev() {
            if let Some((_, t, _)) = s.bindings.iter().find(|(k, _, _)| k == n) {
                return *t;
            }
        }
        VarType::Unknown
    }
    fn is_raw_c(&self, n: &str) -> bool {
        for s in self.scopes.iter().rev() {
            if let Some((_, _, r)) = s.bindings.iter().find(|(k, _, _)| k == n) {
                return *r;
            }
        }
        false
    }

    fn set_current_module(&mut self, module: Option<&str>) {
        self.current_module = module.map(str::to_owned);
    }

    fn current_module(&self) -> Option<&str> {
        self.current_module.as_deref()
    }
}

fn type_from_name(name: &str) -> VarType {
    match name {
        "Compiler" | "self" | "Self" => VarType::Compiler,
        "String" | "string" => VarType::CString,
        "integer" | "int" => VarType::Integer,
        "bool" | "Boolean" => VarType::Boolean,
        "CompilerStatus" => VarType::CompilerStatus,
        value if value.starts_with("Vec") || value == "Array" => VarType::Array,
        _ => VarType::Unknown,
    }
}

/// Emit a complete C program from a Roller AST.
pub fn emit_program(program: &Program, script_name: &str) -> Result<String, TranspileError> {
    emit_program_with_library_paths(program, script_name, &[])
}

/// Emit a complete C program with additional `.roller` library directories.
///
/// The script-adjacent `lib` directory and the process-local `lib` directory
/// remain available. Language tooling uses the additional paths to resolve
/// workspace libraries even when its process was started elsewhere.
pub fn emit_program_with_library_paths(
    program: &Program,
    script_name: &str,
    additional_library_paths: &[PathBuf],
) -> Result<String, TranspileError> {
    let mut out = String::with_capacity(4096);
    let mut sc = ScopeStack::new();
    let script_dir = Path::new(script_name).parent().unwrap_or(Path::new("."));
    let mut library_paths = vec![script_dir.join("lib")];
    library_paths.extend(additional_library_paths.iter().cloned());
    library_paths.push(PathBuf::from("lib"));
    library_paths.dedup();
    let mut modules = ModuleRegistry::new(library_paths);

    writeln!(out, "// Generated by Roller from {}", script_name).unwrap();
    writeln!(out, "#include \"roller-runtime.h\"").unwrap();
    writeln!(out).unwrap();

    // Load imports and collect inline library names
    for item in &program.items {
        match item {
            TopLevelItem::Import(imp) => {
                modules.load_module(&imp.module)?;
            }
            TopLevelItem::Library(lib) => {
                modules.register_inline(&lib.name);
            }
            _ => {}
        }
    }

    let imported_programs: Vec<Program> = modules.modules.values().cloned().collect();
    for imported in &imported_programs {
        modules.register_program_metadata(imported)?;
    }
    modules.register_program_metadata(program)?;

    emit_method_prototypes(&mut out, &modules);

    // Emit imported module functions
    let mut emitted = HashSet::new();
    for item in &program.items {
        if let TopLevelItem::Import(imp) = item {
            if !emitted.insert(imp.module.clone()) {
                continue;
            }
            writeln!(out, "// === Import: {} ===", imp.module).unwrap();
            if let Some(prog) = modules.modules.get(&imp.module).cloned() {
                let mut ms = ScopeStack::new();
                ms.set_current_module(Some(&imp.module));
                for item in &prog.items {
                    match item {
                        TopLevelItem::Section(s) => {
                            emit_lib_fn(&mut out, &imp.module, s, &mut ms, &modules)?;
                        }
                        TopLevelItem::Library(lib) => {
                            for lib_item in &lib.items {
                                if let LibraryItem::Compiler(compiler) = lib_item {
                                    emit_compiler_constructor(&mut out, &lib.name, compiler);
                                }
                            }
                            for lib_item in &lib.items {
                                match lib_item {
                                    LibraryItem::Function(f) => {
                                        emit_lib_func_fn(
                                            &mut out, &lib.name, f, &mut ms, &modules,
                                        )?;
                                    }
                                    LibraryItem::Implement(implementation) => {
                                        for method in &implementation.methods {
                                            let prefix = format!(
                                                "impl_{}_{}",
                                                lib.name, implementation.compiler_name
                                            );
                                            emit_lib_func_fn(
                                                &mut out, &prefix, method, &mut ms, &modules,
                                            )?;
                                        }
                                    }
                                    LibraryItem::Compiler(_) => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            writeln!(out).unwrap();
        }
    }

    emit_method_dispatchers(&mut out, &modules)?;

    // Phase 3: Emit user program — libraries first, then constants and sections
    // Collect sections for main() dispatch
    let (mut names, mut has_p) = (Vec::new(), Vec::new());

    // 3a: Emit libraries
    for item in &program.items {
        if let TopLevelItem::Library(lib) = item {
            writeln!(out, "// === Library: {} ===", lib.name).unwrap();
            let mut lib_sc = ScopeStack::new();
            lib_sc.set_current_module(Some(&lib.name));
            for lib_item in &lib.items {
                if let LibraryItem::Compiler(compiler) = lib_item {
                    emit_compiler_constructor(&mut out, &lib.name, compiler);
                }
            }
            for lib_item in &lib.items {
                match lib_item {
                    LibraryItem::Function(f) => {
                        emit_lib_func_fn(&mut out, &lib.name, f, &mut lib_sc, &modules)?;
                    }
                    LibraryItem::Implement(implementation) => {
                        for method in &implementation.methods {
                            let prefix =
                                format!("impl_{}_{}", lib.name, implementation.compiler_name);
                            emit_lib_func_fn(&mut out, &prefix, method, &mut lib_sc, &modules)?;
                        }
                    }
                    LibraryItem::Compiler(_) => {}
                }
            }
            writeln!(out).unwrap();
        }
    }

    // 3b: Emit constants and sections
    for item in &program.items {
        match item {
            TopLevelItem::Import(_) | TopLevelItem::Library(_) => {}
            TopLevelItem::Constant(c) => {
                writeln!(out, "#line {} \"{}\"", c.span.start.line, script_name).unwrap();
                match &c.value.kind {
                    ExpressionKind::StringLiteral(s) => {
                        writeln!(out, "static const char* {} = \"{}\";", c.name, esc(s)).unwrap();
                        sc.define_raw(&c.name, VarType::CString);
                    }
                    ExpressionKind::IntegerLiteral(n) => {
                        writeln!(out, "static const uint64_t {} = {};", c.name, n).unwrap();
                        sc.define_raw(&c.name, VarType::Integer);
                    }
                    ExpressionKind::BooleanLiteral(b) => {
                        writeln!(
                            out,
                            "static const int {} = {};",
                            c.name,
                            if *b { 1 } else { 0 }
                        )
                        .unwrap();
                        sc.define_raw(&c.name, VarType::Boolean);
                    }
                    _ => {
                        let v = emit_expr(&c.value, &mut sc, &modules)?;
                        writeln!(out, "static RValue {} = {};", c.name, v).unwrap();
                    }
                }
                writeln!(out).unwrap();
            }
            TopLevelItem::Section(s) => {
                names.push(s.name.clone());
                has_p.push(!s.parameters.is_empty());
                emit_sec_fn(&mut out, s, script_name, &mut sc, &modules)?;
            }
        }
    }

    // main()
    writeln!(out, "int main(int argc, char **argv) {{").unwrap();
    writeln!(out, "    r_arena_init();").unwrap();
    writeln!(out, "    r_main_argc = argc;").unwrap();
    writeln!(out, "    r_main_argv = argv;").unwrap();
    writeln!(out, "    const char *section = NULL;").unwrap();
    writeln!(out, "    if (argc > 1) section = argv[1];").unwrap();
    writeln!(
        out,
        "    if (!section) {{ fprintf(stderr, \"error: no section specified\\n\"); return 2; }}"
    )
    .unwrap();
    writeln!(out, "    int jobs = 0;").unwrap();
    writeln!(out, "    if (argc > 2) jobs = atoi(argv[2]);").unwrap();
    writeln!(
        out,
        "    if (jobs > 0) r_parallel_jobs = jobs; else r_parallel_jobs = 1;"
    )
    .unwrap();
    writeln!(out, "    for (int i = 3; i < argc; i++) {{").unwrap();
    writeln!(
        out,
        "        if (strcmp(argv[i], \"--dry-run\") == 0) r_dry_run = 1;"
    )
    .unwrap();
    writeln!(
        out,
        "        if (strcmp(argv[i], \"--verbose\") == 0) r_verbose = 1;"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "    const char *root = getenv(\"ROLLER_ROOT\");").unwrap();
    writeln!(
        out,
        "    if (root) strncpy(r_project_root, root, sizeof(r_project_root) - 1);"
    )
    .unwrap();
    writeln!(out, "    if (chdir(r_project_root) != 0) {{").unwrap();
    writeln!(out, "        fprintf(stderr, \"error: cannot chdir to %s: %s\\n\", r_project_root, strerror(errno)); return 1;").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "    int exit_code = 0;").unwrap();
    writeln!(out, "    if (setjmp(r_error_jmp) != 0) {{").unwrap();
    writeln!(
        out,
        "        if (strncmp(r_error_msg, \"__exit__\", 8) == 0) exit_code = atoi(r_error_msg + 8);"
    )
    .unwrap();
    writeln!(out, "        else {{ fprintf(stderr, \"error at line %d: %s\\n\", r_error_line, r_error_msg); exit_code = 4; }}").unwrap();
    writeln!(out, "    }} else {{").unwrap();
    let mut first = true;
    for (n, hp) in names.iter().zip(has_p.iter()) {
        let c = sid(n);
        if first {
            write!(out, "        if ").unwrap();
            first = false;
        } else {
            write!(out, "        else if ").unwrap();
        }
        if *hp {
            writeln!(
                out,
                "(strcmp(section, \"{}\") == 0) exit_code = section_{}(jobs);",
                n, c
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "(strcmp(section, \"{}\") == 0) exit_code = section_{}();",
                n, c
            )
            .unwrap();
        }
    }
    writeln!(out, "        else {{ fprintf(stderr, \"error: unknown section '%s'\\n\", section); exit_code = 4; }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "    r_arena_destroy();").unwrap();
    writeln!(out, "    return exit_code;").unwrap();
    writeln!(out, "}}").unwrap();
    Ok(out)
}

fn emit_method_prototypes(out: &mut String, modules: &ModuleRegistry) {
    let mut methods: Vec<_> = modules.compiler_methods.values().flatten().collect();
    methods.sort_by(|left, right| left.function_name.cmp(&right.function_name));
    methods.dedup_by(|left, right| left.function_name == right.function_name);
    for method in methods {
        let mut parameters = vec!["RValue receiver".to_string()];
        parameters.extend((0..method.argument_count).map(|index| format!("RValue arg{index}")));
        parameters.push("int _line".to_string());
        writeln!(
            out,
            "static RValue {}({});",
            method.function_name,
            parameters.join(", ")
        )
        .unwrap();
    }
    writeln!(out).unwrap();
}

fn emit_method_dispatchers(
    out: &mut String,
    modules: &ModuleRegistry,
) -> Result<(), TranspileError> {
    let mut method_names: Vec<_> = modules.compiler_methods.keys().cloned().collect();
    method_names.sort();
    for method_name in method_names {
        let bindings = modules.method_bindings(&method_name).unwrap_or_default();
        if bindings.is_empty() {
            continue;
        }
        emit_method_dispatcher(out, &method_name, bindings, false);
        if bindings.iter().any(|binding| binding.parallelable) {
            emit_method_dispatcher(out, &method_name, bindings, true);
        }
    }
    Ok(())
}

fn emit_method_dispatcher(
    out: &mut String,
    method_name: &str,
    bindings: &[MethodBinding],
    parallel_only: bool,
) {
    let dispatcher_name = if parallel_only {
        format!("r_dispatch_parallel_{}", sid(method_name))
    } else {
        format!("r_dispatch_{}", sid(method_name))
    };
    writeln!(
        out,
        "static RValue {}(RValue receiver, RValue arguments, int _line) {{",
        dispatcher_name
    )
    .unwrap();
    for binding in bindings {
        if parallel_only && !binding.parallelable {
            writeln!(
                out,
                "    if (r_compiler_is(receiver, \"{}\")) {{ r_error(_line, \"method {} is not paralleable for compiler implementation '%s'\", r_compiler_implementation(receiver, _line)); return r_unit(); }}",
                esc(&binding.implementation),
                esc(method_name),
            )
            .unwrap();
            continue;
        }
        let mut arguments = (0..binding.argument_count)
            .map(|index| format!("r_array_at(arguments, {index}, _line)"))
            .collect::<Vec<_>>();
        arguments.push("_line".to_string());
        writeln!(
            out,
            "    if (r_compiler_is(receiver, \"{}\")) {{ r_array_require_length(arguments, {}, \"{}\", _line); return {}(receiver, {}); }}",
            esc(&binding.implementation),
            binding.argument_count,
            esc(method_name),
            binding.function_name,
            arguments.join(", "),
        )
        .unwrap();
    }
    writeln!(
        out,
        "    r_error(_line, \"compiler implementation '%s' does not implement {}\", r_compiler_implementation(receiver, _line));",
        esc(method_name)
    )
    .unwrap();
    writeln!(out, "    return r_unit();").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
}

fn emit_compiler_constructor(out: &mut String, library: &str, compiler: &CompilerDeclaration) {
    writeln!(
        out,
        "static RValue r_lib_{}_new_{}(void) {{",
        sid(library),
        sid(&compiler.name)
    )
    .unwrap();
    writeln!(
        out,
        "    RValue compiler = r_compiler_instance(\"{}::{}\");",
        esc(library),
        esc(&compiler.name)
    )
    .unwrap();
    for field in &compiler.fields {
        let initial_value = match type_from_name(&field.type_name) {
            VarType::Array => "r_array_new()",
            VarType::CString => "r_string(\"\")",
            VarType::Boolean => "r_boolean(0)",
            VarType::Integer => "r_integer(0)",
            _ => "r_unit()",
        };
        writeln!(
            out,
            "    r_compiler_set(compiler, \"{}\", {}, {});",
            esc(&field.name),
            initial_value,
            field.span.start.line
        )
        .unwrap();
    }
    writeln!(out, "    return compiler;").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
}

fn emit_lib_fn(
    out: &mut String,
    module: &str,
    section: &roller_parser::SectionDeclaration,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<(), TranspileError> {
    let cn = format!("r_lib_{}_{}", module, sid(&section.name));
    let params = if section.parameters.is_empty() {
        "void".into()
    } else {
        let mut p: Vec<String> = section
            .parameters
            .iter()
            .map(|pr| format!("RValue {}", sid(&pr.name)))
            .collect();
        p.push("int _line".into());
        p.join(", ")
    };
    writeln!(out, "static RValue {}({}) {{", cn, params).unwrap();
    sc.push();
    for p in &section.parameters {
        sc.define(&p.name, type_from_name(&p.type_name));
    }
    emit_block(out, &section.body, "lib", sc, m, "    ")?;
    sc.pop();
    writeln!(out, "    return r_unit();").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
    Ok(())
}

/// Emit a function from an inline library or override block.
fn emit_lib_func_fn(
    out: &mut String,
    prefix: &str,
    func: &FunctionDeclaration,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<(), TranspileError> {
    let cn = format!("r_lib_{}_{}", prefix, sid(&func.name));
    let mut params: Vec<String> = func
        .parameters
        .iter()
        .map(|pr| format!("RValue {}", sid(&pr.name)))
        .collect();
    params.push("int _line".into());
    let params_str = params.join(", ");
    writeln!(out, "static RValue {}({}) {{", cn, params_str).unwrap();
    sc.push();
    for p in &func.parameters {
        sc.define(&p.name, type_from_name(&p.type_name));
    }
    emit_block(out, &func.body, "lib", sc, m, "    ")?;
    sc.pop();
    writeln!(out, "    return r_unit();").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
    Ok(())
}

fn emit_sec_fn(
    out: &mut String,
    section: &roller_parser::SectionDeclaration,
    sn: &str,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<(), TranspileError> {
    writeln!(out, "#line {} \"{}\"", section.span.start.line, sn).unwrap();
    let params = if section.parameters.is_empty() {
        "void".into()
    } else {
        section
            .parameters
            .iter()
            .map(|p| format!("int {}", sid(&p.name)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    writeln!(
        out,
        "static int section_{}({}) {{",
        sid(&section.name),
        params
    )
    .unwrap();
    sc.push();
    for p in &section.parameters {
        sc.define_raw(&p.name, VarType::Integer);
    }
    emit_block(out, &section.body, sn, sc, m, "    ")?;
    sc.pop();
    writeln!(out, "    return 0;").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
    Ok(())
}

fn emit_block(
    out: &mut String,
    block: &Block,
    sn: &str,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
    ind: &str,
) -> Result<(), TranspileError> {
    sc.push();
    for stmt in &block.statements {
        emit_stmt(out, stmt, sn, sc, m, ind)?;
    }
    sc.pop();
    Ok(())
}

fn emit_stmt(
    out: &mut String,
    stmt: &Statement,
    sn: &str,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
    ind: &str,
) -> Result<(), TranspileError> {
    match stmt {
        Statement::Let { name, value, span } => {
            let code = emit_expr(value, sc, m)?;
            let ty = infer_ty(value, sc, m);
            sc.define(name, ty);
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            writeln!(out, "{}RValue {} = {};", ind, sid(name), code).unwrap();
        }
        Statement::Assignment {
            target,
            value,
            span,
        } => {
            let value_code = emit_expr(value, sc, m)?;
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            match &target.kind {
                ExpressionKind::Identifier(name) if sc.get_type(name) == VarType::Compiler => {
                    writeln!(
                        out,
                        "{}r_compiler_assign({}, {}, {});",
                        ind,
                        sid(name),
                        value_code,
                        span.start.line
                    )
                    .unwrap();
                }
                ExpressionKind::Identifier(name) => {
                    writeln!(out, "{}{} = {};", ind, sid(name), value_code).unwrap();
                }
                ExpressionKind::MemberAccess { receiver, member } => {
                    let receiver_code = emit_expr(receiver, sc, m)?;
                    writeln!(
                        out,
                        "{}r_compiler_set({}, \"{}\", {}, {});",
                        ind,
                        receiver_code,
                        esc(member),
                        value_code,
                        span.start.line
                    )
                    .unwrap();
                }
                _ => {
                    return Err(TranspileError::InvalidOperation {
                        message: "assignment target must be a binding or compiler field".into(),
                        span: target.span,
                    });
                }
            }
        }
        Statement::If {
            condition,
            then_block,
            else_block,
            span,
        } => {
            let cc = emit_expr(condition, sc, m)?;
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            writeln!(
                out,
                "{}if (r_value_truthy({}, {})) {{",
                ind, cc, span.start.line
            )
            .unwrap();
            emit_block(out, then_block, sn, sc, m, &format!("{ind}    "))?;
            if let Some(eb) = else_block {
                writeln!(out, "{}}} else {{", ind).unwrap();
                emit_block(out, eb, sn, sc, m, &format!("{ind}    "))?;
            }
            writeln!(out, "{}}}", ind).unwrap();
        }
        Statement::ForParallel {
            binding,
            iterable,
            body,
            span,
        } => {
            let ic = emit_expr(iterable, sc, m)?;
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            writeln!(out, "{}{{", ind).unwrap();
            writeln!(out, "{}    RValue _iter = {};", ind, ic).unwrap();
            writeln!(out, "{}    r_parallel_begin();", ind).unwrap();
            writeln!(
                out,
                "{}    for (size_t _i = 0; _i < _iter.as.array->count; _i++) {{",
                ind
            )
            .unwrap();
            writeln!(
                out,
                "{}        RValue {} = _iter.as.array->elements[_i];",
                ind,
                sid(binding)
            )
            .unwrap();
            sc.push();
            // Iterables are dynamically typed.  The element can be a path,
            // command description, source record, or any future runtime value.
            sc.define(binding, VarType::Unknown);
            emit_block(out, body, sn, sc, m, &format!("{ind}        "))?;
            sc.pop();
            writeln!(out, "{}    }}", ind).unwrap();
            writeln!(out, "{}    r_parallel_execute();", ind).unwrap();
            writeln!(out, "{}    r_parallel_end();", ind).unwrap();
            writeln!(out, "{}}}", ind).unwrap();
        }
        Statement::Parallel { expression, span } => {
            let code = emit_parallel(expression, sc, m)?;
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            writeln!(out, "{}{}", ind, code).unwrap();
        }
        Statement::Expression { expression, span } => {
            let code = emit_expr(expression, sc, m)?;
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            writeln!(out, "{}{};", ind, code).unwrap();
        }
        Statement::Return { value, span } => {
            let code = emit_expr(value, sc, m)?;
            writeln!(out, "{}#line {} \"{}\"", ind, span.start.line, sn).unwrap();
            writeln!(out, "{}return {};", ind, code).unwrap();
        }
    }
    Ok(())
}

fn emit_parallel(
    expr: &Expression,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<String, TranspileError> {
    let ExpressionKind::MethodCall {
        receiver,
        method,
        arguments,
    } = &expr.kind
    else {
        return Err(TranspileError::InvalidOperation {
            message: "parallel requires a compiler method call".into(),
            span: expr.span,
        });
    };
    if !m.has_parallelable_method(method) {
        return Err(TranspileError::InvalidOperation {
            message: format!("compiler method `{method}` is not declared paralleable"),
            span: expr.span,
        });
    }
    if infer_ty(receiver, sc, m) != VarType::Compiler {
        return Err(TranspileError::Type {
            message: "parallel method receiver must be a compiler implementation".into(),
            span: receiver.span,
        });
    }
    let call = emit_compiler_dispatch(receiver, method, arguments, expr.span, sc, m, true)?;
    Ok(format!(
        "r_parallel_collect_begin(); (void){}; r_parallel_collect_end();",
        call
    ))
}

fn emit_expr(
    expr: &Expression,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<String, TranspileError> {
    match &expr.kind {
        ExpressionKind::IntegerLiteral(n) => Ok(format!("r_integer({})", n)),
        ExpressionKind::StringLiteral(s) => Ok(format!("r_string(\"{}\")", esc(s))),
        ExpressionKind::BooleanLiteral(b) => Ok(format!("r_boolean({})", if *b { 1 } else { 0 })),
        ExpressionKind::Identifier(name) => Ok(sid(name)),
        ExpressionKind::Array(elems) => {
            let mut code = String::from("({ RValue _arr = r_array_new(); ");
            for e in elems {
                write!(code, "r_array_push(&_arr, {}); ", emit_expr(e, sc, m)?).unwrap();
            }
            code.push_str("_arr; })");
            Ok(code)
        }
        ExpressionKind::Reference(inner) => Ok(format!("&{}", emit_expr(inner, sc, m)?)),
        ExpressionKind::Binary {
            left,
            operator,
            right,
        } => {
            let (lc, rc, ln) = (
                emit_expr(left, sc, m)?,
                emit_expr(right, sc, m)?,
                expr.span.start.line,
            );
            match operator {
                BinaryOperator::And => Ok(format!(
                    "(r_value_truthy({}, {}) ? {} : r_boolean(0))",
                    lc, ln, rc
                )),
                BinaryOperator::Or => Ok(format!(
                    "(r_value_truthy({}, {}) ? r_boolean(1) : {})",
                    lc, ln, rc
                )),
                BinaryOperator::Equal => Ok(format!("r_value_eq({}, {}, {})", lc, rc, ln)),
                BinaryOperator::NotEqual => Ok(format!("r_value_neq({}, {}, {})", lc, rc, ln)),
            }
        }
        ExpressionKind::NamespaceAccess { namespace, member } => emit_ns(namespace, member, sc, m),
        ExpressionKind::Call { callee, arguments } => {
            emit_call(callee, arguments, expr.span, sc, m)
        }
        ExpressionKind::MethodCall {
            receiver,
            method,
            arguments,
        } => emit_method(receiver, method, arguments, expr.span, sc, m),
        ExpressionKind::MemberAccess { receiver, member } => Ok(format!(
            "r_compiler_get({}, \"{}\", {})",
            emit_expr(receiver, sc, m)?,
            esc(member),
            expr.span.start.line
        )),
        ExpressionKind::Not(inner) => {
            let ic = emit_expr(inner, sc, m)?;
            Ok(format!("r_value_not({}, {})", ic, expr.span.start.line))
        }
        ExpressionKind::Index { object, index } => {
            let oc = emit_expr(object, sc, m)?;
            let ic = emit_expr(index, sc, m)?;
            Ok(format!(
                "r_array_get({}, {}, {})",
                oc, ic, expr.span.start.line
            ))
        }
    }
}

/// Resolve sys::X::Y namespace chain to a C function name.
fn resolve_sys_chain(sub: &str, member: &str) -> Option<&'static str> {
    match sub {
        "process" => match member {
            "run" => Some("r_sys_process_run"),
            "output" => Some("r_sys_process_output"),
            "status" => Some("r_sys_process_status"),
            "spawn" => Some("r_sys_process_spawn"),
            "wait" => Some("r_sys_process_wait"),
            "kill" => Some("r_sys_process_kill"),
            _ => None,
        },
        "fs" => match member {
            "read" => Some("r_sys_fs_read"),
            "write" => Some("r_sys_fs_write"),
            "exists" => Some("r_sys_fs_exists"),
            "is_file" => Some("r_sys_fs_is_file"),
            "is_dir" => Some("r_sys_fs_is_dir"),
            "size" => Some("r_sys_fs_size"),
            "mtime" => Some("r_sys_fs_mtime"),
            "mkdir" => Some("r_sys_fs_mkdir"),
            "mkdir_all" => Some("r_sys_fs_mkdir_all"),
            "mkdir_parent" => Some("r_sys_fs_mkdir_parent"),
            "remove_file" => Some("r_sys_fs_remove_file"),
            "remove_dir" => Some("r_sys_fs_remove_dir_all"),
            "rename" => Some("r_sys_fs_rename"),
            "copy" => Some("r_sys_fs_copy"),
            "read_dir" => Some("r_sys_fs_read_dir"),
            "find_recursive" => Some("r_sys_dir_recursive"),
            _ => None,
        },
        "io" => match member {
            "read_line" => Some("r_sys_io_read_line"),
            "print" => Some("r_sys_io_print"),
            "eprint" => Some("r_sys_io_eprint"),
            "flush" => Some("r_sys_io_flush"),
            _ => None,
        },
        "env" => match member {
            "get" => Some("r_sys_env_get"),
            "set" => Some("r_sys_env_set"),
            "cwd" => Some("r_sys_env_cwd"),
            "chdir" => Some("r_sys_env_chdir"),
            "args" => Some("r_sys_env_args"),
            _ => None,
        },
        "str" => match member {
            "concat" => Some("r_sys_str_concat"),
            "contains" => Some("r_sys_str_contains"),
            _ => None,
        },
        "time" => match member {
            "sleep" => Some("r_sys_time_sleep"),
            "now_ms" => Some("r_sys_time_now_ms"),
            _ => None,
        },
        "cmd" => match member {
            "which" => Some("r_sys_cmd_which"),
            "is_exists" => Some("r_sys_cmd_is_exists"),
            _ => None,
        },
        "path" => match member {
            "join" => Some("r_sys_path_join"),
            "replace_extension" => Some("r_sys_path_replace_extension"),
            "extension" => Some("r_sys_path_extension"),
            _ => None,
        },
        _ => None,
    }
}

fn emit_ns(
    namespace: &Expression,
    member: &str,
    _sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<String, TranspileError> {
    // Handle deep namespace chains: sys::process::*, sys::fs::*, etc.
    if let ExpressionKind::NamespaceAccess {
        namespace: inner_ns,
        member: inner_member,
    } = &namespace.kind
    {
        if let ExpressionKind::Identifier(inner_name) = &inner_ns.kind {
            if inner_name == "sys" {
                if let Some(fname) = resolve_sys_chain(inner_member, member) {
                    return Ok(fname.into());
                }
            }
        }
    }

    let ns = ident(namespace)?;
    if ns == "self"
        && let Some(module) = _sc.current_module()
        && m.has_compiler(module, member)
    {
        return Ok(format!("r_lib_{}_new_{}()", sid(module), sid(member)));
    }
    match (ns.as_str(), member) {
        ("Compiler", "new") => return Ok("r_compiler_new()".into()),
        ("Compiler", "AVAILABLE") => return Ok("r_compiler_status_available()".into()),
        ("Compiler", "UNAVAILABLE" | "NOTFOUND") => {
            return Ok("r_compiler_status_unavailable()".into());
        }
        _ => {}
    }
    if ns == "sys" {
        return match member {
            "find_executable" => Ok("r_sys_find_executable".into()),
            "dir_recursive" => Ok("r_sys_dir_recursive".into()),
            "process_run" => Ok("r_sys_process_run".into()),
            "set_parallel_jobs" => Ok("r_sys_set_parallel_jobs".into()),
            "parallel_add_job" => Ok("r_sys_parallel_add_job".into()),
            "array_new" => Ok("r_array_new".into()),
            "array_push" => Ok("r_array_push".into()),
            _ => Err(TranspileError::Name {
                message: format!("unknown sys::{}", member),
                span: namespace.span,
            }),
        };
    }
    // Handle sys::process::*, sys::fs::*, sys::io::*, sys::env::*, sys::str::*, sys::time::*
    if let ExpressionKind::NamespaceAccess {
        namespace: inner_ns,
        member: inner_member,
    } = &namespace.kind
    {
        if let ExpressionKind::Identifier(inner_name) = &inner_ns.kind {
            if inner_name == "sys" {
                if let Some(fname) = resolve_sys_chain(inner_member, member) {
                    return Ok(fname.into());
                }
            }
        }
    }
    if m.has_module(&ns) {
        return Ok(format!("r_lib_{}_{}", ns, sid(member)));
    }
    match (ns.as_str(), member) {
        ("log", "error" | "err") => Ok("r_log_error".into()),
        ("log", "info") => Ok("r_log_info".into()),
        ("roller", "exit") => Ok("r_exit_impl".into()),
        ("roller", "set_parallel_jobs") => Ok("r_sys_set_parallel_jobs".into()),
        ("process", "run") => Ok("r_sys_process_run".into()),
        ("dir", "recursive") => Ok("r_sys_dir_recursive".into()),
        _ => Err(TranspileError::Name {
            message: format!("unknown {}::{}", ns, member),
            span: namespace.span,
        }),
    }
}

fn emit_call(
    callee: &Expression,
    args: &[Expression],
    span: Span,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<String, TranspileError> {
    let ln = span.start.line;
    if let ExpressionKind::NamespaceAccess { namespace, member } = &callee.kind {
        // Handle deep namespace chains: sys::process::*, sys::fs::*, etc.
        if let ExpressionKind::NamespaceAccess {
            namespace: inner_ns,
            member: inner_member,
        } = &namespace.kind
        {
            if let ExpressionKind::Identifier(inner_name) = &inner_ns.kind {
                if inner_name == "sys" {
                    let func_name = resolve_sys_chain(inner_member, member);
                    if let Some(fname) = func_name {
                        let ac = emit_args_rv(args, sc, m)?;
                        return Ok(format!("{}({}, {})", fname, ac.join(", "), ln));
                    }
                }
            }
        }
        // Skip deep chain handling — let emit_expr → emit_ns handle it
        if !matches!(namespace.kind, ExpressionKind::NamespaceAccess { .. }) {
            let ns = ident(namespace)?;
            match (ns.as_str(), member.as_str()) {
                ("Compiler", "new") => return Ok("r_compiler_new()".into()),
                ("Compiler", "AVAILABLE")
                | ("Compiler", "UNAVAILABLE")
                | ("Compiler", "NOTFOUND") => {
                    return emit_ns(namespace, member, sc, m);
                }
                _ => {}
            }
            if ns == "sys" {
                let cc = emit_ns(namespace, member, sc, m)?;
                let ac = emit_args_rv(args, sc, m)?;
                return Ok(format!("{}({}, {})", cc, ac.join(", "), ln));
            }
            if m.has_module(&ns) {
                let cc = format!("r_lib_{}_{}", ns, sid(member));
                let ac = emit_args_rv(args, sc, m)?;
                if ac.is_empty() {
                    return Ok(format!("{}({})", cc, ln));
                }
                return Ok(format!("{}({}, {})", cc, ac.join(", "), ln));
            }
            match (ns.as_str(), member.as_str()) {
                ("log", "error") | ("log", "err") | ("log", "info") => {
                    arity(args, 1, span)?;
                    let ac = emit_expr(&args[0], sc, m)?;
                    return Ok(format!(
                        "{}({})",
                        emit_ns(namespace, member, sc, m)?,
                        str_a(&args[0], &ac, sc)
                    ));
                }
                ("roller", "exit") => {
                    arity(args, 1, span)?;
                    let ac = emit_expr(&args[0], sc, m)?;
                    return Ok(format!(
                        "{{ {}({}); }}",
                        emit_ns(namespace, member, sc, m)?,
                        int_a(&args[0], &ac, sc)
                    ));
                }
                ("roller", "set_parallel_jobs") => {
                    arity(args, 1, span)?;
                    let ac = emit_expr(&args[0], sc, m)?;
                    return Ok(format!(
                        "r_sys_set_parallel_jobs({}, {})",
                        int_a(&args[0], &ac, sc),
                        ln
                    ));
                }
                ("process", "run") => {
                    if args.is_empty() || args.len() > 2 {
                        return Err(TranspileError::InvalidOperation {
                            message: "process::run expects 1-2 args".into(),
                            span,
                        });
                    }
                    let ac = emit_args_rv(args, sc, m)?;
                    if ac.len() == 1 {
                        return Ok(format!("r_sys_process_run({}, r_unit(), {})", ac[0], ln));
                    }
                    return Ok(format!("r_sys_process_run({}, {}, {})", ac[0], ac[1], ln));
                }
                ("dir", "recursive") => {
                    arity(args, 1, span)?;
                    let ac = emit_args_rv(args, sc, m)?;
                    return Ok(format!("r_sys_dir_recursive({}, {})", ac[0], ln));
                }
                _ => {}
            }
        }
    }
    let cc = emit_expr(callee, sc, m)?;
    let ac: Vec<String> = args
        .iter()
        .map(|a| emit_expr(a, sc, m))
        .collect::<Result<_, _>>()?;
    Ok(format!("{}({})", cc, ac.join(", ")))
}

fn emit_method(
    receiver: &Expression,
    method: &str,
    args: &[Expression],
    span: Span,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<String, TranspileError> {
    let ln = span.start.line;

    if matches!(&receiver.kind, ExpressionKind::Identifier(name) if name == "dir")
        && method == "recursive"
    {
        arity(args, 1, span)?;
        let arguments = emit_args_rv(args, sc, m)?;
        return Ok(format!("r_sys_dir_recursive({}, {})", arguments[0], ln));
    }

    let recv_ty = infer_ty(receiver, sc, m);
    if recv_ty == VarType::Compiler && m.method_bindings(method).is_some() {
        return emit_compiler_dispatch(receiver, method, args, span, sc, m, false);
    }

    let rc = emit_expr(receiver, sc, m)?;
    match method {
        "push" | "push_str" => {
            arity(args, 1, span)?;
            let ac = emit_args_rv(args, sc, m)?;
            return Ok(format!("r_array_push_value({}, {}, {})", rc, ac[0], ln));
        }
        "push_vec" => {
            arity(args, 1, span)?;
            let ac = emit_args_rv(args, sc, m)?;
            return Ok(format!("r_array_extend({}, {}, {})", rc, ac[0], ln));
        }
        "copy" => {
            arity(args, 0, span)?;
            return Ok(format!("r_array_copy({}, {})", rc, ln));
        }
        "is_empty" => {
            arity(args, 0, span)?;
            return Ok(format!("r_value_is_empty({}, {})", rc, ln));
        }
        "join" => {
            arity(args, 1, span)?;
            let ac = emit_args_rv(args, sc, m)?;
            return Ok(format!("r_array_join({}, {}, {})", rc, ac[0], ln));
        }
        _ => {}
    }

    Err(TranspileError::Name {
        message: format!("unknown method `{method}` for receiver type {recv_ty:?}"),
        span,
    })
}

fn emit_compiler_dispatch(
    receiver: &Expression,
    method: &str,
    args: &[Expression],
    span: Span,
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
    parallel_only: bool,
) -> Result<String, TranspileError> {
    let receiver_code = emit_expr(receiver, sc, m)?;
    let argument_codes = emit_args_rv(args, sc, m)?;
    let arguments = value_array(&argument_codes);
    let prefix = if parallel_only {
        "r_dispatch_parallel_"
    } else {
        "r_dispatch_"
    };
    Ok(format!(
        "{}{}({}, {}, {})",
        prefix,
        sid(method),
        receiver_code,
        arguments,
        span.start.line
    ))
}

fn value_array(values: &[String]) -> String {
    let mut code = String::from("({ RValue _args = r_array_new(); ");
    for value in values {
        write!(code, "r_array_push(&_args, {}); ", value).unwrap();
    }
    code.push_str("_args; })");
    code
}

fn infer_ty(expr: &Expression, sc: &ScopeStack, _m: &ModuleRegistry) -> VarType {
    match &expr.kind {
        ExpressionKind::IntegerLiteral(_) => VarType::Integer,
        ExpressionKind::StringLiteral(_) => VarType::CString,
        ExpressionKind::BooleanLiteral(_) => VarType::Boolean,
        ExpressionKind::Identifier(name) => sc.get_type(name),
        ExpressionKind::Array(_) => VarType::Array,
        ExpressionKind::Reference(inner) => infer_ty(inner, sc, _m),
        ExpressionKind::Binary { .. } => VarType::Boolean,
        ExpressionKind::NamespaceAccess { namespace, member } => {
            let ns = ident(namespace).unwrap_or_default();
            if ns == "self"
                && let Some(module) = sc.current_module()
                && _m.has_compiler(module, member)
            {
                return VarType::Compiler;
            }
            match (ns.as_str(), member.as_str()) {
                ("Compiler", "new") => VarType::Compiler,
                ("Compiler", "AVAILABLE" | "UNAVAILABLE" | "NOTFOUND") => VarType::CompilerStatus,
                _ => VarType::Unknown,
            }
        }
        ExpressionKind::MethodCall {
            receiver, method, ..
        } => {
            if infer_ty(receiver, sc, _m) == VarType::Compiler
                && _m.method_bindings(method).is_some()
            {
                _m.method_return_type(method)
            } else {
                match method.as_str() {
                    "copy" => VarType::Array,
                    "is_empty" => VarType::Boolean,
                    "join" => VarType::CString,
                    "push" | "push_str" | "push_vec" => VarType::Unit,
                    _ => VarType::Unknown,
                }
            }
        }
        ExpressionKind::Call { callee, .. } => {
            if let ExpressionKind::NamespaceAccess { namespace, member } = &callee.kind
                && let Ok(module) = ident(namespace)
                && _m.has_module(&module)
            {
                _m.function_return_type(&module, member)
            } else {
                infer_ty(callee, sc, _m)
            }
        }
        // Compiler fields are implementation-local dynamic values.  A field
        // with the same name may intentionally have a different type in each
        // compiler implementation.
        ExpressionKind::MemberAccess { .. } => VarType::Unknown,
        ExpressionKind::Not(_) => VarType::Boolean,
        ExpressionKind::Index { .. } => VarType::Unknown,
    }
}

fn emit_args_rv(
    args: &[Expression],
    sc: &mut ScopeStack,
    m: &ModuleRegistry,
) -> Result<Vec<String>, TranspileError> {
    args.iter()
        .map(|a| {
            // Strip reference (&) — RValue contains pointers internally, so value passing works
            let expr = match &a.kind {
                ExpressionKind::Reference(inner) => inner,
                _ => a,
            };
            let code = emit_expr(expr, sc, m)?;
            if let ExpressionKind::Identifier(name) = &expr.kind {
                if sc.is_raw_c(name) {
                    return Ok(match sc.get_type(name) {
                        VarType::CString => format!("r_string({})", code),
                        VarType::Integer => format!("r_integer((uint64_t){})", code),
                        VarType::Boolean => format!("r_boolean({})", code),
                        _ => code,
                    });
                }
            }
            Ok(code)
        })
        .collect()
}

fn ident(e: &Expression) -> Result<String, TranspileError> {
    match &e.kind {
        ExpressionKind::Identifier(n) => Ok(n.clone()),
        _ => Err(TranspileError::InvalidOperation {
            message: "expected identifier".into(),
            span: e.span,
        }),
    }
}
fn sid(n: &str) -> String {
    n.replace('-', "_")
}
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            _ => o.push(c),
        }
    }
    o
}
fn str_a(arg: &Expression, code: &str, sc: &ScopeStack) -> String {
    match &arg.kind {
        ExpressionKind::StringLiteral(_) => format!("{}.as.string", code),
        ExpressionKind::Identifier(name)
            if sc.get_type(name) == VarType::CString && sc.is_raw_c(name) =>
        {
            code.into()
        }
        _ => format!("{}.as.string", code),
    }
}
fn int_a(arg: &Expression, code: &str, sc: &ScopeStack) -> String {
    match &arg.kind {
        ExpressionKind::Identifier(name) if sc.is_raw_c(name) => code.into(),
        _ => format!("(int)({}.as.integer)", code),
    }
}
fn arity(args: &[Expression], expected: usize, span: Span) -> Result<(), TranspileError> {
    if args.len() != expected {
        Err(TranspileError::InvalidOperation {
            message: format!("expected {} arg(s), got {}", expected, args.len()),
            span,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roller_parser::{Lexer, Parser as RollerParser};
    fn parse(s: &str) -> Program {
        let t = Lexer::new(s).tokenize().unwrap();
        RollerParser::new(t).parse_program().unwrap()
    }
    fn tp(s: &str) -> String {
        emit_program(&parse(s), "test.roller").unwrap()
    }

    #[test]
    fn empty_sec() {
        assert!(tp("section build() {}").contains("static int section_build(void)"));
    }
    #[test]
    fn log_info() {
        assert!(
            tp(r#"section b() { log::info("hi"); }"#)
                .contains("r_log_info(r_string(\"hi\").as.string)")
        );
    }
    #[test]
    fn const_str() {
        assert!(
            tp(r#"#define X "a"
section b() {}"#)
            .contains("static const char* X = \"a\"")
        );
    }
    #[test]
    fn let_int() {
        assert!(tp("section b() { let x = 42; }").contains("RValue x = r_integer(42)"));
    }
    #[test]
    fn compiler_new() {
        assert!(tp("section b() { let c = Compiler::new(); }").contains("r_compiler_new()"));
    }
    #[test]
    fn sys_ns() {
        assert!(
            tp(r#"section b() { let x = sys::find_executable("g"); }"#)
                .contains("r_sys_find_executable")
        );
    }
    #[test]
    fn import_gcc() {
        let c = tp(r#"import "gcc"
section b() {}"#);
        assert!(c.contains("r_lib_gcc_get_compiler"));
        assert!(c.contains("r_string(\"-O3\")"));
    }
    #[test]
    fn import_zig_compiler_is_implemented_in_roller() {
        let c = tp(r#"import "zig"
section b() {}"#);
        assert!(c.contains("r_compiler_instance(\"zig::zig\")"));
        assert!(c.contains("r_array_extend("));
        assert!(!c.contains("r_sys_detect_compiler"));
    }
    #[test]
    fn import_method() {
        assert!(
            tp(r#"import "gcc"
section b() { let c = Compiler::new(); let x = c.setflag("-c"); }"#)
            .contains("r_dispatch_setflag(")
        );
    }
    #[test]
    fn dispatch() {
        let c = tp("section a(jobs: int) {} section b() {}");
        assert!(c.contains("section_a(jobs)"));
        assert!(c.contains("section_b()"));
    }
    #[test]
    fn equality() {
        assert!(tp("section b() { let x = true == false; }").contains("r_value_eq("));
    }
    #[test]
    fn not_operator() {
        assert!(tp("section b() { let x = !true; }").contains("r_value_not("));
    }
    #[test]
    fn array_index() {
        let c = tp("section b() { let a = [1,2,3]; let x = a[0]; }");
        assert!(c.contains("r_array_get("));
    }
    #[test]
    fn library_inline() {
        let c = tp(r#"library "test" { function greet() { return 1; } } section b() {}"#);
        assert!(c.contains("r_lib_test_greet"));
    }
    #[test]
    fn function_return_type() {
        let c = tp(r#"library "test" { function foo() -> int { return 1; } } section b() {}"#);
        assert!(c.contains("r_lib_test_foo"));
    }
    #[test]
    fn compiler_implementation_uses_dynamic_dispatch_without_build_intrinsics() {
        let c = tp(r#"library "test" {
                compiler cc { flags: Vec<String> }
                function select(compiler: Compiler) -> CompilerStatus {
                    compiler = self::cc;
                    return Compiler::AVAILABLE;
                }
                implement Self::cc {
                    function setflag(compiler: self, flag: String) -> Compiler {
                        compiler.flags.push(flag);
                        return compiler;
                    }
                }
            }
            section b() {
                let c = Compiler::new();
                test::select(&c);
                let configured = c.setflag("-c");
            }"#);
        assert!(c.contains("r_compiler_instance(\"test::cc\")"));
        assert!(c.contains("r_dispatch_setflag("));
        assert!(c.contains("r_array_push_value("));
        assert!(!c.contains("r_c_compile_context"));
        assert!(!c.contains("r_c_compile_one"));
        assert!(!c.contains("r_c_link_objects"));
    }
    #[test]
    fn compiler_implementations_can_use_different_field_types_and_method_shapes() {
        let c = tp(r#"library "numeric" {
                compiler tool { optimize: integer, inputs: Vec<String> }
                function select(compiler: Compiler) { compiler = self::tool; }
                implement Self::tool {
                    paralleable function compile(compiler: self, input: String) -> Compiler {
                        return compiler;
                    }
                }
            }
            library "symbolic" {
                compiler tool { optimize: String, inputs: Vec<symbolic::Node> }
                implement Self::tool {
                    paralleable function compile(
                        compiler: self,
                        graph: Map<String, Vec<symbolic::Node>>,
                        options: symbolic::Options
                    ) -> String {
                        return "done";
                    }
                }
            }
            section build() {
                let compiler = Compiler::new();
                numeric::select(&compiler);
                for-parallel input in ["source.any"] {
                    parallel compiler.compile(input);
                }
            }"#);
        assert!(c.contains("r_compiler_set(compiler, \"optimize\", r_integer(0)"));
        assert!(c.contains("r_compiler_set(compiler, \"optimize\", r_string(\"\")"));
        assert!(c.contains("r_array_require_length(arguments, 1, \"compile\""));
        assert!(c.contains("r_array_require_length(arguments, 2, \"compile\""));
        assert!(c.contains("r_dispatch_parallel_compile"));
    }
    #[test]
    fn library_with_sys_cmd() {
        let c = tp(
            r#"library "test" { function check() { let x = sys::cmd::is_exists("gcc"); } } section b() {}"#,
        );
        assert!(c.contains("r_sys_cmd_is_exists"));
    }
}
