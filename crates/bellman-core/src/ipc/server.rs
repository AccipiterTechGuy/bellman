//! The IPC server: one `UnixListener`, per-client reader/writer threads, and
//! the client registry the fire path sends through.
//!
//! Sending is a **bounded nonblocking attempt**: the fire producer and the
//! publication pump hand a frame to [`IpcHandle::send`], which is a
//! `try_send` into a per-client bounded queue — a slow or wedged socket
//! fails the attempt instantly and never queues fire records behind work.
//! Confirmation is never the write: it is the first valid reply accepted by
//! the shared ingest path, exactly like the file transport's pickup.

use super::{
    set_advertised, CLAIM_SCHEMA_V1, LOCK_FILE_NAME, OUT_QUEUE_DEPTH,
};
use crate::reply::quarantine::fnv1a64_hex;
use crate::reply::{gate, IngestOutcome, ReplyDocument, ReplyEngine, ReplyRejection};
use crate::reply::MAX_REPLY_FILE_BYTES;
use crate::store::{Store, Timer, TimerId, TransportMode, TransportProjection};
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Bounded write patience for one client frame — a wedged peer costs its
/// own writer thread at most this, never the fire path (which only
/// `try_send`s into the bounded queue).
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// What one bounded send attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// At least one client holding the timer accepted the frame.
    Queued,
    /// No live client accepted it (none connected, or every channel was
    /// full/closed). For `auto` runs this is the unconfirmed-IPC-failure
    /// that may fall back to files; for explicit `ipc` runs the pump retries.
    NoClient,
}

/// The shareable send-side handle: the fire path (transport selection,
/// immediate attempt) and the publication pump (bounded retries) both go
/// through this. Cheap to clone; the registry is shared.
#[derive(Clone)]
pub struct IpcHandle {
    socket_path: Arc<PathBuf>,
    registry: Arc<Mutex<Registry>>,
}

#[derive(Default)]
struct Registry {
    clients: HashMap<TimerId, Vec<ClientEntry>>,
    next_id: u64,
}

struct ClientEntry {
    id: u64,
    out: SyncSender<Vec<u8>>,
}

