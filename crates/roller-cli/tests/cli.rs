use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_succeeds() {
    Command::cargo_bin("roller")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("SECTION"));
}

#[test]
fn missing_default_script_fails() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("roller")
        .unwrap()
        .current_dir(temp.path())
        .arg("build")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("build.roller"));
}

#[test]
fn reads_an_explicit_script() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("Build.roller");
    fs::write(&script, "section build() { log::info(\"ok\"); }\n").unwrap();

    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .arg("build")
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));
}

#[test]
fn zero_jobs_is_rejected_as_a_cli_error() {
    Command::cargo_bin("roller")
        .unwrap()
        .args(["build", "--jobs", "0"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid value '0'"));
}

#[test]
fn check_reports_success_for_valid_source() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("build.roller");
    fs::write(&script, "section build() {}\n").unwrap();

    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .arg("build")
        .arg("--check")
        .assert()
        .success()
        .stdout(predicate::str::contains("syntax OK"));
}

#[test]
fn parser_error_has_source_location_and_exit_code_three() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("build.roller");
    fs::write(&script, "section build() { let x = 1 }\n").unwrap();

    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .arg("build")
        .arg("--check")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("expected `;`"))
        .stderr(predicate::str::contains("build.roller:1:"))
        .stderr(predicate::str::contains("^"));
}

#[test]
fn clean_fallback_removes_only_project_build_directory() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("build.roller");
    fs::write(&script, "section build() {}\n").unwrap();
    fs::create_dir_all(temp.path().join(".roller/build/nested")).unwrap();
    fs::write(temp.path().join(".roller/build/nested/a.o"), "").unwrap();
    fs::write(temp.path().join("keep.txt"), "keep").unwrap();

    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .arg("clean")
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"));
    assert!(!temp.path().join(".roller/build").exists());
    assert!(temp.path().join("keep.txt").exists());
}

#[test]
fn hello_c_example_builds_when_a_compiler_is_available() {
    let has_compiler = executable_exists("gcc") || executable_exists("clang");
    if !has_compiler {
        eprintln!("skipping hello-c integration test: neither gcc nor clang is available");
        return;
    }
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/hello-c")
        .canonicalize()
        .unwrap();
    let script = example.join("build.roller");

    Command::cargo_bin("roller")
        .unwrap()
        .args([script.as_os_str(), std::ffi::OsStr::new("clean")])
        .assert()
        .success();
    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .args(["build", "--jobs", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("LINK myproject"));

    let output = std::process::Command::new(example.join("myproject"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Hello from Roller!\n"
    );

    Command::cargo_bin("roller")
        .unwrap()
        .args([script.as_os_str(), std::ffi::OsStr::new("clean")])
        .assert()
        .success();
    fs::remove_file(example.join("myproject")).unwrap();
}

#[test]
fn hello_zig_example_builds_when_zig_is_available() {
    if !executable_exists("zig") {
        eprintln!("skipping hello-zig integration test: zig is not available");
        return;
    }
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/hello-zig")
        .canonicalize()
        .unwrap();
    let script = example.join("build.roller");

    Command::cargo_bin("roller")
        .unwrap()
        .args([script.as_os_str(), std::ffi::OsStr::new("clean")])
        .assert()
        .success();
    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .args(["build", "--jobs", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("LINK myproject"));

    let output = std::process::Command::new(example.join("myproject"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Hello from Roller!\n"
    );

    Command::cargo_bin("roller")
        .unwrap()
        .args([script.as_os_str(), std::ffi::OsStr::new("clean")])
        .assert()
        .success();
    fs::remove_file(example.join("myproject")).unwrap();
}

#[test]
fn compiler_implementations_keep_field_types_and_compile_signatures_independent() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("build.roller"),
        r#"library "numeric" {
    compiler tool { optimize: integer }
    function select(compiler: Compiler) { compiler = self::tool; }
    implement Self::tool {
        function compile(compiler: self, input: String) -> Compiler {
            compiler.optimize = 1;
            return compiler;
        }
    }
}

library "symbolic" {
    compiler tool { optimize: String }
    function select(compiler: Compiler) { compiler = self::tool; }
    implement Self::tool {
        function compile(compiler: self, input: String, mode: String) -> String {
            compiler.optimize = mode;
            return compiler.optimize;
        }
    }
}

section build() {
    let compiler = Compiler::new();
    symbolic::select(&compiler);
    let result = compiler.compile("main.zig", "ReleaseFast");
    log::info(result);
}
"#,
    )
    .unwrap();

    Command::cargo_bin("roller")
        .unwrap()
        .current_dir(temp.path())
        .arg("build")
        .assert()
        .success()
        .stdout(predicate::str::contains("ReleaseFast"));
}

fn executable_exists(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(name).is_file())
    })
}

#[test]
fn invalid_character_is_a_frontend_error() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("build.roller");
    fs::write(&script, "section build() { @ }\n").unwrap();
    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .arg("build")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("unexpected character `@`"));
}

#[test]
fn missing_section_is_a_runtime_error() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("build.roller");
    fs::write(&script, "section build() {}\n").unwrap();
    Command::cargo_bin("roller")
        .unwrap()
        .arg(&script)
        .arg("missing")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("unknown section"));
}

#[test]
fn excessive_job_count_is_rejected() {
    Command::cargo_bin("roller")
        .unwrap()
        .args(["build", "--jobs", "999999"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("must not exceed 1024"));
}
