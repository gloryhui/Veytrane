//! Opt-in, bounded UDP/PUNCH diagnostics for the Windows Agent.
//!
//! This sink is deliberately not connected to the general tracing subscriber.
//! Callers may only emit the explicitly modelled fields below.

use crate::identity::NodeId;
use crate::nat::punch::PunchToken;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    net::SocketAddr,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_RECORDS: u64 = 4096;
const MAX_SECONDS: u64 = 3600;

struct Sink {
    file: File,
    deadline: Instant,
    bytes: u64,
    records: u64,
}

impl Sink {
    fn write(&mut self, line: &str) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        let length = line.len() as u64 + 1;
        if self.records >= MAX_RECORDS || self.bytes.saturating_add(length) > MAX_BYTES {
            return false;
        }
        if self.file.write_all(line.as_bytes()).is_err()
            || self.file.write_all(b"\n").is_err()
            || self.file.flush().is_err()
        {
            return false;
        }
        self.bytes += length;
        self.records += 1;
        true
    }
}

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();

/// Create a private, new diagnostic file and start its fixed deadline.
///
/// Existing paths are rejected so a symlink, reparse point or loosely protected
/// file cannot be reused accidentally.
pub fn configure(path: &Path, seconds: u64) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic log path must be absolute",
        ));
    }
    if seconds == 0 || seconds > MAX_SECONDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("diagnostic seconds must be in 1..={MAX_SECONDS}"),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic log path must have a parent directory",
        )
    })?;
    validate_directory_chain(parent)?;
    let file = open_new_private(path)?;
    let configured = Sink {
        file,
        deadline: Instant::now() + Duration::from_secs(seconds),
        bytes: 0,
        records: 0,
    };
    let slot = SINK.get_or_init(|| Mutex::new(None));
    let mut sink = slot
        .lock()
        .map_err(|_| io::Error::other("diagnostic log state is unavailable"))?;
    if sink.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "diagnostic log is already configured",
        ));
    }
    *sink = Some(configured);
    Ok(())
}

fn validate_directory_chain(path: &Path) -> io::Result<()> {
    for current in path.ancestors() {
        let metadata = std::fs::symlink_metadata(current)?;
        if metadata.file_type().is_symlink()
            || crate::storage::atomic::is_reparse(&metadata)
            || !metadata.is_dir()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic log parent chain must contain only normal directories",
            ));
        }
    }
    Ok(())
}

fn open_new_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    protect_current_user(path)?;
    Ok(file)
}

#[cfg(windows)]
fn protect_current_user(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
    };

    let descriptor = crate::storage::permissions::windows::Descriptor::new(false)?;
    let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if text[..text.len() - 1].contains(&0) {
        return Err(io::Error::other("diagnostic log path contains NUL"));
    }
    let result = unsafe {
        SetFileSecurityW(
            text.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.pointer(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_record(line: String) {
    let Some(slot) = SINK.get() else {
        return;
    };
    let Ok(mut sink) = slot.lock() else {
        return;
    };
    let Some(active) = sink.as_mut() else {
        return;
    };
    if !active.write(&line) {
        *sink = None;
    }
}

fn enabled() -> bool {
    let Some(slot) = SINK.get() else {
        return false;
    };
    let Ok(mut sink) = slot.lock() else {
        return false;
    };
    if sink
        .as_ref()
        .is_some_and(|active| Instant::now() >= active.deadline)
    {
        *sink = None;
        return false;
    }
    sink.is_some()
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn token_digest(token: &PunchToken) -> String {
    hex::encode(&Sha256::digest(token.as_bytes())[..6])
}

fn optional_peer(peer: Option<NodeId>) -> String {
    peer.map_or_else(|| "null".to_owned(), |peer| format!("\"{}\"", peer.to_hex()))
}

fn optional_generation(generation: Option<u64>) -> String {
    format!(
        "{}",
        generation.map_or_else(|| "null".to_owned(), |value| value.to_string())
    )
}

fn optional_addr(addr: Option<SocketAddr>) -> String {
    addr.map_or_else(|| "null".to_owned(), |addr| format!("\"{addr}\""))
}

fn format_candidates(candidates: &[SocketAddr]) -> String {
    candidates
        .iter()
        .map(|candidate| format!("\"{candidate}\""))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn startup(max_seconds: u64) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"diagnostic_start","max_seconds":{},"max_bytes":{},"max_records":{}}}"#,
        timestamp_ms(),
        max_seconds.min(MAX_SECONDS),
        MAX_BYTES,
        MAX_RECORDS
    ));
}

