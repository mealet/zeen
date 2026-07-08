# Zeen Libraries

## `core`
Small compiler core's modules that being injected to the compiler. <br/>
Creating big implementation modules in core library leads to heavy compiler binary, <br/>
please avoid non-compiler elements that can be moved to standard library (watch below).

Automatically includes in code.

## `std`
Standard library, must be included to have access:
```zeen
use std.string; // or any other modules
```
Some of modules can be system dependent (use system variables, specifications, functions, etc.)
