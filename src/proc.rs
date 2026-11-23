//! Child processes with piped stdio, read on background threads.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

pub struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<String>,
    readers_alive: Arc<AtomicUsize>,
    exit: Option<Option<i32>>,
}

impl Proc {
    pub fn spawn(mut cmd: Command, ctx: &eframe::egui::Context) -> std::io::Result<Proc> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        no_window(&mut cmd);
        let mut child = cmd.spawn()?;
        let (tx, rx) = channel();
        let alive = Arc::new(AtomicUsize::new(0));
        if let Some(out) = child.stdout.take() {
            pump(out, tx.clone(), ctx.clone(), alive.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, tx, ctx.clone(), alive.clone());
        }
        let stdin = child.stdin.take();
        Ok(Proc { child, stdin, rx, readers_alive: alive, exit: None })
    }

    /// All output received since the last call.
    pub fn drain(&mut self) -> String {
        let mut s = String::new();
        while let Ok(chunk) = self.rx.try_recv() {
            s.push_str(&chunk);
        }
        s
    }

    /// `Some(code)` once the process has exited *and* all its output was read.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        if self.exit.is_none() {
            if let Ok(Some(status)) = self.child.try_wait() {
                if self.readers_alive.load(Ordering::SeqCst) == 0 {
                    self.exit = Some(status.code());
                }
            }
        }
        self.exit
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn send(&mut self, text: &str) {
        if let Some(s) = &mut self.stdin {
            let _ = s.write_all(text.as_bytes());
            let _ = s.flush();
        }
    }

    /// Kill the process and everything it started.
    pub fn kill(&mut self) {
        if !self.is_running() {
            return;
        }
        #[cfg(windows)]
        {
            let mut c = Command::new("taskkill");
            c.args(["/PID", &self.child.id().to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            no_window(&mut c);
            let _ = c.status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

fn pump<R: Read + Send + 'static>(
    mut r: R,
    tx: Sender<String>,
    ctx: eframe::egui::Context,
    alive: Arc<AtomicUsize>,
) {
    alive.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    let text = take_utf8(&mut pending);
                    if !text.is_empty() {
                        if tx.send(text).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
            }
        }
        if !pending.is_empty() {
            let _ = tx.send(String::from_utf8_lossy(&pending).into_owned());
        }
        alive.fetch_sub(1, Ordering::SeqCst);
        ctx.request_repaint();
    });
}

/// Decode as much UTF-8 as possible, keeping an incomplete trailing sequence.
fn take_utf8(p: &mut Vec<u8>) -> String {
    match std::str::from_utf8(p) {
        Ok(s) => {
            let s = s.to_owned();
            p.clear();
            s
        }
        Err(e) if e.error_len().is_none() => {
            let valid = e.valid_up_to();
            let s = String::from_utf8_lossy(&p[..valid]).into_owned();
            p.drain(..valid);
            s
        }
        Err(_) => {
            let s = String::from_utf8_lossy(p).into_owned();
            p.clear();
            s
        }
    }
}

/// Don't flash a console window for child processes on Windows.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Strip ANSI escape sequences and carriage returns from terminal output.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.peek() {
                Some('[') => {
                    it.next();
                    while let Some(d) = it.next() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(d) = it.next() {
                        if d == '\x07' {
                            break;
                        }
                        if d == '\x1b' {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            },
            '\r' => {}
            '\x08' => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Append to a scrollback buffer, dropping old lines past `cap` bytes.
pub fn append_capped(buf: &mut String, s: &str, cap: usize) {
    buf.push_str(s);
    if buf.len() > cap {
        let mut idx = buf.len() - cap * 3 / 4;
        while !buf.is_char_boundary(idx) {
            idx += 1;
        }
        if let Some(nl) = buf[idx..].find('\n') {
            idx += nl + 1;
        }
        buf.drain(..idx);
    }
}
