# @dhruv2mars/javelin

[Javelin](https://javelin-codes.vercel.app) is version control for code that people and
agents write together: the `javelin` command line replaces Git, and `javelin-node` hosts
repositories on infrastructure you run.

```sh
npm i -g @dhruv2mars/javelin      # or: bun add -g @dhruv2mars/javelin
javelin --version
javelin init
```

This package holds no program itself. npm or bun installs the one optional dependency that
matches your machine, `@dhruv2mars/javelin-PLATFORM`, and the `javelin` and `javelin-node`
commands run its binaries directly, without Node.

| Platform | Package | Status |
| --- | --- | --- |
| macOS, Apple silicon | `@dhruv2mars/javelin-darwin-arm64` | supported |
| macOS, Intel | `@dhruv2mars/javelin-darwin-x64` | supported |
| Linux x86_64, glibc 2.28+ | `@dhruv2mars/javelin-linux-x64` | supported |
| Linux arm64, glibc 2.28+ | `@dhruv2mars/javelin-linux-arm64` | supported |

Do not install with `--omit=optional` or `--no-optional`: that leaves the commands with no
binary to run. Windows has no release yet. Javelin is pre-release. Source: https://entire.com/et/dhruv2mars/javelin
