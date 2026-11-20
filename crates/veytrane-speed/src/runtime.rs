//! Lease-bound memory speed business on an existing authenticated connection.
//! The session's sole BI reader routes control here; one UNI owner routes data.
use quinn::{Connection, RecvStream, SendStream};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};
use veytrane_node::live_authorization::RemoteAuthorization;
use veytrane_node::{
    activity::{Activity, ExclusivePermit},
    authorization::ResourcePolicy,
    dispatch::{BiStream, RESET_INVALID, RESET_UNAUTHORIZED, open_bi},
    frame_budget::{BufferedBytes, FrameBudget},
    live_authorization::LiveAuthorization,
    operation_id::TaskId,
    p2pf::MAX_FRAME_BYTES,
    wire::StreamKind,
};
use veytrane_transport::{
    error::{Error, Result},
    identity::NodeId,
    raw_frame::{read_raw_frame_limited, write_raw_frame_limited},
};

use crate::{
    control::{SpeedArbiter, SpeedControl, SpeedDirection, SpeedLease},
    memory::{receive_memory, send_memory},
    wire::SpeedFrame,
};
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_secs(30);
const BLOCK: usize = 64 * 1024;
const HEADER_LIMIT: u32 = 4096;
pub const DEFAULT_MAX_PEER_BUSINESS_STREAMS: usize = 8;

fn failure(message: &str) -> Error {
    Error::Protocol(message.into())
}

