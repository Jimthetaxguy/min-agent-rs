//! Read-only workspace tools. `prepare` validates arguments and path policy before any
//! content is read; `execute` performs the bounded read. Every tool has the `Read` effect.
use crate::{model::ToolSpec, redact};
use cap_std::{ambient_authority, fs::Dir};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fmt,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::Instant,
};

/// Largest byte count one `read_file` call may request.
pub const READ_LIMIT: usize = 8192;
/// Entries examined by one `list_files` call before it reports truncation.
pub const LIST_SCAN_LIMIT: usize = 5_000;
/// Entries examined by one `search_text` call before it reports truncation.
pub const SEARCH_SCAN_LIMIT: usize = 20_000;
/// Total file bytes one `search_text` call may read.
pub const SEARCH_BYTE_LIMIT: u64 = 16 * 1024 * 1024;
/// Files larger than this are skipped by `search_text` (and reported as truncation).
pub const FILE_SCAN_LIMIT: u64 = 262_144;
const SEARCH_DEPTH: usize = 8;
const RESULT_RESERVE: usize = 512;
/// Bytes read before and after a page so redaction sees tokens the page boundary cuts.
const REDACT_CONTEXT_BEFORE: u64 = READ_LIMIT as u64;
const REDACT_CONTEXT_AFTER: u64 = 256;

/// The effect class of a prepared call. Only `Read` exists: adding a variant is gated on
/// effect typing, attempt fencing, digest-bound approval, and a process supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    /// Protocol violation: the model named a tool that was never offered.
    UnknownTool,
    InvalidArguments,
    NotFound,
    Denied,
    WrongType,
    Io,
    /// The run or tool deadline elapsed; this stops the run rather than feeding back.
    Deadline,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub message: String,
}

impl ToolError {
    fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// JSON sent back to the model as the tool result.
    pub fn to_result(&self) -> String {
        json!({"error":{"kind":self.kind,"message":self.message}}).to_string()
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

type ToolResult<T> = Result<T, ToolError>;

fn denied<T>(message: &str) -> ToolResult<T> {
    Err(ToolError::new(ToolErrorKind::Denied, message))
}

fn io_error(error: io::Error) -> ToolError {
    // Only the kind is reported: OS messages can carry host paths.
    match error.kind() {
        io::ErrorKind::NotFound => {
            ToolError::new(ToolErrorKind::NotFound, "Workspace path not found")
        }
        io::ErrorKind::PermissionDenied => ToolError::new(
            ToolErrorKind::Denied,
            "Permission denied by the operating system",
        ),
        _ => ToolError::new(ToolErrorKind::Io, "I/O error while reading the workspace"),
    }
}

fn check_deadline(deadline: Instant) -> ToolResult<()> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(ToolError::new(
            ToolErrorKind::Deadline,
            "Tool deadline elapsed",
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default = "dot")]
    path: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_bytes")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    #[serde(default = "dot")]
    path: String,
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

enum Op {
    List(ListArgs),
    Read(ReadArgs),
    Search(SearchArgs),
}

/// A validated call. Opaque: only `Workspace::prepare` can construct one, so every
/// executed call has passed argument bounds and path policy.
pub struct Prepared(Op);

impl Prepared {
    pub fn effect(&self) -> Effect {
        match self.0 {
            Op::List(_) | Op::Read(_) | Op::Search(_) => Effect::Read,
        }
    }

    pub fn tool(&self) -> &'static str {
        match self.0 {
            Op::List(_) => "list_files",
            Op::Read(_) => "read_file",
            Op::Search(_) => "search_text",
        }
    }

    /// Workspace-relative path the call targets (for trace records; no content).
    pub fn path(&self) -> &str {
        match &self.0 {
            Op::List(a) => &a.path,
            Op::Read(a) => &a.path,
            Op::Search(a) => &a.path,
        }
    }
}

/// Successful tool output plus metadata for the run record.
#[derive(Debug)]
pub struct ToolOutput {
    pub content: String,
    pub truncated: bool,
    pub redacted: usize,
}

fn dot() -> String {
    ".".into()
}
fn default_limit() -> usize {
    100
}
fn default_bytes() -> usize {
    READ_LIMIT
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
                | "id_dsa"
                | "id_ecdsa"
                | "id_ed25519"
                | ".git-credentials"
                | ".netrc"
                | "_netrc"
                | ".npmrc"
                | ".pypirc"
                | ".pgpass"
                | ".docker"
                | ".kube"
        )
        || [
            ".pem",
            ".key",
            ".p12",
            ".pfx",
            ".keystore",
            ".jks",
            ".tfstate",
            ".tfstate.backup",
        ]
        .iter()
        .any(|suffix| n.ends_with(suffix))
}

