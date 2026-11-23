//! JDK discovery, compiling and running Java code.

use crate::proc::no_window;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

pub const REQUIRED_MAJOR: u32 = 25;

#[derive(Clone)]
pub struct Jdk {
    /// `None` means "use whatever `java`/`javac` is on PATH".
    pub home: Option<PathBuf>,
    pub origin: String,
    pub version: String,
    pub major: u32,
    pub has_javac: bool,
}

pub fn exe_name(n: &str) -> String {
    if cfg!(windows) { format!("{n}.exe") } else { n.to_string() }
}

impl Jdk {
    /// Search order: settings override, bundled `jdk/` next to the exe,
    /// JAVA_HOME, then PATH. A JDK older than 25 is skipped when a newer one exists.
    pub fn detect(custom: Option<&Path>) -> Jdk {
        let exe = crate::settings::exe_dir();
        let mut cands: Vec<(Option<PathBuf>, &str)> = Vec::new();
        if let Some(c) = custom {
            cands.push((Some(c.to_path_buf()), "custom"));
        }
        cands.push((Some(exe.join("jdk")), "bundled"));
        cands.push((Some(exe.join("runtime")), "bundled"));
        if let Some(p) = exe.parent() {
            cands.push((Some(p.join("jdk")), "bundled"));
        }
        if let Ok(h) = std::env::var("JAVA_HOME") {
            cands.push((Some(PathBuf::from(h)), "JAVA_HOME"));
        }
        cands.push((None, "PATH"));

        let mut fallback: Option<Jdk> = None;
        for (home, origin) in cands {
            if let Some(h) = &home {
                if !h.join("bin").join(exe_name("java")).is_file() {
                    continue;
                }
            }
            let mut jdk = Jdk { home, origin: origin.to_string(), version: String::new(), major: 0, has_javac: false };
            jdk.probe();
            if jdk.major == 0 {
                continue;
            }
            // A user-chosen JDK always wins.
            if origin == "custom" || (jdk.major >= REQUIRED_MAJOR && jdk.has_javac) {
                return jdk;
            }
            if fallback.as_ref().is_none_or(|f| jdk.major > f.major) {
                fallback = Some(jdk);
            }
        }
        fallback.unwrap_or(Jdk {
            home: None,
            origin: "none".into(),
            version: "No JDK found".into(),
            major: 0,
            has_javac: false,
        })
    }

    pub fn tool(&self, name: &str) -> PathBuf {
        match &self.home {
            Some(h) => h.join("bin").join(exe_name(name)),
            None => PathBuf::from(name),
        }
    }

    pub fn bin_dir(&self) -> Option<PathBuf> {
        self.home.as_ref().map(|h| h.join("bin"))
    }

    fn probe(&mut self) {
        let mut c = Command::new(self.tool("java"));
        c.arg("-version");
        no_window(&mut c);
        if let Ok(o) = c.output() {
            let s = String::from_utf8_lossy(&o.stderr).to_string();
            self.version = s.lines().next().unwrap_or("").trim().to_string();
            self.major = parse_major(&s);
        }
        self.has_javac = match &self.home {
            Some(h) => h.join("bin").join(exe_name("javac")).is_file(),
            None => {
                let mut c = Command::new("javac");
                c.arg("-version");
                no_window(&mut c);
                c.output().is_ok()
            }
        };
    }

    /// Environment so `java`/`javac` in the terminal resolve to this JDK.
    pub fn apply_env(&self, cmd: &mut Command) {
        if let (Some(home), Some(bin)) = (&self.home, self.bin_dir()) {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut parts = vec![bin];
            parts.extend(std::env::split_paths(&path));
            if let Ok(joined) = std::env::join_paths(parts) {
                cmd.env("PATH", joined);
            }
            cmd.env("JAVA_HOME", home);
        }
    }
}

fn parse_major(s: &str) -> u32 {
    // e.g. `java version "25.0.2" 2026-01-20 LTS` or `openjdk version "1.8.0_402"`
    let Some(start) = s.find('"') else { return 0 };
    let rest = &s[start + 1..];
    let ver = &rest[..rest.find('"').unwrap_or(rest.len())];
    let mut nums = ver.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    match nums.next().and_then(|n| n.parse::<u32>().ok()) {
        Some(1) => nums.next().and_then(|n| n.parse().ok()).unwrap_or(0),
        Some(n) => n,
        None => 0,
    }
}

pub const IGNORED_DIRS: &[&str] = &["out", "target", "build", "bin", "node_modules"];

pub fn is_ignored_dir(name: &str) -> bool {
    name.starts_with('.') || IGNORED_DIRS.contains(&name)
}

/// `src/main/java` (Maven/Gradle layout), else `src`, else the project root.
pub fn source_root(project: &Path) -> PathBuf {
    for c in ["src/main/java", "src"] {
        let p = project.join(c);
        if p.is_dir() {
            return p;
        }
    }
    project.to_path_buf()
}

pub fn out_dir(project: &Path) -> PathBuf {
    project.join("out")
}

pub fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !is_ignored_dir(&name) {
                collect_sources(&p, out);
            }
        } else if name.ends_with(".java") {
            out.push(p);
        }
    }
}

static PACKAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*package\s+([\w.]+)\s*;").unwrap());
static MAIN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bvoid\s+main\s*\(").unwrap());
static PROBLEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(.+?\.java):(\d+): (error|warning): (.*)$").unwrap());

pub fn has_main(src: &str) -> bool {
    MAIN_RE.is_match(src)
}

/// Fully-qualified class name for a source file (works for Java 25 compact
/// source files too, whose implicit class is named after the file).
pub fn main_class(file: &Path, src: &str) -> String {
    let stem = file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    match PACKAGE_RE.captures(src) {
        Some(c) => format!("{}.{}", &c[1], stem),
        None => stem,
    }
}

/// Package name implied by a directory's location under the source root.
pub fn package_for_dir(src_root: &Path, dir: &Path) -> Option<String> {
    let rel = dir.strip_prefix(src_root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    if parts.is_empty() { None } else { Some(parts.join(".")) }
}

fn remove_class_files(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            remove_class_files(&p);
        } else if p.extension().is_some_and(|x| x == "class") {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn classpath(project: &Path) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut cp = out_dir(project).to_string_lossy().to_string();
    let lib = project.join("lib");
    if lib.is_dir() {
        cp.push_str(sep);
        cp.push_str(&lib.join("*").to_string_lossy());
    }
    cp
}

const JAVA_FLAGS: &[&str] = &["-Dstdout.encoding=UTF-8", "-Dstderr.encoding=UTF-8", "-Dstdin.encoding=UTF-8"];

/// `javac` over every source in the project, into `out/`.
pub fn build_command(jdk: &Jdk, project: &Path) -> Result<(Command, usize), String> {
    let src = source_root(project);
    let mut files = Vec::new();
    collect_sources(&src, &mut files);
    if files.is_empty() {
        return Err(format!("No .java files found in {}", src.display()));
    }
    let out = out_dir(project);
    std::fs::create_dir_all(&out).map_err(|e| format!("Cannot create {}: {e}", out.display()))?;
    remove_class_files(&out);
    let argfile = out.join(".javaide-sources.txt");
    let list: String = files
        .iter()
        .map(|f| format!("\"{}\"\n", f.to_string_lossy().replace('\\', "/")))
        .collect();
    std::fs::write(&argfile, list).map_err(|e| e.to_string())?;

    let mut c = Command::new(jdk.tool("javac"));
    c.current_dir(project)
        .arg("-d")
        .arg(&out)
        .args(["-encoding", "UTF-8", "-g", "-Xlint:all,-serial,-processing"])
        .arg("-cp")
        .arg(classpath(project))
        .arg(format!("@{}", argfile.to_string_lossy()));
    Ok((c, files.len()))
}

pub fn run_command(jdk: &Jdk, project: &Path, main: &str, args: &str) -> Command {
    let mut c = Command::new(jdk.tool("java"));
    c.current_dir(project)
        .args(JAVA_FLAGS)
        .arg("-cp")
        .arg(classpath(project))
        .arg(main)
        .args(split_args(args));
    c
}

/// Single-file (and Java 22+ multi-file) source launcher: `java File.java`.
pub fn source_launch_command(jdk: &Jdk, file: &Path, args: &str) -> Command {
    let mut c = Command::new(jdk.tool("java"));
    if let Some(dir) = file.parent() {
        c.current_dir(dir);
    }
    c.args(JAVA_FLAGS).arg(file).args(split_args(args));
    c
}

pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[derive(Clone)]
pub struct Problem {
    pub file: PathBuf,
    pub line: usize,
    pub is_error: bool,
    pub message: String,
}

pub fn parse_problems(output: &str, base: &Path) -> Vec<Problem> {
    PROBLEM_RE
        .captures_iter(output)
        .map(|c| {
            let f = PathBuf::from(&c[1]);
            Problem {
                file: if f.is_absolute() { f } else { base.join(f) },
                line: c[2].parse().unwrap_or(1),
                is_error: &c[3] == "error",
                message: c[4].trim().to_string(),
            }
        })
        .collect()
}

/// Skeleton for a newly created `.java` file.
pub fn template(class_name: &str, package: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(p) = package {
        s.push_str(&format!("package {p};\n\n"));
    }
    s.push_str(&format!(
        "public class {class_name} {{\n\n    public static void main(String[] args) {{\n        System.out.println(\"Hello from {class_name}!\");\n    }}\n}}\n"
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_major("java version \"25.0.2\" 2026-01-20 LTS"), 25);
        assert_eq!(parse_major("openjdk version \"1.8.0_402\""), 8);
        assert_eq!(parse_major("openjdk version \"26-ea\""), 26);
    }

    #[test]
    fn problems_with_windows_paths() {
        let out = "F:\\proj\\src\\Main.java:3: error: ';' expected\n        int x = 1\n1 error\n";
        let p = parse_problems(out, Path::new("F:\\proj"));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].line, 3);
        assert!(p[0].is_error);
        assert!(p[0].file.ends_with("Main.java"));
    }

    #[test]
    fn main_class_and_args() {
        assert_eq!(main_class(Path::new("a/b/App.java"), "package com.x;\nclass App {}"), "com.x.App");
        assert_eq!(main_class(Path::new("Hello.java"), "void main() {}"), "Hello");
        assert_eq!(split_args(r#"one "two three" 'x y' z"#), vec!["one", "two three", "x y", "z"]);
    }
}