fn local_error(error: impl std::fmt::Display) -> Error {
    Error::Protocol(error.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub enum SpeedStatus {
    Running,
    Completed,
    Cancelled,
    Interrupted,
}
#[derive(Clone, Debug)]
#[allow(dead_code)] // T010 reads the domain snapshot.
pub struct SpeedSnapshot {
    pub test_id: TaskId,
    pub direction: SpeedDirection,
    pub owner: NodeId,
    pub seconds: u16,
    pub status: SpeedStatus,
    pub bytes: u64,
    pub elapsed: Duration,
    pub bytes_per_second: f64,
    pub rtt: Duration,
}
impl SpeedSnapshot {
    fn new(lease: &SpeedLease) -> Self {
        Self {
            test_id: lease.test_id.clone(),
            direction: lease.direction,
            owner: lease.owner,
            seconds: lease.seconds,
            status: SpeedStatus::Running,
            bytes: 0,
            elapsed: Duration::ZERO,
            bytes_per_second: 0.0,
            rtt: Duration::ZERO,
        }
    }
}

#[derive(Clone)]
pub struct SpeedService {
    peers: Arc<Mutex<HashMap<NodeId, SpeedPeer>>>,
    activity: Activity,
    changed: watch::Sender<u64>,
    frame_budget: FrameBudget,
}

impl SpeedService {
    pub fn new(activity: Activity, changed: watch::Sender<u64>, frame_budget: FrameBudget) -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            activity,
            changed,
            frame_budget,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn peer(&self, peer: NodeId) -> Option<SpeedPeer> {
        self.peer_internal(peer)
    }

    fn peer_internal(&self, peer: NodeId) -> Option<SpeedPeer> {
        self.peers.lock().unwrap().get(&peer).cloned()
    }

    pub async fn start(
        &self,
        peer: NodeId,
        direction: SpeedDirection,
        seconds: u16,
    ) -> Result<SpeedSnapshot> {
        self.peer_internal(peer)
            .ok_or_else(|| failure("请先连接测速对端"))?
            .start(direction, seconds)
            .await
    }

    pub fn cancel(&self, peer: NodeId, id: &TaskId) -> Result<()> {
        self.peer_internal(peer)
            .ok_or_else(|| failure("测速连接已断开"))?
            .cancel(id)
    }

    pub fn snapshots(&self) -> HashMap<NodeId, SpeedSnapshot> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(peer, speed)| speed.snapshot().map(|snapshot| (*peer, snapshot)))
            .collect()
    }

    pub fn revoke_authorization(&self, peer: NodeId, authorization: RemoteAuthorization) {
        if let Some(speed) = self.peer_internal(peer) {
            speed.revoke_authorization(authorization);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_duration(&self, peer: NodeId, duration: Duration) -> bool {
        if let Some(speed) = self.peer_internal(peer) {
            speed.set_test_duration(duration);
            true
        } else {
            false
        }
    }

    fn register(
        &self,
        connection: Connection,
        local: NodeId,
        remote: NodeId,
    ) -> SpeedPeerRegistration {
        let speed = SpeedPeer::new(
            connection,
            local,
            remote,
            self.activity.clone(),
            self.changed.clone(),
        );
        self.peers.lock().unwrap().insert(remote, speed.clone());
        SpeedPeerRegistration {
            service: self.clone(),
            peer: remote,
            speed,
        }
    }

    pub async fn serve_peer(
        &self,
        connection: Connection,
        peer: NodeId,
        local: NodeId,
        authorization: LiveAuthorization,
        resources: watch::Receiver<ResourcePolicy>,
        bi_budget: Arc<Semaphore>,
        mut incoming: mpsc::Receiver<BiStream>,
    ) -> Result<()> {
        let registration = self.register(connection.clone(), local, peer);
        let speed = registration.handle();
        let uni_slot = bi_budget
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure("测速并发槽已满"))?;
        let mut streams = JoinSet::new();
        {
            let speed = speed.clone();
            streams.spawn(async move {
                let _uni_slot = uni_slot;
                speed.serve_uni().await;
                Ok(())
            });
        }
        loop {
            tokio::select! {
                accepted = incoming.recv() => {
                    let Some(BiStream { mut send, mut recv }) = accepted else { break };
                    if !authorization.current().inbound_authorized() {
                        let _ = send.reset(4u32.into());
                        let _ = recv.stop(4u32.into());
                        continue;
                    }
                    let permit = match bi_budget.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            let _ = send.reset(4u32.into());
                            let _ = recv.stop(4u32.into());
                            continue;
                        }
                    };
                    let service = self.clone();
                    let speed = speed.clone();
                    let authorization = authorization.clone();
                    let resources = resources.clone();
                    streams.spawn(async move {
                        let _permit = permit;
                        let result = authorization.guard(true, async {
                            let buffered = tokio::time::timeout(
                                Duration::from_secs(5),
                                service.frame_budget.read_raw(&mut recv, None),
                            )
                            .await
                            .map_err(|_| failure("测速流首帧超时"))??;
                            let BufferedBytes { bytes, _lease } = buffered;
                            let first = match SpeedFrame::decode(&bytes) {
                                Ok(first) => first,
                                Err(_) => {
                                    let _ = send.reset(RESET_INVALID.into());
                                    let _ = recv.stop(RESET_INVALID.into());
                                    return Err(failure("节点测速流类型不符"));
                                }
                            };
                            drop(bytes);
                            let allowed = resources.borrow().for_peer(peer);
                            if allowed.disabled || !allowed.speed {
                                let _ = send.reset(RESET_UNAUTHORIZED.into());
                                let _ = recv.stop(RESET_UNAUTHORIZED.into());
                                return Err(failure("节点测速资源权限拒绝"));
                            }
                            drop(_lease);
                            speed.serve_control(send, recv, first).await
                        })
                        .await;
                        if let Err(error) = &result {
                            tracing::warn!(%error, "测速业务流终止");
                        }
                        result
                    });
                }
                result = streams.join_next(), if !streams.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::warn!(%error, "测速任务终止");
                    }
                }
            }
        }
        streams.abort_all();
        while streams.join_next().await.is_some() {}
        drop(registration);
        Ok(())
    }
}

struct SpeedPeerRegistration {
    service: SpeedService,
    peer: NodeId,
    speed: SpeedPeer,
}

impl SpeedPeerRegistration {
    fn handle(&self) -> SpeedPeer {
        self.speed.clone()
    }
}

