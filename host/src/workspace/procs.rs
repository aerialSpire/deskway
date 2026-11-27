//! The programs AI clients started on this computer (docs/architecture.md, "Workspace and process
//! lifetime"). They are the computer's, not a session's: an AI client's session ending, its
//! seat given up or stopped, leaves them running. Each one's output (standard output and error
//! together, or a terminal's screen output) is kept in a buffer of the last `KEEP` bytes, read
//! by offset, so an AI client that comes back reads on where it stopped.
//!
//! Two kinds, one table: `run` starts a program with pipes and no input; `proc_start` starts
//! one in a terminal of its own (`platform::start_terminal`), which takes input (`write`) and
//! whose output is read without the terminal's control sequences.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::ClientIdentity;
use crate::workspace::{Answer, Code, WorkError};

/// Output kept per program.
const KEEP: usize = 1 << 20;
/// Output returned in one answer, at most.
const ANSWER: usize = 256 << 10;
/// Programs kept in the table: the oldest that has ended goes first.
const MOST: usize = 64;
/// How long `proc_read` may wait for output, at most.
const READ_WAIT_MOST: u64 = 30_000;
/// The administrator's programs (the Windows admin worker's) are numbered from here, so an id
/// says whose table it is in.
pub const ADMIN_FIRST: u64 = 1 << 32;

/// A program the table keeps: how it ends, is ended, and takes input.
pub trait Program: Send {
    fn pid(&self) -> u32;
    /// Its exit code once it has ended.
    fn exit(&mut self) -> Option<i32>;
    /// Ends it and every program it started.
    fn kill(&mut self) -> std::io::Result<()>;
    /// Its input: a terminal's; None for a program started without one.
    fn input(&mut self) -> Option<&mut dyn Write>;
}

/// A program just started, and where its output comes from.
pub struct Started {
    pub program: Box<dyn Program>,
    pub output: Vec<Box<dyn Read + Send>>,
}

/// A program started with pipes (`run`): no input.
struct Piped(Child);

impl Program for Piped {
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn exit(&mut self) -> Option<i32> {
        self.0.try_wait().ok().flatten().map(|s| s.code().unwrap_or(-1))
    }
    fn kill(&mut self) -> std::io::Result<()> {
        crate::platform::kill_tree(self.0.id());
        let _ = self.0.kill();
        Ok(())
    }
    fn input(&mut self) -> Option<&mut dyn Write> {
        None
    }
}

/// The output of one program: the last `KEEP` bytes of all it printed.
#[derive(Default)]
pub struct Output {
    buf: VecDeque<u8>,
    /// Bytes printed in all, so offsets stay valid as old bytes go.
    total: u64,
}

impl Output {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend(data);
        self.total += data.len() as u64;
        let over = self.buf.len().saturating_sub(KEEP);
        self.buf.drain(..over);
    }

    /// From `from` on, at most `ANSWER` bytes: (the bytes, where the next read starts, whether
    /// bytes before what is returned were lost).
    fn from(&self, from: u64) -> (Vec<u8>, u64, bool) {
        let start = self.total - self.buf.len() as u64;
        let lost = from < start;
        let at = (from.clamp(start, self.total) - start) as usize;
        let end = (at + ANSWER).min(self.buf.len());
        (self.buf.range(at..end).copied().collect(), start + end as u64, lost)
    }
}

/// A terminal's output as text: without its control sequences (colours, cursor moves, titles).
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte from @ to ~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            // Two-byte sequences (ESC 7, ESC =, ESC ( B...): a character set's letter follows.
            Some('(' | ')') => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// A key `proc_write` takes by name, as a terminal sends it.
fn key(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" => "\r",
        "tab" => "\t",
        "backspace" => "\u{7f}",
        "escape" | "esc" => "\u{1b}",
        "up" => "\u{1b}[A",
        "down" => "\u{1b}[B",
        "right" => "\u{1b}[C",
        "left" => "\u{1b}[D",
        "home" => "\u{1b}[H",
        "end" => "\u{1b}[F",
        "delete" => "\u{1b}[3~",
        "ctrl+c" => "\u{3}",
        "ctrl+d" => "\u{4}",
        "ctrl+z" => "\u{1a}",
        "ctrl+l" => "\u{c}",
        _ => return None,
    })
}

struct Proc {
    command: String,
    starter: ClientIdentity,
    started: u64,
    started_at: Instant,
    terminal: bool,
    program: Mutex<Box<dyn Program>>,
    output: Arc<Mutex<Output>>,
}

