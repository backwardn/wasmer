//! This file will run at build time to autogenerate the WASI regression tests
//! It will compile the files indicated in TESTS, to:executable and .wasm
//! - Compile with the native rust target to get the expected output
//! - Compile with the latest WASI target to get the wasm
//! - Generate the test that will compare the output of running the .wasm file
//!   with wasmer with the expected output

use glob::glob;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use std::io;
use std::io::prelude::*;

use super::util;
use super::wasi_version::*;
use serde::{Deserialize, Serialize};

/// Compile and execute the test file as native code, saving the results to be
/// compared against later.
///
/// This function attempts to clean up its output after it executes it.
fn generate_native_output(
    temp_dir: &Path,
    file: &str,
    normalized_name: &str,
    args: &[String],
) -> io::Result<String> {
    let executable_path = temp_dir.join(normalized_name);
    println!(
        "Compiling program {} to native at {}",
        file,
        executable_path.to_string_lossy()
    );
    let native_out = Command::new("rustc")
        .arg(file)
        .arg("-o")
        .args(args)
        .arg(&executable_path)
        .output()
        .expect("Failed to compile program to native code");
    util::print_info_on_error(&native_out, "COMPILATION FAILED");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = executable_path
            .metadata()
            .expect("native executable")
            .permissions();
        perm.set_mode(0o766);
        println!(
            "Setting execute permissions on {}",
            executable_path.to_string_lossy()
        );
        fs::set_permissions(&executable_path, perm)?;
    }

    println!(
        "Executing native program at {}",
        executable_path.to_string_lossy()
    );
    // workspace root
    const EXECUTE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let result = Command::new(&executable_path)
        .current_dir(EXECUTE_DIR)
        .output()
        .expect("Failed to execute native program");
    util::print_info_on_error(&result, "NATIVE PROGRAM FAILED");

    Ok(String::from_utf8(result.stdout).unwrap())
}

/// compile the Wasm file for the given version of WASI
///
/// returns the path of where the wasm file is
fn compile_wasm_for_version(
    temp_dir: &Path,
    file: &str,
    base_dir: &Path,
    rs_mod_name: &str,
    version: WasiVersion,
) -> io::Result<PathBuf> {
    let out_dir = base_dir.join(version.get_directory_name());
    if !out_dir.exists() {
        fs::create_dir(&out_dir)?;
    }
    let wasm_out_name = {
        let mut wasm_out_name = out_dir.join(rs_mod_name);
        wasm_out_name.set_extension("wasm");
        wasm_out_name
    };
    println!("Reading contents from file `{}`", file);
    let file_contents: String = {
        let mut fc = String::new();
        let mut f = fs::OpenOptions::new().read(true).open(&file)?;
        f.read_to_string(&mut fc)?;
        fc
    };

    let temp_wasi_rs_file_name = temp_dir.join(format!("wasi_modified_version_{}.rs", rs_mod_name));
    {
        let mut actual_file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(&temp_wasi_rs_file_name)
            .unwrap();
        actual_file.write_all(b"#![feature(wasi_ext)]\n").unwrap();
        actual_file.write_all(file_contents.as_bytes()).unwrap();
    }

    println!(
        "Compiling wasm module `{}` with toolchain `{}`",
        &wasm_out_name.to_string_lossy(),
        version.get_compiler_toolchain()
    );
    let mut command = Command::new("rustc");

    command
        .arg(format!("+{}", version.get_compiler_toolchain()))
        .arg("--target=wasm32-wasi")
        .arg("-C")
        .arg("opt-level=z")
        .arg(&temp_wasi_rs_file_name)
        .arg("-o")
        .arg(&wasm_out_name);
    println!("Command {:?}", command);

    let wasm_compilation_out = command.output().expect("Failed to compile program to wasm");
    util::print_info_on_error(&wasm_compilation_out, "WASM COMPILATION");
    println!(
        "Removing file `{}`",
        &temp_wasi_rs_file_name.to_string_lossy()
    );

    // to prevent commiting huge binary blobs forever
    let wasm_strip_out = Command::new("wasm-strip")
        .arg(&wasm_out_name)
        .output()
        .expect("Failed to strip compiled wasm module");
    util::print_info_on_error(&wasm_strip_out, "STRIPPING WASM");
    let wasm_opt_out = Command::new("wasm-opt")
        .arg("-Oz")
        .arg(&wasm_out_name)
        .arg("-o")
        .arg(&wasm_out_name)
        .output()
        .expect("Failed to optimize compiled wasm module with wasm-opt!");
    util::print_info_on_error(&wasm_opt_out, "OPTIMIZING WASM");

    Ok(wasm_out_name)
}