impl IpcHandle {
    /// A handle with its own registry, not (yet) bound to a server. The
    /// engine holds this so transport selection and publication can send;
    /// [`IpcServer::spawn`] drives the accept side on the same registry.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path: Arc::new(socket_path),
            registry: Arc::new(Mutex::new(Registry::default())),
        }
    }

    /// The socket path clients connect to (advertised in `timer.json` and
    /// IPC fire messages).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// True when at least one client currently holds this timer — the
    /// `auto` transport selection test at fire time.
    pub fn has_client(&self, timer_id: &TimerId) -> bool {
        let reg = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        reg.clients
            .get(timer_id)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Bounded nonblocking send to every client holding `timer_id`
    /// (consumers deduplicate by `run_id`). Instant by construction.
    pub fn send(&self, timer_id: &TimerId, frame: &[u8]) -> SendOutcome {
        let mut reg = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        let Some(list) = reg.clients.get_mut(timer_id) else {
            return SendOutcome::NoClient;
        };
        let mut any = false;
        list.retain(|c| match c.out.try_send(frame.to_vec()) {
            Ok(()) => {
                any = true;
                true
            }
            // A full queue is a wedged client: this attempt fails for it;
            // its own writer thread keeps draining (or dies and prunes it).
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
        if list.is_empty() {
            reg.clients.remove(timer_id);
        }
        if any {
            SendOutcome::Queued
        } else {
            SendOutcome::NoClient
        }
    }

    fn register(&self, timer_id: TimerId) -> (u64, Receiver<Vec<u8>>) {
        let (tx, rx) = sync_channel(OUT_QUEUE_DEPTH);
        let mut reg = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        reg.next_id = reg.next_id.wrapping_add(1);
        let id = reg.next_id;
        reg.clients
            .entry(timer_id)
            .or_default()
            .push(ClientEntry { id, out: tx });
        (id, rx)
    }

    fn unregister(&self, timer_id: &TimerId, id: u64) {
        let mut reg = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(list) = reg.clients.get_mut(timer_id) {
            list.retain(|c| c.id != id);
            if list.is_empty() {
                reg.clients.remove(timer_id);
            }
        }
    }
}

/// Everything one client connection needs (shared, cheap to clone into the
/// per-connection thread).
struct ClientCtx {
    data_dir: PathBuf,
    db_path: PathBuf,
    engine: ReplyEngine,
    handle: IpcHandle,
}

/// Server configuration. The socket path is explicit so tests bind inside
/// their temp dir; production passes [`super::default_socket_path`].
pub struct IpcConfig {
    pub socket_path: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    /// The one reply engine — the same instance the file watcher uses, so
    /// both adapters share ingest, deadlines and status projection.
    pub engine: ReplyEngine,
    /// The send-side handle (already shared with the engine/dispatcher).
    pub handle: IpcHandle,
}

/// A running IPC server. Holds the server-instance lock for its lifetime;
/// drop stops the accept thread and removes the socket file.
pub struct IpcServer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    _lock: gate::GateGuard,
}

impl IpcServer {
    /// Bind the socket and start accepting. Unix only (the Windows named
    /// pipe with a current-user ACL is specified but not implemented here).
    #[cfg(unix)]
    pub fn spawn(cfg: IpcConfig) -> io::Result<Self> {
        use std::os::unix::net::UnixListener;

        let dir = cfg
            .socket_path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?
            .to_path_buf();
        std::fs::create_dir_all(&dir)?;
        private_dir(&dir)?;

        // Hold the server-instance lock BEFORE bind: at most one live
        // server per socket path, across processes.
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = match gate::try_acquire_file(&lock_path)? {
            Some(g) => g,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "another Bellman IPC server holds {}",
                        lock_path.display()
                    ),
                ))
            }
        };

        prepare_socket_path(&cfg.socket_path)?;
        let listener = UnixListener::bind(&cfg.socket_path)?;
        private_socket(&cfg.socket_path)?;
        listener.set_nonblocking(true)?;
        set_advertised(cfg.socket_path.clone());

        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(ClientCtx {
            data_dir: cfg.data_dir,
            db_path: cfg.db_path,
            engine: cfg.engine,
            handle: cfg.handle,
        });
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("bellman-ipc".into())
            .spawn(move || accept_loop(listener, ctx, thread_stop))
            .map_err(|e| io::Error::other(format!("spawn ipc accept thread: {e}")))?;

        Ok(Self {
            stop,
            thread: Some(thread),
            socket_path: cfg.socket_path,
            _lock: lock,
        })
    }

    /// Non-Unix placeholder: the Windows named pipe (current-user ACL) is
    /// specified by the card but not implemented in this build.
    #[cfg(not(unix))]
    pub fn spawn(cfg: IpcConfig) -> io::Result<Self> {
        let _ = cfg;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "bellman IPC transport: unix socket only in this build (windows named pipe not implemented)",
        ))
    }

    /// The socket path this server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Stop accepting and remove the socket file.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    ctx: Arc<ClientCtx>,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let ctx = Arc::clone(&ctx);
                let _ = std::thread::Builder::new()
                    .name("bellman-ipc-client".into())
                    .spawn(move || handle_client(stream, ctx));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("bellman: ipc accept: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Directory 0700 — same trust boundary as the file protocol (the OS gates
/// same-user processes, nobody else lists or connects).
#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Socket 0600 — owner-only connect.
#[cfg(unix)]
fn private_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Stale-path discipline: remove a pre-existing path only when `lstat`
/// proves it is a socket owned by the current user AND a connection attempt
/// proves no live server answers. A non-socket or a symlink at the
/// configured path is never unlinked.
#[cfg(unix)]
fn prepare_socket_path(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::os::unix::net::UnixStream;

    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() || !ft.is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to remove non-socket at configured socket path {}",
                path.display()
            ),
        ));
    }
    if meta.uid() != super::current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket at {} is owned by another user", path.display()),
        ));
    }
    // An owned socket: live server, or genuinely stale?
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("a live server already answers at {}", path.display()),
        ));
    }
    std::fs::remove_file(path)
}

/// The claim frame: the first (and only pre-registration) frame a client
/// sends. Validated like a file reply — `app_name` must match the timer's
/// explicit integration owner; there is no first-acker ownership over IPC.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClaimDoc {
    schema: Option<String>,
    app_name: Option<String>,
    timer_id: Option<Uuid>,
}

/// Read one newline-delimited frame, enforcing R12 before parsing: at most
/// [`MAX_REPLY_FILE_BYTES`] is buffered; more without a newline is
/// `InvalidData` (the peer is disconnected, the prefix never retained).
/// Returns `Ok(None)` on a clean EOF.
#[cfg(unix)]
fn read_frame(reader: &mut BufReader<std::os::unix::net::UnixStream>, buf: &mut Vec<u8>) -> io::Result<Option<()>> {
    buf.clear();
    let n = {
        let mut limited = (&mut *reader).take(MAX_REPLY_FILE_BYTES + 1);
        limited.read_until(b'\n', buf)?
    };
    if n == 0 {
        return Ok(None);
    }
    if !buf.ends_with(b"\n") {
        if buf.len() as u64 > MAX_REPLY_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame exceeds the 64 KB reply cap",
            ));
        }
        // EOF mid-frame: treat as a closed connection.
        return Ok(None);
    }
    Ok(Some(()))
}

