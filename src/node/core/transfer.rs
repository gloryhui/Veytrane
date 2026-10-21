//! Authenticated single-file streams. Disk work stays off the network/UI executors.
use crate::node::core::protocol::{
    self, Actor, Entry, ErrorCode, Frame, Message, TaskProtocol, WireTaskState,
};
use crate::node::core::task_model::{
    ProgressHint, TaskDiagnostic, TaskDirection, TaskErrorCode, TaskId, TaskRecord, TaskState,
    system_time_unix_ms,
};
use crate::node::core::task_store::TaskStore;
use crate::node::core::transfer_files as disk;
use crate::{
    error::{Error, Result},
    identity::NodeId,
    protocol::manifest::FileManifest,
    transfer::resume::ChunkBitmap,
};
use quinn::{Connection, RecvStream, SendStream};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const PAUSE_TIMEOUT: Duration = Duration::from_secs(5);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const WINDOW: usize = 4;
const MAX_RECEIVE_FILES: usize = 3;

#[cfg(test)]
type TestGate = Arc<
    Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
>;

#[derive(Clone)]
pub(crate) struct TransferService {
    unified_connections: Arc<Mutex<HashSet<usize>>>,
    resource_scopes: Arc<
        Mutex<HashMap<usize, watch::Receiver<crate::node::core::authorization::ResourcePolicy>>>,
    >,
    store: Arc<Mutex<TaskStore>>,
    receive_root: Arc<Mutex<PathBuf>>,
    active: Arc<Mutex<HashMap<TaskId, Active>>>,
    changed: watch::Sender<u64>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    fair_writes: crate::node::core::queue::FairWrites,
    upload_limit: crate::node::core::bandwidth::ByteLimit,
    download_limit: crate::node::core::bandwidth::ByteLimit,
    flow_peers: Arc<Mutex<HashSet<NodeId>>>,
    incremental_peers: Arc<Mutex<HashSet<NodeId>>>,
    previews: Arc<Mutex<HashMap<TaskId, crate::node::core::incremental::Pending>>>,
    preview_requests: Arc<Mutex<HashMap<NodeId, crate::node::core::incremental::Request>>>,
    comparisons: Arc<tokio::sync::Semaphore>,
    frame_budget: crate::node::core::frame_budget::FrameBudget,
    rate_origin: Instant,
    rates: Arc<Mutex<HashMap<TaskId, crate::node::core::queue::RateSampler>>>,
    sender_queue: Arc<Mutex<crate::node::core::queue::TaskQueue>>,
    recovery: Arc<Mutex<crate::node::core::auto_resume::Recovery>>,
    pub(crate) receive_space: crate::node::core::space_budget::SpaceBudget,
    measurements: Arc<Mutex<crate::node::core::task_details::Measurements>>,
    detail_selection: Arc<Mutex<Option<crate::node::core::task_details::Selection>>>,
    automatic_sends: Arc<Mutex<HashMap<TaskId, crate::node::core::auto_resume::Ticket>>>,
    pub(crate) activity: crate::node::core::activity::Activity,
    speed_peers: Arc<Mutex<HashMap<NodeId, crate::node::core::speed::SpeedPeer>>>,
    #[cfg(test)]
    drop_completion: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    pub(crate) first_chunk_gate: TestGate,
    #[cfg(test)]
    statistics_cleanup_gate: TestGate,
    #[cfg(test)]
    pub(crate) checkpoint_gate: TestGate,
    #[cfg(test)]
    admission_gate: TestGate,
    #[cfg(test)]
    source_cleanup_gate: TestGate,
    #[cfg(test)]
    resume_admission_gate: TestGate,
    #[cfg(test)]
    pub(crate) publication_error: Arc<Mutex<Option<std::io::ErrorKind>>>,
}
#[allow(dead_code)] // T010 consumes these safe domain fields in its task list.
pub(crate) struct TransferPresentation {
    pub tasks: Vec<TaskRecord>,
    pub events: Vec<crate::node::core::task_events::TaskEvent>,
    pub resync_required: bool,
    pub bytes_per_second: HashMap<TaskId, f64>,
    pub groups: HashMap<TaskId, crate::node::core::queue::GroupProgress>,
    pub queue: crate::node::core::queue::QueueMetrics,
}
struct ReceiveMode {
    automatic: bool,
    incremental: bool,
}
struct SendMode {
    automatic: bool,
    ticket: Option<crate::node::core::auto_resume::Ticket>,
}
struct Active {
    peer: NodeId,
    transport: usize,
    direction: TaskDirection,
    metadata: bool,
    pause: watch::Sender<bool>,
}
pub(crate) struct ScheduledSend {
    pub entry: crate::node::core::queue::QueueEntry,
    generation: u64,
    _slot: SendSlot,
}
struct SendSlot {
    _activity: crate::node::core::activity::FilePermit,
    service: TransferService,
    id: TaskId,
    generation: u64,
}
impl Drop for SendSlot {
    fn drop(&mut self) {
        self.service
            .sender_queue
            .lock()
            .unwrap()
            .finish_if(&self.id, self.generation);
        self.service.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
}
struct ActiveGuard {
    _activity: Option<crate::node::core::activity::FilePermit>,
    service: TransferService,
    id: TaskId,
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        {
            // Claim shares this lock order. Publish an idle task only after
            // this attempt's rates and measurements have been retired.
            let mut active = self.service.active.lock().unwrap();
            #[cfg(test)]
            if let Some((reached, release)) =
                self.service.statistics_cleanup_gate.lock().unwrap().take()
            {
                let _ = reached.send(());
                let _ = release.blocking_recv();
            }
            self.service.rates.lock().unwrap().remove(&self.id);
            self.service
                .measurements
                .lock()
                .unwrap()
                .finish(&self.id, Instant::now());
            active.remove(&self.id);
        }
        self.service
            .changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}
impl std::fmt::Debug for TransferService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferService").finish_non_exhaustive()
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| disk::failure("文件后台任务中断"))?
}

pub(crate) struct TransportModeGuard {
    service: TransferService,
    transport: usize,
}
impl Drop for TransportModeGuard {
    fn drop(&mut self) {
        self.service
            .unified_connections
            .lock()
            .unwrap()
            .remove(&self.transport);
        self.service
            .resource_scopes
            .lock()
            .unwrap()
            .remove(&self.transport);
    }
}

pub(crate) struct FileInlets {
    pub files: tokio::sync::mpsc::Receiver<crate::node::core::dispatch::BiStream>,
    pub speed: tokio::sync::mpsc::Receiver<crate::node::core::dispatch::BiStream>,
    pub resources: watch::Receiver<crate::node::core::authorization::ResourcePolicy>,
    files_open: bool,
    speed_open: bool,
}
impl FileInlets {
    pub(crate) fn new(
        files: tokio::sync::mpsc::Receiver<crate::node::core::dispatch::BiStream>,
        speed: tokio::sync::mpsc::Receiver<crate::node::core::dispatch::BiStream>,
        resources: watch::Receiver<crate::node::core::authorization::ResourcePolicy>,
    ) -> Self {
        Self {
            files,
            speed,
            resources,
            files_open: true,
            speed_open: true,
        }
    }
}
async fn next_incoming(
    connection: &Connection,
    incoming: &mut Option<FileInlets>,
) -> Option<(
    Option<crate::node::core::wire::StreamKind>,
    crate::node::core::dispatch::BiStream,
)> {
    let Some(inlets) = incoming else {
        return connection
            .accept_bi()
            .await
            .ok()
            .map(|(send, recv)| (None, crate::node::core::dispatch::BiStream { send, recv }));
    };
    loop {
        if !inlets.files_open && !inlets.speed_open {
            return None;
        }
        tokio::select! {
            _ = connection.closed() => return None,
            stream = inlets.files.recv(), if inlets.files_open => match stream {
                Some(stream) => return Some((Some(crate::node::core::wire::StreamKind::File), stream)),
                None => inlets.files_open = false,
            },
            stream = inlets.speed.recv(), if inlets.speed_open => match stream {
                Some(stream) => return Some((Some(crate::node::core::wire::StreamKind::Speed), stream)),
                None => inlets.speed_open = false,
            }
        }
    }
}