/// Workspace-relative display form: `a/b`, never `./a/b`.
fn display(path: &Path) -> String {
    let parts: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// Serialized size of a JSON value, used to keep results under the output cap.
fn size(value: &Value) -> usize {
    value.to_string().len()
}

pub struct Workspace {
    root: Dir,
    pub display_path: PathBuf,
}

impl Workspace {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        use anyhow::{ensure, Context};
        let display_path = path.canonicalize().context("Cannot resolve workspace")?;
        ensure!(display_path.is_dir(), "Workspace must be a directory");
        let root = Dir::open_ambient_dir(&display_path, ambient_authority())
            .context("Cannot open workspace capability")?;
        Ok(Self { root, display_path })
    }

    fn checked(&self, raw: &str) -> ToolResult<PathBuf> {
        if raw.is_empty() || raw.len() > 4096 {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                "Path must be 1..4096 bytes; use \".\" for the workspace root",
            ));
        }
        let mut path = PathBuf::new();
        for component in Path::new(raw).components() {
            match component {
                Component::Normal(name) => {
                    let Some(name_text) = name.to_str() else {
                        return Err(ToolError::new(
                            ToolErrorKind::InvalidArguments,
                            "Non-UTF8 path unsupported",
                        ));
                    };
                    if blocked(name_text) {
                        return denied("Sensitive or excluded path is denied");
                    }
                    path.push(name);
                    let metadata = self.root.symlink_metadata(&path).map_err(io_error)?;
                    if metadata.file_type().is_symlink() {
                        return denied("Symlink paths are denied");
                    }
                }
                Component::CurDir => {}
                _ => return denied("Absolute paths and parent traversal are denied"),
            }
        }
        if path.as_os_str().is_empty() {
            path.push(".");
        }
        Ok(path)
    }

    fn directory(&self, raw: &str, tool: &str) -> ToolResult<PathBuf> {
        let path = self.checked(raw)?;
        if !self.root.metadata(&path).map_err(io_error)?.is_dir() {
            return Err(ToolError::new(
                ToolErrorKind::WrongType,
                format!("{tool} requires a directory"),
            ));
        }
        Ok(path)
    }

    fn regular_file(&self, raw: &str) -> ToolResult<PathBuf> {
        let path = self.checked(raw)?;
        // Rejects FIFOs, sockets, and devices before any open (opening a FIFO blocks).
        if !self.root.metadata(&path).map_err(io_error)?.is_file() {
            return Err(ToolError::new(
                ToolErrorKind::WrongType,
                "Only regular files may be read",
            ));
        }
        Ok(path)
    }

    pub fn prepare(&self, name: &str, arguments: Value) -> ToolResult<Prepared> {
        fn args<T: serde::de::DeserializeOwned>(arguments: Value) -> ToolResult<T> {
            serde_json::from_value(arguments)
                .map_err(|e| ToolError::new(ToolErrorKind::InvalidArguments, e.to_string()))
        }
        let bad = |message: &str| Err(ToolError::new(ToolErrorKind::InvalidArguments, message));
        match name {
            "list_files" => {
                let a: ListArgs = args(arguments)?;
                if a.limit == 0 || a.limit > 200 {
                    return bad("List limit must be 1..200");
                }
                self.directory(&a.path, name)?;
                Ok(Prepared(Op::List(a)))
            }
            "read_file" => {
                let a: ReadArgs = args(arguments)?;
                if a.limit == 0 || a.limit > READ_LIMIT {
                    return bad("Read limit must be 1..8192 bytes");
                }
                self.regular_file(&a.path)?;
                Ok(Prepared(Op::Read(a)))
            }
            "search_text" => {
                let a: SearchArgs = args(arguments)?;
                if a.query.is_empty() || a.query.len() > 256 {
                    return bad("Query must be 1..256 bytes");
                }
                if a.limit == 0 || a.limit > 200 {
                    return bad("Search limit must be 1..200");
                }
                self.directory(&a.path, name)?;
                Ok(Prepared(Op::Search(a)))
            }
            _ => Err(ToolError::new(
                ToolErrorKind::UnknownTool,
                "Unknown or unavailable tool",
            )),
        }
    }

    /// Opens a validated regular file, rejecting hardlinked aliases.
    fn open_file(&self, path: &Path) -> ToolResult<(std::fs::File, u64)> {
        // cap-std enforces beneath-root resolution at open, not merely a
        // canonicalize-then-open check. No host path is passed to open.
        let file = self.root.open(path).map_err(io_error)?.into_std();
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(ToolError::new(
                ToolErrorKind::WrongType,
                "File type changed during the call",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return denied("Multiply linked files are denied");
            }
        }
        Ok((file, metadata.len()))
    }

    /// Entries of one directory, sorted by name, examining at most `cap` entries.
    fn sorted_entries(
        &self,
        dir: &Path,
        cap: usize,
        deadline: Instant,
    ) -> ToolResult<(Vec<(String, cap_std::fs::FileType)>, bool)> {
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in self.root.read_dir(dir).map_err(io_error)? {
            check_deadline(deadline)?;
            if entries.len() >= cap {
                truncated = true;
                break;
            }
            let entry = entry.map_err(io_error)?;
            let kind = entry.file_type().map_err(io_error)?;
            entries.push((entry.file_name().to_string_lossy().into_owned(), kind));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok((entries, truncated))
    }

    pub fn execute(
        &self,
        call: Prepared,
        deadline: Instant,
        output_limit: usize,
    ) -> ToolResult<ToolOutput> {
        check_deadline(deadline)?;
        let budget = output_limit.saturating_sub(RESULT_RESERVE);
        let output = match call.0 {
            Op::Read(a) => self.read(a, output_limit)?,
            Op::List(a) => {
                let path = self.directory(&a.path, "list_files")?;
                let (all, mut truncated) = self.sorted_entries(&path, LIST_SCAN_LIMIT, deadline)?;
                let mut entries = Vec::new();
                let mut used = 0;
                for (name, kind) in all {
                    if blocked(&name) || kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                        continue;
                    }
                    let entry =
                        json!({"name":name,"kind":if kind.is_dir() {"directory"} else {"file"}});
                    used += size(&entry) + 1;
                    if entries.len() >= a.limit || used > budget {
                        truncated = true;
                        break;
                    }
                    entries.push(entry);
                }
                ToolOutput {
                    content: json!({"path":display(&path),"entries":entries,"truncated":truncated,"scan_limit":LIST_SCAN_LIMIT}).to_string(),
                    truncated,
                    redacted: 0,
                }
            }
            Op::Search(a) => self.search(a, deadline, budget)?,
        };
        check_deadline(deadline)?;
        debug_assert!(output.content.len() <= output_limit);
        if output.content.len() > output_limit {
            return Err(ToolError::new(
                ToolErrorKind::Io,
                "Tool output exceeded its cap",
            ));
        }
        Ok(output)
    }

    fn read(&self, a: ReadArgs, output_limit: usize) -> ToolResult<ToolOutput> {
        let path = self.regular_file(&a.path)?;
        let (mut file, len) = self.open_file(&path)?;
        if a.offset > len {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                "Read offset exceeds file length",
            ));
        }
        // Read context around the page: up to one full page before (so a PEM body whose
        // BEGIN line is on an earlier page is recognized) and a little after (so a token cut
        // by the page end is recognized). Only the page itself is returned.
        let before = a.offset.min(REDACT_CONTEXT_BEFORE);
        file.seek(SeekFrom::Start(a.offset - before))
            .map_err(io_error)?;
        let pre = before as usize;
        let mut context = Vec::new();
        file.take(before + a.limit as u64 + 3 + REDACT_CONTEXT_AFTER)
            .read_to_end(&mut context)
            .map_err(io_error)?;
        let bytes = &context[pre..];
        let ranges = redact::find(&context);
        // Up to 3 extra bytes let a character straddling the limit be completed.
        let mut end = char_end(
            &bytes[..bytes.len().min(a.limit + 3)],
            a.limit.min(bytes.len()),
        );
        let rel = display(&path);
        let render = |end: usize| {
            let (chunk, redacted) = redact::apply(&context, &ranges, pre..pre + end);
            let (text, lossy) = match String::from_utf8(chunk) {
                Ok(text) => (text, false),
                Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
            };
            let next = a.offset + end as u64;
            let truncated = next < len;
            let mut value = json!({"path":rel,"offset":a.offset,"next_offset":next,"truncated":truncated,"content":text});
            if lossy {
                value["encoding"] = json!("lossy");
            }
            if redacted > 0 {
                value["redacted"] = json!(redacted);
            }
            (value.to_string(), truncated, redacted)
        };
        let mut rendered = render(end);
        if rendered.0.len() > output_limit {
            // Escaping (control characters become \u00XX) can inflate content; shrink the
            // page to fit rather than failing, and let the model paginate from next_offset.
            let (mut lo, mut hi) = (0, end);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if render(char_end(bytes, mid)).0.len() <= output_limit {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            end = char_end(bytes, lo);
            rendered = render(end);
        }
        Ok(ToolOutput {
            content: rendered.0,
            truncated: rendered.1,
            redacted: rendered.2,
        })
    }

    fn search(&self, a: SearchArgs, deadline: Instant, budget: usize) -> ToolResult<ToolOutput> {
        let start = self.directory(&a.path, "search_text")?;
        let mut pending = vec![(start, 0usize)];
        let mut scanned = 0usize;
        let mut bytes_read = 0u64;
        let mut hits = Vec::new();
        let mut used = 0;
        let mut truncated = false;
        let mut redacted_total = 0;
        let mut skipped = 0usize;
        'walk: while let Some((dir, depth)) = pending.pop() {
            let remaining = SEARCH_SCAN_LIMIT - scanned;
            let (entries, capped) = self.sorted_entries(&dir, remaining, deadline)?;
            truncated |= capped;
            let mut subdirs = Vec::new();
            for (name, kind) in entries {
                check_deadline(deadline)?;
                scanned += 1;
                if blocked(&name) || kind.is_symlink() {
                    continue;
                }
                let path = dir.join(&name);
                if kind.is_dir() {
                    if depth < SEARCH_DEPTH {
                        subdirs.push((path, depth + 1));
                    } else {
                        truncated = true;
                    }
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                let Ok((file, len)) = self.open_file(&path) else {
                    skipped += 1;
                    continue;
                };
                if len > FILE_SCAN_LIMIT || bytes_read + len > SEARCH_BYTE_LIMIT {
                    truncated = true;
                    continue;
                }
                let mut bytes = Vec::new();
                file.take(FILE_SCAN_LIMIT + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_error)?;
                bytes_read += bytes.len() as u64;
                if std::str::from_utf8(&bytes).is_err() {
                    skipped += 1;
                    continue;
                }
                // Match against redacted lines so search cannot probe a credential's
                // characters one query at a time.
                let ranges = redact::find(&bytes);
                let rel = display(&path);
                let mut line_start = 0;
                for (line, raw_line) in bytes.split(|b| *b == b'\n').enumerate() {
                    let window = line_start..line_start + raw_line.len();
                    line_start = window.end + 1;
                    let (clean, redacted) = redact::apply(&bytes, &ranges, window);
                    let clean = String::from_utf8_lossy(&clean);
                    let content = clean.strip_suffix('\r').unwrap_or(&clean);
                    if !content.contains(&a.query) {
                        continue;
                    }
                    let snippet: String = content.chars().take(160).collect();
                    let hit = json!({"path":rel,"line":line + 1,"text":snippet});
                    used += size(&hit) + 1;
                    if hits.len() >= a.limit || used > budget {
                        truncated = true;
                        break 'walk;
                    }
                    redacted_total += redacted;
                    hits.push(hit);
                }
            }
            if scanned >= SEARCH_SCAN_LIMIT {
                truncated = truncated || !subdirs.is_empty() || !pending.is_empty();
                break;
            }
            // Depth-first in name order: push in reverse so the smallest name pops first.
            pending.extend(subdirs.into_iter().rev());
        }
        // `skipped_files` counts binary, unreadable, and hardlinked files: not searched.
        let mut value = json!({"matches":hits,"truncated":truncated,"entries_scanned":scanned,"skipped_files":skipped,"scan_limit":SEARCH_SCAN_LIMIT});
        if redacted_total > 0 {
            value["redacted"] = json!(redacted_total);
        }
        Ok(ToolOutput {
            content: value.to_string(),
            truncated,
            redacted: redacted_total,
        })
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

/// Largest cut at or before `limit` that does not split a UTF-8 sequence. If that would be
/// empty (limit smaller than the first character), extends forward to complete it.
fn char_end(bytes: &[u8], limit: usize) -> usize {
    let limit = limit.min(bytes.len());
    if limit == bytes.len() {
        return limit;
    }
    let mut end = limit;
    while end > 0 && limit - end < 3 && is_continuation(bytes[end]) {
        end -= 1;
    }
    if end == 0 && limit > 0 {
        end = 1;
        while end < bytes.len() && is_continuation(bytes[end]) {
            end += 1;
        }
    }
    end
}

/// Tool schemas, sorted by name so the catalog bytes are stable across runs.
pub fn definitions() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_files",
            description: "List immediate nonsecret workspace entries, sorted by name. Bounded scan; no symlinks.",
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}),
        },
        ToolSpec {
            name: "read_file",
            description: "Read a regular nonsecret workspace file. offset/limit are bytes; pages end on UTF-8 boundaries. Maximum 8192 bytes; paginate using next_offset. Known credential formats are redacted.",
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":8192}},"required":["path"],"additionalProperties":false}),
        },
        ToolSpec {
            name: "search_text",
            description: "Literal text search in the workspace, depth-first in name order: at most 20000 entries, depth 8, 256 KiB per file, 16 MiB total. Output marks truncation.",
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200}},"required":["query"],"additionalProperties":false}),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    fn run(w: &Workspace, tool: &str, args: Value) -> Value {
        let call = w.prepare(tool, args).unwrap();
        serde_json::from_str(&w.execute(call, later(), 32_768).unwrap().content).unwrap()
    }

    #[test]
    fn reads_and_rejects_unsafe_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("hello"), "abc").unwrap();
        std::fs::write(temp.path().join(".env"), "secret").unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        for (bad, kind) in [
            ("/etc/passwd", ToolErrorKind::Denied),
            ("../outside", ToolErrorKind::Denied),
            (".env", ToolErrorKind::Denied),
            ("missing", ToolErrorKind::NotFound),
        ] {
            let error = w.prepare("read_file", json!({"path":bad})).err().unwrap();
            assert_eq!(error.kind, kind, "{bad}");
        }
        assert_eq!(
            run(&w, "read_file", json!({"path":"hello"}))["content"],
            "abc"
        );
        assert_eq!(
            w.prepare("run_shell", json!({})).err().unwrap().kind,
            ToolErrorKind::UnknownTool
        );
        assert_eq!(
            w.prepare("read_file", json!({"path":"hello","extra":true}))
                .err()
                .unwrap()
                .kind,
            ToolErrorKind::InvalidArguments
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_hardlinks_and_fifos_are_not_read() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "x outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("escape")).unwrap();
        std::fs::hard_link(outside.path(), temp.path().join("hard")).unwrap();
        let fifo = temp.path().join("pipe");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let w = Workspace::open(temp.path()).unwrap();
        assert_eq!(
            w.prepare("read_file", json!({"path":"escape"}))
                .err()
                .unwrap()
                .kind,
            ToolErrorKind::Denied
        );
        let call = w.prepare("read_file", json!({"path":"hard"})).unwrap();
        assert_eq!(
            w.execute(call, later(), 32_768).err().unwrap().kind,
            ToolErrorKind::Denied
        );
        // A FIFO is rejected by metadata before open, which would otherwise block.
        assert_eq!(
            w.prepare("read_file", json!({"path":"pipe"}))
                .err()
                .unwrap()
                .kind,
            ToolErrorKind::WrongType
        );
        // Symlink and FIFO are skipped; the hardlink is listed but its content is refused.
        let listed = run(&w, "list_files", json!({}));
        assert_eq!(listed["entries"], json!([{"name":"hard","kind":"file"}]));
        // The hardlink holds matching text, but linked files are skipped, not searched.
        let found = run(&w, "search_text", json!({"query":"x"}));
        assert!(found["matches"].as_array().unwrap().is_empty());
        assert_eq!(found["skipped_files"], 1);
    }

    #[test]
    fn pagination_never_splits_utf8() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("u.txt"), "héllo").unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let first = run(&w, "read_file", json!({"path":"u.txt","limit":2}));
        assert_eq!(first["content"], "h");
        assert_eq!(first["next_offset"], 1);
        let second = run(
            &w,
            "read_file",
            json!({"path":"u.txt","offset":1,"limit":1}),
        );
        // limit smaller than one character still makes progress by one whole character.
        assert_eq!(second["content"], "é");
        assert_eq!(second["next_offset"], 3);
        let rest = run(&w, "read_file", json!({"path":"u.txt","offset":3}));
        assert_eq!(rest["content"], "llo");
        assert_eq!(rest["truncated"], false);
        assert!(rest.get("encoding").is_none());
    }

    #[test]
    fn control_characters_shrink_to_fit_instead_of_failing() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("nul.bin"), vec![0u8; 8192]).unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let call = w.prepare("read_file", json!({"path":"nul.bin"})).unwrap();
        let out = w.execute(call, later(), 32_768).unwrap();
        assert!(out.content.len() <= 32_768);
        assert!(out.truncated);
        let value: Value = serde_json::from_str(&out.content).unwrap();
        let next = value["next_offset"].as_u64().unwrap();
        assert!(next > 4000 && next < 8192, "{next}");
    }

    #[test]
    fn credentials_in_content_are_redacted() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.py"),
            "KEY = 'AKIAABCDEFGHIJKLMNOP'\n",
        )
        .unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let read = run(&w, "read_file", json!({"path":"config.py"}));
        assert_eq!(read["content"], "KEY = '[REDACTED]'\n");
        assert_eq!(read["redacted"], 1);
        let found = run(&w, "search_text", json!({"query":"KEY"}));
        assert_eq!(found["matches"][0]["text"], "KEY = '[REDACTED]'");
    }

    #[test]
    fn redaction_survives_offsets_page_splits_and_search_probes() {
        let temp = tempfile::tempdir().unwrap();
        let key = "AKIAABCDEFGHIJKLMNOP";
        std::fs::write(temp.path().join("c.txt"), format!("id={key};\n")).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC".repeat(8)
        );
        std::fs::write(temp.path().join("k.txt"), &pem).unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        // Offset past the prefix, and a page ending inside the token.
        let late = run(&w, "read_file", json!({"path":"c.txt","offset":8}));
        assert!(
            !late["content"].as_str().unwrap().contains("CDEFGH"),
            "{late}"
        );
        let cut = run(&w, "read_file", json!({"path":"c.txt","limit":10}));
        assert_eq!(cut["content"], "id=[REDACTED]");
        // A page inside the key body, far from the BEGIN line.
        let body = run(
            &w,
            "read_file",
            json!({"path":"k.txt","offset":100,"limit":50}),
        );
        assert_eq!(body["content"], "[REDACTED]");
        // Search matches redacted text, so it cannot confirm credential prefixes.
        for query in ["AKIAABC", "MIIEvQ"] {
            let found = run(&w, "search_text", json!({"query":query}));
            assert!(found["matches"].as_array().unwrap().is_empty(), "{query}");
        }
    }

    #[test]
    fn credential_files_are_excluded_by_name() {
        for name in [
            ".env.local",
            ".git",
            ".git-credentials",
            ".netrc",
            ".npmrc",
            ".pypirc",
            ".pgpass",
            ".kube",
            ".docker",
            "id_ecdsa",
            "server.pem",
            "prod.tfstate",
            "store.jks",
            "credentials.json",
        ] {
            assert!(blocked(name), "{name}");
        }
        for name in [".gitignore", ".github", "main.rs", "README.md", "keys.rs"] {
            assert!(!blocked(name), "{name}");
        }
    }

    #[test]
    fn listing_and_search_are_sorted_and_relative() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["z", "a", "m", "b"] {
            std::fs::write(temp.path().join(name), "needle").unwrap();
        }
        std::fs::create_dir_all(temp.path().join("sub/deep")).unwrap();
        std::fs::write(temp.path().join("sub/deep/f.txt"), "needle").unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let listed = run(&w, "list_files", json!({}));
        let names: Vec<_> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a", "b", "m", "sub", "z"]);
        let found = run(&w, "search_text", json!({"query":"needle"}));
        let paths: Vec<_> = found["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(paths, ["a", "b", "m", "z", "sub/deep/f.txt"]);
        let scoped = run(&w, "search_text", json!({"query":"needle","path":"./sub"}));
        assert_eq!(scoped["matches"][0]["path"], "sub/deep/f.txt");
    }

    #[test]
    fn search_covers_large_trees() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..1000 {
            let body = if n % 333 == 0 { "needle" } else { "hay" };
            std::fs::write(temp.path().join(format!("f{n:04}.txt")), body).unwrap();
        }
        let w = Workspace::open(temp.path()).unwrap();
        let found = run(&w, "search_text", json!({"query":"needle"}));
        assert_eq!(found["matches"].as_array().unwrap().len(), 4);
        assert_eq!(found["truncated"], false);
        assert_eq!(found["entries_scanned"], 1000);
    }

    #[test]
    fn pagination_search_and_list_bounds() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("hello.txt"), "first\nneedle\nlast\n").unwrap();
        std::fs::write(temp.path().join(".env"), "needle secret").unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let out = run(&w, "read_file", json!({"path":"hello.txt","limit":6}));
        assert_eq!(out["content"], "first\n");
        assert_eq!(out["next_offset"], 6);
        assert_eq!(out["truncated"], true);
        let out = run(&w, "search_text", json!({"query":"needle"}));
        assert_eq!(out["matches"].as_array().unwrap().len(), 1);
        assert_eq!(out["matches"][0]["line"], 2);
        let out = run(&w, "list_files", json!({}));
        assert_eq!(out["entries"].as_array().unwrap().len(), 1);
        let out = run(&w, "list_files", json!({"limit":1}));
        assert_eq!(out["truncated"], false);
        std::fs::write(temp.path().join("second.txt"), "").unwrap();
        let out = run(&w, "list_files", json!({"limit":1}));
        assert_eq!(out["truncated"], true);
    }

    #[test]
    fn elapsed_tool_deadline_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let call = w.prepare("list_files", json!({})).unwrap();
        assert_eq!(
            w.execute(call, Instant::now(), 32_768).err().unwrap().kind,
            ToolErrorKind::Deadline
        );
    }

    #[test]
    fn every_tool_is_read_only_and_catalog_is_sorted() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("f"), "x").unwrap();
        let w = Workspace::open(temp.path()).unwrap();
        let names: Vec<_> = definitions().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for (name, args) in [
            ("list_files", json!({})),
            ("read_file", json!({"path":"f"})),
            ("search_text", json!({"query":"x"})),
        ] {
            let prepared = w.prepare(name, args).unwrap();
            assert_eq!(prepared.effect(), Effect::Read);
            assert_eq!(prepared.tool(), name);
        }
    }

    #[test]
    fn char_end_boundaries() {
        let s = "aé€𝄞".as_bytes(); // 1 + 2 + 3 + 4 bytes
        assert_eq!(char_end(s, 2), 1);
        assert_eq!(char_end(s, 3), 3);
        assert_eq!(char_end(s, 5), 3);
        assert_eq!(char_end(s, 6), 6);
        assert_eq!(char_end(s, 9), 6);
        assert_eq!(char_end(s, 10), 10);
        assert_eq!(char_end("𝄞x".as_bytes(), 1), 4);
    }
}
