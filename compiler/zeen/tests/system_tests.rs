use assert_cmd::Command;
use std::{fs, process::Output};

struct FailedTest {
    pub test_case: String,
    pub error: String,
}

struct Expected {
    pub output: String,
    pub exit_code: Option<i32>,
    pub free_output: bool,
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
            } else if path.extension().and_then(|os_str| os_str.to_str()) == Some("zn") {
                if let Some(stem) = path.file_stem().and_then(|os_str| os_str.to_str()) {
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
}

fn parse_expected(content: &str) -> Expected {
    let mut exit_code = None;
    let mut free_output = false;

    let output = if content.starts_with("@!") {
        let header_end = content
            .find('\n')
            .map(|index| index + 1)
            .unwrap_or(content.len());

        for token in content[..header_end].split_whitespace().skip(1) {
            match token {
                "free-output" => free_output = true,
                token => {
                    if let Ok(number) = token.parse::<i32>() {
                        exit_code = Some(number);
                    }
                }
            }
        }

        content[header_end..].to_string()
    } else {
        content.to_string()
    };

    Expected {
        output,
        exit_code,
        free_output,
    }
}

fn run_binary(binary_path: &str) -> Output {
    std::process::Command::new(binary_path)
        .output()
        .expect("failed to run test binary")
}

#[test]
fn golden_system_tests() -> anyhow::Result<()> {
    let test_cases = discover_test_cases();
    let tests_count = test_cases.len();
    let tests_count_digits = count_digits(tests_count);

    let mut passed_tests: Vec<String> = Vec::new();
    let mut failed_tests: Vec<FailedTest> = Vec::new();

    for (index, (test_name, test_path)) in test_cases.into_iter().enumerate() {
        let current_number_digits = count_digits(index + 1).wrapping_sub(1);
        let numeration = format!(
            "{}{}|",
            index + 1,
            " ".repeat(tests_count_digits - current_number_digits)
        );

        println!("{numeration} Running test: `{test_name}`");

        let input_file = format!("tests/test_cases/{test_path}.zn");
        let expected_file = format!("tests/test_cases/{test_path}.expected");

        let binary_name = format!("zeen-test-{}", test_path.replace("/", "_"));
        let binary_path = std::env::temp_dir().join(&binary_name);
        let binary_path_str = binary_path.to_str().unwrap();

        let mut compile_cmd = Command::cargo_bin(env!("CARGO_PKG_NAME"))?;
        compile_cmd.arg(&input_file).arg(binary_path_str);

        let compilation_result = compile_cmd.assert().try_success();
        if let Err(compilation_error) = compilation_result {
            failed_tests.push(FailedTest {
                test_case: test_name.clone(),
                error: compilation_error.to_string(),
            });
            continue;
        }

        let expected_content = fs::read_to_string(&expected_file)?;
        let expected = parse_expected(&expected_content);

        let output = run_binary(binary_path_str);
        let actual_exit_code = output.status.code().unwrap_or(-1);
        let actual_stdout = String::from_utf8_lossy(&output.stdout).to_string();

        let mut errors = Vec::new();

        if !expected.free_output {
            if actual_stdout != expected.output {
                errors.push(format!(
                    "Output mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                    expected.output, actual_stdout
                ));
            }
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

        let _ = fs::remove_file(&binary_path);

        if errors.is_empty() {
            passed_tests.push(test_name);
        } else {
            failed_tests.push(FailedTest {
                test_case: test_name,
                error: errors.join("\n"),
            });
        }
    }

    println!();
    println!("Passed tests ({}):", passed_tests.len());
    for test_name in &passed_tests {
        println!("  ✓ {test_name}");
    }

    println!();
    println!("Failed tests ({}):", failed_tests.len());
    for failed_test in &failed_tests {
        println!("  ✗ {}", failed_test.test_case);
        println!("\n{}", failed_test.error);
        println!();
    }

    println!("successful/everything: {}/{}", passed_tests.len(), tests_count);

    if failed_tests.is_empty() {
        return Ok(());
    }

    Err(anyhow::anyhow!("Tests failed"))
}