impl TransferService {
    pub(crate) fn set_unified_connection(
        &self,
        connection: &Connection,
        unified: bool,
        scope: Option<watch::Receiver<crate::node::core::authorization::ResourcePolicy>>,
    ) -> TransportModeGuard {
        let mut modes = self.unified_connections.lock().unwrap();
        if unified {
            modes.insert(connection.stable_id());
        } else {
            modes.remove(&connection.stable_id());
        }
        let mut scopes = self.resource_scopes.lock().unwrap();
        if let Some(scope) = scope {
            scopes.insert(connection.stable_id(), scope);
        } else {
            scopes.remove(&connection.stable_id());
        }
        TransportModeGuard {
            service: self.clone(),
            transport: connection.stable_id(),
        }
    }
    fn is_unified(&self, connection: &Connection) -> bool {
        self.unified_connections
            .lock()
            .unwrap()
            .contains(&connection.stable_id())
    }
    async fn open_file_stream(&self, connection: &Connection) -> Result<(SendStream, RecvStream)> {
        let stream = crate::node::core::dispatch::open_bi(
            connection,
            crate::node::core::wire::StreamKind::File,
            self.is_unified(connection),
        )
        .await?;
        Ok((stream.send, stream.recv))
    }
    pub async fn cancel_task(&self, id: TaskId) -> Result<()> {
        self.recovery.lock().unwrap().cancel(&id);
        let active = self.active.clone();
        let queue = self.sender_queue.clone();
        let task = id.clone();
        self.store(move |store| {
            store.cancel(&task).map_err(disk::local_error)?;
            let active = active.lock().unwrap();
            if let Some(attempt) = active.get(&task) {
                attempt.pause.send_replace(true);
            }
            let mut queue = queue.lock().unwrap();
            queue.remove_queued(&task);
            if !active.contains_key(&task)
                && let Some(generation) = queue.slot_generation(&task)
            {
                queue.finish_if(&task, generation);
            }
            Ok(())
        })
        .await?;
        self.wait_previous_attempt(&id).await
    }
    pub fn new(store: TaskStore, receive_root: PathBuf) -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            unified_connections: Default::default(),
            resource_scopes: Default::default(),
            store: Arc::new(Mutex::new(store)),
            receive_root: Arc::new(Mutex::new(receive_root)),
            active: Arc::new(Mutex::new(HashMap::new())),
            activity: crate::node::core::activity::Activity::new(changed.clone()),
            speed_peers: Arc::new(Mutex::new(HashMap::new())),
            changed,
            epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            fair_writes: crate::node::core::queue::FairWrites::default(),
            upload_limit: Default::default(),
            download_limit: Default::default(),
            flow_peers: Default::default(),
            incremental_peers: Default::default(),
            previews: Default::default(),
            preview_requests: Default::default(),
            comparisons: Arc::new(tokio::sync::Semaphore::new(2)),
            frame_budget: crate::node::core::frame_budget::FrameBudget::default(),
            rate_origin: Instant::now(),
            rates: Arc::new(Mutex::new(HashMap::new())),
            sender_queue: Arc::new(Mutex::new(crate::node::core::queue::TaskQueue::default())),
            recovery: Default::default(),
            receive_space: Default::default(),
            measurements: Default::default(),
            detail_selection: Default::default(),
            automatic_sends: Default::default(),
            #[cfg(test)]
            drop_completion: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            first_chunk_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            statistics_cleanup_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            checkpoint_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            admission_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            source_cleanup_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            resume_admission_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            publication_error: Arc::new(Mutex::new(None)),
        }
    }
    async fn store<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut TaskStore) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        let epoch = self.epoch.clone();
        let generation = epoch.load(std::sync::atomic::Ordering::Acquire);
        let result = blocking(move || {
            let mut store = store.lock().map_err(|_| disk::failure("任务库锁不可用"))?;
            if generation != epoch.load(std::sync::atomic::Ordering::Acquire) {
                return Err(disk::failure("旧传输会话已取消"));
            }
            work(&mut store)
        })
        .await;
        if result.is_ok() {
            self.changed
                .send_modify(|version| *version = version.wrapping_add(1));
        }
        result
    }
    async fn read_store<T: Send + 'static>(
        &self,
        work: impl FnOnce(&TaskStore) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        blocking(move || work(&*store.lock().map_err(|_| disk::failure("任务库锁不可用"))?)).await
    }
    pub fn set_auto_resume(&self, enabled: bool) {
        self.recovery.lock().unwrap().enable(enabled);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub fn cancel_peer_recovery(&self, peer: NodeId) -> bool {
        let pending = self.recovery.lock().unwrap().cancel_peer(peer);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        pending
    }
    pub fn network_lost(&self, peer: NodeId) {
        let active = self.active.lock().unwrap();
        let mut recovery = self.recovery.lock().unwrap();
        for (id, _) in active
            .iter()
            .filter(|(_, a)| a.peer == peer && *a.pause.borrow())
        {
            recovery.cancel(id);
        }
        recovery.lost(peer, Instant::now());
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub async fn settle_network_loss(&self, peer: NodeId) -> Result<()> {
        let tickets = self.recovery.lock().unwrap().network_tasks(peer);
        for ticket in tickets {
            self.wait_previous_attempt(&ticket.id).await?;
            let recovery = self.recovery.clone();
            self.store(move |store| {
                let mut recovery = recovery.lock().unwrap();
                if !recovery.same_attempt(&ticket) {
                    return Ok(());
                }
                let record = disk::bound_task(store, peer, &ticket.id)?;
                if record.state().needs_startup_recovery() && record.state() != TaskState::Pausing {
                    store
                        .transition(
                            &ticket.id,
                            TaskState::Interrupted,
                            Some(TaskDiagnostic::new(
                                TaskErrorCode::NetworkInterrupted,
                                false,
                            )),
                            system_time_unix_ms().map_err(disk::local_error)?,
                        )
                        .map_err(disk::local_error)?;
                } else if record.state() != TaskState::Interrupted
                    || !automatic_record_allowed(&record)
                {
                    recovery.cancel(&ticket.id);
                }
                Ok(())
            })
            .await?;
        }
        Ok(())
    }
    pub fn auto_due(
        &self,
        ready: impl FnMut(NodeId, TaskDirection) -> bool,
        busy: impl FnMut(NodeId) -> bool,
    ) -> (Vec<NodeId>, Vec<crate::node::core::auto_resume::Ticket>) {
        self.recovery
            .lock()
            .unwrap()
            .due(Instant::now(), ready, busy)
    }
    /// Recheck the original binding and current-run intention in the same store
    /// writer as queue admission. Old workers must release their slots first.
    pub async fn auto_recover(
        &self,
        connection: &Connection,
        ticket: crate::node::core::auto_resume::Ticket,
    ) -> Result<()> {
        let mut changed = self.changed.subscribe();
        let cancellation = async {
            loop {
                if ticket.cancelled() && !self.recovery.lock().unwrap().started(&ticket) {
                    return;
                }
                if changed.changed().await.is_err() {
                    return;
                }
            }
        };
        let work = async {
            self.wait_previous_attempt(&ticket.id).await?;
            let request_id = ticket.id.clone();
            let request_peer = ticket.peer;
            let ticket = ticket.clone();
            let recovery = self.recovery.clone();
            let automatic_sends = self.automatic_sends.clone();
            let queue = self.sender_queue.clone();
            let epoch = self.epoch.clone();
            let generation = epoch.load(std::sync::atomic::Ordering::Acquire);
            let direction = self
                .store(move |store| {
                    let mut recovery = recovery.lock().unwrap();
                    if !recovery.valid(&ticket) {
                        return Err(disk::failure("自动恢复已取消"));
                    }
                    let record = disk::bound_task(store, ticket.peer, &ticket.id)?;
                    if record.state() == TaskState::Completed || !automatic_record_allowed(&record)
                    {
                        recovery.cancel(&ticket.id);
                        return Err(disk::failure("任务需要手动继续"));
                    }
                    if record.state() != TaskState::Interrupted {
                        store
                            .transition(
                                &ticket.id,
                                TaskState::Interrupted,
                                Some(TaskDiagnostic::new(
                                    TaskErrorCode::NetworkInterrupted,
                                    false,
                                )),
                                system_time_unix_ms().map_err(disk::local_error)?,
                            )
                            .map_err(disk::local_error)?;
                    }
                    if record.direction() == TaskDirection::Send {
                        automatic_sends
                            .lock()
                            .unwrap()
                            .insert(ticket.id.clone(), ticket.clone());
                        Self::admit_locked(
                            store,
                            &queue,
                            ticket.peer,
                            vec![ticket.id.clone()],
                            &epoch,
                            generation,
                        )?;
                        recovery.admitted(&ticket.id);
                    } else {
                        disk::transition(store, &ticket.id, TaskState::Queued)?;
                    }
                    Ok(record.direction())
                })
                .await?;
            if direction == TaskDirection::Receive {
                self.request_resume(connection, request_peer, request_id, true)
                    .await?;
            }
            Ok(())
        };
        let deadline = tokio::time::sleep_until(
            ticket
                .deadline
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(300))
                .into(),
        );
        let (result, stopped) = tokio::select! {
            biased;
            _ = cancellation => (Ok(()), true),
            _ = deadline => {
                self.recovery.lock().unwrap().stop_ticket(&ticket);
                (Ok(()), true)
            },
            result = work => (result, false),
        };
        if stopped && ticket.direction == TaskDirection::Receive {
            let lookup = ticket.clone();
            let recovery = self.recovery.clone();
            let _ = self
                .store(move |store| {
                    if recovery.lock().unwrap().same_attempt(&lookup) {
                        let record = disk::bound_task(store, lookup.peer, &lookup.id)?;
                        if record.state() == TaskState::Queued {
                            disk::transition(store, &lookup.id, TaskState::Interrupted)?;
                        }
                    }
                    Ok(())
                })
                .await;
        }
        self.recovery.lock().unwrap().finished(&ticket);
        result
    }
    pub fn set_send_limit(&self, value: u8) -> Result<()> {
        self.sender_queue
            .lock()
            .unwrap()
            .set_limit(value)
            .map_err(disk::failure)?;
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        Ok(())
    }
    pub fn set_file_limits(&self, limits: crate::node::core::bandwidth::FileLimits) -> Result<()> {
        limits.validate().map_err(disk::failure)?;
        self.upload_limit.set_kib(limits.upload_kib);
        self.download_limit.set_kib(limits.download_kib);
        Ok(())
    }
    pub fn set_peer_incremental_support(&self, peer: NodeId, supported: bool) {
        let mut peers = self.incremental_peers.lock().unwrap();
        if supported {
            peers.insert(peer);
        } else {
            peers.remove(&peer);
        }
        if let Some(request) = self.preview_requests.lock().unwrap().remove(&peer) {
            request.cancellation.cancel();
        }
        self.previews
            .lock()
            .unwrap()
            .retain(|_, preview| preview.summary.peer != peer);
    }
    pub fn cancel_preview(&self, id: &TaskId) {
        self.preview_requests.lock().unwrap().retain(|_, request| {
            if &request.id == id {
                request.cancellation.cancel();
                false
            } else {
                true
            }
        });
        self.previews.lock().unwrap().remove(id);
    }
    pub fn begin_preview(&self, peer: NodeId, id: TaskId) -> Result<()> {
        let mut requests = self.preview_requests.lock().unwrap();
        requests.retain(|_, request| {
            if request.created.elapsed() >= crate::node::core::incremental::PREVIEW_LIFETIME {
                request.cancellation.cancel();
                false
            } else {
                true
            }
        });
        if requests.len() >= 8 && !requests.contains_key(&peer) {
            return Err(disk::failure("待预览设备数量已达上限"));
        }
        if let Some(old) = requests.insert(
            peer,
            crate::node::core::incremental::Request {
                id,
                created: Instant::now(),
                cancellation: crate::node::core::files::ScanCancellation::default(),
            },
        ) {
            old.cancellation.cancel();
        }
        self.previews
            .lock()
            .unwrap()
            .retain(|_, p| p.summary.peer != peer);
        Ok(())
    }
    #[cfg(test)]
    pub async fn preview_directory(
        &self,
        connection: &Connection,
        peer: NodeId,
        source: PathBuf,
    ) -> Result<crate::node::core::incremental::Preview> {
        let request = TaskId::generate();
        self.begin_preview(peer, request.clone())?;
        self.preview_directory_request(connection, peer, source, request)
            .await
    }
    pub async fn preview_directory_request(
        &self,
        connection: &Connection,
        peer: NodeId,
        source: PathBuf,
        request: TaskId,
    ) -> Result<crate::node::core::incremental::Preview> {
        let _activity = self.activity.file(peer)?;
        let _permit = self
            .comparisons
            .clone()
            .try_acquire_owned()
            .map_err(|_| disk::failure("目录比较繁忙，请稍后重试"))?;
        let epoch = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        let supported = self.incremental_peers.lock().unwrap().contains(&peer);
        struct CancelScan(crate::node::core::files::ScanCancellation);
        impl Drop for CancelScan {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let (cancellation, started) = {
            let requests = self.preview_requests.lock().unwrap();
            let active = requests
                .get(&peer)
                .filter(|r| r.id == request)
                .ok_or_else(|| disk::failure("目录预览已取消"))?;
            (active.cancellation.clone(), active.created)
        };
        let _scan_guard = CancelScan(cancellation.clone());
        let remote_cancel = cancellation.clone();
        let source_path = source.clone();
        let records = tokio::time::timeout(
            crate::node::core::incremental::PREVIEW_LIFETIME.saturating_sub(started.elapsed()),
            blocking(move || {
                let _permit = _permit;
                crate::node::core::files::scan_directory(peer, &source, &cancellation)
            }),
        )
        .await
        .map_err(|_| disk::failure("目录扫描超时，请选择较小目录"))??;
        let mut summary = crate::node::core::incremental::Preview {
            id: request.clone(),
            peer,
            source: source_path,
            added: 0,
            changed: 0,
            unchanged: 0,
            directories: 0,
            transfer_bytes: 0,
            saved_bytes: 0,
            supported,
        };
        for record in &records {
            remote_cancel.check()?;
            if epoch != self.epoch.load(std::sync::atomic::Ordering::Acquire)
                || started.elapsed() >= crate::node::core::incremental::PREVIEW_LIFETIME
            {
                return Err(disk::failure("目录预览已失效，请重新选择"));
            }
            let Some(details) = record.file_details() else {
                summary.directories += 1;
                continue;
            };
            let difference = if supported {
                let comparison = self.compare_remote(connection, record);
                tokio::pin!(comparison);
                let mut polling = tokio::time::interval(Duration::from_millis(100));
                loop {
                    tokio::select! {
                        result = &mut comparison => break result?,
                        _ = polling.tick() => { remote_cancel.check()?; if started.elapsed() >= crate::node::core::incremental::PREVIEW_LIFETIME { return Err(disk::failure("目录比较超时，请选择较小目录")); } }
                    }
                }
            } else {
                protocol::Difference::Added
            };
            match difference {
                protocol::Difference::Added => summary.added += 1,
                protocol::Difference::Changed => summary.changed += 1,
                protocol::Difference::Unchanged => summary.unchanged += 1,
            }
            if difference == protocol::Difference::Unchanged {
                summary.saved_bytes += details.manifest.total_len;
            } else {
                summary.transfer_bytes += details.manifest.total_len;
            }
        }
        if connection.close_reason().is_some()
            || epoch != self.epoch.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(disk::failure("目录预览连接已失效"));
        }
        let mut requests = self.preview_requests.lock().unwrap();
        if !requests.get(&peer).is_some_and(|r| r.id == request) {
            return Err(disk::failure("目录预览已取消或被新的预览替代"));
        }
        requests.get_mut(&peer).unwrap().created = Instant::now();
        let mut previews = self.previews.lock().unwrap();
        previews.retain(|_, p| {
            p.created.elapsed() < crate::node::core::incremental::PREVIEW_LIFETIME
                && p.summary.peer != peer
        });
        if previews.len() >= 8 {
            return Err(disk::failure("待确认目录数量已达上限"));
        }
        previews.insert(
            summary.id.clone(),
            crate::node::core::incremental::Pending {
                summary: summary.clone(),
                records,
                connection: connection.stable_id(),
                epoch,
                created: Instant::now(),
            },
        );
        Ok(summary)
    }
    async fn compare_remote(
        &self,
        connection: &Connection,
        record: &TaskRecord,
    ) -> Result<protocol::Difference> {
        let details = record.file_details().unwrap();
        tokio::time::timeout(
            crate::node::core::incremental::COMPARE_TIMEOUT + Duration::from_secs(5),
            async {
                let (mut send, mut recv) = self
                    .open_file_stream(connection)
                    .await
                    .map_err(disk::local_error)?;
                protocol::write(
                    &mut send,
                    &Frame {
                        request_id: 1,
                        message: Message::CompareFile {
                            task_id: record.task_id().clone(),
                            relative_path: details.relative_path.clone(),
                            manifest: Box::new(details.manifest.clone()),
                        },
                    },
                )
                .await?;
                send.finish().map_err(disk::local_error)?;
                let reply = self.frame_budget.read(&mut recv, None).await?;
                match reply.frame {
                    Frame {
                        request_id: 1,
                        message:
                            Message::ComparedFile {
                                task_id,
                                root_hash,
                                difference,
                            },
                    } if task_id == *record.task_id()
                        && root_hash == details.manifest.root_hash =>
                    {
                        Ok(difference)
                    }
                    Frame {
                        request_id: 1,
                        message: Message::Error { task_id, code },
                    } if task_id == *record.task_id() => Err(remote_error(code)),
                    _ => Err(disk::failure("目录比较响应身份不符")),
                }
            },
        )
        .await
        .map_err(|_| disk::failure("目录比较响应超时"))?
    }
    pub async fn confirm_directory(
        &self,
        connection: &Connection,
        peer: NodeId,
        id: TaskId,
    ) -> Result<Vec<TaskId>> {
        let _activity = self.activity.file(peer)?;
        self.preview_requests
            .lock()
            .unwrap()
            .retain(|_, request| request.id != id);
        let pending = self
            .previews
            .lock()
            .unwrap()
            .remove(&id)
            .ok_or_else(|| disk::failure("目录预览已失效，请重新选择"))?;
        if pending.summary.peer != peer
            || pending.connection != connection.stable_id()
            || connection.close_reason().is_some()
            || pending.epoch != self.epoch.load(std::sync::atomic::Ordering::Acquire)
            || pending.created.elapsed() >= crate::node::core::incremental::PREVIEW_LIFETIME
        {
            return Err(disk::failure("目录预览已失效，请重新选择"));
        }
        let epoch = self.epoch.clone();
        let queue = self.sender_queue.clone();
        let generation = pending.epoch;
        self.store(move |store| {
            if generation != epoch.load(std::sync::atomic::Ordering::Acquire) {
                return Err(disk::failure("旧传输会话已取消"));
            }
            let mut records = pending.records;
            if pending.summary.supported {
                for record in &mut records {
                    if record.file_details().is_some() {
                        record.enable_incremental().map_err(disk::local_error)?;
                    }
                }
            }
            let ids = store.create_selection(records).map_err(disk::local_error)?;
            for id in &ids {
                disk::transition(store, id, TaskState::Queued)?;
            }
            Self::admit_locked(store, &queue, peer, ids.clone(), &epoch, generation)?;
            Ok(ids)
        })
        .await
    }
    pub fn set_peer_flow_support(&self, peer: NodeId, supported: bool) {
        let mut peers = self.flow_peers.lock().unwrap();
        if supported {
            peers.insert(peer);
        } else {
            peers.remove(&peer);
        }
    }
    fn check_flow_support(&self, peer: NodeId, limited: bool) -> Result<()> {
        if limited && !self.flow_peers.lock().unwrap().contains(&peer) {
            Err(disk::failure(
                "对端不支持文件限速保活，请更新对端软件或关闭文件限速",
            ))
        } else {
            Ok(())
        }
    }
    pub fn queue_metrics(&self) -> crate::node::core::queue::QueueMetrics {
        self.sender_queue.lock().unwrap().metrics()
    }
    pub fn dispatch_ready(&self, connections: &HashMap<NodeId, Connection>) -> Vec<ScheduledSend> {
        let mut activity = self.activity.lock();
        let mut queue = self.sender_queue.lock().unwrap();
        let mut selected = Vec::new();
        while let Some(entry) = queue.dispatch(|peer| {
            !activity.speed_active(peer)
                && connections
                    .get(&peer)
                    .is_some_and(|c| c.close_reason().is_none())
        }) {
            let generation = queue.slot_generation(&entry.id).unwrap();
            activity
                .add_file(entry.peer)
                .expect("dispatch admission lock");
            let slot = SendSlot {
                _activity: self.activity.file_permit(entry.peer),
                service: self.clone(),
                id: entry.id.clone(),
                generation,
            };
            selected.push(ScheduledSend {
                entry,
                generation,
                _slot: slot,
            });
        }
        selected
    }
    /// Only explicitly queued tasks enter the current-run executor. Restored
    /// Interrupted/Paused records require a new user continuation command.
    pub async fn enqueue_tasks(&self, peer: NodeId, ids: Vec<TaskId>) -> Result<()> {
        for id in &ids {
            self.recovery.lock().unwrap().cancel(id);
            self.automatic_sends.lock().unwrap().remove(id);
        }
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        let generation = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        let lookup = ids.clone();
        let previous = self
            .read_store(move |store| {
                let mut previous = Vec::new();
                for id in lookup {
                    let record = disk::bound_task(store, peer, &id)?;
                    if record.direction() != TaskDirection::Send {
                        return Err(disk::failure("接收任务不能加入发送队列"));
                    }
                    if can_continue(&record) {
                        previous.push(id);
                    }
                }
                Ok(previous)
            })
            .await?;
        // Paused/Interrupted is visible before the old executor's RAII cleanup
        // finishes. A deliberate Continue must await it rather than be mistaken
        // for an idempotent duplicate of an activity that is already ending.
        for id in previous {
            self.wait_previous_attempt(&id).await?;
        }
        let epoch = self.epoch.clone();
        let queue = self.sender_queue.clone();
        self.store(move |store| Self::admit_locked(store, &queue, peer, ids, &epoch, generation))
            .await?;
        #[cfg(test)]
        self.admission_committed().await;
        Ok(())
    }
    /// Caller owns the store writer until admission is complete. Pause/readers
    /// cannot observe Queued before its entry exists. Queue locks stay short;
    /// disk snapshot commits do not hold the scheduler admission mutex.
    fn admit_locked(
        store: &mut TaskStore,
        queue: &Mutex<crate::node::core::queue::TaskQueue>,
        peer: NodeId,
        ids: Vec<TaskId>,
        epoch: &std::sync::atomic::AtomicU64,
        generation: u64,
    ) -> Result<()> {
        let mut entries = Vec::new();
        for id in ids {
            let record = disk::bound_task(store, peer, &id)?;
            if record.direction() != TaskDirection::Send {
                return Err(disk::failure("接收任务不能加入发送队列"));
            }
            if record.state() == TaskState::Completed
                || queue.lock().unwrap().slot_generation(&id).is_some()
            {
                continue;
            }
            if record.state() != TaskState::Queued && !can_continue(&record) {
                return Err(disk::failure("任务当前不能继续"));
            }
            entries.push(crate::node::core::queue::QueueEntry {
                id,
                peer,
                metadata: record.directory_details().is_some(),
            });
        }
        // Validate the complete batch before changing any lifecycle state.
        for entry in &entries {
            if store.task(&entry.id).map_err(disk::local_error)?.state() != TaskState::Queued {
                disk::transition(store, &entry.id, TaskState::Queued)?;
            }
        }
        let admission = {
            let mut queue = queue.lock().unwrap();
            if generation != epoch.load(std::sync::atomic::Ordering::Acquire) {
                Err("旧传输会话已取消")
            } else {
                queue.enqueue(entries.clone())
            }
        };
        if let Err(message) = admission {
            for entry in entries {
                let retained = {
                    let queue = queue.lock().unwrap();
                    queue.is_queued(&entry.id) || queue.slot_generation(&entry.id).is_some()
                };
                if !retained
                    && store.task(&entry.id).map_err(disk::local_error)?.state()
                        == TaskState::Queued
                {
                    disk::transition(store, &entry.id, TaskState::Interrupted)?;
                }
            }
            return Err(disk::failure(message));
        }
        Ok(())
    }
    #[cfg(test)]
    async fn admission_committed(&self) {
        let gate = self.admission_gate.lock().unwrap().take();
        if let Some((reached, release)) = gate {
            let _ = reached.send(());
            let _ = release.await;
        }
    }
    pub async fn execute_queued(
        &self,
        scheduled: ScheduledSend,
        connection: Connection,
    ) -> Result<()> {
        let ScheduledSend {
            entry,
            generation,
            _slot,
        } = scheduled;
        let _owned_slot = _slot;
        self.execute_send(&connection, entry.peer, entry.id, generation)
            .await
    }
    async fn reserve_send(&self, peer: NodeId, id: TaskId) -> Result<SendSlot> {
        let lookup = id.clone();
        let record = self
            .read_store(move |store| disk::bound_task(store, peer, &lookup))
            .await?;
        if record.direction() != TaskDirection::Send {
            return Err(disk::failure("只能发送本机已选择任务"));
        }
        let _activity = self.activity.file(peer)?;
        let generation = self
            .sender_queue
            .lock()
            .unwrap()
            .reserve_direct(crate::node::core::queue::QueueEntry {
                id: id.clone(),
                peer,
                metadata: record.directory_details().is_some(),
            })
            .map_err(disk::failure)?;
        Ok(SendSlot {
            _activity,
            service: self.clone(),
            id,
            generation,
        })
    }
    pub async fn pause_task(&self, id: TaskId) -> Result<()> {
        let waiting = self.recovery.lock().unwrap().cancel(&id);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        let active = self.active.clone();
        let queue = self.sender_queue.clone();
        self.store(move |store| {
            // Serialize the durable state with admission. A reserved executor
            // has not necessarily claimed its stream yet; cancel its nonce too.
            let active = active.lock().unwrap();
            if let Some(task) = active.get(&id) {
                task.pause.send_if_modified(|paused| {
                    if *paused {
                        false
                    } else {
                        *paused = true;
                        true
                    }
                });
                return Ok(());
            }
            let mut queue = queue.lock().unwrap();
            let queued = queue.is_queued(&id);
            let generation = queue.slot_generation(&id);
            if waiting && !queued && generation.is_none() {
                let mut state = store.task(&id).map_err(disk::local_error)?.state();
                if state.needs_startup_recovery() && state != TaskState::Pausing {
                    disk::transition(store, &id, TaskState::Interrupted)?;
                    state = TaskState::Interrupted;
                }
                if state == TaskState::Interrupted {
                    disk::transition(store, &id, TaskState::Queued)?;
                }
                if matches!(state, TaskState::Interrupted | TaskState::Queued) {
                    disk::transition(store, &id, TaskState::Pausing)?;
                    return disk::transition(store, &id, TaskState::Paused);
                }
            }
            if !queued && generation.is_none() {
                return Err(disk::failure("任务当前未传输或排队"));
            }
            if store.task(&id).map_err(disk::local_error)?.state() != TaskState::Queued {
                return Err(disk::failure("任务当前不能暂停"));
            }
            disk::transition(store, &id, TaskState::Pausing)?;
            disk::transition(store, &id, TaskState::Paused)?;
            queue.remove_queued(&id);
            if let Some(generation) = generation {
                queue.finish_if(&id, generation);
            }
            Ok(())
        })
        .await
    }
    pub async fn revoke_authorization(
        &self,
        peer: NodeId,
        authorization: crate::node::core::remote_auth::RemoteAuthorization,
    ) -> Result<()> {
        self.recovery
            .lock()
            .unwrap()
            .cancel_denied(peer, authorization);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        if let Some(speed) = self.speed_peers.lock().unwrap().get(&peer) {
            speed.revoke_authorization(authorization);
        }
        let denied = move |direction| match direction {
            TaskDirection::Send => !authorization.outbound_authorized(),
            TaskDirection::Receive => !authorization.inbound_authorized(),
        };
        for task in self.active.lock().unwrap().values() {
            if task.peer == peer && denied(task.direction) {
                task.pause.send_replace(true);
            }
        }
        let queue = self.sender_queue.clone();
        self.store(move |store| {
            let ids = store
                .list()
                .iter()
                .filter(|t| {
                    t.peer_id().as_str() == peer.to_hex()
                        && denied(t.direction())
                        && t.state().needs_startup_recovery()
                })
                .map(|t| t.task_id().clone())
                .collect::<Vec<_>>();
            for id in ids {
                queue.lock().unwrap().remove_queued(&id);
                disk::transition(store, &id, TaskState::Interrupted)?;
            }
            Ok(())
        })
        .await
    }
    pub async fn interrupt_all(&self) -> Result<()> {
        self.recovery.lock().unwrap().clear();
        self.automatic_sends.lock().unwrap().clear();
        {
            let mut requests = self.preview_requests.lock().unwrap();
            for request in requests.values() {
                request.cancellation.cancel();
            }
            requests.clear();
        }
        self.previews.lock().unwrap().clear();
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.sender_queue.lock().unwrap().clear();
        self.flow_peers.lock().unwrap().clear();
        self.incremental_peers.lock().unwrap().clear();
        self.store(|store| {
            let ids = store
                .list()
                .iter()
                .filter(|task| task.state().needs_startup_recovery())
                .map(|task| task.task_id().clone())
                .collect::<Vec<_>>();
            for id in ids {
                disk::transition(store, &id, TaskState::Interrupted)?;
            }
            Ok(())
        })
        .await
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    #[allow(dead_code)] // T010 task list consumes this snapshot API.
    pub async fn snapshot(&self) -> Result<Vec<TaskRecord>> {
        self.read_store(|store| Ok(store.list().to_vec())).await
    }
    pub(crate) fn remove_completed_task(&self, id: &TaskId) -> Result<()> {
        {
            let mut store = self
                .store
                .lock()
                .map_err(|_| disk::failure("任务库锁不可用"))?;
            store.remove_completed(id).map_err(disk::local_error)?;
        }
        self.rates.lock().unwrap().remove(id);
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
        Ok(())
    }
    pub(crate) fn remove_completed_history(
        &self,
        mut ids: std::collections::BTreeSet<TaskId>,
    ) -> Result<Vec<TaskId>> {
        {
            let active = self.active.lock().unwrap();
            ids.retain(|id| !active.contains_key(id));
        }
        let removed = self
            .store
            .lock()
            .map_err(|_| disk::failure("任务库锁不可用"))?
            .remove_completed_batch(&ids)
            .map_err(disk::local_error)?;
        if !removed.is_empty() {
            let mut rates = self.rates.lock().unwrap();
            for id in &removed {
                rates.remove(id);
            }
            self.changed
                .send_modify(|version| *version = version.wrapping_add(1));
        }
        Ok(removed)
    }
    pub(crate) fn delete_completed_receive_file(&self, id: &TaskId) -> Result<()> {
        if self.active.lock().unwrap().contains_key(id) {
            return Err(disk::failure("传输任务仍在活动，不能删除文件"));
        }
        {
            let mut store = self
                .store
                .lock()
                .map_err(|_| disk::failure("任务库锁不可用"))?;
            let record = store.task(id).map_err(disk::local_error)?;
            disk::delete_completed_receive_file(&record)?;
            store.remove_completed(id).map_err(disk::local_error)?;
        }
        self.rates.lock().unwrap().remove(id);
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
        Ok(())
    }
    pub(crate) fn receive_root_path(&self) -> PathBuf {
        self.receive_root.lock().unwrap().clone()
    }
    pub fn set_receive_root(&self, root: PathBuf) {
        self.recovery.lock().unwrap().clear();
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        *self.receive_root.lock().unwrap() = root;
    }
    pub async fn select_paths(&self, peer: NodeId, paths: Vec<PathBuf>) -> Result<Vec<TaskId>> {
        let generation = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        let selections = blocking(move || crate::node::core::drop_send::classify(paths)).await?;
        let mut ids = Vec::new();
        for selection in selections {
            if generation != self.epoch.load(std::sync::atomic::Ordering::Acquire) {
                return Err(disk::failure(
                    "拖拽扫描期间网络会话已重建；已加入项保留，请重新检查任务列表",
                ));
            }
            let result = match selection {
                crate::node::core::drop_send::Selection::File(path) => {
                    self.select_file(peer, path).await.map(|id| vec![id])
                }
                crate::node::core::drop_send::Selection::Directory(path) => {
                    self.select_directory(peer, path).await
                }
            };
            match result {
                Ok(selected) => ids.extend(selected),
                Err(error) if ids.is_empty() => return Err(error),
                Err(error) => {
                    return Err(disk::failure(&format!(
                        "拖拽批次未全部加入，已加入 {} 个任务并保留；其余项需重新提交：{error}",
                        ids.len()
                    )));
                }
            }
        }
        Ok(ids)
    }
    pub async fn select_file(&self, peer: NodeId, source: PathBuf) -> Result<TaskId> {
        let _selection_activity = self.activity.file(peer)?;
        struct CancelScan(crate::node::core::files::ScanCancellation);
        impl Drop for CancelScan {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let generation = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        let epoch = self.epoch.clone();
        let queue = self.sender_queue.clone();
        let cancellation = crate::node::core::files::ScanCancellation::default();
        let _guard = CancelScan(cancellation.clone());
        let commit_cancellation = cancellation.clone();
        // Hashing may take minutes. It never holds the task-store writer lock.
        let record =
            blocking(move || disk::scan_selected_file(peer, source, &cancellation)).await?;
        let id = self
            .store(move |store| {
                if generation != epoch.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(disk::failure("旧传输会话已取消"));
                }
                commit_cancellation.check()?;
                let id = record.task_id().clone();
                store.create(record).map_err(disk::local_error)?;
                disk::transition(store, &id, TaskState::Queued)?;
                Self::admit_locked(store, &queue, peer, vec![id.clone()], &epoch, generation)?;
                Ok(id)
            })
            .await?;
        #[cfg(test)]
        self.admission_committed().await;
        Ok(id)
    }
    pub async fn select_directory(&self, peer: NodeId, source: PathBuf) -> Result<Vec<TaskId>> {
        let _selection_activity = self.activity.file(peer)?;
        struct CancelScan(crate::node::core::files::ScanCancellation);
        impl Drop for CancelScan {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let generation = self.epoch.load(std::sync::atomic::Ordering::Acquire);
        let epoch = self.epoch.clone();
        let queue = self.sender_queue.clone();
        let cancellation = crate::node::core::files::ScanCancellation::default();
        let _guard = CancelScan(cancellation.clone());
        let commit_cancellation = cancellation.clone();
        let records = blocking(move || {
            crate::node::core::files::scan_directory(peer, &source, &cancellation)
        })
        .await?;
        let ids = self
            .store(move |store| {
                if generation != epoch.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(disk::failure("旧传输会话已取消"));
                }
                commit_cancellation.check()?;
                let ids = store.create_selection(records).map_err(disk::local_error)?;
                for id in &ids {
                    disk::transition(store, id, TaskState::Queued)?;
                }
                Self::admit_locked(store, &queue, peer, ids.clone(), &epoch, generation)?;
                Ok(ids)
            })
            .await?;
        #[cfg(test)]
        self.admission_committed().await;
        Ok(ids)
    }
    /// Test convenience for existing directory protocol regressions.
    #[cfg(test)]
    pub async fn send_selection(
        &self,
        connection: &Connection,
        peer: NodeId,
        ids: Vec<TaskId>,
    ) -> Result<()> {
        let mut failure = None;
        for id in ids {
            if connection.close_reason().is_some() {
                return Err(disk::failure("目录传输连接中断，其余任务可手动继续"));
            }
            if let Err(error) = self.send_file(connection, peer, id).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub async fn task(&self, id: TaskId) -> Result<TaskRecord> {
        self.read_store(move |store| store.task(&id).map_err(disk::local_error))
            .await
    }
    /// Simultaneous recovery requests share a fresh attempt. Never reuse an
    /// old connection's ending worker, or a cancelled local queue intention.
    fn automatic_attempt_exists(&self, id: &TaskId, peer: NodeId, transport: usize) -> bool {
        let queued = self
            .automatic_sends
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|ticket| ticket.peer == peer && !ticket.cancelled());
        let fresh = self.active.lock().unwrap().get(id).is_some_and(|active| {
            active.peer == peer
                && active.direction == TaskDirection::Send
                && active.transport == transport
                && !*active.pause.borrow()
        });
        (queued || fresh) && self.recovery.lock().unwrap().automatic_allowed(id)
    }
    async fn acknowledge_automatic_resume(send: &mut SendStream, id: TaskId) -> Result<()> {
        protocol::write(
            send,
            &Frame {
                request_id: 1,
                message: Message::AutomaticResumeTask { task_id: id },
            },
        )
        .await?;
        send.finish()
            .map_err(|_| disk::failure("自动恢复回执发送失败"))?;
        Ok(())
    }
    async fn wait_previous_attempt(&self, id: &TaskId) -> Result<()> {
        self.wait_attempt_or_reuse(id, None).await.map(|_| ())
    }
    async fn wait_attempt_or_reuse(
        &self,
        id: &TaskId,
        automatic: Option<(NodeId, usize)>,
    ) -> Result<bool> {
        let mut changed = self.subscribe();
        tokio::time::timeout(PAUSE_TIMEOUT, async {
            loop {
                if automatic.is_some_and(|(peer, transport)| {
                    self.automatic_attempt_exists(id, peer, transport)
                }) {
                    return Ok(true);
                }
                if !self.active.lock().unwrap().contains_key(id)
                    && self
                        .sender_queue
                        .lock()
                        .unwrap()
                        .slot_generation(id)
                        .is_none()
                {
                    return Ok(false);
                }
                changed
                    .changed()
                    .await
                    .map_err(|_| disk::failure("任务状态通知已关闭"))?;
            }
        })
        .await
        .map_err(|_| disk::failure("上次传输清理超时"))?
    }
    fn claim(
        &self,
        peer: NodeId,
        id: &TaskId,
        direction: TaskDirection,
        metadata: bool,
        send_generation: Option<u64>,
        transport: usize,
    ) -> Result<(ActiveGuard, watch::Receiver<bool>)> {
        let mut activity = self.activity.lock();
        let mut active = self.active.lock().unwrap();
        if active.contains_key(id) {
            return Err(disk::failure("同一任务已有传输活动"));
        }
        if direction == TaskDirection::Send {
            if self.sender_queue.lock().unwrap().slot_generation(id) != send_generation
                || send_generation.is_none()
            {
                return Err(disk::failure("发送槽已失效"));
            }
        } else {
            let global = active
                .values()
                .filter(|a| a.direction == TaskDirection::Receive && a.metadata == metadata)
                .count();
            let per_peer = active
                .values()
                .filter(|a| {
                    a.direction == TaskDirection::Receive
                        && a.metadata == metadata
                        && a.peer == peer
                })
                .count();
            let (global_limit, peer_limit) = if metadata {
                (crate::node::core::queue::MAX_METADATA_ACTIVE, 2)
            } else {
                (MAX_RECEIVE_FILES, 3)
            };
            if global >= global_limit || per_peer >= peer_limit {
                return Err(disk::failure("接收资源忙，请稍后继续"));
            }
        }
        let file_permit = if direction == TaskDirection::Receive {
            activity.add_file(peer)?;
            Some(self.activity.file_permit(peer))
        } else {
            None
        };
        let (pause, rx) = watch::channel(false);
        active.insert(
            id.clone(),
            Active {
                peer,
                transport,
                direction,
                metadata,
                pause,
            },
        );
        self.measurements
            .lock()
            .unwrap()
            .start(id.clone(), Instant::now());
        self.recovery
            .lock()
            .unwrap()
            .enroll(id.clone(), peer, direction);
        Ok((
            ActiveGuard {
                _activity: file_permit,
                service: self.clone(),
                id: id.clone(),
            },
            rx,
        ))
    }
    async fn claim_receive(
        &self,
        send: &mut SendStream,
        peer: NodeId,
        id: &TaskId,
        metadata: bool,
        transport: usize,
    ) -> Result<(ActiveGuard, watch::Receiver<bool>)> {
        match self.claim(peer, id, TaskDirection::Receive, metadata, None, transport) {
            Ok(claim) => Ok(claim),
            Err(error) => {
                tokio::time::timeout(
                    IDLE_TIMEOUT,
                    protocol::write(
                        send,
                        &Frame {
                            request_id: 1,
                            message: Message::Error {
                                task_id: id.clone(),
                                code: ErrorCode::Busy,
                            },
                        },
                    ),
                )
                .await
                .map_err(|_| disk::failure("接收资源忙回执超时"))??;
                let _ = send.finish();
                Err(error)
            }
        }
    }
    #[cfg(test)]
    pub fn pause(&self, id: &TaskId) -> Result<()> {
        let active = self.active.lock().unwrap();
        let pause = &active
            .get(id)
            .ok_or_else(|| disk::failure("任务当前未传输"))?
            .pause;
        if *pause.borrow() {
            return Ok(());
        }
        pause.send(true).map_err(|_| disk::failure("任务已结束"))
    }
    async fn state(&self, id: &TaskId, state: TaskState) -> Result<()> {
        if matches!(
            state,
            TaskState::Pausing | TaskState::Paused | TaskState::Completed | TaskState::Failed
        ) {
            self.recovery.lock().unwrap().cancel(id);
        }
        let id = id.clone();
        self.store(move |store| disk::transition(store, &id, state))
            .await
    }
    async fn finish_error(&self, id: &TaskId, error: &Error, connection: &Connection) {
        let id = id.clone();
        let text = error.to_string();
        let space_code = match error {
            Error::Io(e) => e
                .get_ref()
                .and_then(|cause| {
                    cause.downcast_ref::<crate::node::core::space_budget::SpaceError>()
                })
                .map(crate::node::core::space_budget::SpaceError::diagnostic),
            _ => None,
        };
        let (state, code, retryable) = if text.contains("源文件内容已变化") {
            (TaskState::Failed, TaskErrorCode::SourceChanged, false)
        } else if text.contains("源文件不可用") {
            (TaskState::Failed, TaskErrorCode::SourceUnavailable, true)
        } else if text.contains("资源忙") {
            (TaskState::Failed, TaskErrorCode::PeerBusy, true)
        } else if matches!(error, Error::Io(e) if e.kind() == std::io::ErrorKind::StorageFull) {
            (TaskState::Failed, TaskErrorCode::DiskFull, true)
        } else if let Some(code) = space_code {
            (TaskState::Failed, code, true)
        } else if is_storage_error(error) {
            (TaskState::Failed, TaskErrorCode::StorageUnavailable, true)
        } else {
            (
                TaskState::Interrupted,
                TaskErrorCode::NetworkInterrupted,
                false,
            )
        };
        let reason = connection.close_reason();
        let network = matches!(
            reason,
            Some(quinn::ConnectionError::TimedOut | quinn::ConnectionError::Reset)
        ) || self.recovery.lock().unwrap().automatic_allowed(&id);
        let transport_error = matches!(error, Error::Io(_))
            || reason
                .as_ref()
                .is_some_and(|r| matches!(error, Error::Protocol(t) if t == &r.to_string()))
            || matches!(error, Error::Protocol(t) if t == "文件流已关闭" || t == "文件流中断");
        if network && transport_error && state != TaskState::Failed {
            let peer = self.active.lock().unwrap().get(&id).map(|a| a.peer);
            if let Some(peer) = peer {
                self.network_lost(peer);
            }
        }
        if state == TaskState::Failed || !network || !transport_error {
            self.recovery.lock().unwrap().cancel(&id);
        }
        let _ = self
            .store(move |store| {
                let record = store.task(&id).map_err(disk::local_error)?;
                if record.state().is_terminal() || record.state() == TaskState::Interrupted {
                    return Ok(());
                }
                store
                    .transition(
                        &id,
                        state,
                        Some(TaskDiagnostic::new(code, retryable)),
                        system_time_unix_ms().map_err(disk::local_error)?,
                    )
                    .map_err(disk::local_error)
            })
            .await;
    }
    fn monotonic_ms(&self) -> u64 {
        self.rate_origin
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }
    fn reset_rate(&self, id: &TaskId, bytes: u64) {
        self.rates
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default()
            .reset(self.monotonic_ms(), bytes);
    }
    async fn progress(&self, id: &TaskId, bytes: u64) -> Result<()> {
        let lookup = id.clone();
        self.store(move |store| {
            store
                .set_progress_hint(
                    &lookup,
                    ProgressHint::new(bytes, system_time_unix_ms().map_err(disk::local_error)?),
                )
                .map_err(disk::local_error)
        })
        .await?;
        self.rates
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default()
            .observe(self.monotonic_ms(), bytes);
        Ok(())
    }
    /// An authoritative snapshot accompanies bounded/coalesced events. A UI
    /// rebuilds from tasks when resync_required, rather than trusting a lost delta.
    #[allow(dead_code)] // T010 consumes the T008 presentation contract.
    pub async fn presentation(&self) -> Result<TransferPresentation> {
        let store = self.store.clone();
        let (tasks, events, resync_required) = blocking(move || {
            let mut store = store.lock().map_err(|_| disk::failure("任务库锁不可用"))?;
            let tasks = store.list().to_vec();
            let events = store.drain_events();
            let resync = store.take_event_resync_required();
            Ok((tasks, events, resync))
        })
        .await?;
        let now = self.monotonic_ms();
        let rates = self.rates.lock().unwrap();
        let bytes_per_second = tasks
            .iter()
            .map(|record| {
                let value = rates.get(record.task_id()).map_or(0.0, |rate| {
                    rate.bytes_per_second(now, record.state() == TaskState::Transferring)
                });
                (record.task_id().clone(), value)
            })
            .collect();
        let mut children: HashMap<TaskId, Vec<TaskRecord>> = HashMap::new();
        for record in &tasks {
            if let Some(group) = record.group_id() {
                children
                    .entry(group.clone())
                    .or_default()
                    .push(record.clone());
            }
        }
        let groups = children
            .into_iter()
            .map(|(id, children)| (id, crate::node::core::queue::group_progress(&children)))
            .collect();
        Ok(TransferPresentation {
            tasks,
            events,
            resync_required,
            bytes_per_second,
            groups,
            queue: self.queue_metrics(),
        })
    }
    pub(crate) fn select_detail(
        &self,
        selection: Option<crate::node::core::task_details::Selection>,
    ) {
        *self.detail_selection.lock().unwrap() = selection;
    }
    /// Only called on a background worker; keep lookup and validation in the
    /// store reader, so cleanup cannot change the binding during validation.
    pub(crate) fn reveal_task(
        &self,
        selection: &crate::node::core::task_details::Selection,
    ) -> Result<PathBuf> {
        let store = self
            .store
            .lock()
            .map_err(|_| disk::failure("任务库锁不可用"))?;
        crate::node::core::task_details::reveal(store.list(), selection)
    }
    /// Called only on GPUI's background executor. Copy small display fields,
    /// never chunk manifests, while holding the store reader. No UI disk/locks.
    pub(crate) fn ui_snapshot(&self) -> Result<crate::node::core::snapshot::Snapshot> {
        let now = self.monotonic_ms();
        let selection = self.detail_selection.lock().unwrap().clone();
        let (mut tasks, detail) = {
            let store = self
                .store
                .lock()
                .map_err(|_| disk::failure("任务库锁不可用"))?;
            let rates = self.rates.lock().unwrap();
            let mut measurements = self.measurements.lock().unwrap();
            measurements.retain(store.list());
            let wall = system_time_unix_ms().unwrap_or(-1);
            let recovery = self.recovery.lock().unwrap();
            let detail = selection.as_ref().and_then(|selection| {
                crate::node::core::task_details::project(
                    store.list(),
                    selection,
                    |record| {
                        rates.get(record.task_id()).map_or(0., |rate| {
                            rate.bytes_per_second(now, record.state() == TaskState::Transferring)
                        })
                    },
                    |id| measurements.sample(id, Instant::now()),
                    |id| recovery.pending(id),
                    wall,
                )
            });
            let tasks = store
                .list()
                .iter()
                .map(|record| {
                    let rate = rates.get(record.task_id()).map_or(0.0, |r| {
                        r.bytes_per_second(now, record.state() == TaskState::Transferring)
                    });
                    (record.created_at_unix_ms(), {
                        let mut row =
                            crate::node::core::snapshot::TaskRow::from_record(record, rate);
                        row.auto_waiting = recovery.pending(record.task_id());
                        row
                    })
                })
                .collect::<Vec<_>>();
            (tasks, detail)
        };
        tasks.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
        Ok(crate::node::core::snapshot::Snapshot {
            space: self.receive_space.summary(),
            detail_selection: selection,
            detail,
            tasks: tasks.into_iter().map(|(_, row)| row).collect(),
            queue: self.queue_metrics(),
            speeds: self.speed_snapshots(),
        })
    }
    async fn receipt(&self, id: &TaskId) -> Result<()> {
        self.recovery.lock().unwrap().forget(id);
        self.state(id, TaskState::Finalizing).await?;
        let id = id.clone();
        self.store(move |store| {
            store
                .commit_receipt(&id, system_time_unix_ms().map_err(disk::local_error)?)
                .map_err(disk::local_error)
        })
        .await
    }
    #[cfg(test)]
    pub async fn send_file(&self, connection: &Connection, peer: NodeId, id: TaskId) -> Result<()> {
        self.wait_previous_attempt(&id).await?;
        let slot = self.reserve_send(peer, id.clone()).await?;
        self.execute_send(connection, peer, id, slot.generation)
            .await
    }
    async fn execute_send(
        &self,
        connection: &Connection,
        peer: NodeId,
        id: TaskId,
        generation: u64,
    ) -> Result<()> {
        let lookup = id.clone();
        self.read_store(move |store| {
            let record = disk::bound_task(store, peer, &lookup)?;
            if record.direction() != TaskDirection::Send {
                return Err(disk::failure("只能发送本机已选择任务"));
            }
            Ok(())
        })
        .await?;
        let current = self.task(id.clone()).await?;
        // This executor already owns the new reserved nonce. Callers waited
        // for old cleanup before reserving/admitting; never wait on our own slot.
        let (_guard, pause) = self.claim(
            peer,
            &id,
            TaskDirection::Send,
            current.directory_details().is_some(),
            Some(generation),
            connection.stable_id(),
        )?;
        let ticket = self.automatic_sends.lock().unwrap().remove(&id);
        let mode = SendMode {
            automatic: ticket.is_some(),
            ticket,
        };
        let result = self
            .send_inner(connection, peer, &id, pause, None, mode)
            .await;
        if let Err(error) = &result {
            self.finish_error(&id, error, connection).await;
        }
        #[cfg(test)]
        {
            let gate = self.source_cleanup_gate.lock().unwrap().take();
            if let Some((reached, release)) = gate {
                let _ = reached.send(());
                let _ = release.await;
            }
        }
        result
    }
    async fn send_inner(
        &self,
        connection: &Connection,
        peer: NodeId,
        id: &TaskId,
        pause: watch::Receiver<bool>,
        continue_reply: Option<&mut SendStream>,
        mode: SendMode,
    ) -> Result<()> {
        let automatic = mode.automatic;
        if automatic {
            self.measurements.lock().unwrap().automatic(id);
        }
        let lookup = id.clone();
        let recovery = self.recovery.clone();
        let record = self
            .store(move |store| {
                let record = disk::bound_task(store, peer, &lookup)?;
                if mode
                    .ticket
                    .as_ref()
                    .is_some_and(crate::node::core::auto_resume::Ticket::cancelled)
                {
                    if record.state() == TaskState::Queued {
                        disk::transition(store, &lookup, TaskState::Interrupted)?;
                    }
                    return Err(disk::failure("自动恢复已取消"));
                }
                if automatic {
                    check_automatic(store, &recovery, peer, &lookup)?;
                }
                disk::begin_attempt(store, &lookup)
            })
            .await?;
        if record.direction() != TaskDirection::Send {
            return Err(disk::failure("只能发送本机已选择任务"));
        }
        if record.state() == TaskState::Completed {
            return Ok(());
        }
        if record.directory_details().is_some() {
            return self
                .send_directory_entry(connection, peer, &record, continue_reply, automatic)
                .await;
        }
        self.check_flow_support(peer, self.upload_limit.enabled())?;
        let source = record.clone();
        let file = blocking(move || disk::verify_source(&source)).await?;
        let details = record.file_details().unwrap();
        let (send, recv) = tokio::time::timeout(IDLE_TIMEOUT, self.open_file_stream(connection))
            .await
            .map_err(|_| disk::failure("文件流打开超时"))?
            .map_err(disk::local_error)?;
        let mut io = TaskIo::new(send, recv, peer, id.clone(), self.frame_budget.clone());
        let offer = Message::Offer {
            task_id: id.clone(),
            group_id: record.group_id().cloned(),
            relative_path: details.relative_path.clone(),
            entry: Entry::File(Box::new(details.manifest.clone())),
        };
        let offer = if details.incremental && self.incremental_peers.lock().unwrap().contains(&peer)
        {
            Message::IncrementalOffer {
                task_id: id.clone(),
                group_id: record.group_id().unwrap().clone(),
                relative_path: details.relative_path.clone(),
                entry: Entry::File(Box::new(details.manifest.clone())),
                automatic,
            }
        } else if automatic {
            offer.automatic()
        } else {
            offer
        };
        io.send(offer).await?;
        if let Some(reply) = continue_reply {
            protocol::write(
                reply,
                &Frame {
                    request_id: 1,
                    message: if automatic {
                        Message::AutomaticResumeTask {
                            task_id: id.clone(),
                        }
                    } else {
                        Message::ResumeTask {
                            task_id: id.clone(),
                        }
                    },
                },
            )
            .await?;
            let _ = reply.finish();
        }
        self.state(id, TaskState::Transferring).await?;
        let result = self
            .sender_loop(&mut io, id, &details.manifest, file, pause)
            .await;
        if let Err(error) = &result {
            io.report_error(error).await;
        }
        result
    }
    async fn send_directory_entry(
        &self,
        connection: &Connection,
        peer: NodeId,
        record: &TaskRecord,
        continue_reply: Option<&mut SendStream>,
        automatic: bool,
    ) -> Result<()> {
        let path = record.local_path().to_path_buf();
        blocking(move || {
            crate::storage::secure_fs::root(&path)?;
            Ok(())
        })
        .await
        .map_err(|_| disk::failure("源文件不可用"))?;
        let (send, recv) = tokio::time::timeout(IDLE_TIMEOUT, self.open_file_stream(connection))
            .await
            .map_err(|_| disk::failure("目录流打开超时"))?
            .map_err(disk::local_error)?;
        let id = record.task_id();
        let mut io = TaskIo::new(send, recv, peer, id.clone(), self.frame_budget.clone());
        let offer = Message::Offer {
            task_id: id.clone(),
            group_id: record.group_id().cloned(),
            relative_path: record.relative_path().unwrap().to_owned(),
            entry: Entry::Directory,
        };
        io.send(if automatic { offer.automatic() } else { offer })
            .await?;
        if let Some(reply) = continue_reply {
            protocol::write(
                reply,
                &Frame {
                    request_id: 1,
                    message: if automatic {
                        Message::AutomaticResumeTask {
                            task_id: id.clone(),
                        }
                    } else {
                        Message::ResumeTask {
                            task_id: id.clone(),
                        }
                    },
                },
            )
            .await?;
            let _ = reply.finish();
        }
        self.state(id, TaskState::Transferring).await?;
        let response = tokio::time::timeout(IDLE_TIMEOUT, io.incoming.recv())
            .await
            .map_err(|_| disk::failure("目录回执超时"))?
            .ok_or_else(|| disk::failure("目录流中断"))??;
        let crate::node::core::frame_budget::BufferedFrame {
            frame: response,
            _lease,
        } = response;
        io.observe(&response)?;
        match response.message {
            Message::Completed { .. } => {
                self.receipt(id).await?;
                io.finish();
                Ok(())
            }
            Message::Error { code, .. } => Err(remote_error(code)),
            _ => Err(disk::failure("目录回执非法")),
        }
    }
    async fn sender_loop(
        &self,
        io: &mut TaskIo,
        id: &TaskId,
        manifest: &FileManifest,
        file: disk::VerifiedSource,
        mut pause: watch::Receiver<bool>,
    ) -> Result<()> {
        let file = Arc::new(Mutex::new(file));
        let writer_ticket = self.fair_writes.ticket(id.clone())?;
        let mut resumed = false;
        let mut pending_pause = false;
        let mut acknowledged = HashSet::new();
        let mut sent = HashSet::new();
        let mut bytes = 0;
        let mut sampled = Instant::now();
        let mut pausing: Option<Instant> = None;
        loop {
            if io.gate.state(id) == Some(WireTaskState::Paused) {
                self.state(id, TaskState::Paused).await?;
                io.finish();
                return Ok(());
            }
            // No prepared payload while waiting for a request/ACK or reading disk.
            // Keep remaining byte credit, release the unused share immediately.
            writer_ticket.park();
            let incoming = next_input(io, &mut pause, pausing).await?;
            let frame = match incoming {
                Input::Pause => {
                    if pausing.is_none() {
                        self.state(id, TaskState::Pausing).await?;
                        pausing = Some(Instant::now());
                    }
                    if resumed {
                        io.send(Message::Pause {
                            task_id: id.clone(),
                        })
                        .await?;
                    } else {
                        pending_pause = true;
                    }
                    continue;
                }
                Input::Frame(frame) => frame,
            };
            let crate::node::core::frame_budget::BufferedFrame { frame, _lease } = frame;
            match frame.message {
                Message::Resume { have, .. } => {
                    let bitmap = ChunkBitmap::from_bytes(manifest.chunk_count(), &have)?;
                    acknowledged.extend(bitmap.present());
                    bytes = bytes_for(manifest, &acknowledged);
                    self.reset_rate(id, bytes);
                    resumed = true;
                    self.progress(id, bytes).await?;
                    if pending_pause {
                        io.send(Message::Pause {
                            task_id: id.clone(),
                        })
                        .await?;
                        pending_pause = false;
                    }
                }
                Message::RequestChunk { index, .. } => {
                    if !resumed
                        || sent.len() >= WINDOW
                        || acknowledged.contains(&index)
                        || !sent.insert(index)
                    {
                        return Err(disk::failure("重复或未授权分片请求"));
                    }
                    let source = file.clone();
                    let manifest = manifest.clone();
                    let data = blocking(move || {
                        disk::source_chunk(
                            &mut *source.lock().map_err(|_| disk::failure("源文件锁不可用"))?,
                            &manifest,
                            index,
                        )
                    })
                    .await?;
                    self.check_flow_support(io.peer, self.upload_limit.enabled())?;
                    let flow_supported = self.flow_peers.lock().unwrap().contains(&io.peer);
                    io.send_chunk(
                        &writer_ticket,
                        &self.upload_limit,
                        flow_supported,
                        &mut pause,
                        Message::Chunk {
                            task_id: id.clone(),
                            index,
                            data,
                        },
                    )
                    .await?;
                }
                Message::FlowWait { .. } => {}
                Message::ChunkAck { index, .. } => {
                    if !sent.remove(&index) || !acknowledged.insert(index) {
                        return Err(disk::failure("重复或未知分片确认"));
                    }
                    bytes += manifest.chunk_range(index).unwrap().1 as u64;
                    if sampled.elapsed() >= PROGRESS_INTERVAL {
                        self.progress(id, bytes).await?;
                        sampled = Instant::now();
                    }
                }
                Message::Pause { .. } => {
                    if pausing.is_none() {
                        self.state(id, TaskState::Pausing).await?;
                        pausing = Some(Instant::now());
                        // Require the receiver's checkpoint acknowledgement before closing.
                        // Otherwise its outstanding ChunkAck frames can hit a reset stream.
                        io.send(Message::Pause {
                            task_id: id.clone(),
                        })
                        .await?;
                    }
                    io.send(Message::Paused {
                        task_id: id.clone(),
                        pause_request_id: frame.request_id,
                    })
                    .await?;
                }
                Message::Paused { .. } => {}
                Message::Completed {
                    receipt_version, ..
                } => {
                    if receipt_version == 2 {
                        self.recovery.lock().unwrap().forget(id);
                        let id = id.clone();
                        self.state(&id, TaskState::Finalizing).await?;
                        self.store(move |store| {
                            store
                                .commit_skipped(
                                    &id,
                                    system_time_unix_ms().map_err(disk::local_error)?,
                                )
                                .map_err(disk::local_error)
                        })
                        .await?;
                    } else {
                        self.receipt(id).await?;
                    }
                    self.progress(id, manifest.total_len).await?;
                    io.finish();
                    return Ok(());
                }
                Message::Error { code, .. } => return Err(remote_error(code)),
                _ => return Err(disk::failure("发送任务收到非法消息")),
            }
        }
    }
    #[allow(dead_code)] // T010 commands.
    pub async fn start_speed(
        &self,
        peer: NodeId,
        direction: crate::node::core::protocol::SpeedDirection,
        seconds: u16,
    ) -> Result<crate::node::core::speed::SpeedSnapshot> {
        let speed = self
            .speed_peers
            .lock()
            .unwrap()
            .get(&peer)
            .cloned()
            .ok_or_else(|| disk::failure("请先连接测速对端"))?;
        speed.start(direction, seconds).await
    }
    #[allow(dead_code)] // T010 commands.
    pub fn cancel_speed(&self, peer: NodeId, id: &TaskId) -> Result<()> {
        self.speed_peers
            .lock()
            .unwrap()
            .get(&peer)
            .ok_or_else(|| disk::failure("测速连接已断开"))?
            .cancel(id)
    }
    #[cfg(test)]
    pub(crate) fn test_speed_idle(&self, peer: NodeId) -> bool {
        self.activity.lock().can_speed(peer)
    }
    #[cfg(test)]
    pub(crate) fn gate_test_send_cleanup(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.source_cleanup_gate.lock().unwrap() = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }
    #[cfg(test)]
    pub(crate) fn set_test_speed_duration(&self, peer: NodeId, duration: Duration) -> bool {
        if let Some(speed) = self.speed_peers.lock().unwrap().get(&peer) {
            speed.set_test_duration(duration);
            true
        } else {
            false
        }
    }
    #[allow(dead_code)] // T010 presentation.
    pub fn speed_snapshots(&self) -> HashMap<NodeId, crate::node::core::speed::SpeedSnapshot> {
        self.speed_peers
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(peer, speed)| speed.snapshot().map(|s| (*peer, s)))
            .collect()
    }
    #[cfg(test)]
    pub async fn serve_peer_with_speed(
        &self,
        connection: Connection,
        peer: NodeId,
        local: NodeId,
    ) -> Result<()> {
        let (_, allowed_forward_targets) = tokio::sync::watch::channel(Vec::new());
        self.serve_peer_with_speed_and_targets(
            connection,
            peer,
            local,
            crate::node::core::remote_auth::RemoteAuthorization::BOTH,
            allowed_forward_targets,
        )
        .await
    }

    pub async fn serve_peer_with_speed_and_targets(
        &self,
        connection: Connection,
        peer: NodeId,
        local: NodeId,
        authorization: impl Into<crate::node::core::remote_auth::LiveAuthorization>,
        allowed_forward_targets: watch::Receiver<
            Vec<crate::node::core::config::AllowedForwardTarget>,
        >,
    ) -> Result<()> {
        self.serve_peer_transport(
            connection,
            peer,
            local,
            authorization.into(),
            allowed_forward_targets,
            None,
        )
        .await
    }
    pub(crate) async fn serve_dispatched(
        &self,
        connection: Connection,
        peer: NodeId,
        local: NodeId,
        authorization: crate::node::core::remote_auth::LiveAuthorization,
        allowed_forward_targets: watch::Receiver<
            Vec<crate::node::core::config::AllowedForwardTarget>,
        >,
        incoming: FileInlets,
    ) -> Result<()> {
        self.serve_peer_transport(
            connection,
            peer,
            local,
            authorization,
            allowed_forward_targets,
            Some(incoming),
        )
        .await
    }
    async fn serve_peer_transport(
        &self,
        connection: Connection,
        peer: NodeId,
        local: NodeId,
        authorization: crate::node::core::remote_auth::LiveAuthorization,
        allowed_forward_targets: watch::Receiver<
            Vec<crate::node::core::config::AllowedForwardTarget>,
        >,
        incoming: Option<FileInlets>,
    ) -> Result<()> {
        let speed = crate::node::core::speed::SpeedPeer::new_with_mode(
            connection.clone(),
            local,
            peer,
            self.activity.clone(),
            self.changed.clone(),
            self.is_unified(&connection),
        );
        self.speed_peers.lock().unwrap().insert(peer, speed.clone());
        struct RemovePeer {
            modes: Arc<Mutex<HashSet<usize>>>,
            transport: usize,
            map: Arc<Mutex<HashMap<NodeId, crate::node::core::speed::SpeedPeer>>>,
            peer: NodeId,
            speed: crate::node::core::speed::SpeedPeer,
        }
        impl Drop for RemovePeer {
            fn drop(&mut self) {
                self.speed.cancel_all();
                self.modes.lock().unwrap().remove(&self.transport);
                let mut map = self.map.lock().unwrap();
                if map
                    .get(&self.peer)
                    .is_some_and(|s| s.same_connection(&self.speed))
                {
                    map.remove(&self.peer);
                }
            }
        }
        let _guard = RemovePeer {
            modes: self.unified_connections.clone(),
            transport: connection.stable_id(),
            map: self.speed_peers.clone(),
            peer,
            speed: speed.clone(),
        };
        self.serve_peer_inner(
            connection,
            peer,
            Some(speed),
            authorization,
            allowed_forward_targets,
            incoming,
        )
        .await
    }
    /// Test convenience; production always enables negotiated speed business.
    #[cfg(test)]
    pub async fn serve_peer(&self, connection: Connection, peer: NodeId) -> Result<()> {
        let (_, allowed_forward_targets) = watch::channel(Vec::new());
        self.serve_peer_inner(
            connection,
            peer,
            None,
            crate::node::core::remote_auth::RemoteAuthorization::BOTH.into(),
            allowed_forward_targets,
            None,
        )
        .await
    }
    async fn serve_peer_inner(
        &self,
        connection: Connection,
        peer: NodeId,
        speed: Option<crate::node::core::speed::SpeedPeer>,
        authorization: crate::node::core::remote_auth::LiveAuthorization,
        allowed_forward_targets: watch::Receiver<
            Vec<crate::node::core::config::AllowedForwardTarget>,
        >,
        mut incoming: Option<FileInlets>,
    ) -> Result<()> {
        let mut streams = JoinSet::new();
        if let Some(speed) = speed.clone() {
            streams.spawn(async move {
                speed.serve_uni().await;
                Ok(())
            });
        }
        loop {
            tokio::select! {
                accepted=next_incoming(&connection, &mut incoming)=> {
                    let Some((kind, crate::node::core::dispatch::BiStream {mut send, mut recv})) = accepted else { break };
                    let resources = incoming.as_ref().map(|inlets| inlets.resources.clone())
                        .or_else(|| self.resource_scopes.lock().unwrap().get(&connection.stable_id()).cloned());
                    if !authorization.current().inbound_authorized() || streams.len()>=8 {let _=send.reset(4u32.into());let _=recv.stop(4u32.into());continue;}
                    let service=self.clone();let connection=connection.clone();let speed=speed.clone();let allowed_forward_targets=allowed_forward_targets.clone();
                    let authorization = authorization.clone();
                    streams.spawn(async move {
                        authorization.guard(true, async {
                        let buffered=tokio::time::timeout(Duration::from_secs(5),service.frame_budget.read(&mut recv,None)).await.map_err(|_|disk::failure("文件流首帧超时"))??;
                        let crate::node::core::frame_budget::BufferedFrame {frame:mut first,_lease}=buffered;
                        if kind.is_some() || resources.is_some() {
                            let allowed = resources.as_ref().map(|r| r.borrow().for_peer(peer)).unwrap_or_default();
                            let actual = kind.unwrap_or(match first.message {
                                Message::Speed(_) => crate::node::core::wire::StreamKind::Speed,
                                Message::TunnelOpen { .. } => crate::node::core::wire::StreamKind::Service,
                                _ => crate::node::core::wire::StreamKind::File,
                            });
                            let kind_matches = match kind {
                                Some(crate::node::core::wire::StreamKind::File) => !matches!(first.message, Message::Speed(_) | Message::TunnelOpen { .. }),
                                Some(crate::node::core::wire::StreamKind::Speed) => matches!(first.message, Message::Speed(_)),
                                Some(_) => false,
                                None => true,
                            };
                            let permitted = !allowed.disabled && match actual {
                                crate::node::core::wire::StreamKind::File => allowed.files,
                                crate::node::core::wire::StreamKind::Speed => allowed.speed,
                                crate::node::core::wire::StreamKind::Service => kind.is_none(),
                            };
                            if !kind_matches || !permitted {
                                let code = if !kind_matches { crate::node::core::dispatch::RESET_INVALID } else { crate::node::core::dispatch::RESET_UNAUTHORIZED };
                                let _ = send.reset(code.into()); let _ = recv.stop(code.into());
                                return Err(disk::failure("节点业务类型或资源权限拒绝"));
                            }
                        }
                        let incremental = matches!(&first.message, Message::IncrementalOffer { .. });
                        let incremental_automatic = matches!(&first.message, Message::IncrementalOffer { automatic: true, .. });
                        if incremental {
                            if !service.incremental_peers.lock().unwrap().contains(&peer) { return Err(disk::failure("未协商目录增量能力")); }
                            if let Message::IncrementalOffer { task_id, group_id, relative_path, entry, .. } = first.message.clone() {
                                first.message = Message::Offer { task_id, group_id: Some(group_id), relative_path, entry };
                            }
                        }
                        let automatic=incremental_automatic || first.message.manual().is_some();
                        if let Some(message)=first.message.manual() {first.message=message;}
                        match &first.message {
                            Message::CompareFile { task_id, relative_path, manifest } if first.request_id == 1 => {
                                if !service.incremental_peers.lock().unwrap().contains(&peer) { return Err(disk::failure("未协商目录增量能力")); }
                                let permit = match service.comparisons.clone().try_acquire_owned() {
                                    Ok(permit) => permit,
                                    Err(_) => { protocol::write(&mut send, &Frame { request_id: 1, message: Message::Error { task_id: task_id.clone(), code: ErrorCode::Busy } }).await?; send.finish().map_err(disk::local_error)?; return Ok(()); }
                                };
                                let owner = service.activity.file(peer)?;
                                let root = service.receive_root.lock().unwrap().clone();
                                let relative = relative_path.clone(); let manifest = *manifest.clone(); let root_hash = manifest.root_hash; let task_id = task_id.clone();
                                let compared = blocking(move || { let _permit = permit; let _owner = owner; crate::node::core::incremental::compare(root, relative, manifest) }).await;
                                let (difference, _) = match compared {
                                    Ok(compared) => compared,
                                    Err(error) => { protocol::write(&mut send, &Frame { request_id: 1, message: Message::Error { task_id, code: error_code(&error) } }).await?; send.finish().map_err(disk::local_error)?; return Err(error); }
                                };
                                protocol::write(&mut send, &Frame { request_id: 1, message: Message::ComparedFile { task_id, root_hash, difference } }).await?;
                                send.finish().map_err(disk::local_error)?;
                                Ok(())
                            },
                            Message::TunnelOpen{target}=> {
                                drop(_lease);
                                let allowed=allowed_forward_targets.borrow().clone();
                                crate::node::core::tunnel::serve_open(send,recv,*target,peer,&allowed).await
                            },
                            Message::Speed(_)=> { let speed=speed.ok_or_else(||disk::failure("对端不支持测速执行"))?;drop(_lease);speed.serve_control(send,recv,first).await },
                            Message::Offer{..} => service.receive_offer(send,recv,&connection,peer,crate::node::core::frame_budget::BufferedFrame{frame:first,_lease},ReceiveMode {automatic,incremental}).await,
                            Message::ResumeTask{task_id} if first.request_id==1 => {
                                // This recovery path initiates a new Offer stream, so it also
                                // needs the local outgoing right; it is not a stream reply.
                                if !authorization.current().outbound_authorized() { let _=send.reset(3u32.into()); let _=recv.stop(3u32.into()); return Err(disk::failure("出站文件访问尚未授权")); }
                                let id=task_id.clone();let lookup=id.clone();
                                let checked=service.read_store(move|store|disk::bound_task(store,peer,&lookup)).await;
                                if !matches!(checked, Ok(ref record) if record.direction()==TaskDirection::Send && record.state()!=TaskState::Completed && (if automatic { service.recovery.lock().unwrap().automatic_allowed(&id) && automatic_record_allowed(record) } else { can_continue(record) })) {
                                    let code = match &checked {
                                        Ok(record) if record.direction()==TaskDirection::Send => match record.diagnostic().map(TaskDiagnostic::code) {
                                            Some(TaskErrorCode::SourceChanged) => ErrorCode::SourceChanged,
                                            Some(TaskErrorCode::SourceUnavailable) => ErrorCode::SourceMissing,
                                            _ => ErrorCode::InvalidState,
                                        },
                                        _ => ErrorCode::UnknownTask,
                                    };
                                    protocol::write(&mut send,&Frame{request_id:1,message:Message::Error{task_id:id.clone(),code}}).await?;
                                    let _=send.finish();return Err(remote_error(code));
                                }
                                if service.wait_attempt_or_reuse(&id, automatic.then_some((peer,connection.stable_id()))).await? {
                                    return Self::acknowledge_automatic_resume(&mut send,id).await;
                                }
                                #[cfg(test)]
                                if automatic {
                                    let gate = service.resume_admission_gate.lock().unwrap().take();
                                    if let Some((reached, release)) = gate {
                                        let _ = reached.send(());
                                        let _ = release.await;
                                    }
                                }
                                let slot=match service.reserve_send(peer,id.clone()).await {
                                    Ok(slot)=>slot,
                                    Err(error)=> {
                                        // An executor may claim its fair-queue slot while the
                                        // store read for reserve_send yields. Recheck that race.
                                        if automatic && service.automatic_attempt_exists(&id,peer,connection.stable_id()) {
                                            return Self::acknowledge_automatic_resume(&mut send,id).await;
                                        }
                                        protocol::write(&mut send,&Frame{request_id:1,message:Message::Error{task_id:id.clone(),code:ErrorCode::Busy}}).await?;let _=send.finish();return Err(error);
                                    }
                                };
                                let metadata=checked.as_ref().unwrap().directory_details().is_some();
                                let (_guard,pause)=match service.claim(peer,&id,TaskDirection::Send,metadata,Some(slot.generation),connection.stable_id()) {
                                    Ok(claim)=>claim,
                                    Err(error)=> {
                                        protocol::write(&mut send,&Frame{request_id:1,message:Message::Error{task_id:id.clone(),code:ErrorCode::Busy}}).await?;
                                        let _=send.finish();return Err(error);
                                    }
                                };
                                let ticket = service.automatic_sends.lock().unwrap().remove(&id);
                                let result=authorization.guard(false, service.send_inner(&connection,peer,&id,pause,Some(&mut send),SendMode {automatic,ticket})).await;
                                if let Err(error)=&result {
                                    let _=protocol::write(&mut send,&Frame{request_id:1,message:Message::Error{task_id:id.clone(),code:error_code(error)}}).await;
                                    let _=send.finish();service.finish_error(&id,error,&connection).await;
                                }
                                result
                            }
                            _=>Err(disk::failure("桌面流入口仅接受任务、测速或 TCP 隧道请求")),
                        }
                        }).await
                    });
                }
                result=streams.join_next(),if !streams.is_empty()=> {if let Some(Err(error))=result {tracing::warn!(%error,"桌面文件任务终止");}}
            }
        }
        streams.abort_all();
        while streams.join_next().await.is_some() {}
        Ok(())
    }
    pub async fn resume(&self, connection: &Connection, peer: NodeId, id: TaskId) -> Result<()> {
        let lookup = id.clone();
        let record = self
            .read_store(move |store| disk::bound_task(store, peer, &lookup))
            .await?;
        if record.direction() == TaskDirection::Send {
            #[cfg(test)]
            return self.send_file(connection, peer, id).await;
            #[cfg(not(test))]
            return self.enqueue_tasks(peer, vec![id]).await;
        }
        if !can_continue(&record) {
            return Err(disk::failure("任务当前不能继续"));
        }
        self.wait_previous_attempt(&id).await?;
        self.state(&id, TaskState::Queued).await?;
        self.request_resume(connection, peer, id, false).await
    }
    async fn request_resume(
        &self,
        connection: &Connection,
        _peer: NodeId,
        id: TaskId,
        automatic: bool,
    ) -> Result<()> {
        let result = tokio::time::timeout(IDLE_TIMEOUT, async {
            let (mut send, mut recv) = self
                .open_file_stream(connection)
                .await
                .map_err(disk::local_error)?;
            protocol::write(
                &mut send,
                &Frame {
                    request_id: 1,
                    message: if automatic {
                        Message::AutomaticResumeTask {
                            task_id: id.clone(),
                        }
                    } else {
                        Message::ResumeTask {
                            task_id: id.clone(),
                        }
                    },
                },
            )
            .await?;
            let response = protocol::read(&mut recv).await?;
            let _ = send.finish();
            match response.message {
                Message::ResumeTask { task_id } | Message::AutomaticResumeTask { task_id }
                    if task_id == id && response.request_id == 1 =>
                {
                    Ok(())
                }
                Message::Error { task_id, code } if task_id == id => Err(remote_error(code)),
                _ => Err(disk::failure("继续响应身份不符")),
            }
        })
        .await
        .unwrap_or_else(|_| Err(disk::failure("继续请求超时")));
        if let Err(error) = &result {
            self.finish_error(&id, error, connection).await;
        }
        result
    }
    async fn receive_offer(
        &self,
        mut send: SendStream,
        recv: RecvStream,
        connection: &Connection,
        peer: NodeId,
        buffered: crate::node::core::frame_budget::BufferedFrame,
        mode: ReceiveMode,
    ) -> Result<()> {
        let ReceiveMode {
            automatic,
            incremental,
        } = mode;
        let crate::node::core::frame_budget::BufferedFrame {
            frame: first,
            _lease,
        } = buffered;
        let Message::Offer {
            task_id,
            relative_path,
            entry,
            group_id,
        } = first.message.clone()
        else {
            return Err(disk::failure("需要目录或文件 Offer"));
        };
        if entry == Entry::Directory {
            return self
                .receive_directory(
                    send,
                    recv,
                    connection,
                    peer,
                    crate::node::core::frame_budget::BufferedFrame {
                        frame: first,
                        _lease,
                    },
                    automatic,
                )
                .await;
        }
        let Entry::File(manifest) = entry else {
            unreachable!()
        };
        if let Err(error) = self.check_flow_support(peer, self.download_limit.enabled()) {
            let _ = protocol::write(
                &mut send,
                &Frame {
                    request_id: 1,
                    message: Message::Error {
                        task_id,
                        code: ErrorCode::UnsupportedCapability,
                    },
                },
            )
            .await;
            let _ = send.finish();
            return Err(error);
        }
        let (_guard, pause) = self
            .claim_receive(&mut send, peer, &task_id, false, connection.stable_id())
            .await?;
        let _guard = Arc::new(_guard);
        if automatic {
            self.measurements.lock().unwrap().automatic(&task_id);
        }
        let mut io = TaskIo::new(send, recv, peer, task_id.clone(), self.frame_budget.clone());
        if incremental {
            let frame = Frame {
                request_id: first.request_id,
                message: Message::IncrementalOffer {
                    task_id: task_id.clone(),
                    group_id: group_id
                        .clone()
                        .ok_or_else(|| disk::failure("增量文件缺少目录身份"))?,
                    relative_path: relative_path.clone(),
                    entry: Entry::File(manifest.clone()),
                    automatic,
                },
            };
            io.observe(&frame)?;
        } else {
            io.observe(&first)?;
        }
        drop(first);
        let root = self.receive_root.lock().unwrap().clone();
        let id = task_id.clone();
        let recovery = self.recovery.clone();
        let record = self
            .store(move |store| {
                if automatic {
                    check_automatic(store, &recovery, peer, &id)?;
                }
                let record = disk::accept_entry(
                    store,
                    peer,
                    &id,
                    &root,
                    relative_path,
                    *manifest,
                    group_id,
                )?;
                if incremental
                    && record.state() != TaskState::Completed
                    && !record.file_details().unwrap().incremental
                {
                    store.enable_incremental(&id).map_err(disk::local_error)?;
                    return store.task(&id).map_err(disk::local_error);
                }
                Ok(record)
            })
            .await?;
        drop(_lease);
        if record.file_details().unwrap().receipt_committed {
            let completed = record.clone();
            let owner = _guard.clone();
            if blocking(move || {
                let _owner = owner;
                if completed.file_details().unwrap().skipped_existing {
                    disk::cleanup_skipped_staging(&completed)
                } else {
                    disk::cleanup_staging(&completed)
                }
            })
            .await
            .is_err()
            {
                tracing::warn!("持久回执重发；暂存清理仍需重试");
            }
            io.send(Message::Completed {
                task_id,
                root_hash: record.file_details().unwrap().manifest.root_hash,
                receipt_version: if incremental && record.file_details().unwrap().skipped_existing {
                    2
                } else {
                    1
                },
            })
            .await?;
            io.finish();
            return Ok(());
        }
        self.state(&task_id, TaskState::Transferring).await?;
        if incremental && !record.file_details().unwrap().publish_prepared {
            match self
                .try_existing_file(&mut io, &record, _guard.clone())
                .await
            {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    io.report_error(&error).await;
                    self.finish_error(&task_id, &error, connection).await;
                    return Err(error);
                }
            }
        }
        let local = record.clone();
        let budget = self.receive_space.clone();
        let owner = _guard.clone();
        let (download, reservation) =
            match blocking(move || disk::open_download_with_space(&local, &budget, owner)).await {
                Ok(admitted) => admitted,
                Err(error) => {
                    io.report_error(&error).await;
                    self.finish_error(&task_id, &error, connection).await;
                    return Err(error);
                }
            };
        let download = Arc::new(Mutex::new(download));
        let result = self
            .receiver_loop(&mut io, &record, download.clone(), pause, &reservation)
            .await;
        if result.is_ok() {
            self.recovery.lock().unwrap().forget(&task_id);
        }
        if let Err(error) = &result {
            let checkpoint = download.clone();
            let lease = reservation.clone();
            let _ = blocking(move || {
                let _lease = lease;
                checkpoint
                    .lock()
                    .map_err(|_| disk::failure("接收文件锁不可用"))?
                    .checkpoint()
            })
            .await;
            io.report_error(error).await;
            self.finish_error(&task_id, error, connection).await;
        }
        result
    }
    /// Keep the attempt alive through background hashing, sync and receipt commit.
    async fn try_existing_file(
        &self,
        io: &mut TaskIo,
        record: &TaskRecord,
        owner: Arc<ActiveGuard>,
    ) -> Result<bool> {
        let local = record.clone();
        let pool = self.comparisons.clone();
        let hashing_owner = owner.clone();
        let comparison = async move {
            let permit = pool
                .acquire_owned()
                .await
                .map_err(|_| disk::failure("目录比较已关闭"))?;
            blocking(move || {
                let _permit = permit;
                let _owner = hashing_owner;
                let details = local.file_details().unwrap();
                crate::node::core::incremental::compare(
                    local.local_path().to_path_buf(),
                    details.relative_path.clone(),
                    details.manifest.clone(),
                )
            })
            .await
        };
        tokio::pin!(comparison);
        let mut keepalive = tokio::time::interval(Duration::from_secs(5));
        let checked = loop {
            tokio::select! {
                result = &mut comparison => break result?,
                _ = keepalive.tick() => { io.send(Message::FlowWait { task_id: record.task_id().clone() }).await?; }
            }
        };
        let Some(proof) = checked.1 else {
            return Ok(false);
        };
        self.state(record.task_id(), TaskState::Finalizing).await?;
        let id = record.task_id().clone();
        let commit_owner = owner.clone();
        self.store(move |store| {
            let _owner = commit_owner;
            proof.make_durable()?;
            store
                .commit_skipped(&id, system_time_unix_ms().map_err(disk::local_error)?)
                .map_err(disk::local_error)
        })
        .await?;
        let completed = record.clone();
        if blocking(move || {
            let _owner = owner;
            disk::cleanup_skipped_staging(&completed)
        })
        .await
        .is_err()
        {
            tracing::warn!("跳过文件回执已提交；暂存清理仍需重试");
        }
        self.progress(
            record.task_id(),
            record.file_details().unwrap().manifest.total_len,
        )
        .await?;
        #[cfg(test)]
        if self
            .drop_completion
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            let _ = io.send.reset(77u32.into());
            return Err(disk::failure("测试在持久回执后切断完成消息"));
        }
        io.send(Message::Completed {
            task_id: record.task_id().clone(),
            root_hash: record.file_details().unwrap().manifest.root_hash,
            receipt_version: 2,
        })
        .await?;
        io.finish();
        self.recovery.lock().unwrap().forget(record.task_id());
        Ok(true)
    }
    async fn receive_directory(
        &self,
        mut send: SendStream,
        recv: RecvStream,
        connection: &Connection,
        peer: NodeId,
        buffered: crate::node::core::frame_budget::BufferedFrame,
        automatic: bool,
    ) -> Result<()> {
        let crate::node::core::frame_budget::BufferedFrame {
            frame: first,
            _lease,
        } = buffered;
        let Message::Offer {
            task_id,
            group_id,
            relative_path,
            entry: Entry::Directory,
        } = first.message.clone()
        else {
            return Err(disk::failure("目录 Offer 非法"));
        };
        let (_guard, _pause) = self
            .claim_receive(&mut send, peer, &task_id, true, connection.stable_id())
            .await?;
        let _guard = Arc::new(_guard);
        if automatic {
            self.measurements.lock().unwrap().automatic(&task_id);
        }
        let mut io = TaskIo::new(send, recv, peer, task_id.clone(), self.frame_budget.clone());
        io.observe(&first)?;
        drop(first);
        let root = self.receive_root.lock().unwrap().clone();
        let id = task_id.clone();
        let recovery = self.recovery.clone();
        let record = self
            .store(move |store| {
                if automatic {
                    check_automatic(store, &recovery, peer, &id)?;
                }
                disk::accept_directory(store, peer, &id, &root, relative_path, group_id)
            })
            .await?;
        drop(_lease);
        let result = async {
            if !record.receipt_committed() {
                self.state(&task_id, TaskState::Transferring).await?;
                self.state(&task_id, TaskState::Finalizing).await?;
                let local = record.clone();
                let budget = self.receive_space.clone();
                let owner = _guard.clone();
                self.store(move |store| {
                    let lease = budget.reserve_directory(&local)?;
                    lease.keep_alive(owner);
                    crate::node::core::publish::directory(store, &local)
                })
                .await?;
            }
            io.send(Message::Completed {
                task_id: task_id.clone(),
                root_hash: crate::protocol::manifest::ChunkHash::of(
                    record.relative_path().unwrap().as_bytes(),
                ),
                receipt_version: 1,
            })
            .await?;
            io.finish();
            Ok(())
        }
        .await;
        if result.is_ok() {
            self.recovery.lock().unwrap().forget(&task_id);
        }
        if let Err(error) = &result {
            io.report_error(error).await;
            self.finish_error(&task_id, error, connection).await;
        }
        result
    }
    async fn receiver_loop(
        &self,
        io: &mut TaskIo,
        record: &TaskRecord,
        download: Arc<Mutex<crate::storage::PartialDownload>>,
        mut pause: watch::Receiver<bool>,
        reservation: &crate::node::core::space_budget::Reservation,
    ) -> Result<()> {
        let id = record.task_id();
        let manifest = &record.file_details().unwrap().manifest;
        let initial = download.clone();
        let lease = reservation.clone();
        let (have, missing, mut bytes) = blocking(move || {
            let _lease = lease;
            let download = initial
                .lock()
                .map_err(|_| disk::failure("接收文件锁不可用"))?;
            let present = download.bitmap().present().into_iter().collect();
            Ok((
                download.bitmap().to_bytes(),
                download.missing(),
                bytes_for(download.manifest(), &present),
            ))
        })
        .await?;
        let mut missing: VecDeque<_> = missing.into();
        let mut outstanding = HashSet::new();
        let mut sampled = Instant::now();
        let mut pausing = None;
        let mut remote_pause = None;
        let mut checkpointed = false;
        io.send(Message::Resume {
            task_id: id.clone(),
            root_hash: manifest.root_hash,
            have,
        })
        .await?;
        self.reset_rate(id, bytes);
        self.progress(id, bytes).await?;
        loop {
            if bytes == manifest.total_len {
                self.state(id, TaskState::Finalizing).await?;
                let record = record.clone();
                let output = download.clone();
                #[cfg(test)]
                let publication_error = self.publication_error.clone();
                let lease = reservation.clone();
                self.store(move |store| {
                    let _lease = lease;
                    #[cfg(test)]
                    if let Some(kind) = publication_error.lock().unwrap().take() {
                        return Err(
                            std::io::Error::new(kind, "test-only publication I/O failure").into(),
                        );
                    }
                    disk::publish(
                        store,
                        &record,
                        &mut *output
                            .lock()
                            .map_err(|_| disk::failure("接收文件锁不可用"))?,
                    )
                })
                .await?;
                self.progress(id, manifest.total_len).await?;
                #[cfg(test)]
                if self
                    .drop_completion
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                {
                    let _ = io.send.reset(77u32.into());
                    return Err(disk::failure("测试在持久回执后切断完成消息"));
                }
                io.send(Message::Completed {
                    task_id: id.clone(),
                    root_hash: manifest.root_hash,
                    receipt_version: 1,
                })
                .await?;
                io.finish();
                return Ok(());
            }
            if pausing.is_none() {
                while outstanding.len() < WINDOW {
                    let Some(index) = missing.pop_front() else {
                        break;
                    };
                    outstanding.insert(index);
                    io.send(Message::RequestChunk {
                        task_id: id.clone(),
                        index,
                    })
                    .await?;
                }
            } else if outstanding.is_empty() {
                if !checkpointed {
                    let file = download.clone();
                    let lease = reservation.clone();
                    blocking(move || {
                        let _lease = lease;
                        file.lock()
                            .map_err(|_| disk::failure("接收文件锁不可用"))?
                            .checkpoint()
                    })
                    .await?;
                    checkpointed = true;
                }
                if let Some(request_id) = remote_pause.take() {
                    io.send(Message::Paused {
                        task_id: id.clone(),
                        pause_request_id: request_id,
                    })
                    .await?;
                }
                if io.gate.state(id) == Some(WireTaskState::Paused) {
                    self.progress(id, bytes).await?;
                    self.state(id, TaskState::Paused).await?;
                    io.finish();
                    return Ok(());
                }
            }
            let frame = match next_input(io, &mut pause, pausing).await? {
                Input::Pause => {
                    if pausing.is_none() {
                        self.state(id, TaskState::Pausing).await?;
                        pausing = Some(Instant::now());
                    }
                    io.send(Message::Pause {
                        task_id: id.clone(),
                    })
                    .await?;
                    continue;
                }
                Input::Frame(frame) => frame,
            };
            let crate::node::core::frame_budget::BufferedFrame { frame, _lease } = frame;
            match frame.message {
                Message::Chunk { index, data, .. } => {
                    if !outstanding.remove(&index) {
                        return Err(disk::failure("收到未请求或重复的分片"));
                    }
                    self.check_flow_support(io.peer, self.download_limit.enabled())?;
                    io.wait_download(&self.download_limit, data.len(), &mut pause)
                        .await?;
                    let output = download.clone();
                    let length = data.len() as u64;
                    let lease = reservation.clone();
                    blocking(move || {
                        output
                            .lock()
                            .map_err(|_| disk::failure("接收文件锁不可用"))?
                            .write_chunk(index, &data)?;
                        lease.written(length);
                        Ok(())
                    })
                    .await?;
                    #[cfg(test)]
                    {
                        let checkpointed =
                            download.lock().unwrap().durable_bitmap().count_set() > 0;
                        if checkpointed {
                            let gate = self.checkpoint_gate.lock().unwrap().take();
                            if let Some((reached, release)) = gate {
                                let _ = reached.send(());
                                let _ = release.await;
                            }
                        }
                        let gate = self.first_chunk_gate.lock().unwrap().take();
                        if let Some((reached, release)) = gate {
                            let _ = reached.send(());
                            let _ = release.await;
                        }
                    }
                    bytes += length;
                    io.send(Message::ChunkAck {
                        task_id: id.clone(),
                        index,
                    })
                    .await?;
                    if sampled.elapsed() >= PROGRESS_INTERVAL {
                        self.progress(id, bytes).await?;
                        sampled = Instant::now();
                    }
                }
                Message::Pause { .. } => {
                    if pausing.is_none() {
                        self.state(id, TaskState::Pausing).await?;
                        pausing = Some(Instant::now());
                    }
                    remote_pause = Some(frame.request_id);
                }
                Message::Paused { .. } => {}
                Message::Error { code, .. } => return Err(remote_error(code)),
                _ => return Err(disk::failure("接收任务收到非法消息")),
            }
        }
    }
}

