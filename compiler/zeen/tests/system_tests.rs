use assert_cmd::Command;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

struct FailedTest {
    pub test_case: String,
    pub modes: String,
    pub error: String,
}

#[derive(Clone, Copy)]
enum ModeName {
    Debug,
    Release,
}

impl ModeName {
    fn as_str(&self) -> &'static str {
        match self {
            ModeName::Debug => "Debug",
            ModeName::Release => "Release",
        }
    }
}

struct Expected {
    pub output: String,
    pub exit_code: Option<i32>,
    pub free_output: bool,
    pub skip_windows: bool,
    pub skip_unix: bool,
}

fn normalize_output(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn count_digits(mut number: usize) -> usize {
    if number == 0 {
        return 1;
    }

    let mut count = 0;
    while number != 0 {
        number /= 10;
        count += 1;
    }

    count
}

fn test_jobs() -> usize {
    std::env::var("ZEEN_TEST_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|jobs| jobs.get())
                .unwrap_or(1)
        })
}

fn discover_test_cases() -> Vec<(String, String)> {
    const TEST_CASES_DIRECTORY: &str = "tests/test_cases";
    let mut test_cases = Vec::new();

    discover_test_cases_recursive(TEST_CASES_DIRECTORY, &mut test_cases, "");

    test_cases
}

fn discover_test_cases_recursive(
    dir: &str,
    test_cases: &mut Vec<(String, String)>,
    relative_path: &str,
) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                let dir_name = path.file_name().unwrap().to_str().unwrap();

                let new_relative_path = if relative_path.is_empty() {
                    dir_name.to_string()
                } else {
                    format!("{relative_path}/{dir_name}")
                };

                discover_test_cases_recursive(
                    path.to_str().unwrap(),
                    test_cases,
                    &new_relative_path,
                );
            } else if path.extension().and_then(|os_str| os_str.to_str()) == Some("zn")
                && let Some(stem) = path.file_stem().and_then(|os_str| os_str.to_str())
            {
                let expected_path = path.with_extension("expected");

                if expected_path.exists() {
                    let test_name = if relative_path.is_empty() {
                        stem.to_string()
                    } else {
                        format!("{relative_path}/{stem}")
                    };

                    let test_path = if relative_path.is_empty() {
                        stem.to_string()
                    } else {
                        format!("{relative_path}/{stem}")
                    };

                    test_cases.push((test_name, test_path));
                }
            }
        }
    }
}

fn parse_expected(content: &str) -> Expected {
    let mut exit_code = None;
    let mut free_output = false;
    let mut skip_windows = false;
    let mut skip_unix = false;

    let output = if content.starts_with("@!") {
        let header_end = content
            .find('\n')
            .map(|index| index + 1)
            .unwrap_or(content.len());

        for token in content[..header_end].split_whitespace().skip(1) {
            match token {
                "free-output" => free_output = true,
                "skip-windows" => skip_windows = true,
                "skip-unix" => skip_unix = true,
                token => {
                    if let Ok(number) = token.parse::<i32>() {
                        exit_code = Some(number);
                    }
                }
            }
        }

        normalize_output(&content[header_end..])
    } else {
        normalize_output(content)
    };

    Expected {
        output,
        exit_code,
        free_output,
        skip_windows,
        skip_unix,
    }
}

fn run_binary(binary_path: &str) -> Output {
    std::process::Command::new(binary_path)
        .output()
        .expect("failed to run test binary")
}

/// The repository's std library root, passed as `--std` so golden tests can
/// reach `use std.*` modules from the filesystem (std is never embedded).
fn std_dir() -> &'static Path {
    static STD_DIR: OnceLock<PathBuf> = OnceLock::new();

    STD_DIR.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../lib/std")
            .canonicalize()
            .expect("repo `lib/std` must exist next to the compiler workspace")
    })
}

struct Case {
    name: String,
    path: String,
    skipped: bool,
    numeration: String,
}

