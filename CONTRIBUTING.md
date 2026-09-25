Hello! Thank you for your interest in the project's life.
Here's the contribution guide for you:

### Repository Issues
Found a bug? Wanna suggest an improvement? You can always open an issue in this repository:
1. Open repo's [issues](https://github.com/mealet/zeen/issues)
2. Please ensure that your problem/improvement wasn't already reported
3. Push the button `Create issue`
4. Write a clear title and describe the problem or suggestion

### Pull Requests
> [!NOTE]
> ### Ensure that
> - your code matches the project's code style
> - your code doesn't break compiler's work
> - you've written readable and understandable code
> - you've added/changed necessary tests

> [!NOTE]
> ### Please do...
> - Fix all analyzer warnings. Use: `just clippy`
> - Format code by rust formatter. Use: `just fmt`
> - Provide related unit tests for the changes
> - Provide short description of your work in pull request

### Tests
All tests are run from the `compiler/` directory with `just test` (uses cargo-nextest if installed, otherwise falls back to `cargo test`). <br/>
It is used to ensure that your changes don't break other program pieces. <br/>
Integration tests are the most important part of contribution, so always be sure that you've added necessary tests (including system golden tests, see below). <br/>
- Rust Tests Guide: [Cargo Tests Guide](https://doc.rust-lang.org/cargo/guide/tests.html)
- Golden system tests live in `compiler/zeen/tests/test_cases/`: each case is a `.zn` file plus an `.expected` file with the expected stdout (optional first line `@! <exit-code> free-output`), compiled and run in both Debug and Release by `compiler/zeen/tests/system_tests.rs`

### Building
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

The executable lands in `compiler/target/release`. <br/>
Utility recipes (`just run`, `just test`, `just fmt`, `just clippy`) are also run from `compiler/`.