impl Proc {
    fn exit(&self) -> Option<i32> {
        self.program.lock().unwrap().exit()
    }
}

/// One program in the workspace's list.
#[derive(Deserialize, Serialize)]
pub struct Record {
    #[serde(flatten)]
    pub info: Info,
    pub starter: ClientIdentity,
}

/// The program's state and output size.
#[derive(Deserialize, Serialize)]
pub struct Info {
    id: u64,
    command: String,
    pid: u32,
    terminal: bool,
    started: u64,
    running: bool,
    exit_code: Option<i32>,
    output_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    running_for: Option<u64>,
}

#[derive(Default)]
pub struct Table {
    procs: Mutex<BTreeMap<u64, Arc<Proc>>>,
    /// The id the last program got.
    next: Mutex<u64>,
}

impl Table {
    /// A table whose first program is `first`.
    #[cfg(windows)]
    pub fn numbered_from(first: u64) -> Table {
        Table { next: Mutex::new(first - 1), ..Default::default() }
    }

    /// Starts `command` and waits for it, at most `wait_ms`: then it runs on, and its id reads
    /// on from where the answer stopped.
    pub fn run(&self, command: &str, cwd: Option<&str>, wait_ms: u64, elevated: bool, starter: ClientIdentity) -> Answer {
        let mut child = crate::platform::start_program(command, cwd, elevated).map_err(cannot_start)?;
        let output: Vec<Box<dyn Read + Send>> = [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>)].into_iter().flatten().collect();
        let (id, p) = self.keep(command, false, Started { program: Box::new(Piped(child)), output }, starter);
        let until = Instant::now() + Duration::from_millis(wait_ms);
        while p.exit().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(30));
        }
        // What it printed last may still be on its way from the readers.
        if p.exit().is_some() {
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(self.answer(id, &p, 0))
    }

    /// Starts `command` (a shell, when there is none) in a terminal of `cols` × `rows`, and
    /// answers with what it shows after a moment.
    pub fn start(&self, command: Option<&str>, cwd: Option<&str>, cols: u16, rows: u16, elevated: bool, starter: ClientIdentity) -> Answer {
        let started = crate::platform::start_terminal(command, cwd, cols.clamp(20, 500), rows.clamp(5, 200), elevated).map_err(cannot_start)?;
        let (id, p) = self.keep(command.unwrap_or("(shell)"), true, started, starter);
        self.wait_output(&p, 0, 1000);
        Ok(self.answer(id, &p, 0))
    }

    /// Keeps a program started: its output read as it comes, and room made for it.
    fn keep(&self, command: &str, terminal: bool, started: Started, starter: ClientIdentity) -> (u64, Arc<Proc>) {
        let started_at = Instant::now();
        let started_unix = crate::auth::unix_now();
        let output = Arc::new(Mutex::new(Output::default()));
        for mut r in started.output {
            let out = output.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    out.lock().unwrap().push(&buf[..n]);
                }
            });
        }
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let p = Arc::new(Proc { command: command.to_string(), starter, started: started_unix, started_at, terminal, program: Mutex::new(started.program), output });
        let mut all = self.procs.lock().unwrap();
        all.insert(id, p.clone());
        // Room for the next: programs that ended go, oldest first.
        while all.len() > MOST {
            let Some(old) = all.iter().find(|(_, q)| q.exit().is_some()).map(|(k, _)| *k) else { break };
            all.remove(&old);
        }
        (id, p)
    }

    /// Waits, at most `ms`, until the program has printed past `from` or has ended.
    fn wait_output(&self, p: &Proc, from: u64, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until && p.output.lock().unwrap().total <= from && p.exit().is_none() {
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn answer(&self, id: u64, p: &Proc, from: u64) -> Value {
        let exit = p.exit();
        let (bytes, next, lost) = p.output.lock().unwrap().from(from);
        let text = if p.terminal { plain(&bytes) } else { String::from_utf8_lossy(&bytes).into_owned() };
        json!({ "id": id, "running": exit.is_none(), "exit_code": exit, "output": text, "next": next, "earlier_output_lost": lost })
    }

    fn get(&self, id: u64) -> Result<Arc<Proc>, WorkError> {
        self.procs.lock().unwrap().get(&id).cloned().ok_or_else(|| WorkError::new(Code::NotFound, format!("no program {id}"), "proc_list shows those kept."))
    }

    /// What it printed from `from` on, once there is some (or it has ended), waiting at most
    /// `wait_ms`.
    pub fn read(&self, id: u64, from: u64, wait_ms: u64) -> Answer {
        let p = self.get(id)?;
        self.wait_output(&p, from, wait_ms.min(READ_WAIT_MOST));
        Ok(self.answer(id, &p, from))
    }

    /// Types into a terminal's program: `text`, then `keys` by name.
    pub fn write(&self, id: u64, text: Option<&str>, keys: &[String]) -> Answer {
        let p = self.get(id)?;
        let mut bytes = text.unwrap_or_default().to_string();
        for k in keys {
            bytes.push_str(key(k).ok_or_else(|| WorkError::new(Code::InvalidArgs, format!("no key {k}"), "Keys: enter, tab, backspace, escape, up, down, left, right, home, end, delete, ctrl+c, ctrl+d, ctrl+z, ctrl+l."))?);
        }
        if p.exit().is_some() {
            return Err(WorkError::new(Code::Failed, format!("program {id} has ended"), "proc_read its last output, or proc_start another."));
        }
        let mut program = p.program.lock().unwrap();
        let input = program.input().ok_or_else(|| WorkError::new(Code::InvalidArgs, format!("program {id} takes no input"), "Programs take input when started with proc_start."))?;
        input.write_all(bytes.as_bytes()).and_then(|()| input.flush()).map_err(|e| WorkError::new(Code::Io, e.to_string(), "proc_read it: it may have ended."))?;
        Ok(json!({ "written": bytes.len(), "next": p.output.lock().unwrap().total }))
    }

    /// Ends the program and what it started.
    pub fn kill(&self, id: u64) -> Answer {
        let p = self.get(id)?;
        {
            let mut program = p.program.lock().unwrap();
            if program.exit().is_none() {
                program.kill().map_err(|e| WorkError::new(Code::Failed, format!("cannot end program {id}: {e}"), "Call proc_list to check what is still running."))?;
            }
        }
        let until = Instant::now() + Duration::from_secs(5);
        while p.exit().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(30));
        }
        let end = p.output.lock().unwrap().total;
        Ok(self.answer(id, &p, end))
    }

    pub fn list(&self) -> Value {
        let all = self.procs.lock().unwrap();
        let records: Vec<_> = all.iter().map(|(&id, p)| {
            let (pid, exit_code) = {
                let mut program = p.program.lock().unwrap();
                (program.pid(), program.exit())
            };
            let running = exit_code.is_none();
            let info = Info {
                id, command: p.command.clone(), pid, terminal: p.terminal, started: p.started,
                running, exit_code, output_bytes: p.output.lock().unwrap().total,
                running_for: running.then(|| p.started_at.elapsed().as_secs()),
            };
            Record { info, starter: p.starter.clone() }
        }).collect();
        json!(records)
    }
}