fn automatic_record_allowed(record: &TaskRecord) -> bool {
    matches!(
        record.state(),
        TaskState::Queued
            | TaskState::Connecting
            | TaskState::Negotiating
            | TaskState::Transferring
            | TaskState::Finalizing
    ) || (record.state() == TaskState::Interrupted
        && record
            .diagnostic()
            .is_none_or(|d| d.code() == TaskErrorCode::NetworkInterrupted))
        || record.state() == TaskState::Completed
}
fn check_automatic(
    store: &TaskStore,
    recovery: &Mutex<crate::node::core::auto_resume::Recovery>,
    peer: NodeId,
    id: &TaskId,
) -> Result<()> {
    // An Offer lost before receiver admission is still a new authenticated
    // delivery. Known IDs must retain their current-run recovery fence.
    if !store.list().iter().any(|record| record.task_id() == id) {
        return Ok(());
    }
    let record = disk::bound_task(store, peer, id)?;
    if record.state() != TaskState::Completed
        && (!recovery.lock().unwrap().automatic_allowed(id) || !automatic_record_allowed(&record))
    {
        return Err(disk::failure("任务需要手动继续"));
    }
    Ok(())
}
fn can_continue(record: &TaskRecord) -> bool {
    matches!(record.state(), TaskState::Paused | TaskState::Interrupted)
        || (record.state() == TaskState::Failed
            && record
                .diagnostic()
                .is_some_and(TaskDiagnostic::is_retryable))
}