/// Returns the a Vec of the test modules created
fn compile(temp_dir: &Path, file: &str, wasi_versions: &[WasiVersion]) {
    let src_code: String = fs::read_to_string(file).unwrap();
    let options: WasiOptions = extract_args_from_source_file(&src_code).unwrap_or_default();

    assert!(file.ends_with(".rs"));
    let rs_mod_name = {
        Path::new(&file.to_lowercase())
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string()
    };
    let base_dir = Path::new(file).parent().unwrap();
    let output = generate_native_output(temp_dir, &file, &rs_mod_name, &options.args)
        .expect("Generate native output");

    let test = WasiTest { output, options };
    let test_serialized = serde_json::to_string_pretty(&test).unwrap();

    wasi_versions
        .into_iter()
        .map(|&version| {
            let out_dir = base_dir.join(version.get_directory_name());
            if !out_dir.exists() {
                fs::create_dir(&out_dir).unwrap();
            }
            let wasm_out_name = {
                let mut wasm_out_name = out_dir.join(rs_mod_name.clone());
                wasm_out_name.set_extension("out");
                wasm_out_name
            };
            println!("Writing test output to {}", wasm_out_name.to_string_lossy());
            fs::write(&wasm_out_name, test_serialized.clone()).unwrap();

            println!("Compiling wasm version {:?}", version);
            compile_wasm_for_version(temp_dir, file, base_dir, &rs_mod_name, version)
                .expect(&format!("Could not compile Wasm to WASI version {:?}, perhaps you need to install the `{}` rust toolchain", version, version.get_compiler_toolchain()));
        }).for_each(drop); // Do nothing with it, but let the iterator be consumed/iterated.
}

const WASI_TEST_SRC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../wasi_test_resources/*.rs");
pub fn build(wasi_versions: &[WasiVersion]) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    for entry in glob(WASI_TEST_SRC_DIR).unwrap() {
        match entry {
            Ok(path) => {
                let test = path.to_str().unwrap();
                compile(temp_dir.path(), test, wasi_versions);
            }
            Err(e) => println!("{:?}", e),
        }
    }
    println!("All modules generated.");
}

/// This is the structure of the `.out` file
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WasiTest {
    /// The program expected output
    pub output: String,
    /// The program options
    pub options: WasiOptions,
}

/// The options provied when executed a WASI Wasm program
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WasiOptions {
    /// Mapped pre-opened dirs
    pub mapdir: Vec<(String, String)>,
    /// Environment vars
    pub env: Vec<(String, String)>,
    /// Program arguments
    pub args: Vec<String>,
    /// Pre-opened directories
    pub dir: Vec<String>,
}

/// Pulls args to the program out of a comment at the top of the file starting with "// WasiOptions:"
fn extract_args_from_source_file(source_code: &str) -> Option<WasiOptions> {
    if source_code.starts_with("// WASI:") {
        let mut args = WasiOptions::default();
        for arg_line in source_code
            .lines()
            .skip(1)
            .take_while(|line| line.starts_with("// "))
        {
            let tokenized = arg_line
                .split_whitespace()
                // skip trailing space
                .skip(1)
                .map(String::from)
                .collect::<Vec<String>>();
            let command_name = {
                let mut cn = tokenized[0].clone();
                assert_eq!(
                    cn.pop(),
                    Some(':'),
                    "Final character of argname must be a colon"
                );
                cn
            };

            match command_name.as_ref() {
                "mapdir" => {
                    if let [alias, real_dir] = &tokenized[1].split(':').collect::<Vec<&str>>()[..] {
                        args.mapdir.push((alias.to_string(), real_dir.to_string()));
                    } else {
                        eprintln!(
                            "Parse error in mapdir {} not parsed correctly",
                            &tokenized[1]
                        );
                    }
                }
                "env" => {
                    if let [name, val] = &tokenized[1].split('=').collect::<Vec<&str>>()[..] {
                        args.env.push((name.to_string(), val.to_string()));
                    } else {
                        eprintln!("Parse error in env {} not parsed correctly", &tokenized[1]);
                    }
                }
                "dir" => {
                    args.dir.push(tokenized[1].to_string());
                }
                "arg" => {
                    args.args.push(tokenized[1].to_string());
                }
                e => {
                    eprintln!("WARN: comment arg: `{}` is not supported", e);
                }
            }
        }
        return Some(args);
    }
    None
}
