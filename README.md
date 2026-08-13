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

## License
Project is licensed under the Apache 2.0 license. See LICENSE file for more information.
