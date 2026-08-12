use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

const BUILD_ROLLER_TEMPLATE: &str = r#"import "gcc"
import "clang"

section build(jobs: int)
{
    roller::set_parallel_jobs(jobs);

    let cc = Compiler::new();

    if (gcc::get_compiler(&cc) != Compiler::AVAILABLE)
        && (clang::get_compiler(&cc) != Compiler::AVAILABLE)
    {
        log::error(
            "No available C compiler was found. Roller checked gcc and clang."
        );
        roller::exit(1);
    }

    let obj_compiler = cc.setflag("-c");

    for-parallel file in dir.recursive("./src")
    {
        parallel obj_compiler.compile(file);
    }

    cc.link(obj_compiler.outputs(), "myproject");
}

section run()
{
    process::run("./myproject");
}
"#;

fn main() -> ExitCode {
    match roller_cli::prepare_from(std::env::args_os()) {
        Ok(invocation) => {
            // Handle --init flag
            if invocation.init {
                return init_project(&invocation.script);
            }

            // Debug output modes
            if invocation.dump_tokens {
                println!("{:#?}", invocation.tokens);
                return ExitCode::SUCCESS;
            }
            if invocation.dump_ast {
                println!("{:#?}", invocation.program);
                return ExitCode::SUCCESS;
            }
            if invocation.check {
                println!("{}: syntax OK", invocation.script.display());
                return ExitCode::SUCCESS;
            }

            // Transpile mode: generate C, compile, execute
            let project_root = invocation
                .script
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));

            // Handle `clean` section without script definition
            let has_requested_section = invocation.program.items.iter().any(|item| {
                matches!(
                    item,
                    roller_parser::TopLevelItem::Section(section)
                        if section.name == invocation.section
                )
            });
            if invocation.section == "clean" && !has_requested_section {
                if invocation.dry_run {
                    println!(
                        "would remove {}",
                        project_root.join(".roller").join("build").display()
                    );
                    return ExitCode::SUCCESS;
                }
                return match roller_cli::clean_build_directory(project_root) {
                    Ok(true) => {
                        println!(
                            "removed {}",
                            project_root.join(".roller").join("build").display()
                        );
                        ExitCode::SUCCESS
                    }
                    Ok(false) => {
                        println!("nothing to clean");
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("error: {error}");
                        ExitCode::FAILURE
                    }
                };
            }

            // Transpile: AST → C
            let c_source = match roller_transpiler::emit_program(
                &invocation.program,
                &invocation.script.display().to_string(),
            ) {
                Ok(c) => c,
                Err(error) => {
                    eprintln!("transpile error: {error}");
                    return ExitCode::from(3);
                }
            };

            // Write C source to .roller/
            let roller_dir = project_root.join(".roller");
            if let Err(e) = std::fs::create_dir_all(&roller_dir) {
                eprintln!("error: cannot create {}: {e}", roller_dir.display());
                return ExitCode::FAILURE;
            }

            let c_path = roller_dir.join("build_script.c");
            let h_path = roller_dir.join("roller-runtime.h");

            if let Err(e) = std::fs::write(&c_path, &c_source) {
                eprintln!("error: cannot write {}: {e}", c_path.display());
                return ExitCode::FAILURE;
            }

            // Write the runtime header
            if let Err(e) = std::fs::write(h_path, roller_transpiler::runtime_header()) {
                eprintln!("error: cannot write runtime header: {e}");
                return ExitCode::FAILURE;
            }

            // Compile: try tcc, then gcc, then clang
            let exit_code = compile_and_run(
                &c_path,
                &roller_dir,
                project_root,
                &invocation.section,
                invocation.jobs.get(),
                invocation.dry_run,
                invocation.verbose,
            );

            // Clean up generated files unless --keep-artifacts
            if !invocation.verbose {
                let _ = std::fs::remove_file(&c_path);
            }

            exit_code
        }
        Err(roller_cli::CliError::Arguments(error)) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            ExitCode::from(code)
        }
        Err(error @ (roller_cli::CliError::Lex(_) | roller_cli::CliError::Parse(_))) => {
            let args = roller_cli::Cli::parse();
            let script = match args.targets.as_slice() {
                [_] => std::path::PathBuf::from("build.roller"),
                [script, _] => std::path::PathBuf::from(script),
                _ => std::path::PathBuf::from("build.roller"),
            };
            let source = std::fs::read_to_string(&script).unwrap_or_default();
            eprintln!(
                "{}",
                roller_cli::format_frontend_diagnostic(&error, &script, &source)
            );
            ExitCode::from(3)
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Compile the generated C source and run the resulting binary.
fn compile_and_run(
    c_path: &Path,
    roller_dir: &Path,
    project_root: &Path,
    section: &str,
    jobs: usize,
    dry_run: bool,
    verbose: bool,
) -> ExitCode {
    let include_dir = roller_dir.display().to_string();

    // Try compilers in order: tcc → gcc → clang
    let compilers: &[(&str, &[&str])] = &[
        ("tcc", &["-run", "-I"]),
        ("gcc", &["-O0", "-o", "-I"]),
        ("clang", &["-O0", "-o", "-I"]),
    ];

    let section_owned = section.to_string();
    let jobs_str = jobs.to_string();
    let project_root_str = project_root.display().to_string();

    for &(compiler, _) in compilers {
        // Check if compiler exists
        if which(compiler).is_none() {
            continue;
        }

        let binary_path = roller_dir.join("build_script");

        let status = if compiler == "tcc" {
            // tcc -run -I <dir> <file> <section> <jobs> [flags]
            let mut args: Vec<&str> = vec!["-run", "-I", &include_dir];
            args.push(c_path.to_str().unwrap_or(""));
            args.push(&section_owned);
            args.push(&jobs_str);
            if dry_run {
                args.push("--dry-run");
            }
            if verbose {
                args.push("--verbose");
            }

            if verbose {
                eprintln!("+ {} {}", compiler, args.join(" "));
            }

            std::process::Command::new(compiler)
                .args(&args)
                .env("ROLLER_ROOT", &project_root_str)
                .status()
        } else {
            // gcc/clang -O0 -I <dir> -o <binary> <file>
            let compile_status = std::process::Command::new(compiler)
                .args([
                    "-O0",
                    "-I",
                    &include_dir,
                    "-o",
                    binary_path.to_str().unwrap_or(""),
                    c_path.to_str().unwrap_or(""),
                ])
                .stdout(std::process::Stdio::null())
                .status();

            match compile_status {
                Ok(status) if status.success() => {
                    // Execute the binary
                    let mut run_args: Vec<&str> = vec![&section_owned];
                    run_args.push(&jobs_str);
                    if dry_run {
                        run_args.push("--dry-run");
                    }
                    if verbose {
                        run_args.push("--verbose");
                    }

                    if verbose {
                        eprintln!("+ {} {}", binary_path.display(), run_args.join(" "));
                    }

                    std::process::Command::new(&binary_path)
                        .args(&run_args)
                        .env("ROLLER_ROOT", &project_root_str)
                        .status()
                }
                Ok(status) => {
                    eprintln!(
                        "error: {} compilation failed (exit {})",
                        compiler,
                        status.code().unwrap_or(-1)
                    );
                    continue;
                }
                Err(e) => {
                    eprintln!("error: cannot run {}: {e}", compiler);
                    continue;
                }
            }
        };

        match status {
            Ok(status) => {
                return ExitCode::from(status.code().unwrap_or(1) as u8);
            }
            Err(e) => {
                eprintln!("error: execution failed: {e}");
                return ExitCode::from(5);
            }
        }
    }

    eprintln!("error: no C compiler found. Roller requires tcc, gcc, or clang.");
    eprintln!("Install TCC for fastest builds: https://bellard.org/tcc/");
    ExitCode::from(1)
}

/// Initialize a new Roller project by creating a build.roller file.
fn init_project(script_path: &Path) -> ExitCode {
    if script_path.exists() {
        eprintln!("error: {} already exists", script_path.display());
        return ExitCode::FAILURE;
    }

    match std::fs::write(script_path, BUILD_ROLLER_TEMPLATE) {
        Ok(()) => {
            println!("Created {}", script_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: cannot create {}: {e}", script_path.display());
            ExitCode::FAILURE
        }
    }
}

/// Check if an executable exists in PATH.
fn which(name: &str) -> Option<String> {
    let path_env = std::env::var("PATH").ok()?;
    for dir in path_env.split(':') {
        let full = format!("{}/{}", dir, name);
        if std::path::Path::new(&full).is_file() {
            // Check execute permission
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&full) {
                if meta.permissions().mode() & 0o111 != 0 {
                    return Some(full);
                }
            }
        }
    }
    None
}