fn cannot_start(e: std::io::Error) -> WorkError {
    if e.kind() == std::io::ErrorKind::Unsupported {
        return WorkError::new(Code::ElevationUnavailable, format!("cannot start it as an administrator: {e}"), "Run it without elevated, or ask the person at the computer.");
    }
    WorkError::new(Code::Failed, format!("cannot start it: {e}"), "Check the command and the folder.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_read_by_offset_and_the_oldest_goes_past_the_limit() {
        let mut o = Output::default();
        o.push(b"hello ");
        o.push(b"world");
        for (from, expected) in [(0, &b"hello world"[..]), (6, b"world"), (11, b""), (u64::MAX, b"")] {
            assert_eq!(o.from(from), (expected.to_vec(), 11, false), "from={from}");
        }
        o.push(&vec![b'x'; KEEP]);
        let (bytes, next, lost) = o.from(0);
        assert!(lost, "what went past the limit is said to be lost");
        assert_eq!((bytes.len(), next), (ANSWER, 11 + ANSWER as u64), "one answer at most, from the oldest kept");
    }

    #[test]
    fn a_terminals_output_is_read_without_its_control_sequences() {
        for (raw, want) in [
            ("plain", "plain"),
            ("\u{1b}[32mgreen\u{1b}[0m", "green"),
            ("\u{1b}[?25l\u{1b}[2J\u{1b}[HPS C:\\> ", "PS C:\\> "),
            ("\u{1b}]0;title\u{7}after", "after"),
            ("\u{1b}]0;title\u{1b}\\after", "after"),
            ("\u{1b}(Bok", "ok"),
            ("a\r\nb", "a\r\nb"),
        ] {
            assert_eq!(plain(raw.as_bytes()), want, "{raw:?}");
        }
    }

    #[test]
    fn keys_are_named_as_a_terminal_sends_them() {
        for (name, want) in [("Enter", Some("\r")), ("ctrl+c", Some("\u{3}")), ("up", Some("\u{1b}[A")), ("f13", None)] {
            assert_eq!(key(name), want, "{name}");
        }
    }

    /// How long a test waits for a shell. The first PowerShell start on a fresh CI machine can
    /// take well over 20 seconds; on a working computer it takes about one.
    const SHELL_START: Duration = Duration::from_secs(90);

    #[cfg(windows)]
    const RUN: [(&str, i64, &[&str]); 2] = [
        ("Write-Output 'hi 中文'; cmd /c echo 中文2", 0, &["hi 中文", "中文2"]),
        ("Write-Output 'before exit'; exit 7", 7, &["before exit"]),
    ];
    #[cfg(unix)]
    const RUN: [(&str, i64, &[&str]); 2] = [
        ("echo 'hi 中文'; echo 中文2", 0, &["hi 中文", "中文2"]),
        ("echo 'before exit'; exit 7", 7, &["before exit"]),
    ];
    #[cfg(windows)]
    const SLOW: &str = "Start-Sleep 30";
    #[cfg(unix)]
    const SLOW: &str = "sleep 30";
    #[cfg(windows)]
    const PTY_LINE: &str = "Write-Output ('deskway' + '-pty')";
    #[cfg(unix)]
    const PTY_LINE: &str = "printf '%s-%s\\n' deskway pty";

    #[test]
    fn a_program_runs_and_one_that_takes_longer_runs_on() {
        let t = Table::default();
        let own = ClientIdentity { device: "c1".into(), name: "First".into() };
        let other = ClientIdentity { device: "c2".into(), name: "Second".into() };
        for (command, code, expected) in RUN {
            let done = t.run(command, None, SHELL_START.as_millis() as u64, false, own.clone()).unwrap();
            assert_eq!((done["running"].as_bool(), done["exit_code"].as_i64()), (Some(false), Some(code)), "{done}");
            let output = done["output"].as_str().unwrap();
            assert!(expected.iter().all(|text| output.contains(text)), "{done}");
        }
        let slow = t.run(SLOW, None, 200, false, other.clone()).unwrap();
        assert_eq!(slow["running"], true, "a wait that ends leaves it running");
        let id = slow["id"].as_u64().unwrap();
        let list = t.list();
        let records = list.as_array().unwrap();
        assert_eq!(records.len(), RUN.len() + 1);
        for (i, record) in records.iter().enumerate() {
            let running = i == RUN.len();
            let starter = if running { &other } else { &own };
            assert_eq!(record["starter"], json!(starter));
            assert_eq!(record["running"], running);
            assert_eq!(record.get("running_for").is_some_and(Value::is_u64), running);
        }
        assert_eq!(t.write(id, Some("x"), &[]).unwrap_err().code, Code::InvalidArgs, "a piped program takes no input");
        let killed = t.kill(id).unwrap();
        assert_eq!(killed["running"], false, "{killed}");
        assert_eq!(t.read(99, 0, 0).unwrap_err().code, Code::NotFound);
    }

    /// A shell in a terminal: it shows a prompt, takes a line, answers it, and leaves on exit.
    #[test]
    fn a_shell_in_a_terminal_takes_input_and_answers() {
        let t = Table::default();
        let starter = ClientIdentity { device: "c1".into(), name: "First".into() };
        let started = t.start(None, None, 100, 30, false, starter.clone()).unwrap();
        let id = started["id"].as_u64().unwrap();
        assert_eq!(started["running"], true, "{started}");
        let from = started["next"].as_u64().unwrap();
        t.write(id, Some(PTY_LINE), &["enter".into()]).unwrap();
        let end = Instant::now() + SHELL_START;
        let mut seen = String::new();
        let mut at = from;
        while !seen.contains("deskway-pty") && Instant::now() < end {
            let r = t.read(id, at, 2000).unwrap();
            seen.push_str(r["output"].as_str().unwrap());
            at = r["next"].as_u64().unwrap();
        }
        assert!(seen.contains("deskway-pty"), "the shell answered: {seen:?}");
        assert!(!seen.contains('\u{1b}'), "without control sequences: {seen:?}");
        assert_eq!(t.write(id, Some("x"), &["f13".into()]).unwrap_err().code, Code::InvalidArgs);
        t.write(id, Some("exit"), &["enter".into()]).unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while t.read(id, 0, 0).unwrap()["running"] == true && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(t.read(id, 0, 0).unwrap()["running"], false, "exit ends it");
        let list = t.list();
        assert_eq!(list[0]["terminal"], true);
        assert_eq!(list[0]["starter"], json!(starter));
    }
}
