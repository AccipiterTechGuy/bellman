//! The IPC server: one listener for all of Bellman (Unix socket; named
//! pipe with a current-user ACL on Windows), per-client reader/writer
//! threads, and the client registry the fire path sends through.
//!
//! Sending is a **bounded nonblocking attempt**: the fire producer and the
//! publication pump hand a frame to [`IpcHandle::send`], which is a
//! `try_send` into a per-client bounded queue — a slow or wedged peer fails
//! the attempt instantly and never queues fire records behind work.
//! Confirmation is never the write: it is the first valid reply accepted by
//! the shared ingest path, exactly like the file transport's pickup.

use super::{set_advertised, CLAIM_SCHEMA_V1, OUT_QUEUE_DEPTH};
#[cfg(unix)]
use super::LOCK_FILE_NAME;
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
#[cfg(unix)]
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

#[cfg(not(any(unix, windows)))]
compile_error!("ipc::server supports only unix and windows targets");

/// Bounded write patience for one client frame — a wedged peer costs its
/// own writer thread at most this, never the fire path (which only
/// `try_send`s into the bounded queue). Unix sets SO_SNDTIMEO; on Windows
/// the bounded queue alone bounds the fire path and a wedged reader's
/// writer thread parks on the pipe, which is equally isolated.
#[cfg(unix)]
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
    /// True only while a server is actually bound and accepting. Transport
    /// selection tests THIS, not the handle's existence: a handle without a
    /// live server (failed bind, a stopped server) must resolve every
    /// firing to the file transport — the fallback that already works.
    live: Arc<AtomicBool>,
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
            live: Arc::new(AtomicBool::new(false)),
        }
    }

    /// True while a server is bound and accepting on this registry — the
    /// transport-selection test (`ipc`/`auto` degrade to files otherwise).
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    pub(crate) fn set_live(&self, live: bool) {
        self.live.store(live, Ordering::SeqCst);
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
/// drop stops the accept thread (and removes the socket file on Unix).
pub struct IpcServer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    handle: IpcHandle,
    #[cfg(unix)]
    _lock: gate::GateGuard,
    #[cfg(windows)]
    _win_guard: win_imp::WinGuard,
}

impl IpcServer {
    /// Bind the socket and start accepting (Unix).
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
        let handle = cfg.handle;
        let ctx = Arc::new(ClientCtx {
            data_dir: cfg.data_dir,
            db_path: cfg.db_path,
            engine: cfg.engine,
            handle: handle.clone(),
        });
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("bellman-ipc".into())
            .spawn(move || accept_loop(listener, ctx, thread_stop))
            .map_err(|e| io::Error::other(format!("spawn ipc accept thread: {e}")))?;
        handle.set_live(true);

