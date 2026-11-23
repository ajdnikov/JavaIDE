# Java IDE (Rust)

A small, portable IDE for Java 25, written in Rust with [egui](https://github.com/emilk/egui).
It's a single `JavaIDE.exe` with a JDK bundled next to it: no installer, no internet connection,
and nothing written outside its own folder.

## Features

- **Dark mode** (default) and light mode: toolbar button, *View → Dark mode*, or Settings
- **Explorer sidebar**: browse folders, open files, and right-click to create, rename, delete (to the recycle bin) or reveal files, or to open a terminal in a folder
- **Editor**: Java syntax highlighting, line numbers, tabs, smart indent, bracket/brace/quote auto-closing (`{`, `(`, `[`, quotes), `Ctrl+/` comment toggle, Tab/Shift+Tab block indent, and CRLF files saved back as CRLF
- **Code completion**: suggestions appear from the first typed letter, or press `Ctrl+Space`. Every method and field of the JDK classes the IDE knows (strings, collections, streams, `java.time`, concurrency, …) and of your own classes — including inherited ones, enum constants and record components — is offered, also after chained calls (`list.get(0).trim().`). Matches are prefix, camel-case (`sb` → `StringBuilder`), substring or fuzzy, and items you accepted recently are suggested first (remembered between sessions)
  - keywords and snippets (`sout`, `psvm`, `main`, `fori`, `foreach`, `trycatch`, `println`, …)
  - JDK classes and their members (`String`, `List`, `Map`, `Scanner`, `Math`, `Stream`, Java 25 `IO`, …)
  - type-aware members after `.`, for example `List<String> xs` → `xs.add`, `var m = new HashMap<>()` → `m.put`, and `System.out.println`
  - your own project's classes, methods, fields and record components
- **Build & run**: `F5` runs `javac` on the whole project into `out/`, then runs the main class with `java`
  - Output is streamed live, and an **stdin** box lets you type input to `Scanner` / `IO.readln`
  - The **Problems** tab lists compiler errors; click one to jump to that line
  - Java 25 compact source files (`void main() { … }` with no class) work too
  - Files outside an opened folder run with the source launcher (`java File.java`)
  - Jars in a `lib/` folder go on the classpath automatically
- **Terminal**: a built-in shell (`cmd` on Windows) that starts in the project folder, with the bundled JDK on `PATH`, so `java`, `javac` and `jshell` work there

## Shortcuts

| Key | Action |
| --- | --- |
| F5 / Shift+F5 | Run / stop |
| Ctrl+B | Build |
| Ctrl+Space | Completion |
| Ctrl+/ | Toggle comment |
| Ctrl+S / Ctrl+Shift+S | Save / save all |
| Ctrl+N / Ctrl+O | New file / open folder |
| Ctrl+W / Ctrl+Tab | Close / next tab |
| Ctrl+` | Terminal |
| Ctrl+= / Ctrl+- | Font size |

## Project layout

Open any folder. Sources are taken from `src/main/java` if it exists, otherwise `src`,
otherwise the folder itself. Classes compile to `out/`. The main class is the current file
if it has a `main` method; otherwise the IDE uses the first file with one (it prefers `Main.java`).

## Portable folder layout

```
JavaIDE\
  JavaIDE.exe
  jdk\                    <- any JDK 25+ (the IDE finds it automatically)
  workspace\HelloWorld\   <- sample project, opened on first launch
  javaide-settings.json   <- created on first run (theme, font size, last folder)
```

The IDE looks for a JDK in this order: the folder chosen in Settings, `jdk\` next to the exe,
`JAVA_HOME`, then `PATH`. It prefers Java 25 or newer.

## Building

Requires Rust (stable) and, for packaging, a JDK 25 on the build machine.

```powershell
cargo test                # unit tests (completion, editor behaviour, javac parsing)
.\package.ps1             # -> dist\JavaIDE  (release exe + JDK + sample workspace)
.\package.ps1 -Zip        # also creates dist\JavaIDE.zip
.\package.ps1 -Jdk D:\jdk-25
```

Note: when you bundle a JDK, its license applies when you share the folder. Oracle JDK 25 is
distributed under the Oracle No-Fee Terms and Conditions, and Eclipse Temurin is GPLv2 with
Classpath Exception. Both allow redistribution.

## Limitations

- The terminal uses pipes, not a full PTY. Ordinary commands work, but full-screen programs
  (vim, less) and interactive prompts that need a real TTY don't. Use **Restart** to kill a
  stuck command.
- Completion is heuristic (no full Java parser/LSP): it's fast and fully offline, but it only knows
  the JDK classes in its built-in tables and your own project; types of lambda parameters and
  generic methods are not inferred (it then falls back to offering every plausible member).
