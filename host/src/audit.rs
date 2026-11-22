//! What AI clients did on this computer, kept here (docs/architecture.md, "Audit"): one JSON
//! line per tool call when it starts and when it ends, appended only, beside the trusted devices
//! (on Windows in the service's folder, which the signed-in person cannot write). The file is
//! renamed to audit.old.jsonl once it passes 4 MB, so it stays bounded (about 8 MB, both).
//! It is only ever read back, as calls (`calls`: the `audit_read` tool, and the account's
//! browsers through the server), never changed.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const KEEP: u64 = 4 << 20;

/// One line of the log.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Record<'a> {
    /// Unix seconds.
    pub at: u64,
    /// The AI client: its device name, and its credential id.
    pub client: &'a str,
    pub device: &'a str,
    pub session: &'a str,
    pub tool: &'a str,
    /// "start", "ok" or the error's code.
    pub what: &'a str,
    /// The arguments in short (start), or the result in short (end).
    pub detail: String,
    /// Milliseconds the call took (end).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

pub struct Audit {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl Audit {
    /// The log in `dir`; with none, records go nowhere (no config folder: no AI either).
    pub fn new(dir: Option<PathBuf>) -> Audit {
        Audit { path: dir.map(|d| d.join("audit.jsonl")), lock: Mutex::new(()) }
    }

    pub fn write(&self, r: &Record) {
        let Some(path) = &self.path else { return };
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if std::fs::metadata(path).is_ok_and(|m| m.len() > KEEP) {
            let _ = std::fs::rename(path, path.with_file_name("audit.old.jsonl"));
        }
        let line = serde_json::to_string(r).expect("a record serializes") + "\n";
        let written = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = written {
            eprintln!("audit: {e}");
        }
    }
}

/// What a reading asks for: calls started from `since` to `until` (unix seconds, either
/// open), of one tool, of one AI client, only those that failed; `limit` of them after the
/// first `offset`, newest first.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Query {
    pub since: u64,
    pub until: Option<u64>,
    pub tool: Option<String>,
    pub client: Option<String>,
    pub failed: bool,
    pub offset: usize,
    pub limit: usize,
}

/// One tool call: its start's arguments, and how it ended (or `running`, with no end yet).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Call {
    pub at: u64,
    pub client: String,
    pub device: String,
    pub session: String,
    pub tool: String,
    pub args: String,
    /// "ok", "running", or the error's code.
    pub result: String,
    /// The result, or the error, in short.
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

/// One page of calls, and what it is out of: how many calls fit the query, and every AI
/// client and tool in the whole record, to choose from.
#[derive(Serialize, Debug, PartialEq)]
pub struct Page {
    pub calls: Vec<Call>,
    pub total: usize,
    pub clients: Vec<String>,
    pub tools: Vec<String>,
}

/// A line of the record, as read back.
#[derive(Deserialize)]
struct Line {
    at: u64,
    client: String,
    device: String,
    session: String,
    tool: String,
    what: String,
    detail: String,
    ms: Option<u64>,
}

impl Audit {
    /// The calls `q` asks for, from both files: each start joined to its end.
    pub fn calls(&self, q: &Query) -> Page {
        let text = match &self.path {
            Some(path) => {
                let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                std::fs::read_to_string(path.with_file_name("audit.old.jsonl")).unwrap_or_default() + &std::fs::read_to_string(path).unwrap_or_default()
            }
            None => String::new(),
        };
        let mut open: HashMap<(String, String), VecDeque<Call>> = HashMap::new();
        let mut all = Vec::new();
        for r in text.lines().filter_map(|l| serde_json::from_str::<Line>(l).ok()) {
            let key = (r.session.clone(), r.tool.clone());
            if r.what == "start" {
                open.entry(key).or_default().push_back(Call { at: r.at, client: r.client, device: r.device, session: r.session, tool: r.tool, args: r.detail, result: "running".into(), detail: String::new(), ms: None });
                continue;
            }
            match open.get_mut(&key).and_then(VecDeque::pop_front) {
                Some(c) => all.push(Call { result: r.what, detail: r.detail, ms: r.ms, ..c }),
                None => all.push(Call { at: r.at, client: r.client, device: r.device, session: r.session, tool: r.tool, args: String::new(), result: r.what, detail: r.detail, ms: r.ms }),
            }
        }
        all.extend(open.into_values().flatten());
        // Newest first; of calls in the same second, the later one first.
        all.reverse();
        all.sort_by(|a, b| b.at.cmp(&a.at));
        let clients: BTreeSet<String> = all.iter().map(|c| c.client.clone()).collect();
        let tools: BTreeSet<String> = all.iter().map(|c| c.tool.clone()).collect();
        let fits = |c: &&Call| {
            c.at >= q.since
                && q.until.is_none_or(|u| c.at <= u)
                && q.tool.as_deref().is_none_or(|t| c.tool == t)
                && q.client.as_deref().is_none_or(|x| c.client.eq_ignore_ascii_case(x))
                && (!q.failed || (c.result != "ok" && c.result != "running"))
        };
        let total = all.iter().filter(fits).count();
        let calls = all.iter().filter(fits).skip(q.offset).take(q.limit).cloned().collect();
        Page { calls, total, clients: clients.into_iter().collect(), tools: tools.into_iter().collect() }
    }
}