fn bytes_for(manifest: &FileManifest, indices: &HashSet<u32>) -> u64 {
    indices
        .iter()
        .filter_map(|i| manifest.chunk_range(*i))
        .map(|(_, len)| len as u64)
        .sum()
}
fn is_storage_error(error: &Error) -> bool {
    if let Error::Io(error) = error {
        return !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut
        );
    }
    let text = error.to_string();
    text.contains("存储")
        || text.contains("发布")
        || text.contains("接收目标已存在")
        || text.contains("接收根目录")
}
fn error_code(error: &Error) -> ErrorCode {
    let text = error.to_string();
    if text.contains("源文件内容已变化") {
        ErrorCode::SourceChanged
    } else if text.contains("源文件不可用") {
        ErrorCode::SourceMissing
    } else if text.contains("对端不支持文件限速") {
        ErrorCode::UnsupportedCapability
    } else if is_storage_error(error) {
        ErrorCode::Storage
    } else {
        ErrorCode::Interrupted
    }
}
fn remote_error(code: ErrorCode) -> Error {
    disk::failure(match code {
        ErrorCode::SourceChanged => "源文件内容已变化",
        ErrorCode::SourceMissing => "源文件不可用",
        ErrorCode::Storage => "对端存储不可用",
        ErrorCode::Busy => "对端资源忙，请稍后继续",
        ErrorCode::UnsupportedCapability => "对端不支持文件限速保活，请更新软件或关闭文件限速",
        _ => "对端传输中断",
    })
}

