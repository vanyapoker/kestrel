//! Archive contents for the preview panel, listed with `bsdtar -tv` and arranged as a
//! browsable tree.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Listings stop here; enough to browse, small enough to keep in memory.
const MAX_ITEMS: usize = 100_000;
/// Compressed tarballs must be decompressed to list; past this, show what was read.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Node {
    pub name: String,
    pub is_dir: bool,
    /// File size, or for a folder the total of the files inside it.
    pub size: u64,
    pub children: Vec<usize>,
}

/// The archive as a tree; node 0 is the root.
pub struct Tree {
    pub nodes: Vec<Node>,
    pub files: usize,
    pub dirs: usize,
    /// The listing stopped early (too many items, or it took too long).
    pub truncated: bool,
}

impl Tree {
    /// The folder at `path` (names from the root), if it exists.
    pub fn find(&self, path: &[String]) -> Option<usize> {
        let mut at = 0;
        for name in path {
            at = *self.nodes[at]
                .children
                .iter()
                .find(|&&c| self.nodes[c].is_dir && self.nodes[c].name == *name)?;
        }
        Some(at)
    }

    pub fn total_size(&self) -> u64 {
        self.nodes[0].size
    }

    fn build(items: Vec<(String, bool, u64)>, truncated: bool) -> Tree {
        let mut tree = Tree {
            nodes: vec![Node {
                name: String::new(),
                is_dir: true,
                size: 0,
                children: Vec::new(),
            }],
            files: 0,
            dirs: 0,
            truncated,
        };
        let mut index = std::collections::HashMap::<(usize, String), usize>::new();
        for (path, is_dir, size) in items {
            let parts: Vec<&str> = path
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect();
            let Some((last, parents)) = parts.split_last() else {
                continue;
            };
            let mut at = 0;
            let mut chain = vec![0];
            for name in parents {
                at = tree.child(&mut index, at, name, true);
                chain.push(at);
            }
            let node = tree.child(&mut index, at, last, is_dir);
            if !is_dir && tree.nodes[node].size == 0 {
                tree.nodes[node].size = size;
                for &dir in &chain {
                    tree.nodes[dir].size += size;
                }
            }
        }
        tree.files = tree.nodes.iter().skip(1).filter(|n| !n.is_dir).count();
        tree.dirs = tree.nodes.iter().skip(1).filter(|n| n.is_dir).count();
        let order: Vec<Vec<usize>> = tree
            .nodes
            .iter()
            .map(|n| {
                let mut c = n.children.clone();
                c.sort_by(|&a, &b| {
                    let (x, y) = (&tree.nodes[a], &tree.nodes[b]);
                    y.is_dir
                        .cmp(&x.is_dir)
                        .then_with(|| crate::util::natural_cmp(&x.name, &y.name))
                });
                c
            })
            .collect();
        for (node, children) in tree.nodes.iter_mut().zip(order) {
            node.children = children;
        }
        tree
    }

    /// The child `name` of `parent`, created if missing. A folder entry listed after
    /// a file of the same name wins, since paths inside it need a folder.
    fn child(
        &mut self,
        index: &mut std::collections::HashMap<(usize, String), usize>,
        parent: usize,
        name: &str,
        is_dir: bool,
    ) -> usize {
        if let Some(&i) = index.get(&(parent, name.to_string())) {
            self.nodes[i].is_dir |= is_dir;
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_string(),
            is_dir,
            size: 0,
            children: Vec::new(),
        });
        self.nodes[parent].children.push(i);
        index.insert((parent, name.to_string()), i);
        i
    }
}

/// Lists `path` with bsdtar. `cancelled` is polled so an abandoned preview stops early.
pub fn list(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Tree, String> {
    let mut child = Command::new("bsdtar")
        .arg("-tvf")
        .arg(path)
        // A UTF-8 locale prints names as they are, and month names in English.
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("bsdtar: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut items = Vec::new();
        let mut full = false;
        for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
            if let Some(item) = parse_line(&String::from_utf8_lossy(&line)) {
                if items.len() == MAX_ITEMS {
                    full = true;
                    break;
                }
                items.push(item);
            }
        }
        let _ = tx.send((items, full));
    });
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
        s
    });
    let start = Instant::now();
    let mut timed_out = false;
    let (items, mut truncated) = loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("cancelled".into());
                }
                if !timed_out && start.elapsed() > LIST_TIMEOUT {
                    // Killing closes the pipe, so the reader returns what it has.
                    timed_out = true;
                    let _ = child.kill();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("bsdtar failed".into()),
        }
    };
    let _ = child.kill();
    let status = child.wait().ok();
    truncated |= timed_out;
    let errors = errors.join().unwrap_or_default();
    if items.is_empty() && !status.is_some_and(|s| s.success()) {
        let first = errors
            .lines()
            .map(|l| l.trim_start_matches("bsdtar: ").trim())
            .find(|l| !l.is_empty())
            .unwrap_or("Not a readable archive");
        return Err(first.to_string());
    }
    Ok(Tree::build(items, truncated))
}