pub fn punch_send_ok(local: Option<SocketAddr>, token: &PunchToken, target: SocketAddr) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"punch_send","outcome":"ok","local":{},"token_sha256_12":"{}","target":"{}"}}"#,
        timestamp_ms(),
        optional_addr(local),
        token_digest(token),
        target
    ));
}

pub fn punch_send_error(
    local: Option<SocketAddr>,
    token: &PunchToken,
    target: SocketAddr,
    error: &io::Error,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"punch_send","outcome":"failed","local":{},"token_sha256_12":"{}","target":"{}","error_kind":"{:?}","os_error":{}}}"#,
        timestamp_ms(),
        optional_addr(local),
        token_digest(token),
        target,
        error.kind(),
        error.raw_os_error().map_or_else(|| "null".to_owned(), |code| code.to_string())
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn punch_recv(
    local: Option<SocketAddr>,
    token: &PunchToken,
    source: SocketAddr,
    outcome: &'static str,
    peer: Option<NodeId>,
    generation: Option<u64>,
    reason: Option<&'static str>,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"punch_recv","outcome":"{}","local":{},"token_sha256_12":"{}","source":"{}","peer":{},"generation":{},"reason":{}}}"#,
        timestamp_ms(),
        outcome,
        optional_addr(local),
        token_digest(token),
        source,
        optional_peer(peer),
        optional_generation(generation),
        reason.map_or_else(|| "null".to_owned(), |reason| format!("\"{reason}\""))
    ));
}

pub fn punch_claim(
    local: Option<SocketAddr>,
    source: SocketAddr,
    outcome: &'static str,
    peer: Option<NodeId>,
    generation: Option<u64>,
    active_authorizations: usize,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"punch_claim","outcome":"{}","local":{},"source":"{}","peer":{},"generation":{},"active_authorizations":{}}}"#,
        timestamp_ms(),
        outcome,
        optional_addr(local),
        source,
        optional_peer(peer),
        optional_generation(generation),
        active_authorizations
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn punch_watch_register(
    local: Option<SocketAddr>,
    token: &PunchToken,
    id: Option<u64>,
    peer: Option<NodeId>,
    generation: u64,
    succeeded: bool,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"punch_watch_register","outcome":"{}","local":{},"token_sha256_12":"{}","watch_id":{},"peer":{},"generation":{}}}"#,
        timestamp_ms(),
        if succeeded { "ok" } else { "failed" },
        optional_addr(local),
        token_digest(token),
        id.map_or_else(|| "null".to_owned(), |id| id.to_string()),
        optional_peer(peer),
        generation
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn punch_watch_drop(
    local: Option<SocketAddr>,
    token: &PunchToken,
    id: u64,
    peer: Option<NodeId>,
    generation: u64,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"punch_watch_drop","local":{},"token_sha256_12":"{}","watch_id":{},"peer":{},"generation":{}}}"#,
        timestamp_ms(),
        optional_addr(local),
        token_digest(token),
        id,
        optional_peer(peer),
        generation
    ));
}