struct ProgressReader<R> {
    inner: R,
    progress: watch::Sender<u64>,
}
impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buffer);
        let bytes = buffer.filled().len() - before;
        if bytes != 0 {
            self.progress
                .send_modify(|v| *v = v.wrapping_add(bytes as u64));
        }
        result
    }
}

struct TaskIo {
    send: SendStream,
    incoming: mpsc::Receiver<Result<crate::node::core::frame_budget::BufferedFrame>>,
    reader: JoinHandle<()>,
    gate: TaskProtocol,
    read_progress: watch::Receiver<u64>,
    pending: VecDeque<crate::node::core::frame_budget::BufferedFrame>,
    flow_pause: watch::Receiver<bool>,
    peer: NodeId,
    id: TaskId,
}
impl Drop for TaskIo {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
impl TaskIo {
    fn new(
        send: SendStream,
        recv: RecvStream,
        peer: NodeId,
        id: TaskId,
        budget: crate::node::core::frame_budget::FrameBudget,
    ) -> Self {
        let (tx, incoming) = mpsc::channel(2);
        let task_limit = crate::node::core::frame_budget::FrameBudget::task_limit();
        let (progress, read_progress) = watch::channel(0);
        let (pause_notice, flow_pause) = watch::channel(false);
        let mut recv = ProgressReader {
            inner: recv,
            progress,
        };
        let reader = tokio::spawn(async move {
            loop {
                let frame = budget.read(&mut recv, Some(task_limit.clone())).await;
                let failed = frame.is_err();
                if matches!(&frame, Ok(buffered) if matches!(buffered.frame.message, Message::Pause { .. }))
                {
                    pause_notice.send_replace(true);
                }
                if tx.send(frame).await.is_err() || failed {
                    break;
                }
            }
        });
        Self {
            send,
            incoming,
            reader,
            gate: TaskProtocol::new(peer),
            read_progress,
            pending: VecDeque::new(),
            flow_pause,
            peer,
            id,
        }
    }
    fn observe(&mut self, frame: &Frame) -> Result<()> {
        self.gate.observe(self.peer, Actor::Remote, frame)
    }
    async fn send(&mut self, message: Message) -> Result<()> {
        let frame = Frame {
            request_id: self.gate.next_request_id(&self.id, Actor::Local)?,
            message,
        };
        self.gate.observe(self.peer, Actor::Local, &frame)?;
        tokio::time::timeout(IDLE_TIMEOUT, protocol::write(&mut self.send, &frame))
            .await
            .map_err(|_| disk::failure("文件控制帧发送超时"))?
    }
    async fn send_chunk(
        &mut self,
        ticket: &crate::node::core::queue::WriteTicket,
        limit: &crate::node::core::bandwidth::ByteLimit,
        flow_supported: bool,
        pause: &mut watch::Receiver<bool>,
        message: Message,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let frame = Frame {
            request_id: self.gate.next_request_id(&self.id, Actor::Local)?,
            message,
        };
        self.gate.observe(self.peer, Actor::Local, &frame)?;
        let Message::Chunk { data, .. } = &frame.message else {
            return Err(disk::failure("字节调度仅接受分片数据"));
        };
        let encoded = frame.encode()?;
        // Chunk.data is the final postcard field. Keep framing/control bytes
        // outside the payload quantum; verify the layout instead of guessing.
        if !encoded.ends_with(data) {
            return Err(disk::failure("分片编码布局不受支持"));
        }
        let prefix = encoded.len() - data.len();
        drop(frame);
        // Framing is small and unthrottled. Payload I/O has a stall timeout
        // per bounded quantum; intentional budget waits do not consume it.
        tokio::time::timeout(IDLE_TIMEOUT, async {
            self.send
                .write_all(&(encoded.len() as u32).to_le_bytes())
                .await?;
            self.send.write_all(&encoded[..prefix]).await?;
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|_| disk::failure("文件帧头发送超时"))??;
        let mut offset = prefix;
        while offset < encoded.len() {
            if limit.enabled() && !flow_supported {
                return Err(disk::failure(
                    "对端不支持文件限速保活，请更新对端软件或关闭文件限速",
                ));
            }
            ticket.park();
            let maximum = encoded.len() - offset;
            let amount = if *self.flow_pause.borrow() {
                maximum
            } else {
                let acquire = limit.acquire(maximum, pause);
                tokio::pin!(acquire);
                loop {
                    tokio::select! {
                        amount = &mut acquire => break amount,
                        _ = self.flow_pause.changed() => break maximum,
                        frame = self.incoming.recv() => {
                            let frame = frame.ok_or_else(|| disk::failure("文件流已关闭"))??;
                            self.observe(&frame.frame)?;
                            let remote_pause = matches!(frame.frame.message, Message::Pause { .. });
                            if !matches!(frame.frame.message, Message::FlowWait { .. }) {
                                if self.pending.len() >= WINDOW * 2 + 4 {
                                    return Err(disk::failure("文件控制消息积压超限"));
                                }
                                self.pending.push_back(frame);
                            }
                            if remote_pause { break maximum; }
                        }
                    }
                }
            };
            tokio::time::timeout(
                IDLE_TIMEOUT,
                ticket.write_all(&mut self.send, &encoded[offset..offset + amount]),
            )
            .await
            .map_err(|_| disk::failure("文件数据写入停滞超时"))??;
            offset += amount;
        }
        tokio::time::timeout(IDLE_TIMEOUT, self.send.flush())
            .await
            .map_err(|_| disk::failure("文件数据刷新超时"))??;
        Ok(())
    }
    async fn wait_download(
        &mut self,
        limit: &crate::node::core::bandwidth::ByteLimit,
        bytes: usize,
        pause: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        if !limit.enabled() {
            return Ok(());
        }
        let mut remaining = bytes;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        heartbeat.tick().await;
        while remaining > 0 {
            if *self.flow_pause.borrow() || *pause.borrow() {
                break;
            }
            let acquire = limit.acquire(remaining, pause);
            tokio::pin!(acquire);
            let amount = loop {
                tokio::select! {
                    amount = &mut acquire => break amount,
                    _ = self.flow_pause.changed() => break remaining,
                    _ = heartbeat.tick() => {
                        self.send(Message::FlowWait { task_id: self.id.clone() }).await?;
                    }
                }
            };
            remaining -= amount;
        }
        Ok(())
    }
    fn finish(&mut self) {
        let _ = self.send.finish();
    }
    async fn report_error(&mut self, error: &Error) {
        let _ = self
            .send(Message::Error {
                task_id: self.id.clone(),
                code: error_code(error),
            })
            .await;
        self.finish();
    }
}
enum Input {
    Pause,
    Frame(crate::node::core::frame_budget::BufferedFrame),
}
async fn next_input(
    io: &mut TaskIo,
    pause: &mut watch::Receiver<bool>,
    pausing: Option<Instant>,
) -> Result<Input> {
    // A throttled chunk may consume watch::changed() while unblocking its
    // current payload. The pause flag is latched: observing that notification
    // in the byte limiter must not lose the protocol pause handshake.
    if pausing.is_none() && *pause.borrow_and_update() {
        return Ok(Input::Pause);
    }
    if let Some(frame) = io.pending.pop_front() {
        return Ok(Input::Frame(frame));
    }
    let timeout = pausing.map_or(IDLE_TIMEOUT, |started| {
        PAUSE_TIMEOUT.saturating_sub(started.elapsed())
    });
    if timeout.is_zero() {
        return Err(disk::failure("暂停确认超时"));
    }
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            changed=pause.changed()=> {changed.map_err(|_|disk::failure("暂停控制已关闭"))?;return Ok(Input::Pause)}
            frame=io.incoming.recv()=> {let frame=frame.ok_or_else(||disk::failure("文件流已关闭"))??;io.observe(&frame.frame)?;return Ok(Input::Frame(frame))}
            changed=io.read_progress.changed(), if pausing.is_none()=> {
                if changed.is_ok() { deadline.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT); }
            }
            _=&mut deadline=>return Err(disk::failure(if pausing.is_some(){"暂停确认超时"}else{"文件传输超时"})),
        }
    }
}

#[cfg(test)]
mod tests;