/// `s` cut to `max` characters, for a record.
pub fn short(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_lines_appended_and_the_file_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("deskway-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Audit::new(Some(dir.clone()));
        let r = Record { at: 1, client: "Claude Code", device: "c1", session: "s1", tool: "run", what: "start", detail: short(&"x".repeat(300), 200), ms: None };
        a.write(&r);
        a.write(&Record { what: "ok", ms: Some(5), ..r.clone() });
        let text = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!((lines.len(), lines[0]["what"].as_str(), lines[1]["ms"].as_u64()), (2, Some("start"), Some(5)));
        assert_eq!(lines[0]["detail"].as_str().unwrap().chars().count(), 201);
        std::fs::write(dir.join("audit.jsonl"), vec![b'x'; KEEP as usize + 1]).unwrap();
        a.write(&r);
        assert!(dir.join("audit.old.jsonl").exists());
        assert!(std::fs::metadata(dir.join("audit.jsonl")).unwrap().len() < 1000);
        Audit::new(None).write(&r);
    }

    /// Calls from both files, each start joined to its end, newest first, as each query
    /// narrows them, a page at a time.
    #[test]
    fn calls_are_read_back_joined_newest_first_and_a_page_at_a_time() {
        let dir = std::env::temp_dir().join(format!("deskway-audit-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let line = |at: u64, client: &str, session: &str, tool: &str, what: &str, detail: &str| format!("{}\n", serde_json::json!({ "at": at, "client": client, "device": "d", "session": session, "tool": tool, "what": what, "detail": detail, "ms": if what == "start" { None } else { Some(5) } }));
        std::fs::write(dir.join("audit.old.jsonl"), line(1, "Claude", "s1", "run", "start", "npm test") + &line(2, "Claude", "s1", "run", "ok", "exit 0")).unwrap();
        std::fs::write(dir.join("audit.jsonl"), line(3, "Cursor", "s2", "click", "start", "(1, 2)") + &line(4, "Cursor", "s2", "click", "view_only", "no") + &line(5, "Claude", "s3", "run", "start", "sleep") + "not json\n").unwrap();
        let a = Audit::new(Some(dir.clone()));
        let page = |q: Query| a.calls(&q);
        let at = |q: Query| page(q).calls.iter().map(|c| (c.at, c.result.clone())).collect::<Vec<_>>();
        let all = || Query { limit: 10, ..Query::default() };
        let whole = page(all());
        assert_eq!((whole.total, whole.clients.as_slice(), whole.tools.as_slice()), (3, ["Claude".to_string(), "Cursor".into()].as_slice(), ["click".to_string(), "run".into()].as_slice()));
        let first = &whole.calls[2];
        assert_eq!((first.args.as_str(), first.result.as_str(), first.detail.as_str(), first.ms), ("npm test", "ok", "exit 0", Some(5)), "a start joined to its end, across the files");
        // (the query) -> (start, result) of the calls it gives
        for (q, want) in [
            (all(), vec![(5, "running"), (3, "view_only"), (1, "ok")]),
            (Query { limit: 1, offset: 1, ..all() }, vec![(3, "view_only")]),
            (Query { since: 2, until: Some(4), ..all() }, vec![(3, "view_only")]),
            (Query { tool: Some("run".into()), ..all() }, vec![(5, "running"), (1, "ok")]),
            (Query { client: Some("cursor".into()), ..all() }, vec![(3, "view_only")]),
            (Query { failed: true, ..all() }, vec![(3, "view_only")]),
        ] {
            let got = at(q);
            assert_eq!(got.iter().map(|(t, r)| (*t, r.as_str())).collect::<Vec<_>>(), want);
        }
        assert_eq!(page(Query { limit: 1, ..all() }).total, 3, "the total is of all that fit, not of the page");
        assert_eq!(Audit::new(None).calls(&all()), Page { calls: Vec::new(), total: 0, clients: Vec::new(), tools: Vec::new() });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
