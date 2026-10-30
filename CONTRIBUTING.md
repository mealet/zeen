Hello! Thank you for your interest in the project's life.
Here's the contribution guide for you:

## Repository Issues
Found a bug? Wanna suggest an improvement? You can always open an issue in this repository:
1. Open repo's [issues](https://github.com/mealet/zeen/issues)
2. Please ensure that your problem/improvement wasn't already reported (use search)
3. Push the button `New issue`
4. Use one of suggested templates and follow their instructions

## Submitting Changes

> [!NOTE]
> Project uses `justfile` for faster development recipes, so its highly recommended to install [just](https://github.com/casey/just)
> Main recipe file is located in `compiler` folder, list all available recipes by:
> ```bash
> just -l
> ```

1. Fork the repository and branch from `main`, then clone your fork and crate a new branch:
```bash
# Clone
git clone https://github.com/YOU/zeen.git
cd zeen

# Switch to main branch
git switch main

# Create new branch and switch to it
git switch -c NEW_BRANCH
git push origin NEW_BRANCH
```

2. Do your work and commit changes following title prefixes:
```
feat: ...   - New Feature
fix: ...    - Bug Fix
test: ...   - Add or update test(s)
ref: ...    - Code refactor
perf: ...   - Performance improvement(s)
chore: ...  - Dependencies, packages, configs update
ci: ...     - CI/CD changes

lib         |- Libraries changes
lib/core    |
lib/std     |

repo:       - Repository update (README, LICENSE, templates and etc.)
```

3. Commit and push your changes:
```bash
git push -u origin NEW_BRANCH
```
4. Open pull request at https://github.com/mealet/zeen/pulls

_Contributions are accepted under the project's license (see [LICENSE](LICENSE))_

## AI Usage
The use of AI tools and coding agents is allowed and welcome. However, contributors are expected
to understand, review, and take responsibility for all submitted code, regardless of how it was produced.

AI generated contributions are subject to the same quality standards as any other contribution.
Do not submit code you cannot explain, verify, or maintain. Contributions must not compromise the
project's stability, security, or maintainability.

## Pull Requests
> [!NOTE]
> ### Ensure that
> - your code matches the project's code style
> - your code doesn't break codebase work
> - you've written readable and understandable code
> - you've added/changed necessary tests

> [!NOTE]
> ### Please do...
> - Fix all analyzer warnings. Use: `cargo clippy` or `just clippy`
> - Format code by rust formatter. Use: `cargo fmt` or `just fmt`
> - Provide related unit tests for the changes
> - Provide short description of your work in pull request

## Tests
All tests are run from the `compiler/` directory with `just test` (uses cargo-nextest if installed, otherwise falls back to `cargo test`). <br/>
It is used to ensure that your changes don't break other program pieces.

Tests are the most important part of contribution, so always be sure that you've added necessary tests (including system golden tests, see below). <br/>
- Rust Tests Guide: [Cargo Tests Guide](https://doc.rust-lang.org/cargo/guide/tests.html)
- Golden system tests live in `compiler/zeen/tests/test_cases/`: each case is a pair of `.zn` + `.expected` files. Expected file is the expected stdout (with optional first line `@! <exit-code> free-output`), compiled and run in both Debug and Release by `compiler/zeen/tests/system_tests.rs`

## Building
1. Install the [Rust Programming Language](https://www.rust-lang.org/) and [LLVM 22](https://www.llvm.org/docs/GettingStarted.html)
2. Clone the repository:
```sh
git clone https://github.com/mealet/zeen
```
3. Build the compiler:
```sh
cd zeen/compiler
cargo build --release
```

The executable lands in `compiler/target/release`.
