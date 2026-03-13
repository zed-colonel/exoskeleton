# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Exoskeleton, please report it responsibly.

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, please send a description of the vulnerability to the project maintainers
via a [GitHub Security Advisory](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability).

Include:
- A description of the vulnerability
- Steps to reproduce the issue
- Potential impact
- Any suggested fix, if available

We will acknowledge receipt within 48 hours and aim to provide a fix or mitigation
plan within 7 days for critical issues.

## Scope

Exoskeleton is designed as an embedded agent runtime with dual ActionQueue engines.
The current security posture assumes trusted-network deployment:

- Default bind: localhost only
- No built-in authentication on HTTP endpoints (expected to be fronted by a
  reverse proxy or service mesh in production)
- SQLite databases and AQ WAL files should be protected by OS-level file permissions
- The Cognitive AQ handles LLM inference and thread execution — token budgets and
  model escalation policies should be configured to prevent runaway consumption
- The Tool AQ (owned by WorldInterface) executes external actions — adapter-level
  access control and least-privilege policies should be enforced at deployment

## Supported Versions

Security updates are provided for the latest release only.