/// The frame's payload bytes (newline stripped) — what gets parsed and
/// digest-fed to the shared ingest, exactly the file transport's "reply
/// bytes".
fn frame_body(buf: &[u8]) -> &[u8] {
    buf.strip_suffix(b"\n").unwrap_or(buf)
}

#[cfg(unix)]
fn write_line(stream: &mut std::os::unix::net::UnixStream, v: &serde_json::Value) {
    let mut bytes = serde_json::to_vec(v).unwrap_or_default();
    bytes.push(b'\n');
    let _ = stream.write_all(&bytes);
}

#[cfg(unix)]
fn claim_error(stream: &mut std::os::unix::net::UnixStream, error: &str) {
    write_line(
        stream,
        &serde_json::json!({
            "schema": CLAIM_SCHEMA_V1,
            "ok": false,
            "error": error,
        }),
    );
}

#[cfg(unix)]
fn open_store(db_path: &Path) -> io::Result<Store> {
    Store::open_with(
        db_path,
        crate::store::OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .map_err(|e| io::Error::other(format!("ipc client store open: {e}")))
}

/// One client connection: claim → register → replay → reply loop. Replies
/// go through the ONE ingest function under the R10 per-timer gate — the
/// same function, gate and post-commit projection the file watcher uses.
#[cfg(unix)]
fn handle_client(mut stream: std::os::unix::net::UnixStream, ctx: Arc<ClientCtx>) {
    let mut reader = match stream.try_clone() {
        Ok(s) => BufReader::new(s),
        Err(e) => {
            eprintln!("bellman: ipc client clone: {e}");
            return;
        }
    };
    let mut buf = Vec::new();

    // ── Claim phase ─────────────────────────────────────────────────────
    match read_frame(&mut reader, &mut buf) {
        Ok(Some(())) => {}
        _ => return,
    }
    let claim: ClaimDoc = match serde_json::from_slice(frame_body(&buf)) {
        Ok(c) => c,
        Err(_) => {
            claim_error(&mut stream, "invalid_claim");
            return;
        }
    };
    if claim.schema.as_deref() != Some(CLAIM_SCHEMA_V1) {
        claim_error(&mut stream, "bad_schema");
        return;
    }
    let Some(timer_id) = claim.timer_id else {
        claim_error(&mut stream, "missing_timer_id");
        return;
    };
    let Some(app_name) = claim.app_name.filter(|a| !a.is_empty()) else {
        claim_error(&mut stream, "missing_app_name");
        return;
    };
    let store = match open_store(&ctx.db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bellman: ipc client: {e}");
            claim_error(&mut stream, "internal");
            return;
        }
    };
    let timer = match store.get_timer(timer_id) {
        Ok(Some(t)) => t,
        _ => {
            claim_error(&mut stream, "unknown_timer");
            return;
        }
    };
    let owner = store.get_timer_owner(timer_id).ok().flatten();
    if owner.as_deref() != Some(app_name.as_str()) {
        // One rule on both paths: a process naming someone else's timer is
        // rejected exactly like a wrong-`app_name` file reply.
        if let Err(e) =
            ctx.engine
                .log_rejection(&store, &timer, None, ReplyRejection::WrongAppName.as_str())
        {
            eprintln!("bellman: ipc claim rejection log: {e}");
        }
        claim_error(&mut stream, ReplyRejection::WrongAppName.as_str());
        return;
    }

    // ── Registered ──────────────────────────────────────────────────────
    let (client_id, rx) = ctx.handle.register(timer_id);
    write_line(
        &mut stream,
        &serde_json::json!({
            "schema": CLAIM_SCHEMA_V1,
            "ok": true,
            "timer_id": timer_id,
            "app_name": app_name,
        }),
    );
    {
        let handle = ctx.handle.clone();
        let _ = std::thread::Builder::new()
            .name("bellman-ipc-writer".into())
            .spawn(move || {
                writer_loop(stream, rx);
                handle.unregister(&timer_id, client_id);
            });
    }

    // A client claim triggers one replay for the still-current explicit-IPC
    // run (post-`no_ack` sends stopped, but a late claim gets one replay;
    // confirmation revises `no_ack` exactly like late file pickup).
    replay_current_ipc_run(&store, &ctx.handle, &timer);

    // ── Reply loop ──────────────────────────────────────────────────────
    loop {
        match read_frame(&mut reader, &mut buf) {
            Ok(Some(())) => handle_reply_frame(&ctx, &store, &timer, frame_body(&buf)),
            Ok(None) => break,
            Err(e) => {
                if e.kind() == io::ErrorKind::InvalidData {
                    eprintln!("bellman: ipc client oversize frame — disconnected (64 KB cap)");
                }
                break;
            }
        }
    }
    ctx.handle.unregister(&timer_id, client_id);
}

