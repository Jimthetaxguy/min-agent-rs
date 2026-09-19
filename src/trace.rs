//! Append-only JSONL run trace: metadata only, never file contents or model text.
//!
//! Every line is `{"v":1,"seq":N,"wall_time":<unix ms>,"run_id":"...","kind":"...","payload":{...}}`.
//! `seq` is dense per run. Readers must skip kinds they do not know, so new kinds can be
//! added without a version bump; `v` changes only when existing fields change meaning.
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const TRACE_VERSION: u64 = 1;

pub struct Trace {
    sink: Option<Box<dyn Write>>,
    run_id: String,
    seq: u64,
    failed: bool,
}

impl Trace {
    /// A trace that records nothing (the run still gets an ID for its report).
    pub fn disabled() -> Self {
        Self {
            sink: None,
            run_id: new_run_id(),
            seq: 0,
            failed: false,
        }
    }

    pub fn to_writer(sink: Box<dyn Write>) -> Self {
        Self {
            sink: Some(sink),
            ..Self::disabled()
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// True once any write failed; the run then stops rather than continue unaudited.
    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn emit(&mut self, kind: &str, payload: Value) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        if self.failed {
            return;
        }
        let wall_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let line = json!({
            "v": TRACE_VERSION,
            "seq": self.seq,
            "wall_time": wall_time,
            "run_id": self.run_id,
            "kind": kind,
            "payload": payload,
        });
        self.seq += 1;
        let ok = serde_json::to_writer(&mut *sink, &line).is_ok()
            && sink.write_all(b"\n").is_ok()
            && sink.flush().is_ok();
        self.failed = !ok;
    }
}

/// Unique enough for correlating one machine's runs: time, process, and a counter.
fn new_run_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!(
        "{nanos:016x}-{:08x}-{:04x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed) & 0xffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    pub struct Shared(pub Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lines_have_envelope_and_dense_seq() {
        let buffer = Shared::default();
        let mut trace = Trace::to_writer(Box::new(buffer.clone()));
        trace.emit("a", json!({"x":1}));
        trace.emit("b", json!({}));
        let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        for (n, line) in lines.iter().enumerate() {
            assert_eq!(line["v"], 1);
            assert_eq!(line["seq"], n as u64);
            assert_eq!(line["run_id"], trace.run_id());
            assert!(line["wall_time"].as_u64().unwrap() > 0);
        }
        assert_eq!(lines[0]["kind"], "a");
        assert_eq!(lines[0]["payload"]["x"], 1);
        assert!(!trace.failed());
    }

    #[test]
    fn write_failure_is_sticky() {
        let mut trace = Trace::to_writer(Box::new(Broken));
        trace.emit("a", json!({}));
        assert!(trace.failed());
        assert!(!Trace::disabled().failed());
        assert_ne!(Trace::disabled().run_id(), Trace::disabled().run_id());
    }
}