/// One `bsdtar -tv` line: `mode links user group size month day time-or-year name`.
fn parse_line(line: &str) -> Option<(String, bool, u64)> {
    let mut rest = line;
    let mut fields = Vec::with_capacity(8);
    for _ in 0..8 {
        rest = rest.trim_start_matches(' ');
        let end = rest.find(' ')?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    // Exactly one space separates the date from the name, which may start with spaces.
    let mut name = rest.strip_prefix(' ')?;
    let mode = fields[0];
    if mode.starts_with('l') {
        name = name.split_once(" -> ").map_or(name, |(n, _)| n);
    } else if mode.starts_with('h') {
        name = name.split_once(" link to ").map_or(name, |(n, _)| n);
    }
    let name = unescape(name);
    let is_dir = mode.starts_with('d') || name.ends_with('/');
    let size = fields[4].parse().unwrap_or(0);
    (!name.is_empty()).then_some((name, is_dir, size))
}

/// Undoes bsdtar's escaping of backslashes (`\\`) and unprintable bytes (`\ooo`).
fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let octal = bytes
                .get(i + 1..i + 4)
                .filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)));
            if let Some(d) = octal {
                let n = d.iter().fold(0u32, |n, b| n * 8 + u32::from(b - b'0'));
                if let Ok(b) = u8::try_from(n) {
                    out.push(b);
                    i += 4;
                    continue;
                }
            }
            if bytes.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verbose_lines() {
        let p = |l| parse_line(l).unwrap();
        assert_eq!(
            p("drwxr-xr-x  0 vp     vp          0 Oct 26 02:57 src/"),
            ("src/".into(), true, 0)
        );
        assert_eq!(
            p("-rw-r--r--  0 1000   1000     5000 Oct  6  2019 src/a file ü.txt"),
            ("src/a file ü.txt".into(), false, 5000)
        );
        assert_eq!(
            p("lrwxrwxrwx  0 0      0           0 Oct 26 02:57 src/link -> a b.txt"),
            ("src/link".into(), false, 0)
        );
        assert_eq!(
            p(r"-rw-r--r--  0 vp     vp          1 Oct 26 02:57 src/back\\slash\033.txt"),
            ("src/back\\slash\x1b.txt".into(), false, 1)
        );
        assert_eq!(
            p("-rw-r--r--  0 vp     vp          1 Oct 26 02:57  leading space"),
            (" leading space".into(), false, 1)
        );
        assert!(parse_line("bsdtar: Error exit delayed").is_none());
    }

    #[test]
    fn builds_a_tree_with_implicit_folders_and_totals() {
        let tree = Tree::build(
            vec![
                ("b/deep/x.bin".into(), false, 10),
                ("a.txt".into(), false, 5),
                ("b/".into(), true, 0),
                ("b/y.txt".into(), false, 7),
            ],
            false,
        );
        let names = |n: usize| -> Vec<&str> {
            tree.nodes[n]
                .children
                .iter()
                .map(|&c| tree.nodes[c].name.as_str())
                .collect()
        };
        assert_eq!(names(0), ["b", "a.txt"], "folders first");
        let b = tree.find(&["b".into()]).unwrap();
        assert_eq!(names(b), ["deep", "y.txt"]);
        assert_eq!(tree.nodes[b].size, 17);
        assert_eq!(tree.total_size(), 22);
        assert_eq!((tree.files, tree.dirs), (3, 2));
        assert!(tree.find(&["b".into(), "nope".into()]).is_none());
    }

    #[test]
    fn lists_a_real_archive() {
        let d = std::env::temp_dir().join(format!("kestrel-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/deep")).unwrap();
        std::fs::write(d.join("src/deep/f ü.txt"), "hello").unwrap();
        let zip = d.join("t.zip");
        let made = Command::new("bsdtar")
            .current_dir(&d)
            .args(["-a", "-cf", "t.zip", "src"])
            .status();
        if !made.is_ok_and(|s| s.success()) {
            return; // bsdtar isn't installed
        }
        let tree = list(&zip, &|| false).unwrap();
        let deep = tree.find(&["src".into(), "deep".into()]).unwrap();
        let file = tree.nodes[deep].children[0];
        assert_eq!(tree.nodes[file].name, "f ü.txt");
        assert_eq!(tree.nodes[file].size, 5);
        std::fs::write(d.join("junk.zip"), "not an archive").unwrap();
        assert!(list(&d.join("junk.zip"), &|| false).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
}