impl Drop for SpeedPeerRegistration {
    fn drop(&mut self) {
        self.speed.cancel_all();
        let mut peers = self.service.peers.lock().unwrap();
        if peers
            .get(&self.peer)
            .is_some_and(|speed| speed.same_connection(&self.speed))
        {
            peers.remove(&self.peer);
        }
    }
}

#[derive(Clone)]
pub struct SpeedPeer(Arc<Peer>);
struct Peer {
    connection: Connection,
    local: NodeId,
    remote: NodeId,
    activity: Activity,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    requesting: std::sync::atomic::AtomicBool,
    #[cfg(any(test, feature = "test-support"))]
    duration: Mutex<Option<Duration>>,
    #[cfg(any(test, feature = "test-support"))]
    data_gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    #[cfg(any(test, feature = "test-support"))]
    cleanup_gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    #[cfg(any(test, feature = "test-support"))]
    wait_gate: Mutex<Option<oneshot::Sender<()>>>,
}
struct State {
    arbiter: SpeedArbiter,
    active: Option<Active>,
    snapshot: Option<SpeedSnapshot>,
}
struct Active {
    lease: SpeedLease,
    cancel: watch::Sender<bool>,
    uni: Option<oneshot::Sender<RecvStream>>,
    _permit: ExclusivePermit,
    finishing: bool,
}
struct RunGuard {
    peer: SpeedPeer,
    lease: SpeedLease,
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        let old = {
            let mut state = self.peer.0.state.lock().unwrap();
            if state.active.as_ref().is_none_or(|a| a.lease != self.lease) {
                return;
            }
            let _ = state.arbiter.finish(self.peer.0.local, &self.lease);
            if let Some(snapshot) = state.snapshot.as_mut()
                && snapshot.status == SpeedStatus::Running
            {
                snapshot.status = SpeedStatus::Interrupted;
                snapshot.bytes_per_second = 0.0;
            }
            state.active.take()
        };
        drop(old);
        self.peer.notify();
    }
}
struct DataSend(SendStream);
impl Drop for DataSend {
    fn drop(&mut self) {
        let _ = self.0.reset(5u32.into());
    }
}
struct DataRecv(RecvStream);
impl Drop for DataRecv {
    fn drop(&mut self) {
        let _ = self.0.stop(5u32.into());
    }
}