/// One replay of the durable payload for the timer's current run when that
/// run selected IPC and is still unconfirmed. The payload comes from the
/// transport projection (durable across restarts), so a replay never
/// re-mints identity or timing fields.
#[cfg(unix)]
fn replay_current_ipc_run(store: &Store, handle: &IpcHandle, timer: &Timer) {
    if timer.transport != TransportMode::Ipc {
        return; // only explicit-ipc runs replay on claim
    }
    let Ok(Some(claim)) = crate::reply::current_claim(store, timer.id) else {
        return;
    };
    let Ok(Some(row)) = store.get_run_state(claim.run_id) else {
        return;
    };
    if row.selected_transport.as_deref() != Some(crate::store::TRANSPORT_IPC) {
        return;
    }
    // Confirmed or app-closed runs are settled — nothing to replay.
    if crate::reply::publication::has_pickup_signal(&row) || row.is_app_authored_terminal() {
        return;
    }
    if let Ok(Some(proj)) = store.transport_projection(claim.run_id) {
        if proj.kind == TransportProjection::KIND_IPC {
            handle.send(&timer.id, proj.payload.as_bytes());
        }
    }
}

/// One reply frame from a claimed client — the IPC adapter's whole job ends
/// at "here is a parsed reply"; validation, transitions, outbox rows and
/// `status.json` folding all live in the shared ingest.
#[cfg(unix)]
fn handle_reply_frame(ctx: &ClientCtx, store: &Store, timer: &Timer, bytes: &[u8]) {
    let digest = fnv1a64_hex(bytes);
    let doc: ReplyDocument = match serde_json::from_slice(bytes) {
        Ok(d) => d,
        Err(_) => {
            // Worst case for a hostile reply is one bad log line (R9).
            if let Err(e) = ctx.engine.log_rejection(store, timer, None, "invalid JSON") {
                eprintln!("bellman: ipc rejection log: {e}");
            }
            return;
        }
    };
    // The R10 gate serializes this ingest against the file transport and
    // the fire transaction — the engine never locks, its transports do.
    let _gate = match gate::acquire(&ctx.data_dir, timer.id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bellman: ipc gate: {e}");
            return;
        }
    };
    match ctx.engine.ingest(
        store,
        timer,
        &doc,
        &digest,
        Utc::now(),
        Instant::now(),
    ) {
        Ok(IngestOutcome::Applied) => {
            if let Some(run_id) = doc.run_id {
                if let Err(e) = ctx.engine.project_status(store, timer, &run_id) {
                    eprintln!("bellman: ipc status projection: {e}");
                }
            }
        }
        Ok(IngestOutcome::Rejected(reason)) => {
            if let Err(e) = ctx.engine.log_rejection(store, timer, doc.run_id, reason.as_str())
            {
                eprintln!("bellman: ipc rejection log: {e}");
            }
        }
        // Duplicate: nothing changed. Superseded: logged by the engine;
        // there is no stale FILE for this transport to remove.
        Ok(IngestOutcome::Duplicate) | Ok(IngestOutcome::Superseded) => {}
        Err(e) => eprintln!("bellman: ipc ingest: {e}"),
    }
}

/// Drain the bounded per-client queue onto the socket. A write failure (or
/// the send timeout on a wedged peer) ends the loop; the client is then
/// unregistered so later sends/pumps see `NoClient` immediately.
#[cfg(unix)]
fn writer_loop(mut stream: std::os::unix::net::UnixStream, rx: Receiver<Vec<u8>>) {
    use std::os::unix::io::AsFd;
    let _ = rustix::net::sockopt::set_socket_timeout(
        stream.as_fd(),
        rustix::net::sockopt::Timeout::Send,
        Some(WRITE_TIMEOUT),
    );
    for frame in rx {
        if stream.write_all(&frame).and_then(|_| stream.write_all(b"\n")).is_err() {
            break;
        }
    }
}
