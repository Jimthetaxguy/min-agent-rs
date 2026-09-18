use anyhow::{bail, ensure, Context, Result};
use cap_std::{ambient_authority, fs::Dir};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::Instant,
};

pub const OUTPUT_LIMIT: usize = 32_768;
const FILE_SCAN_LIMIT: u64 = 262_144;

pub struct Workspace {
    root: Dir,
    pub display_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    #[serde(default = "dot")]
    path: String,
    #[serde(default = "default_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_bytes")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    #[serde(default = "dot")]
    path: String,
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

pub enum Prepared {
    List(ListArgs),
    Read(ReadArgs),
    Search(SearchArgs),
}

fn dot() -> String {
    ".".into()
}
fn default_limit() -> usize {
    100
}
fn default_bytes() -> usize {
    8192
}

fn blocked(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == ".git"
        || n == ".ssh"
        || n == ".aws"
        || n == ".gnupg"
        || n == "node_modules"
        || n == "target"
        || n.starts_with(".env")
        || matches!(
            n.as_str(),
            "credentials"
                | "credentials.json"
                | "auth.json"
                | "secrets.json"
                | "id_rsa"
                | "id_ed25519"
        )
        || [".pem", ".key", ".p12", ".pfx", ".keystore"]
            .iter()
            .any(|suffix| n.ends_with(suffix))
}

impl Workspace {
    pub fn open(path: &Path) -> Result<Self> {
        let display_path = path.canonicalize().context("Cannot resolve workspace")?;
        ensure!(display_path.is_dir(), "Workspace must be a directory");
        let root = Dir::open_ambient_dir(&display_path, ambient_authority())
            .context("Cannot open workspace capability")?;
        Ok(Self { root, display_path })
    }

    fn checked(&self, raw: &str) -> Result<PathBuf> {
        ensure!(
            !raw.is_empty() && raw.len() <= 4096,
            "Invalid workspace path"
        );
        let mut path = PathBuf::new();
        for component in Path::new(raw).components() {
            match component {
                Component::Normal(name) => {
                    let name_text = name.to_str().context("Non-UTF8 path unsupported")?;
                    ensure!(!blocked(name_text), "Sensitive or excluded path is denied");
                    path.push(name);
                    let metadata = self
                        .root
                        .symlink_metadata(&path)
                        .context("Workspace path not found")?;
                    ensure!(
                        !metadata.file_type().is_symlink(),
                        "Symlink paths are denied"
                    );
                }
                Component::CurDir => {}
                _ => bail!("Absolute paths and parent traversal are denied"),
            }
        }
        if path.as_os_str().is_empty() {
            path.push(".");
        }
        Ok(path)
    }

    pub fn prepare(&self, name: &str, arguments: Value) -> Result<Prepared> {
        match name {
            "list_files" => {
                let a: ListArgs =
                    serde_json::from_value(arguments).context("Invalid list_files arguments")?;
                ensure!(a.limit > 0 && a.limit <= 200, "List limit must be 1..200");
                let p = self.checked(&a.path)?;
                ensure!(
                    self.root.metadata(p)?.is_dir(),
                    "list_files requires a directory"
                );
                Ok(Prepared::List(a))
            }
            "read_file" => {
                let a: ReadArgs =
                    serde_json::from_value(arguments).context("Invalid read_file arguments")?;
                ensure!(
                    a.limit > 0 && a.limit <= 8192,
                    "Read limit must be 1..8192 bytes"
                );
                self.regular_file(&a.path)?;
                Ok(Prepared::Read(a))
            }
            "search_text" => {
                let a: SearchArgs =
                    serde_json::from_value(arguments).context("Invalid search_text arguments")?;
                ensure!(
                    !a.query.is_empty() && a.query.len() <= 256,
                    "Query must be 1..256 bytes"
                );
                ensure!(a.limit > 0 && a.limit <= 200, "Search limit must be 1..200");
                let p = self.checked(&a.path)?;
                ensure!(
                    self.root.metadata(p)?.is_dir(),
                    "search_text requires a directory"
                );
                Ok(Prepared::Search(a))
            }
            _ => bail!("Unknown or unavailable tool"),
        }
    }