pub fn session_attempt_path(
    peer: NodeId,
    generation: u64,
    route_kind: &'static str,
    route: u64,
    local: SocketAddr,
    candidates: &[SocketAddr],
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"session_attempt","peer":"{}","generation":{},"route_kind":"{}","route":{},"local":"{}","candidates":[{}]}}"#,
        timestamp_ms(),
        peer.to_hex(),
        generation,
        route_kind,
        route,
        local,
        format_candidates(candidates)
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn session_quic(
    peer: NodeId,
    generation: u64,
    route: u64,
    operation: &'static str,
    outcome: &'static str,
    local: Option<SocketAddr>,
    remote: Option<SocketAddr>,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"session_quic","peer":"{}","generation":{},"route":{},"operation":"{}","outcome":"{}","local":{},"remote":{}}}"#,
        timestamp_ms(),
        peer.to_hex(),
        generation,
        route,
        operation,
        outcome,
        optional_addr(local),
        optional_addr(remote)
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn session_winner(
    peer: NodeId,
    generation: u64,
    route: u64,
    route_kind: &'static str,
    local: Option<SocketAddr>,
    remote: SocketAddr,
    path_kind: &'static str,
) {
    if !enabled() {
        return;
    }
    write_record(format!(
        r#"{{"ts_unix_ms":{},"event":"session_winner","peer":"{}","generation":{},"route":{},"route_kind":"{}","local":{},"remote":"{}","path_kind":"{}"}}"#,
        timestamp_ms(),
        peer.to_hex(),
        generation,
        route,
        route_kind,
        optional_addr(local),
        remote,
        path_kind
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

    struct TestScope {
        root: PathBuf,
    }

    impl TestScope {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let sequence = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "p2p-file-diagnostic-{name}-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            crate::storage::permissions::ensure_directory(&root).unwrap();
            Self { root }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }
    }

    impl Drop for TestScope {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn local_sink(scope: &TestScope, name: &str, deadline: Instant) -> (Sink, PathBuf) {
        let path = scope.path(name);
        let file = open_new_private(&path).unwrap();
        (
            Sink {
                file,
                deadline,
                bytes: 0,
                records: 0,
            },
            path,
        )
    }

    #[test]
    fn local_sink_stops_at_each_hard_limit() {
        let scope = TestScope::new("sink-limits");
        let line = r#"{"event":"limit-test"}"#;

        let (mut expired, _) =
            local_sink(&scope, "deadline.jsonl", Instant::now() - Duration::from_secs(1));
        assert!(!expired.write(line));
        assert_eq!(expired.file.metadata().unwrap().len(), 0);

        let (mut records, _) =
            local_sink(&scope, "records.jsonl", Instant::now() + Duration::from_secs(30));
        records.records = MAX_RECORDS - 1;
        assert!(records.write(line));
        let written = records.file.metadata().unwrap().len();
        assert!(!records.write(line));
        assert_eq!(records.file.metadata().unwrap().len(), written);
        assert_eq!(records.records, MAX_RECORDS);

        let (mut bytes, _) =
            local_sink(&scope, "bytes.jsonl", Instant::now() + Duration::from_secs(30));
        let line_len = line.len() as u64 + 1;
        bytes.bytes = MAX_BYTES - line_len;
        assert!(bytes.write(line));
        let written = bytes.file.metadata().unwrap().len();
        assert!(!bytes.write(line));
        assert_eq!(bytes.file.metadata().unwrap().len(), written);
        assert_eq!(bytes.bytes, MAX_BYTES);
    }

    #[test]
    fn token_digest_is_short_hex_and_not_the_raw_token() {
        let raw = [0xab; 16];
        let token = PunchToken::from_bytes(raw);
        let digest = token_digest(&token);

        assert_eq!(digest.len(), 12);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(digest, hex::encode(raw));
        assert_ne!(digest, token.to_string());
    }

    #[test]
    fn private_file_rejects_existing_path_and_writes_parseable_jsonl() {
        let scope = TestScope::new("private-jsonl");
        let path = scope.path("events.jsonl");
        let file = open_new_private(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            open_new_private(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );

        let mut sink = Sink {
            file,
            deadline: Instant::now() + Duration::from_secs(30),
            bytes: 0,
            records: 0,
        };
        assert!(sink.write(r#"{"event":"local_sink","value":7}"#));
        drop(sink);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(contents.trim_end()).unwrap();
        assert_eq!(parsed["event"], "local_sink");
        assert_eq!(parsed["value"], 7);
    }
}
