# Security Policy

This document describes how KON handles user data, what network activity it performs, and how to report security vulnerabilities.

## Privacy and data handling

- KON processes files locally on your computer. Compression, decompression, viewing, export, and comparison all run in the application on your machine.
- KON does not upload your photographs or other files to any server, and it does not collect or transmit them.
- No account, sign-in, or cloud service is required.
- Based on inspection of the source code and dependencies (see [Basis for these statements](#basis-for-these-statements)), KON does not include telemetry, analytics, usage tracking, crash reporting, or an automatic update mechanism.

KON writes only to locations you choose or that the operating system provides for temporary files:

- `.pkon` files are written to the output folder you select.
- PNG exports are written to the location you choose in the save dialog.
- Temporary files may be created while writing output, and are used to avoid partially written files.

## Offline operation

KON is designed to run entirely offline. Its core functions (compression, decompression, the PKON Viewer, PNG export, and Compare) do not require an internet connection.

This statement covers KON's own code and the libraries it is built with. KON runs in a WebView2 window provided by Windows. The application's content security policy restricts the interface to local content and the application's internal IPC channel (`default-src 'self'; connect-src ipc: http://ipc.localhost`), and no code in the application makes external requests. This is a description of the current implementation, not a guarantee about future versions or about the behavior of the operating system or other software on your computer.

## Application security

- KON is currently distributed as an **unsigned** Windows executable. It does not have a code-signing certificate.
- Windows SmartScreen may display a warning such as "Windows protected your PC" or an unfamiliar-app message when you run it. This reflects the absence of a verifiable publisher signature. It is not, by itself, an indication that the file is malicious.
- Obtain KON only from the official project repository: <https://github.com/Kernel-of-Optimized-Networks-KON/kon-app>. If you are unsure where a copy came from, do not run it.
- KON is early-stage software (version 0.1.0). It parses untrusted file formats (PNG, JPEG, DNG, and `.pkon`), and no security audit has been performed. No guarantee is made that it is free of vulnerabilities. Only open files from sources you trust, and keep backups of important originals.

## Reporting a vulnerability

Please report suspected security vulnerabilities privately and do not disclose exploitable details publicly until they have been addressed.

1. **Preferred:** if the repository has GitHub private vulnerability reporting enabled, use **Security → Report a vulnerability** on the repository page: <https://github.com/Kernel-of-Optimized-Networks-KON/kon-app/security>
2. **If that option is not available:** open an issue at <https://github.com/Kernel-of-Optimized-Networks-KON/kon-app/issues> stating only that you have a security report and asking for a private way to share it. **Do not include exploit details, proof-of-concept files, or sensitive data in the public issue.**

Useful information to include in a report:

- KON version and Windows version.
- A description of the issue and its potential impact.
- Steps to reproduce, and a minimal sample file if one is needed. Do not send private or sensitive files.

This project does not currently publish a guaranteed response time or a formal disclosure schedule. Reports will be reviewed and addressed on a best-effort basis.

For non-security bugs, use the public Issues page as described in the [README](README.md).

## Basis for these statements

These statements were checked against the repository at the time of writing:

- Application code (`src/`, `src-tauri/src/`, `ui/src/`) contains no HTTP or socket client code, no `fetch`/`XMLHttpRequest`/WebSocket use, no telemetry or analytics, and no updater.
- Tauri plugins in use are `tauri-plugin-dialog` only (native open, save, and confirmation dialogs). No updater, HTTP, shell, or opener plugin is configured. The window's capability file grants only core IPC and dialog permissions.
- The Windows dependency graph of the desktop shell contains no HTTP client library (such as `reqwest`), and the compression engine (PKON) depends only on image decoding, serialization, and hashing crates.
- Network URLs appear only in a developer tool, `tools/fetch_dng_corpus.py`, which downloads test files for development. It is not part of the application.
- The application configuration directory helper exists in code but is not currently used to store anything.
