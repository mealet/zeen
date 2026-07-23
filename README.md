<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/Zeen Letter Black.png">
    <img alt="logo" src="assets/logo/Zeen Letter White.png" width="10%">
  </picture>

  <div>
    <h1>Zeen Programming Language</h1>
    <p>
      Zero-cost Efficient Execution Natively
    </p>
  </div>
</div>

<br/><br/>

<b>⚠️ Project is currently under active development and not ready to use.</b>

<br/>

## Todo List
List to-do before release first stable ready-to-use version:
- [ ] Get language to working basic codegen (fn calls, macros, non-generic structs and functions definitions, includes).
- [ ] Have working move-semantics with RAII (auto drops when release).
- [ ] Get to stable working interfaces and generic types.
- [ ] Figure out and add C-like union declaration with generics.
- [ ] Implement `switch` expression
- [ ] Add ranges expressions for slices and etc.
- [ ] Add basic raw standard library elements (`Option[T]`, `List[T]`, `String` and etc, only basics).
- [ ] Add `Iterator` interface in `core` library for iterator loop _`for`_.
- [ ] Add `Write` interface in core with receiving `[]const T` as an argument

## Ideas
Ideas for the future language ecosystem:
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
>   @println("@var[os_linux]");
> }
> ```

> ### Formatter
> Simple idea: parse AST -> write it back to file with specified rules

> ### LSP Server
> Maybe use `tower-lsp` crate

> ### Docs Builder
> Like in Rust, docs comments:
> ```zn
> /// Function that returnes **0** (supports markdown yeah) 
> fn foo() i32 {
>   return 0;
> }
> ```

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
