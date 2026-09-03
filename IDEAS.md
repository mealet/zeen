List of ideas for future language infrastructure (may be implemented before v1.0.0, but after first stable release v0.1.0):

> [!NOTE]
> ### Preprocessor:
> ```zn
> @os[linux | macos] { ... }
> 
> @os[windows] { ... }
> 
> @env[gnu] { ... } else { ... }
> 
> @arch[x86] { ... }
> 
> @debug { ... }
> 
> @release { ... }
> 
> ----
> 
> fn main() {
>   let is_linux: bool = @var[is_os_linux];
>   @println("{}", is_linux);
> }
> ```

> [!NOTE]
> ### Const functions
> ```
> const fn square(x: u32) u32 {
>   x * x
> }
> ```
> Requires some kind of MIR interpreter with macros comptime replacements and stack trace on const fn panic.

> [!NOTE]
> ### Formatter
> Simple idea: parse AST -> write it back to file with specified rules

> [!NOTE]
> ### LSP Server
> Maybe use `tower-lsp` crate

> [!NOTE]
> ### Docs Builder
> Like in Rust, docs comments:
> ```zn
> /// Function that returnes **0** (supports markdown yeah) 
> fn foo() i32 {
>   return 0;
> }
> ```

> [!NOTE]
> ### Build System With Package Manager
> ```toml
> [project]
> name = "hello world app"
> version = "0.0.0"
> authors = ["..."]
> license = "MIT"
> repo = "..."
> 
> [bin]
> output = "main"
> flags = ["no-warnings", "no-std"]
> 
> [deps]
> json = "https://github.com/mealet/json-zn@0.13.0"
> ```
