# Security Policy

## Supported Versions

Security updates are provided for the latest stable release of Zeen.

Older releases may not receive security fixes. Users are encouraged to upgrade to the latest stable version.

## Reporting a Vulnerability

The preferred way to report security vulnerabilities is through [GitHub Private Vulnerability Reporting](https://github.com/mealet/zeen/security/advisories/new).

**Please do not disclose security vulnerabilities through public GitHub Issues, Discussions, or Pull Requests.**

Alternatively, you can contact the project maintainer, mealet, privately:

- GitHub: https://github.com/mealet
- Email: [workjason34@gmail.com](mailto:workjason34@gmail.com)

When reporting a vulnerability, please include:

- A clear description of the vulnerability.
- The affected component and version.
- Steps to reproduce the issue, if possible.
- Potential security impact.
- Any suggested fixes or mitigations, if available.

Please avoid including sensitive information that is not necessary to demonstrate the vulnerability.

## Response Process

After receiving a vulnerability report, the maintainer will:

1. Review the report and assess its validity and potential impact.
2. Communicate with the reporter to request additional information if necessary.
3. Investigate and work on an appropriate fix.
4. Prepare a security update or patch.
5. Publish a security advisory once it is appropriate to disclose the vulnerability.

The reporter will be kept informed of significant developments whenever reasonably possible.

## Disclosure Policy

Please allow the maintainer reasonable time to investigate and address reported vulnerabilities before publicly disclosing them.

Coordinated disclosure is preferred to reduce the risk of exploitation before a fix becomes available.

The timing and scope of public disclosure will be determined based on the severity of the vulnerability and the availability of a fix.

## Scope

This policy covers security vulnerabilities affecting the Zeen project, including:

- Zeen compiler and its components.
- Standard library.
- Zora build system and package manager.
- Official development tools and integrations.
- Build scripts and release infrastructure maintained by the project.

Third-party dependencies should generally be reported to their respective maintainers, unless the vulnerability is introduced or exposed by Zeen-specific integration.

## Security Considerations

We particularly welcome reports concerning:

- Memory safety violations and undefined behavior.
- Arbitrary code execution or unintended command execution.
- Vulnerabilities in compiler input processing.
- Unsafe FFI interactions.
- Path traversal and unintended filesystem access.
- Dependency and supply-chain vulnerabilities.
- Build system and package manager security issues.

## Acknowledgments

We appreciate responsible security researchers and contributors who help make Zeen safer.

Attribution for reported vulnerabilities may be provided with the reporter's permission.
