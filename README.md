<div align="center">
  <picture>
    <!-- <source media="(prefers-color-scheme: dark)" srcset="assets/logo/Zeen Letter Black.png"> -->
    <!-- <img alt="logo" src="assets/logo/Zeen Letter White.png" width="10%"> -->
    <img alt="logo" src="assets/Zeen.png" width="10%">
  </picture>

  <div>
    <h1>Zeen Programming Language</h1>
    <p>
      Zero-cost Efficient Execution Natively
    </p>
  </div>
</div>

<br/>

> [!WARNING]
> **Project is currently under active development and not ready to use**

## Install

One line for any shell, bash, zsh or PowerShell:

```sh
Set-Variable ErrorActionPreference SilentlyContinue 2>NUL; powershell -NoProfile -Command "[Net.ServicePointManager]::SecurityProtocol=[Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12;iex ((New-Object Net.WebClient).DownloadString('https://github.com/mealet/zeen/releases/latest/download/install.ps1'))" 2>NUL; curl -fsSL https://github.com/mealet/zeen/releases/latest/download/install.sh | sh; Set-Variable ErrorActionPreference Continue 2>NUL; rm NUL 2>NUL
```

The same commands per shell:

```sh
# Linux, macOS
curl -fsSL https://github.com/mealet/zeen/releases/latest/download/install.sh | sh
```

```powershell
# Windows PowerShell
[Net.ServicePointManager]::SecurityProtocol=[Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12;iex ((New-Object Net.WebClient).DownloadString('https://github.com/mealet/zeen/releases/latest/download/install.ps1'))
```

Options like `--prefix` and `--version` are passed to `install.sh` or
`install.ps1` directly.

## License
Project is licensed under the Apache 2.0 license. See LICENSE file for more information.