type CaseFailures = Vec<(ModeName, Vec<String>)>;
type CaseOutcome = anyhow::Result<CaseFailures>;

/// Compiles and runs a single test case in the given mode, returning a list
/// of errors (empty if the test passed for this mode).
fn run_test_case_in_mode(
    input_file: &str,
    expected_file: &str,
    mode: ModeName,
) -> anyhow::Result<Vec<String>> {
    let binary_name = format!(
        "zeen-test-{}-{}",
        input_file.replace("/", "_"),
        mode.as_str().to_lowercase()
    );
    let binary_path = std::env::temp_dir().join(&binary_name);
    let binary_path_str = binary_path.to_str().unwrap();

    let mut compile_cmd = Command::cargo_bin(env!("CARGO_PKG_NAME"))?;
    compile_cmd
        .arg(input_file)
        .arg(binary_path_str)
        .arg("-m")
        .arg(mode.as_str())
        .arg("--std")
        .arg(std_dir());

    let compilation_result = compile_cmd.assert().try_success();
    if let Err(compilation_error) = compilation_result {
        let _ = fs::remove_file(&binary_path);
        return Ok(vec![format!(
            "Compilation failed in {} mode:\n{}",
            mode.as_str(),
            compilation_error
        )]);
    }

    let expected_content = fs::read_to_string(expected_file)?;
    let expected = parse_expected(&expected_content);

    let output = run_binary(binary_path_str);
    let actual_exit_code = output.status.code().unwrap_or(-1);
    let actual_stdout = normalize_output(&String::from_utf8_lossy(&output.stdout));

    let _ = fs::remove_file(&binary_path);

    let mut errors = Vec::new();

    if !expected.free_output && actual_stdout != expected.output {
        errors.push(format!(
            "Output mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
            expected.output, actual_stdout
        ));
    }

    if let Some(expected_exit) = expected.exit_code {
        if actual_exit_code != expected_exit {
            errors.push(format!(
                "Exit code mismatch: expected {expected_exit}, got {actual_exit_code}"
            ));
        }
    } else if !expected.free_output && actual_exit_code != 0 {
        errors.push(format!(
            "Exit code mismatch: expected 0, got {actual_exit_code}"
        ));
    }

    Ok(errors)
}

fn run_case(case: &Case) -> CaseOutcome {
    let input_file = format!("tests/test_cases/{}.zn", case.path);
    let expected_file = format!("tests/test_cases/{}.expected", case.path);

    let debug_errors = run_test_case_in_mode(&input_file, &expected_file, ModeName::Debug)?;
    let release_errors = run_test_case_in_mode(&input_file, &expected_file, ModeName::Release)?;

    let mut failures = Vec::new();

    if !debug_errors.is_empty() {
        failures.push((ModeName::Debug, debug_errors));
    }
    if !release_errors.is_empty() {
        failures.push((ModeName::Release, release_errors));
    }

    Ok(failures)
}

#[test]
fn dbg_macro_prints_containers() -> anyhow::Result<()> {
    let binary_path =
        std::env::temp_dir().join(format!("zeen-test-dbg-container-{}", std::process::id()));
    let binary_path_str = binary_path.to_str().unwrap();

    Command::cargo_bin(env!("CARGO_PKG_NAME"))?
        .arg("tests/test_cases/dbg_container.zn")
        .arg(binary_path_str)
        .args(["-m", "Debug", "--std"])
        .arg(std_dir())
        .assert()
        .success();

    let debug_output = run_binary(binary_path_str);
    let _ = fs::remove_file(&binary_path);
    assert_eq!(
        normalize_output(&String::from_utf8_lossy(&debug_output.stdout)),
        "[dbg_container.zn:6]> `a` = {1, 2, 3, 4}\n\
         [dbg_container.zn:7]> `s` = {1, 2, 3, 4}\n\
         [dbg_container.zn:8]> `p` = {1, 2, 3, 4}\n\
         [dbg_container.zn:9]> `3` = 3\n"
    );

    Command::cargo_bin(env!("CARGO_PKG_NAME"))?
        .arg("tests/test_cases/dbg_container.zn")
        .arg(binary_path_str)
        .args(["-m", "Release", "--std"])
        .arg(std_dir())
        .assert()
        .success();

    let release_output = run_binary(binary_path_str);
    let _ = fs::remove_file(&binary_path);
    assert_eq!(
        normalize_output(&String::from_utf8_lossy(&release_output.stdout)),
        ""
    );

    Ok(())
}

