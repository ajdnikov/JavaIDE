# Java IDE (Rust)

A small, portable IDE for Java 25, written in Rust with [egui](https://github.com/emilk/egui).
It's a single `JavaIDE.exe` with a JDK bundled next to it: no installer, no internet connection,
and nothing written outside its own folder.

## Features

- **Dark mode** (default) and light mode: toolbar button, *View → Dark mode*, or Settings
- **Explorer sidebar**: browse folders, open files, and right-click to create, rename, delete (to the recycle bin) or reveal files, or to open a terminal in a folder
- **Editor**: Java syntax highlighting, line numbers, tabs, bracket/brace/quote auto-closing (`{`, `(`, `[`, quotes), `Ctrl+/` comment toggle, Tab/Shift+Tab block indent, and CRLF files saved back as CRLF
  - Line commands: duplicate, delete and move lines; typing a bracket or quote over a selection wraps it; Home goes to the first non-blank; Backspace removes empty pairs and a whole indent level
  - Open tabs come back on the next start, closed tabs can be reopened, and files changed by another program are reloaded (you are asked first if they have unsaved edits)
  - Files in Windows-1250 are shown correctly, and saving converts them to UTF-8
- **Automatic imports**: a class with exactly one candidate is imported as soon as you move on from typing it, also in pasted code. If there are several (`java.util.Date`, `java.sql.Date`), the status bar says so and `Ctrl+.` lets you choose. `Ctrl+Shift+O` organizes the imports and removes unused ones
- **Automatic spacing**: Enter indents by the structure of the code (blocks, brace-less `if`/`for`, `case`, wrapped statements, method chains, `/** */` comments), and `}` lines up with its block. When a line is finished, the Java formatter fixes its spacing (`int x=5+3;` becomes `int x = 5 + 3;`). `Shift+Alt+F` formats the whole file
- **Find and replace**: `Ctrl+F` / `Ctrl+H`, with match case, whole word and regular expressions
- **Navigation**: go to file (`Ctrl+P`), line (`Ctrl+G`), definition (`F12` or Ctrl+click) and next problem (`F8`), and rename a symbol across the project (`F2`)
- **Block highlight**: with the cursor next to a `{` or `}`, the whole `{...}` block and its pair of braces are highlighted. Braces in strings and comments are ignored
- **Code completion**: powered by the Eclipse JDT Language Server, the same engine as VS Code's Java extension. Suggestions appear while you type or with `Ctrl+Space`, and cover the whole JDK, your project and the jars in `lib/`. Accepting a class **adds its `import` automatically**. While the server starts, or if it is missing, a simpler built-in completion is used
  - Method parameters are filled in as fields: **Tab** and **Shift+Tab** move between them, and repeated names change together
- **Live problems**: errors and warnings appear as you type, without saving or building, as wavy underlines with a marker in the gutter. Hover an underline for the message; the **Problems** tab lists them for the whole project
- **Quick fixes**: press **`Ctrl+.`** on a problem to add a missing import, create a missing class or method, and apply the other fixes and refactorings the Java language server offers
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
| Ctrl+. | Quick fix (add import, ...) |
| Ctrl+Shift+O | Organize imports |
| Shift+Alt+F | Format document |
| F12 / Ctrl+click | Go to definition |
| F2 | Rename symbol |
| F8 / Shift+F8 | Next / previous problem |
| Ctrl+F / Ctrl+H | Find / replace |
| F3 / Shift+F3 | Next / previous match |
| Ctrl+P / Ctrl+G | Go to file / line |
| Ctrl+D | Duplicate line |
| Ctrl+Shift+K | Delete line |
| Alt+Up / Alt+Down | Move line |
| Tab / Shift+Tab | Indent / dedent, or next / previous parameter field |
| Ctrl+/ | Toggle comment |
| Ctrl+S / Ctrl+Shift+S | Save / save all |
| Ctrl+N / Ctrl+O | New file / open folder |
| Ctrl+W / Ctrl+Shift+T | Close / reopen tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
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
  jdtls\                  <- Eclipse JDT Language Server (completion, imports, live problems)
  workspace\HelloWorld\   <- sample project, opened on first launch
  javaide-settings.json   <- created on first run (theme, font size, last folder)
  jdtls-data\             <- created on first run (the language server's project index)
```

The IDE looks for a JDK in this order: the folder chosen in Settings, `jdk\` next to the exe,
`JAVA_HOME`, then `PATH`. It prefers Java 25 or newer.

## Building

Requires Rust (stable) and, for packaging, a JDK 25 and the JDT Language Server on the build
machine. The server is taken from VS Code's Java extension if it is installed.

```powershell
cargo test                # unit tests (completion, editor behaviour, javac parsing)
.\package.ps1             # -> dist\JavaIDE  (release exe + JDK + language server + sample workspace)
.\package.ps1 -Zip        # also creates dist\JavaIDE.zip
.\package.ps1 -Jdk D:\jdk-25 -Jdtls D:\jdtls
```

Note: when you bundle a JDK, its license applies when you share the folder. Oracle JDK 25 is
distributed under the Oracle No-Fee Terms and Conditions, and Eclipse Temurin is GPLv2 with
Classpath Exception. Both allow redistribution. The JDT Language Server is distributed under
the Eclipse Public License 2.0.

## Limitations

- The terminal uses pipes, not a full PTY. Ordinary commands work, but full-screen programs
  (vim, less) and interactive prompts that need a real TTY don't. Use **Restart** to kill a
  stuck command.
- An unsaved scratch file uses the simpler built-in completion until it is saved.
- Refactorings that need a dialog of their own in VS Code (for example generating code with a
  chooser) are not offered in the `Ctrl+.` menu.