impl SpeedPeer {
    fn new(
        connection: Connection,
        local: NodeId,
        remote: NodeId,
        activity: Activity,
        changed: watch::Sender<u64>,
    ) -> Self {
        Self(Arc::new(Peer {
            connection,
            local,
            remote,
            activity,
            changed,
            requesting: std::sync::atomic::AtomicBool::new(false),
            state: Mutex::new(State {
                arbiter: SpeedArbiter::new(local, remote),
                active: None,
                snapshot: None,
            }),
            #[cfg(any(test, feature = "test-support"))]
            duration: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            data_gate: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            cleanup_gate: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            wait_gate: Mutex::new(None),
        }))
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_duration(&self, duration: Duration) {
        *self.0.duration.lock().unwrap() = Some(duration);
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_lease(&self) -> Option<SpeedLease> {
        self.0
            .state
            .lock()
            .unwrap()
            .active
            .as_ref()
            .map(|a| a.lease.clone())
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn gate_data(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached_tx, reached) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        *self.0.data_gate.lock().unwrap() = Some((reached_tx, release_rx));
        (reached, release)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn gate_cleanup(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (tx, reached) = oneshot::channel();
        let (release, rx) = oneshot::channel();
        *self.0.cleanup_gate.lock().unwrap() = Some((tx, rx));
        (reached, release)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn gate_wait(&self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        *self.0.wait_gate.lock().unwrap() = Some(tx);
        rx
    }
    fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    fn notify(&self) {
        self.0.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    fn snapshot(&self) -> Option<SpeedSnapshot> {
        self.0.state.lock().unwrap().snapshot.clone()
    }
    fn cancel(&self, id: &TaskId) -> Result<()> {
        let state = self.0.state.lock().unwrap();
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| failure("当前没有测速"))?;
        if &active.lease.test_id != id {
            return Err(failure("测速任务已改变"));
        }
        active.cancel.send_replace(true);
        Ok(())
    }
    fn cancel_all(&self) {
        if let Some(active) = self.0.state.lock().unwrap().active.as_ref() {
            active.cancel.send_replace(true);
        }
    }
    fn revoke_authorization(&self, authorization: RemoteAuthorization) {
        if let Some(active) = self.0.state.lock().unwrap().active.as_ref() {
            let authorized = if active.lease.owner == self.0.local {
                authorization.outbound_authorized()
            } else {
                authorization.inbound_authorized()
            };
            if !authorized {
                active.cancel.send_replace(true);
            }
        }
    }
    fn prepare(
        &self,
        owner: NodeId,
        id: TaskId,
        direction: SpeedDirection,
        seconds: u16,
        granted: Option<SpeedLease>,
    ) -> Result<(
        RunGuard,
        watch::Receiver<bool>,
        Option<oneshot::Receiver<RecvStream>>,
    )> {
        let mut state = self.0.state.lock().unwrap();
        let permit = self
            .0
            .activity
            .exclusive(self.0.remote)
            .map_err(|_| failure("文件或测速正在进行，请先暂停文件或取消测速"))?;
        let lease = match granted {
            Some(lease) => {
                state
                    .arbiter
                    .accept_grant(self.0.remote, lease.clone(), 0)?;
                lease
            }
            None => state.arbiter.request(owner, id, direction, seconds, 0)?,
        };
        let (cancel, rx) = watch::channel(false);
        let receiver = self.source(&lease) != self.0.local;
        let (uni, uni_rx) = if receiver {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        state.snapshot = Some(SpeedSnapshot::new(&lease));
        state.active = Some(Active {
            lease: lease.clone(),
            cancel,
            uni,
            _permit: permit,
            finishing: false,
        });
        drop(state);
        self.notify();
        Ok((
            RunGuard {
                peer: self.clone(),
                lease,
            },
            rx,
            uni_rx,
        ))
    }
    fn finishing(&self, lease: &SpeedLease) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(active) = state.active.as_mut()
            && active.lease == *lease
        {
            active.finishing = true;
        }
        drop(state);
        self.notify();
    }
    async fn wait_finishing(&self) -> Result<()> {
        let mut changed = self.0.changed.subscribe();
        tokio::time::timeout(CONTROL_TIMEOUT, async {
            loop {
                let finishing = {
                    let state = self.0.state.lock().unwrap();
                    state.active.as_ref().is_some_and(|a| a.finishing)
                };
                if !finishing {
                    return Ok(());
                }
                #[cfg(any(test, feature = "test-support"))]
                if let Some(reached) = self.0.wait_gate.lock().unwrap().take() {
                    let _ = reached.send(());
                }

                changed
                    .changed()
                    .await
                    .map_err(|_| failure("测速清理通知关闭"))?;
            }
        })
        .await
        .map_err(|_| failure("上次测速清理超时"))?
    }
    fn source(&self, lease: &SpeedLease) -> NodeId {
        match lease.direction {
            SpeedDirection::Upload => lease.owner,
            SpeedDirection::Download if lease.owner == self.0.local => self.0.remote,
            SpeedDirection::Download => self.0.local,
        }
    }
    fn duration(&self, lease: &SpeedLease) -> Duration {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(duration) = *self.0.duration.lock().unwrap() {
            return duration;
        }
        Duration::from_secs(u64::from(lease.seconds))
    }
    /// Requester on the higher node sends Request to the one lower coordinator.
    /// The lower requester's first frame is its own already-reserved grant.
    async fn start(&self, direction: SpeedDirection, seconds: u16) -> Result<SpeedSnapshot> {
        use std::sync::atomic::Ordering;
        if self.0.requesting.swap(true, Ordering::AcqRel) {
            return Err(failure("测速请求正在进行"));
        }
        struct RequestGuard(SpeedPeer);
        impl Drop for RequestGuard {
            fn drop(&mut self) {
                self.0.0.requesting.store(false, Ordering::Release);
            }
        }
        let _request = RequestGuard(self.clone());
        self.wait_finishing().await?;
        if !self.0.activity.lock().can_exclusive(self.0.remote) {
            return Err(failure("文件或测速正在进行，请先暂停文件或取消测速"));
        }
        // Validate even if the other peer never answers.
        let id = TaskId::generate();
        let request = SpeedControl::Request {
            test_id: id.clone(),
            direction,
            seconds,
        };
        SpeedFrame {
            request_id: 1,
            control: request.clone(),
        }
        .encode()?;
        let BiStream { mut send, mut recv } = tokio::time::timeout(
            CONTROL_TIMEOUT,
            open_bi(&self.0.connection, StreamKind::Speed),
        )
        .await
        .map_err(|_| failure("测速控制流打开超时"))?
        .map_err(local_error)?;
        let prepared = if self.0.local < self.0.remote {
            let prepared = self.prepare(self.0.local, id, direction, seconds, None)?;
            write(
                &mut send,
                1,
                SpeedControl::Granted(prepared.0.lease.clone()),
            )
            .await?;
            prepared
        } else {
            write(&mut send, 1, request).await?;
            let lease = match read(&mut recv, 1).await? {
                SpeedControl::Granted(lease)
                    if lease.test_id == id
                        && lease.owner == self.0.local
                        && lease.direction == direction
                        && lease.seconds == seconds =>
                {
                    lease
                }
                SpeedControl::Busy { test_id } if test_id == id => {
                    return Err(failure("对端文件或测速正在进行"));
                }
                _ => return Err(failure("测速授权身份或顺序不符")),
            };
            match self.prepare(self.0.local, id.clone(), direction, seconds, Some(lease)) {
                Ok(prepared) => prepared,
                Err(error) => {
                    write(&mut send, 2, SpeedControl::Busy { test_id: id }).await?;
                    let _ = send.finish();
                    return Err(error);
                }
            }
        };
        self.run(send, recv, prepared).await
    }
    /// Called by the existing single authenticated BI dispatcher, never another accept loop.
    async fn serve_control(
        &self,
        mut send: SendStream,
        recv: RecvStream,
        first: SpeedFrame,
    ) -> Result<()> {
        if first.request_id != 1 {
            return Err(failure("测速首帧编号非法"));
        }
        let control = first.control;
        let (id, direction, seconds, grant) = match control {
            SpeedControl::Request {
                test_id,
                direction,
                seconds,
            } if self.0.local < self.0.remote => (test_id, direction, seconds, None),
            SpeedControl::Granted(lease)
                if self.0.local > self.0.remote && lease.owner == self.0.remote =>
            {
                (
                    lease.test_id.clone(),
                    lease.direction,
                    lease.seconds,
                    Some(lease),
                )
            }
            _ => return Err(failure("测速请求协调者或归属不符")),
        };
        self.wait_finishing().await?;
        let was_request = grant.is_none();
        let prepared = match self.prepare(self.0.remote, id.clone(), direction, seconds, grant) {
            Ok(prepared) => prepared,
            Err(error) => {
                write(
                    &mut send,
                    if was_request { 1 } else { 2 },
                    SpeedControl::Busy { test_id: id },
                )
                .await?;
                let _ = send.finish();
                return Err(error);
            }
        };
        if was_request {
            write(
                &mut send,
                1,
                SpeedControl::Granted(prepared.0.lease.clone()),
            )
            .await?;
        }
        self.run(send, recv, prepared).await.map(|_| ())
    }
    /// One owner per authenticated connection. Slow/malformed headers are bounded
    /// independent jobs; an old token is stopped, never delivered to the new run.
    async fn serve_uni(&self) {
        let mut headers = JoinSet::new();
        loop {
            tokio::select! {
                accepted=self.0.connection.accept_uni()=> {
                    let Ok(mut recv)=accepted else { break; };
                    if headers.len()>=8 { let _=recv.stop(5u32.into());continue; }
                    let peer=self.clone();
                    headers.spawn(async move {
                        let header=read(&mut recv,1).await;
                        if let Ok(SpeedControl::Granted(lease))=header {
                            let sender={
                                let mut state=peer.0.state.lock().unwrap();
                                if state.active.as_ref().is_some_and(|a|a.lease==lease && !*a.cancel.borrow()) && state.arbiter.claim_stream(peer.0.remote,&lease).is_ok() {
                                    state.active.as_mut().unwrap().uni.take()
                                } else { None }
                            };
                            if let Some(sender)=sender { match sender.send(recv) { Ok(())=>return,Err(returned)=>recv=returned } }
                        }
                        let _=recv.stop(5u32.into());
                    });
                }
                _=headers.join_next(),if !headers.is_empty()=>{}
            }
        }
        headers.abort_all();
        while headers.join_next().await.is_some() {}
    }
    fn observe(&self, lease: &SpeedLease, bytes: u64, elapsed: Duration, force: bool) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(snapshot) = state.snapshot.as_mut()
            && snapshot.test_id == lease.test_id
            && (force || elapsed >= snapshot.elapsed + Duration::from_secs(1))
        {
            snapshot.bytes = bytes;
            snapshot.elapsed = elapsed;
            snapshot.rtt = self.0.connection.rtt();
            snapshot.bytes_per_second = bytes as f64 / elapsed.as_secs_f64().max(0.000001);
            drop(state);
            self.notify();
        }
    }
    async fn data(
        &self,
        lease: &SpeedLease,
        uni: Option<oneshot::Receiver<RecvStream>>,
    ) -> Result<(u64, Duration)> {
        let duration = self.duration(lease);
        if self.source(lease) == self.0.local {
            #[cfg(any(test, feature = "test-support"))]
            {
                let gate = self.0.data_gate.lock().unwrap().take();
                if let Some((reached, release)) = gate {
                    let _ = reached.send(());
                    let _ = release.await;
                }
            }
            {
                self.0
                    .state
                    .lock()
                    .unwrap()
                    .arbiter
                    .claim_stream(self.0.local, lease)?;
            }
            let stream = tokio::time::timeout(CONTROL_TIMEOUT, self.0.connection.open_uni())
                .await
                .map_err(|_| failure("测速数据流打开超时"))?
                .map_err(local_error)?;
            let mut stream = DataSend(stream);
            write(&mut stream.0, 1, SpeedControl::Granted(lease.clone())).await?;
            let result = send_memory(&mut stream.0, duration, BLOCK, |bytes, elapsed| {
                self.observe(lease, bytes, elapsed, false)
            })
            .await?;
            stream.0.finish().map_err(local_error)?;
            // Finish acknowledgement before the RAII reset wrapper is dropped.
            tokio::time::timeout(IDLE, stream.0.stopped())
                .await
                .map_err(|_| failure("测速发送清理超时"))?
                .map_err(local_error)?;
            Ok(result)
        } else {
            let recv = tokio::time::timeout(
                CONTROL_TIMEOUT,
                uni.ok_or_else(|| failure("测速接收归属不符"))?,
            )
            .await
            .map_err(|_| failure("测速数据首帧超时"))?
            .map_err(local_error)?;
            let mut recv = DataRecv(recv);
            tokio::time::timeout(
                duration + IDLE,
                receive_memory(&mut recv.0, BLOCK, |bytes, elapsed| {
                    self.observe(lease, bytes, elapsed, false)
                }),
            )
            .await
            .map_err(|_| failure("测速超过声明时长"))?
        }
    }
    async fn run(
        &self,
        mut send: SendStream,
        mut recv: RecvStream,
        prepared: (
            RunGuard,
            watch::Receiver<bool>,
            Option<oneshot::Receiver<RecvStream>>,
        ),
    ) -> Result<SpeedSnapshot> {
        let (guard, mut cancel, uni) = prepared;
        let lease = guard.lease.clone();
        // Grant/Request is frame1, Ready frame2 on both endpoints. A granted
        // endpoint must reserve its file gate before either sends data.
        write(&mut send, 2, SpeedControl::Ready(lease.clone())).await?;
        match read(&mut recv, 2).await? {
            SpeedControl::Ready(other) if other == lease => {}
            SpeedControl::Busy { test_id } if test_id == lease.test_id => {
                return Err(failure("对端文件或测速正在进行"));
            }
            _ => return Err(failure("测速 Ready 身份或顺序不符")),
        }
        let (tx, mut messages) = mpsc::channel(4);
        let mut readers = JoinSet::new();
        let control_wait = self.duration(&lease) + IDLE + CONTROL_TIMEOUT;
        readers.spawn(async move {
            let mut sequence = 3;
            loop {
                let control = read_for(&mut recv, sequence, control_wait).await;
                let failed = control.is_err();
                if tx.send(control).await.is_err() || failed {
                    break;
                }
                sequence += 1;
            }
        });
        let outcome = tokio::time::timeout(self.duration(&lease)+IDLE+IDLE, async {
            let data=self.data(&lease,uni);tokio::pin!(data);
            let mut remote_result=None;
            let local=loop {
                tokio::select! {
                    biased;
                    _=cancel.changed()=> {self.finishing(&lease);cancel_request(&mut send,3,&lease,&mut messages).await?;return Err(failure("测速已取消"));}
                    message=messages.recv()=> match message.ok_or_else(||failure("测速控制流关闭"))?? {
                        SpeedControl::Cancel(other) if other==lease=> {self.finishing(&lease);cancel_ack(&mut send,3,&lease).await?;return Err(failure("测速已取消"));},
                        SpeedControl::Result {lease:other,bytes,elapsed_ms} if other==lease && remote_result.is_none()=>remote_result=Some((bytes,elapsed_ms)),
                        _=>return Err(failure("测速数据阶段收到非法控制消息")),
                    },
                    result=&mut data=>break result?,
                }
            };
            let receiver=self.source(&lease)!=self.0.local;
            let local_ms=local.1.as_millis().max(1).min(u128::from(u64::MAX)) as u64;
            write(&mut send,3,SpeedControl::Result { lease:lease.clone(),bytes:local.0,elapsed_ms:local_ms }).await?;
            let remote=match remote_result {
                Some(result)=>result,
                None=>tokio::select! {
                    _=cancel.changed()=> {self.finishing(&lease);cancel_request(&mut send,4,&lease,&mut messages).await?;return Err(failure("测速已取消"));}
                    message=messages.recv()=>match message.ok_or_else(||failure("测速结果流关闭"))?? {
                        SpeedControl::Result {lease:other,bytes,elapsed_ms} if other==lease=>(bytes,elapsed_ms),
                        SpeedControl::Cancel(other) if other==lease=> {self.finishing(&lease);cancel_ack(&mut send,4,&lease).await?;return Err(failure("测速已取消"));},
                        _=>return Err(failure("测速结果身份或顺序不符")),
                    }
                }
            };
            if remote.0!=local.0 || remote.0==0 || remote.1==0 || remote.1>u64::from(lease.seconds)*1000+60000 { return Err(failure("测速结果字节数或耗时不符")); }
            let result=if receiver {(local.0,Duration::from_millis(local_ms))}else{(remote.0,Duration::from_millis(remote.1))};
            // Results validated on both sides. A matching Finished is the
            // completion commit; late local Cancel cannot undo it.
            self.finishing(&lease);
            write(&mut send,4,SpeedControl::Finished(lease.clone())).await?;
            match messages.recv().await.ok_or_else(||failure("测速完成流关闭"))?? {
                SpeedControl::Finished(other) if other==lease=>{},
                SpeedControl::Cancel(other) if other==lease=> {cancel_ack(&mut send,5,&lease).await?;return Err(failure("测速已取消"));},
                _=>return Err(failure("测速完成身份或顺序不符")),
            }
            let _=send.finish();Ok(result)
        }).await.unwrap_or_else(|_|Err(failure("测速会话超过声明时长")));
        readers.abort_all();
        while readers.join_next().await.is_some() {}
        match outcome {
            Ok((bytes, elapsed)) => {
                self.observe(&lease, bytes, elapsed, true);
                {
                    let mut state = self.0.state.lock().unwrap();
                    state.snapshot.as_mut().unwrap().status = SpeedStatus::Completed;
                }
                self.notify();
                #[cfg(any(test, feature = "test-support"))]
                {
                    let gate = self.0.cleanup_gate.lock().unwrap().take();
                    if let Some((reached, release)) = gate {
                        let _ = reached.send(());
                        let _ = release.await;
                    }
                }
                Ok(self.snapshot().unwrap())
            }
            Err(error) => {
                let _ = send.reset(5u32.into());
                let mut state = self.0.state.lock().unwrap();
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.status = if error.to_string().contains("已取消") {
                        SpeedStatus::Cancelled
                    } else {
                        SpeedStatus::Interrupted
                    };
                    snapshot.bytes_per_second = 0.0;
                }
                drop(state);
                self.notify();
                Err(error)
            }
        }
    }
}
// Keep the owned data future alive, unpolled, until the cancellation is
// acknowledged. Resetting a UNI before Cancel is observed can otherwise race
// the control stream and turn an intentional cancellation into a network error.
async fn cancel_request(
    send: &mut SendStream,
    sequence: u64,
    lease: &SpeedLease,
    messages: &mut mpsc::Receiver<Result<SpeedControl>>,
) -> Result<()> {
    write(send, sequence, SpeedControl::Cancel(lease.clone())).await?;
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        loop {
            match messages
                .recv()
                .await
                .ok_or_else(|| failure("测速取消确认流关闭"))??
            {
                SpeedControl::Finished(other) if other == *lease => break,
                SpeedControl::Cancel(other) if other == *lease => {
                    return cancel_ack(send, sequence + 1, lease).await;
                }
                SpeedControl::Result { lease: other, .. } if other == *lease => {}
                _ => return Err(failure("测速取消确认身份不符")),
            }
        }
        send.finish().map_err(local_error)?;
        send.stopped().await.map_err(local_error)?;
        Ok(())
    })
    .await
    .map_err(|_| failure("测速取消确认超时"))?
}
async fn cancel_ack(send: &mut SendStream, sequence: u64, lease: &SpeedLease) -> Result<()> {
    write(send, sequence, SpeedControl::Finished(lease.clone())).await?;
    send.finish().map_err(local_error)?;
    tokio::time::timeout(CONTROL_TIMEOUT, send.stopped())
        .await
        .map_err(|_| failure("测速取消回执清理超时"))?
        .map_err(local_error)?;
    Ok(())
}
async fn write<W: tokio::io::AsyncWrite + Unpin>(
    send: &mut W,
    sequence: u64,
    control: SpeedControl,
) -> Result<()> {
    let frame = SpeedFrame {
        request_id: sequence,
        control,
    };
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        write_raw_frame_limited(send, &frame.encode()?, MAX_FRAME_BYTES).await
    })
    .await
    .map_err(|_| failure("测速控制消息发送超时"))?
}
async fn read<R: tokio::io::AsyncRead + Unpin>(
    recv: &mut R,
    sequence: u64,
) -> Result<SpeedControl> {
    read_for(recv, sequence, CONTROL_TIMEOUT).await
}
async fn read_for<R: tokio::io::AsyncRead + Unpin>(
    recv: &mut R,
    sequence: u64,
    timeout: Duration,
) -> Result<SpeedControl> {
    let bytes = tokio::time::timeout(timeout, read_raw_frame_limited(recv, HEADER_LIMIT))
        .await
        .map_err(|_| failure("测速控制消息读取超时"))??
        .ok_or_else(|| failure("测速控制流提前关闭"))?;
    let frame = SpeedFrame::decode(&bytes)?;
    if frame.request_id != sequence {
        return Err(failure("测速控制编号不符"));
    }
    Ok(frame.control)
}