    fn regular_file(&self, path: &str) -> Result<PathBuf> {
        let path = self.checked(path)?;
        let metadata = self.root.metadata(&path)?;
        ensure!(metadata.is_file(), "Only regular files may be read");
        Ok(path)
    }

    pub fn execute(&self, call: Prepared, deadline: Instant) -> Result<String> {
        ensure!(Instant::now() < deadline, "BudgetExceeded: tool deadline");
        let output = match call {
            Prepared::Read(a) => {
                let path = self.regular_file(&a.path)?;
                // cap-std enforces beneath-root resolution at open, not merely a
                // canonicalize-then-open check. No host path is passed to open.
                let mut file = self.root.open(path)?.into_std();
                let metadata = file.metadata()?;
                ensure!(metadata.is_file(), "Only regular files may be read");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    ensure!(metadata.nlink() == 1, "Multiply linked files are denied");
                }
                ensure!(
                    a.offset <= metadata.len(),
                    "Read offset exceeds file length"
                );
                file.seek(SeekFrom::Start(a.offset))?;
                let mut bytes = Vec::new();
                file.take(a.limit as u64).read_to_end(&mut bytes)?;
                let content = String::from_utf8_lossy(&bytes);
                json!({"path":a.path,"offset":a.offset,"next_offset":a.offset + bytes.len() as u64,"truncated":a.offset + (bytes.len() as u64) < metadata.len(),"content":content})
            }
            Prepared::List(a) => {
                let path = self.checked(&a.path)?;
                let mut entries = Vec::new();
                let mut truncated = false;
                for (scanned, entry) in self.root.read_dir(path)?.enumerate() {
                    if scanned >= 500 {
                        truncated = true;
                        break;
                    }
                    ensure!(Instant::now() < deadline, "BudgetExceeded: tool deadline");
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    let kind = entry.file_type()?;
                    if blocked(&name) || kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                        continue;
                    }
                    if entries.len() >= a.limit {
                        truncated = true;
                        break;
                    }
                    entries.push(
                        json!({"name":name,"kind":if kind.is_dir() {"directory"} else {"file"}}),
                    );
                }
                json!({"path":a.path,"entries":entries,"truncated":truncated,"scan_limit":500})
            }
            Prepared::Search(a) => {
                let start = self.checked(&a.path)?;
                let mut pending = vec![(start, 0usize)];
                let mut scanned = 0usize;
                let mut hits = Vec::new();
                let mut truncated = false;
                'walk: while let Some((dir, depth)) = pending.pop() {
                    for entry in self.root.read_dir(&dir)? {
                        ensure!(Instant::now() < deadline, "BudgetExceeded: tool deadline");
                        scanned += 1;
                        if scanned > 500 {
                            truncated = true;
                            break 'walk;
                        }
                        let entry = entry?;
                        let name = entry.file_name();
                        if blocked(&name.to_string_lossy()) {
                            continue;
                        }
                        let kind = entry.file_type()?;
                        if kind.is_symlink() {
                            continue;
                        }
                        let path = dir.join(name);
                        if kind.is_dir() {
                            if depth < 8 {
                                pending.push((path, depth + 1));
                            } else {
                                truncated = true;
                            }
                        } else if kind.is_file() {
                            let raw = path.to_str().context("Non-UTF8 path unsupported")?;
                            let prepared = self.regular_file(raw)?;
                            let file = self.root.open(prepared)?.into_std();
                            let metadata = file.metadata()?;
                            ensure!(metadata.is_file(), "File type changed during search");
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::MetadataExt;
                                if metadata.nlink() != 1 {
                                    continue;
                                }
                            }
                            if metadata.len() > FILE_SCAN_LIMIT {
                                truncated = true;
                                continue;
                            }
                            let mut bytes = Vec::new();
                            file.take(FILE_SCAN_LIMIT + 1).read_to_end(&mut bytes)?;
                            if bytes.len() as u64 > FILE_SCAN_LIMIT {
                                truncated = true;
                                continue;
                            }
                            let Ok(text) = std::str::from_utf8(&bytes) else {
                                continue;
                            };
                            for (line, content) in text.lines().enumerate() {
                                if content.contains(&a.query) {
                                    if hits.len() >= a.limit {
                                        truncated = true;
                                        break 'walk;
                                    }
                                    hits.push(json!({"path":raw,"line":line+1,"text":content.chars().take(160).collect::<String>()}));
                                }
                            }
                        }
                    }
                }
                json!({"matches":hits,"truncated":truncated,"entries_scanned":scanned.min(500)})
            }
        };
        let output = serde_json::to_string(&output)?;
        ensure!(
            output.len() <= OUTPUT_LIMIT,
            "BudgetExceeded: tool output bytes"
        );
        ensure!(Instant::now() < deadline, "BudgetExceeded: tool deadline");
        Ok(output)
    }
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{"name":"list_files","description":"List immediate nonsecret workspace entries. Bounded scan; no symlinks.","parameters":{"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}}}),
        json!({"type":"function","function":{"name":"read_file","description":"Read a regular nonsecret workspace file. offset/limit are bytes. Maximum 8192 bytes; paginate using next_offset.","parameters":{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":8192}},"required":["path"],"additionalProperties":false}}}),
        json!({"type":"function","function":{"name":"search_text","description":"Literal text search in workspace, at most 500 entries, depth 8, 256 KiB per file. Output marks truncation.","parameters":{"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200}},"required":["query"],"additionalProperties":false}}}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn reads_and_rejects_unsafe_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("hello"), "abc").unwrap();
        std::fs::write(temp.path().join(".env"), "secret").unwrap();
        let workspace = Workspace::open(temp.path()).unwrap();
        for bad in ["/etc/passwd", "../outside", ".env", "missing"] {
            assert!(workspace.prepare("read_file", json!({"path":bad})).is_err());
        }
        let call = workspace
            .prepare("read_file", json!({"path":"hello"}))
            .unwrap();
        assert!(workspace
            .execute(call, Instant::now() + Duration::from_secs(1))
            .unwrap()
            .contains("abc"));
        assert!(workspace.prepare("run_shell", json!({})).is_err());
        assert!(workspace
            .prepare("read_file", json!({"path":"hello","extra":true}))
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hardlinks_are_not_read() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("escape")).unwrap();
        std::fs::hard_link(outside.path(), temp.path().join("hard")).unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        assert!(w.prepare("read_file", json!({"path":"escape"})).is_err());
        let call = w.prepare("read_file", json!({"path":"hard"})).unwrap();
        assert!(w
            .execute(call, Instant::now() + Duration::from_secs(1))
            .is_err());
    }

    #[test]
    fn pagination_search_and_list_bounds() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("hello.txt"), "first\nneedle\nlast\n").unwrap();
        std::fs::write(temp.path().join(".env"), "needle secret").unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let read = w
            .prepare("read_file", json!({"path":"hello.txt","limit":6}))
            .unwrap();
        let out: Value = serde_json::from_str(&w.execute(read, deadline).unwrap()).unwrap();
        assert_eq!(out["content"], "first\n");
        assert_eq!(out["next_offset"], 6);
        assert_eq!(out["truncated"], true);
        let search = w.prepare("search_text", json!({"query":"needle"})).unwrap();
        let out: Value = serde_json::from_str(&w.execute(search, deadline).unwrap()).unwrap();
        assert_eq!(out["matches"].as_array().unwrap().len(), 1);
        assert_eq!(out["matches"][0]["line"], 2);
        let list = w.prepare("list_files", json!({})).unwrap();
        let out: Value = serde_json::from_str(&w.execute(list, deadline).unwrap()).unwrap();
        assert_eq!(out["entries"].as_array().unwrap().len(), 1);
        // Even when all scanned entries are excluded, exhaustion must be reported.
        for n in 0..502 {
            std::fs::write(temp.path().join(format!(".env.{n}")), "").unwrap();
        }
        let list = w.prepare("list_files", json!({})).unwrap();
        let out: Value = serde_json::from_str(&w.execute(list, deadline).unwrap()).unwrap();
        assert_eq!(out["truncated"], true);
    }

    #[test]
    fn elapsed_tool_deadline_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let call = w.prepare("list_files", json!({})).unwrap();
        assert!(w.execute(call, Instant::now()).is_err());
    }
}