        Ok(Self {
            stop,
            thread: Some(thread),
            socket_path: cfg.socket_path,
            handle,
            _lock: lock,
        })
    }

    /// Start the named-pipe server (Windows): `\\.\pipe\bellman-<user>`
    /// with an ACL restricted to the current user.
    #[cfg(windows)]
    pub fn spawn(cfg: IpcConfig) -> io::Result<Self> {
        let pipe = cfg.socket_path.to_string_lossy().into_owned();
        let win_guard = win_imp::WinGuard::acquire(&pipe)?;
        let security = win_imp::PipeSecurity::for_current_user()?;
        set_advertised(cfg.socket_path.clone());

        let stop = Arc::new(AtomicBool::new(false));
        let handle = cfg.handle;
        let ctx = Arc::new(ClientCtx {
            data_dir: cfg.data_dir,
            db_path: cfg.db_path,
            engine: cfg.engine,
            handle: handle.clone(),
        });
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("bellman-ipc".into())
            .spawn(move || {
                if let Err(e) = win_imp::accept_loop(&pipe, &security, ctx, thread_stop) {
                    eprintln!("bellman: ipc accept loop: {e}");
                }
            })
            .map_err(|e| io::Error::other(format!("spawn ipc accept thread: {e}")))?;
        handle.set_live(true);

        Ok(Self {
            stop,
            thread: Some(thread),
            socket_path: cfg.socket_path,
            handle,
            _win_guard: win_guard,
        })
    }

    /// The socket path this server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Stop accepting (and remove the socket file on Unix).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Windows: the accept loop is parked in a blocking ConnectNamedPipe;
        // a throwaway self-connect wakes it so the join below returns.
        #[cfg(windows)]
        win_imp::wake_listener(&self.socket_path);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // No live server anymore: later firings resolve to files.
        self.handle.set_live(false);
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ── Platform-neutral client protocol ─────────────────────────────────────

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
fn read_frame<R: Read>(reader: &mut BufReader<R>, buf: &mut Vec<u8>) -> io::Result<Option<()>> {
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

fn write_line<W: Write>(writer: &mut W, v: &serde_json::Value) {
    let mut bytes = serde_json::to_vec(v).unwrap_or_default();
    bytes.push(b'\n');
    let _ = writer.write_all(&bytes);
}

fn claim_error<W: Write>(writer: &mut W, error: &str) {
    write_line(
        writer,
        &serde_json::json!({
            "schema": CLAIM_SCHEMA_V1,
            "ok": false,
            "error": error,
        }),
    );
}

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
/// Platform-neutral: the transports only differ in how they split a
/// connection into a reader and a writer.
fn serve_client<R: Read, W: Write + Send + 'static>(
    mut reader: BufReader<R>,
    mut writer: W,
    ctx: Arc<ClientCtx>,
) {
    let mut buf = Vec::new();

    // ── Claim phase ─────────────────────────────────────────────────────
    match read_frame(&mut reader, &mut buf) {
        Ok(Some(())) => {}
        _ => return,
    }
    let claim: ClaimDoc = match serde_json::from_slice(frame_body(&buf)) {
        Ok(c) => c,
        Err(_) => {
            claim_error(&mut writer, "invalid_claim");
            return;
        }
    };
    if claim.schema.as_deref() != Some(CLAIM_SCHEMA_V1) {
        claim_error(&mut writer, "bad_schema");
        return;
    }
    let Some(timer_id) = claim.timer_id else {
        claim_error(&mut writer, "missing_timer_id");
        return;
    };
    let Some(app_name) = claim.app_name.filter(|a| !a.is_empty()) else {
        claim_error(&mut writer, "missing_app_name");
        return;
    };
    let store = match open_store(&ctx.db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bellman: ipc client: {e}");
            claim_error(&mut writer, "internal");
            return;
        }
    };
    let timer = match store.get_timer(timer_id) {
        Ok(Some(t)) => t,
        _ => {
            claim_error(&mut writer, "unknown_timer");
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
        claim_error(&mut writer, ReplyRejection::WrongAppName.as_str());
        return;
    }

    // ── Registered ──────────────────────────────────────────────────────
    let (client_id, rx) = ctx.handle.register(timer_id);
    write_line(
        &mut writer,
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
                writer_loop(writer, rx);
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

/// Drain the bounded per-client queue onto the connection. A write failure
/// ends the loop; the client is then unregistered so later sends/pumps see
/// `NoClient` immediately.
fn writer_loop<W: Write>(mut writer: W, rx: Receiver<Vec<u8>>) {
    for frame in rx {
        if writer.write_all(&frame).and_then(|_| writer.write_all(b"\n")).is_err() {
            break;
        }
    }
}

// ── Unix: one socket in a private directory ──────────────────────────────

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

#[cfg(unix)]
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    ctx: Arc<ClientCtx>,
    stop: Arc<AtomicBool>,
) {
    use std::os::unix::io::AsFd;

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // The writer half gets the bounded send timeout (a wedged
                // peer costs only its own writer thread).
                let _ = rustix::net::sockopt::set_socket_timeout(
                    stream.as_fd(),
                    rustix::net::sockopt::Timeout::Send,
                    Some(WRITE_TIMEOUT),
                );
                let reader = match stream.try_clone() {
                    Ok(s) => BufReader::new(s),
                    Err(e) => {
                        eprintln!("bellman: ipc client clone: {e}");
                        continue;
                    }
                };
                let ctx = Arc::clone(&ctx);
                let _ = std::thread::Builder::new()
                    .name("bellman-ipc-client".into())
                    .spawn(move || serve_client(reader, stream, ctx));
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

// ── Windows: one named pipe with a current-user ACL ─────────────────────

/// The Windows transport: `\\.\pipe\bellman-<user>` (one pipe for all of
/// Bellman), each instance created with an SDDL allowing exactly the
/// current user — the same same-user trust boundary as the 0600 socket.
/// The server-instance lock is a named mutex; a second server on the pipe
/// name is refused, exactly like the Unix flock.
#[cfg(windows)]
mod win_imp {
    use super::*;
    use std::os::windows::io::FromRawHandle;
    use std::time::Duration;
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, BOOL, ERROR_ALREADY_EXISTS,
        ERROR_PIPE_CONNECTED, HANDLE, HLOCAL,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TokenUser, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn hr(code: u32) -> HRESULT {
        HRESULT::from_win32(code)
    }

    /// The server-instance lock: a named mutex held for the server's
    /// lifetime (a second server on the same pipe name is refused).
    pub(super) struct WinGuard {
        mutex: usize, // raw HANDLE, kept as usize so the guard stays Send
    }

    impl WinGuard {
        pub(super) fn acquire(pipe: &str) -> io::Result<Self> {
            let name = format!(
                "Local\\bellman-ipc-{}",
                pipe.chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect::<String>()
            );
            let name_w = wide(&name);
            unsafe {
                let mutex = CreateMutexW(None, BOOL(1), PCWSTR(name_w.as_ptr()))
                    .map_err(|e| io::Error::other(format!("CreateMutexW: {e}")))?;
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let _ = CloseHandle(mutex);
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "another Bellman IPC server holds the instance mutex",
                    ));
                }
                Ok(Self {
                    mutex: mutex.0 as usize,
                })
            }
        }
    }

    impl Drop for WinGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(HANDLE(self.mutex as *mut _));
            }
        }
    }

    /// Security attributes whose DACL allows exactly the current user —
    /// built from the user's own token SID as SDDL, never a hardcoded group.
    pub(super) struct PipeSecurity {
        sd: PSECURITY_DESCRIPTOR,
        sa: SECURITY_ATTRIBUTES,
    }

    // The descriptor memory is EXCLUSIVELY owned by this struct (allocated
    // by ConvertStringSecurityDescriptorToSecurityDescriptorW, freed in
    // Drop) and after construction it is only READ — passed as `*const
    // SECURITY_ATTRIBUTES` to CreateNamedPipeW. Moving it onto the accept
    // thread (which then owns it for the thread's whole lifetime) aliases
    // nothing, so Send is sound here.
    unsafe impl Send for PipeSecurity {}

    impl PipeSecurity {
        pub(super) fn for_current_user() -> io::Result<Self> {
            unsafe {
                let mut token = HANDLE::default();
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
                    .map_err(|e| io::Error::other(format!("OpenProcessToken: {e}")))?;
                let mut len = 0u32;
                // First call sizes the buffer (ERROR_INSUFFICIENT_BUFFER).
                let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
                let mut buf = vec![0u8; len as usize];
                let got = if len > 0 {
                    GetTokenInformation(
                        token,
                        TokenUser,
                        Some(buf.as_mut_ptr().cast()),
                        len,
                        &mut len,
                    )
                } else {
                    Err(windows::core::Error::from_win32())
                };
                let _ = CloseHandle(token);
                got.map_err(|e| io::Error::other(format!("GetTokenInformation: {e}")))?;
                let user = &*(buf.as_ptr() as *const TOKEN_USER);
                let mut sid_w = windows::core::PWSTR::null();
                ConvertSidToStringSidW(user.User.Sid, &mut sid_w)
                    .map_err(|e| io::Error::other(format!("ConvertSidToStringSidW: {e}")))?;
                let sid = sid_w
                    .to_string()
                    .map_err(|e| io::Error::other(format!("user sid utf16: {e}")))?;
                let _ = LocalFree(HLOCAL(sid_w.0.cast()));
                // SDDL: DACL-protected, GENERIC_ALL for exactly this user.
                let sddl_w = wide(&format!("D:P(A;;GA;;;{sid})"));
                let mut sd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(sddl_w.as_ptr()),
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )
                .map_err(|e| {
                    io::Error::other(format!("ConvertStringSecurityDescriptorToSecurityDescriptorW: {e}"))
                })?;
                Ok(Self {
                    sd,
                    sa: SECURITY_ATTRIBUTES {
                        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                        lpSecurityDescriptor: sd.0,
                        bInheritHandle: BOOL(0),
                    },
                })
            }
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(HLOCAL(self.sd.0));
            }
        }
    }

    /// Accept loop: one pipe instance per client (that is how named pipes
    /// multiplex — never a server per timer, but one instance per
    /// connection). Instances are SYNCHRONOUS (no FILE_FLAG_OVERLAPPED): the
    /// client threads then do blocking `std::fs::File` I/O on them, exactly
    /// like the Unix socket — std's blocking Read/Write on an *overlapped*
    /// handle can see STATUS_PENDING and abort the process, and two threads
    /// sharing one overlapped file object can satisfy each other's waits.
    /// The blocking `ConnectNamedPipe` is unblocked on shutdown by
    /// [`wake_listener`]'s throwaway self-connect.
    pub(super) fn accept_loop(
        pipe: &str,
        security: &PipeSecurity,
        ctx: Arc<ClientCtx>,
        stop: Arc<AtomicBool>,
    ) -> io::Result<()> {
        let pipe_w = wide(pipe);
        let mut first = true;
        unsafe {
            loop {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let mut open_mode = PIPE_ACCESS_DUPLEX;
                if first {
                    open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
                    first = false;
                }
                let hpipe = CreateNamedPipeW(
                    PCWSTR(pipe_w.as_ptr()),
                    open_mode,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    64,
                    MAX_REPLY_FILE_BYTES as u32 + 1,
                    MAX_REPLY_FILE_BYTES as u32 + 1,
                    0,
                    Some(&security.sa),
                );
                if hpipe.is_invalid() {
                    let e = windows::core::Error::from_win32();
                    return Err(io::Error::other(format!("CreateNamedPipeW: {e}")));
                }
                // Blocking wait for one client (a client that connected
                // between create and connect reports ERROR_PIPE_CONNECTED —
                // also ready).
                if let Err(e) = ConnectNamedPipe(hpipe, None) {
                    if e.code() != hr(ERROR_PIPE_CONNECTED.0) {
                        eprintln!("bellman: ipc ConnectNamedPipe: {e}");
                        let _ = CloseHandle(hpipe);
                        continue;
                    }
                }
                // Woken by the shutdown self-connect: leave quietly.
                if stop.load(Ordering::SeqCst) {
                    let _ = CloseHandle(hpipe);
                    return Ok(());
                }
                // Hand the instance to its client thread (usize hop: HANDLE
                // is not Send; the value is).
                let raw = hpipe.0 as usize;
                let ctx = Arc::clone(&ctx);
                let _ = std::thread::Builder::new()
                    .name("bellman-ipc-client".into())
                    .spawn(move || client_thread(raw, ctx));
            }
        }
    }

    /// Unblock the accept loop's `ConnectNamedPipe` for shutdown: open one
    /// throwaway client connection to the pipe. The connect waits briefly
    /// for a listening instance if the loop is mid-handoff, so retry a few
    /// times — the loop is never away from `ConnectNamedPipe` for long.
    pub(super) fn wake_listener(pipe_path: &Path) {
        for _ in 0..10 {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe_path)
            {
                Ok(throwaway) => {
                    drop(throwaway);
                    return;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// One pipe instance = one client: claim → replies, through the same
    /// platform-neutral protocol the Unix socket uses.
    fn client_thread(raw: usize, ctx: Arc<ClientCtx>) {
        let file = unsafe { std::fs::File::from_raw_handle(raw as *mut _) };
        let reader = match file.try_clone() {
            Ok(f) => BufReader::new(f),
            Err(e) => {
                eprintln!("bellman: ipc pipe clone: {e}");
                return; // file closes the instance handle
            }
        };
        serve_client(reader, file, ctx);
    }
}