#[test]
fn golden_system_tests() -> anyhow::Result<()> {
    let discovered = discover_test_cases();
    let tests_count = discovered.len();
    let tests_count_digits = count_digits(tests_count);
    let is_windows = std::env::consts::OS == "windows";

    let mut cases = Vec::with_capacity(tests_count);
    for (index, (test_name, test_path)) in discovered.into_iter().enumerate() {
        let current_number_digits = count_digits(index + 1).wrapping_sub(1);
        let expected_content =
            fs::read_to_string(format!("tests/test_cases/{test_path}.expected"))?;
        let expected = parse_expected(&expected_content);

        cases.push(Case {
            name: test_name,
            path: test_path,
            skipped: if is_windows {
                expected.skip_windows
            } else {
                expected.skip_unix
            },
            numeration: format!(
                "{}{}|",
                index + 1,
                " ".repeat(tests_count_digits - current_number_digits)
            ),
        });
    }

    let runnable = cases.iter().filter(|case| !case.skipped).count();
    let jobs = test_jobs().min(runnable.max(1));
    let next = AtomicUsize::new(0);
    let outcomes: Vec<Mutex<Option<CaseOutcome>>> =
        cases.iter().map(|_| Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);

                    let Some(case) = cases.get(index) else {
                        break;
                    };

                    if case.skipped {
                        continue;
                    }

                    println!("{} Running test: `{}`", case.numeration, case.name);

                    let result = run_case(case);
                    let mut outcome = outcomes[index]
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *outcome = Some(result);
                }
            });
        }
    });

    let mut passed_tests: Vec<String> = Vec::new();
    let mut failed_tests: Vec<FailedTest> = Vec::new();
    let mut skipped_tests: Vec<String> = Vec::new();

    for (index, case) in cases.iter().enumerate() {
        if case.skipped {
            println!(
                "{} Running test: `{}` [skipped on this platform]",
                case.numeration, case.name
            );
            skipped_tests.push(case.name.clone());
            continue;
        }

        let failures = outcomes[index]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .expect("every runnable case produces a result")?;

        if failures.is_empty() {
            passed_tests.push(case.name.clone());
            continue;
        }

        let modes = if failures.len() == 2 {
            "debug and release".to_string()
        } else {
            failures[0].0.as_str().to_string()
        };

        let mut error = String::new();
        for (mode, mode_errors) in &failures {
            error.push_str(&format!("[{}]\n", mode.as_str()));
            error.push_str(&mode_errors.join("\n"));
            error.push('\n');
        }

        failed_tests.push(FailedTest {
            test_case: case.name.clone(),
            modes,
            error,
        });
    }

    println!();
    println!("Passed tests ({}):", passed_tests.len());
    for test_name in &passed_tests {
        println!("  ✓ {test_name}");
    }

    println!();
    println!("Skipped tests ({}):", skipped_tests.len());
    for test_name in &skipped_tests {
        println!("  - {test_name}");
    }

    println!();
    println!("Failed tests ({}):", failed_tests.len());
    for failed_test in &failed_tests {
        println!(
            "  ✗ {} (failed in {})",
            failed_test.test_case, failed_test.modes
        );
        println!("\n{}", failed_test.error);
        println!();
    }

    let run_count = tests_count - skipped_tests.len();

    println!(
        "successful/everything: {}/{}",
        passed_tests.len(),
        run_count
    );

    if failed_tests.is_empty() {
        return Ok(());
    }

    Err(anyhow::anyhow!("Tests failed"))
}
