use super::*;
use crate::{
    identity::Identity,
    transport::{
        handshake::{handshake_initiator, handshake_responder},
        quic::{ChannelBinding, client_endpoint, server_endpoint},
    },
};
use std::fs;
struct Pair {
    root: PathBuf,
    a: TransferService,
    b: TransferService,
    ca: Connection,
    cb: Connection,
    ia: NodeId,
    ib: NodeId,
    key_a: Identity,
    key_b: Identity,
    ea: quinn::Endpoint,
    eb: quinn::Endpoint,
    tasks: JoinSet<Result<()>>,
}
impl Pair {
    async fn new() -> Self {
        Self::new_inner(false).await
    }
    async fn new_inner(speed: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("p2p-desktop-transfer-{}", rand::random::<u128>()));
        fs::create_dir(&root).unwrap();
        for dir in ["a-receive", "b-receive"] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        let (store_a, _) = TaskStore::open(&root.join("a-state/tasks.json")).unwrap();
        let (store_b, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
        let a = TransferService::new(store_a, root.join("a-receive"));
        let b = TransferService::new(store_b, root.join("b-receive"));
        let identity_a = Identity::generate();
        let identity_b = Identity::generate();
        let key_a = identity_a.clone();
        let key_b = identity_b.clone();
        let ia = identity_a.node_id();
        let ib = identity_b.node_id();
        a.set_peer_incremental_support(ib, true);
        b.set_peer_incremental_support(ia, true);
        a.set_peer_flow_support(ib, true);
        b.set_peer_flow_support(ia, true);
        let ea = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let eb = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let server = eb.clone();
        let remote = tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            let binding = ChannelBinding::from_connection(&connection).unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let outcome = handshake_responder(&mut send, &mut recv, &identity_b, &binding)
                .await
                .unwrap();
            assert_eq!(outcome.peer_node_id, ia);
            send.finish().unwrap();
            crate::node::core::wire::negotiate(
                &connection,
                false,
                crate::node::core::wire::capabilities(true, true, true),
            )
            .await
            .unwrap();
            connection
        });
        let ca = crate::transport::quic::connect(&ea, eb.local_addr().unwrap(), "p2pfile")
            .await
            .unwrap();
        let binding = ChannelBinding::from_connection(&ca).unwrap();
        let (mut send, mut recv) = ca.open_bi().await.unwrap();
        let outcome = handshake_initiator(&mut send, &mut recv, &identity_a, &binding)
            .await
            .unwrap();
        assert_eq!(outcome.peer_node_id, ib);
        send.finish().unwrap();
        crate::node::core::wire::negotiate(
            &ca,
            true,
            crate::node::core::wire::capabilities(true, true, true),
        )
        .await
        .unwrap();
        let cb = remote.await.unwrap();
        let mut tasks = JoinSet::new();
        let service = a.clone();
        let conn = ca.clone();
        tasks.spawn(async move {
            if speed {
                service.serve_peer_with_speed(conn, ib, ia).await
            } else {
                service.serve_peer(conn, ib).await
            }
        });
        let service = b.clone();
        let conn = cb.clone();
        tasks.spawn(async move {
            if speed {
                service.serve_peer_with_speed(conn, ia, ib).await
            } else {
                service.serve_peer(conn, ia).await
            }
        });
        if speed {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if a.speed_peers.lock().unwrap().contains_key(&ib)
                        && b.speed_peers.lock().unwrap().contains_key(&ia)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            a.speed_peers.lock().unwrap()[&ib].set_test_duration(Duration::from_millis(80));
            b.speed_peers.lock().unwrap()[&ia].set_test_duration(Duration::from_millis(80));
        }
        Self {
            root,
            a,
            b,
            ca,
            cb,
            ia,
            ib,
            key_a,
            key_b,
            ea,
            eb,
            tasks,
        }
    }
    async fn reconnect(&mut self) {
        self.ca.close(0u32.into(), b"fixture loss");
        self.ca.closed().await;
        self.cb.closed().await;
        while self.tasks.join_next().await.is_some() {}
        let server = self.eb.clone();
        let identity = self.key_b.clone();
        let peer = self.ia;
        let accept = tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            let binding = ChannelBinding::from_connection(&connection).unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            assert_eq!(
                handshake_responder(&mut send, &mut recv, &identity, &binding)
                    .await
                    .unwrap()
                    .peer_node_id,
                peer
            );
            send.finish().unwrap();
            crate::node::core::wire::negotiate(
                &connection,
                false,
                crate::node::core::wire::capabilities(true, true, true),
            )
            .await
            .unwrap();
            connection
        });
        self.ca =
            crate::transport::quic::connect(&self.ea, self.eb.local_addr().unwrap(), "p2pfile")
                .await
                .unwrap();
        let binding = ChannelBinding::from_connection(&self.ca).unwrap();
        let (mut send, mut recv) = self.ca.open_bi().await.unwrap();
        assert_eq!(
            handshake_initiator(&mut send, &mut recv, &self.key_a, &binding)
                .await
                .unwrap()
                .peer_node_id,
            self.ib
        );
        send.finish().unwrap();
        crate::node::core::wire::negotiate(
            &self.ca,
            true,
            crate::node::core::wire::capabilities(true, true, true),
        )
        .await
        .unwrap();
        self.cb = accept.await.unwrap();
        for (service, connection, peer) in [
            (self.a.clone(), self.ca.clone(), self.ib),
            (self.b.clone(), self.cb.clone(), self.ia),
        ] {
            self.tasks
                .spawn(async move { service.serve_peer(connection, peer).await });
        }
    }
    async fn select(&self, length: usize) -> (TaskId, Vec<u8>) {
        let bytes = (0..length).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let path = self.root.join("文件.bin");
        fs::write(&path, &bytes).unwrap();
        let id = self.a.select_file(self.ib, path).await.unwrap();
        (id, bytes)
    }
    async fn shutdown(mut self) {
        self.ca.close(0u32.into(), b"test done");
        self.cb.close(0u32.into(), b"test done");
        while self.tasks.join_next().await.is_some() {}
        self.ea.close(0u32.into(), b"test done");
        self.eb.close(0u32.into(), b"test done");
        self.ea.wait_idle().await;
        self.eb.wait_idle().await;
        drop(self.a);
        drop(self.b);
        fs::remove_dir_all(self.root).unwrap();
    }
    fn sender(&self, id: TaskId) -> JoinHandle<Result<()>> {
        let service = self.a.clone();
        let conn = self.ca.clone();
        let peer = self.ib;
        tokio::spawn(async move { service.send_file(&conn, peer, id).await })
    }
    fn gate(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *self.b.first_chunk_gate.lock().unwrap() = Some((reached_tx, release_rx));
        (reached, release)
    }
}
#[tokio::test]
async fn drop_batch_preflight_failure_and_epoch_change_cannot_admit_remaining_paths() {
    let pair = Pair::new().await;
    let file = pair.root.join("first.bin");
    fs::write(&file, b"selected file").unwrap();
    let directory = pair.root.join("directory");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("child.bin"), b"child").unwrap();
    assert!(
        pair.a
            .select_paths(pair.ib, vec![file.clone(), pair.root.join("missing")])
            .await
            .is_err()
    );
    assert!(pair.a.snapshot().await.unwrap().is_empty());
    let (reached_tx, reached) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    *pair.a.admission_gate.lock().unwrap() = Some((reached_tx, release_rx));
    let service = pair.a.clone();
    let peer = pair.ib;
    let selection =
        tokio::spawn(async move { service.select_paths(peer, vec![file, directory]).await });
    reached.await.unwrap();
    pair.a.interrupt_all().await.unwrap();
    release.send(()).unwrap();
    assert!(
        selection
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("网络会话已重建")
    );
    let records = pair.a.snapshot().await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state(), TaskState::Interrupted);
    assert_eq!(pair.a.queue_metrics().pending, 0);
    assert!(pair.b.snapshot().await.unwrap().is_empty());
    pair.shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn drop_batch_directory_scan_failure_keeps_previously_admitted_tasks_visible() {
    let pair = Pair::new().await;
    let first = pair.root.join("first.bin");
    fs::write(&first, b"first").unwrap();
    let directory = pair.root.join("invalid-directory");
    fs::create_dir(&directory).unwrap();
    std::os::unix::fs::symlink(&first, directory.join("link.bin")).unwrap();
    let error = pair
        .a
        .select_paths(pair.ib, vec![first, directory])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("已加入 1 个任务并保留"));
    let records = pair.a.snapshot().await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state(), TaskState::Queued);
    assert_eq!(pair.a.queue_metrics().pending, 1);
    pair.shutdown().await;
}
#[tokio::test]
async fn receive_space_precheck_reports_refusal_and_manual_retry_preserves_task() {
    for query_fails in [false, true] {
        let pair = Pair::new().await;
        pair.b.receive_space.set_safety_mib(0).unwrap();
        *pair.b.receive_space.probe_override.lock().unwrap() =
            Some(if query_fails { Err(()) } else { Ok(0) });
        let (id, bytes) = pair.select(128 * 1024).await;
        assert!(pair.sender(id.clone()).await.unwrap().is_err());
        wait_state(&pair.b, &id, TaskState::Failed).await;
        pair.b.wait_previous_attempt(&id).await.unwrap();
        let refused = pair.b.task(id.clone()).await.unwrap();
        assert_eq!(
            refused.diagnostic().unwrap().code(),
            if query_fails {
                TaskErrorCode::SpaceCheckUnavailable
            } else {
                TaskErrorCode::DiskFull
            }
        );
        assert!(refused.diagnostic().unwrap().is_retryable());
        assert!(
            !disk::stage_dir(&refused)
                .unwrap()
                .join("data.part")
                .exists()
        );
        assert!(!pair.root.join("b-receive/文件.bin").exists());
        assert_eq!(pair.b.receive_space.summary().reserved, 0);
        *pair.b.receive_space.probe_override.lock().unwrap() = None;
        pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
        wait_state(&pair.a, &id, TaskState::Completed).await;
        wait_state(&pair.b, &id, TaskState::Completed).await;
        pair.b.wait_previous_attempt(&id).await.unwrap();
        assert_eq!(
            fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
            bytes
        );
        assert_eq!(pair.b.receive_space.summary().reserved, 0);
        pair.shutdown().await;
    }
}
#[tokio::test]
async fn receive_space_concurrent_transfers_release_on_completion_pause_and_disconnect() {
    for termination in 0..3 {
        let mut pair = Pair::new().await;
        pair.a.set_send_limit(2).unwrap();
        pair.b.receive_space.set_safety_mib(0).unwrap();
        let (id, bytes) = pair.select(8 * 1024 * 1024).await;
        let (reached, release) = pair.gate();
        let sender = pair.sender(id.clone());
        reached.await.unwrap();
        let held = pair.b.receive_space.summary().reserved;
        assert!(held >= crate::node::core::space_budget::TASK_OVERHEAD);
        assert!(held < bytes.len() as u64 + crate::node::core::space_budget::TASK_OVERHEAD);
        *pair.b.receive_space.probe_override.lock().unwrap() = Some(Ok(held));
        let other_path = pair.root.join("other.bin");
        fs::write(&other_path, b"another file").unwrap();
        let other = pair.a.select_file(pair.ib, other_path).await.unwrap();
        assert!(pair.sender(other.clone()).await.unwrap().is_err());
        wait_state(&pair.b, &other, TaskState::Failed).await;
        assert_eq!(
            pair.b
                .task(other.clone())
                .await
                .unwrap()
                .diagnostic()
                .unwrap()
                .code(),
            TaskErrorCode::DiskFull
        );
        *pair.b.receive_space.probe_override.lock().unwrap() = None;
        match termination {
            0 => {
                release.send(()).unwrap();
                sender.await.unwrap().unwrap();
            }
            1 => {
                pair.b.pause_task(id.clone()).await.unwrap();
                release.send(()).unwrap();
                sender.await.unwrap().unwrap();
                wait_state(&pair.b, &id, TaskState::Paused).await;
            }
            _ => {
                pair.a.network_lost(pair.ib);
                pair.b.network_lost(pair.ia);
                pair.ca.close(0u32.into(), b"fixture interruption");
                assert!(sender.await.unwrap().is_err());
                pair.cb.closed().await;
                while pair.tasks.join_next().await.is_some() {}
                drop(release);
                pair.a.settle_network_loss(pair.ib).await.unwrap();
                pair.b.settle_network_loss(pair.ia).await.unwrap();
            }
        }
        pair.b.wait_previous_attempt(&id).await.unwrap();
        pair.b.wait_previous_attempt(&other).await.unwrap();
        assert_eq!(pair.b.receive_space.summary().reserved, 0);
        if termination != 0 {
            if termination == 2 {
                pair.reconnect().await;
            }
            pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
        }
        wait_state(&pair.b, &id, TaskState::Completed).await;
        pair.b.wait_previous_attempt(&id).await.unwrap();
        assert_eq!(
            fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
            bytes
        );
        assert_eq!(pair.b.receive_space.summary().reserved, 0);
        pair.shutdown().await;
    }
}
#[tokio::test]
async fn receive_space_directory_refusal_keeps_target_absent_then_manual_retry_works() {
    let pair = Pair::new().await;
    pair.b.receive_space.set_safety_mib(0).unwrap();
    *pair.b.receive_space.probe_override.lock().unwrap() = Some(Ok(0));
    let source = pair.root.join("空目录");
    fs::create_dir(&source).unwrap();
    let id = pair
        .a
        .select_directory(pair.ib, source)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(pair.sender(id.clone()).await.unwrap().is_err());
    wait_state(&pair.b, &id, TaskState::Failed).await;
    pair.b.wait_previous_attempt(&id).await.unwrap();
    assert_eq!(
        pair.b
            .task(id.clone())
            .await
            .unwrap()
            .diagnostic()
            .unwrap()
            .code(),
        TaskErrorCode::DiskFull
    );
    assert!(!pair.root.join("b-receive/空目录").exists());
    assert_eq!(pair.b.receive_space.summary().reserved, 0);
    *pair.b.receive_space.probe_override.lock().unwrap() = None;
    pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
    wait_state(&pair.b, &id, TaskState::Completed).await;
    assert!(pair.root.join("b-receive/空目录").is_dir());
    pair.b.wait_previous_attempt(&id).await.unwrap();
    assert_eq!(pair.b.receive_space.summary().reserved, 0);
    pair.shutdown().await;
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn receive_space_real_enospc_kind_keeps_partial_and_releases_reservation() {
    use std::io::Write;
    // /dev/full returns a genuine kernel ENOSPC without filling a shared
    // filesystem. Route that native error kind through the publication seam.
    let error = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap()
        .write_all(b"space fixture")
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(128 * 1024).await;
    *pair.b.publication_error.lock().unwrap() = Some(error.kind());
    assert!(pair.sender(id.clone()).await.unwrap().is_err());
    wait_state(&pair.b, &id, TaskState::Failed).await;
    pair.b.wait_previous_attempt(&id).await.unwrap();
    let failed = pair.b.task(id.clone()).await.unwrap();
    assert_eq!(failed.diagnostic().unwrap().code(), TaskErrorCode::DiskFull);
    assert!(!failed.receipt_committed());
    assert!(disk::stage_dir(&failed).unwrap().join("data.part").exists());
    assert_eq!(pair.b.receive_space.summary().reserved, 0);
    pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
    wait_state(&pair.b, &id, TaskState::Completed).await;
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn simultaneous_automatic_resume_reuses_the_fresh_sender_attempt() {
    let mut pair = Pair::new().await;
    pair.a.set_auto_resume(true);
    pair.b.set_auto_resume(true);
    let (id, bytes) = pair.select(4 * 1024 * 1024).await;
    let (reached, _release) = pair.gate();
    let sender = pair.sender(id.clone());
    reached.await.unwrap();
    pair.a.network_lost(pair.ib);
    pair.b.network_lost(pair.ia);
    pair.ca.close(0u32.into(), b"fixture loss");
    assert!(sender.await.unwrap().is_err());
    pair.cb.closed().await;
    while pair.tasks.join_next().await.is_some() {}
    pair.a.settle_network_loss(pair.ib).await.unwrap();
    pair.b.settle_network_loss(pair.ia).await.unwrap();
    let lost_transport = pair.ca.stable_id();
    pair.reconnect().await;
    let (admitted_tx, admitted) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    *pair.a.resume_admission_gate.lock().unwrap() = Some((admitted_tx, release_rx));
    let ticket = pair
        .b
        .recovery
        .lock()
        .unwrap()
        .due(
            Instant::now() + Duration::from_secs(5),
            |_, _| true,
            |_| false,
        )
        .1
        .pop()
        .unwrap();
    let service = pair.b.clone();
    let connection = pair.cb.clone();
    let request = tokio::spawn(async move { service.auto_recover(&connection, ticket).await });
    tokio::time::timeout(Duration::from_secs(5), admitted)
        .await
        .unwrap()
        .unwrap();
    let ticket = pair
        .a
        .recovery
        .lock()
        .unwrap()
        .due(
            Instant::now() + Duration::from_secs(5),
            |_, _| true,
            |_| false,
        )
        .1
        .pop()
        .unwrap();
    pair.a.auto_recover(&pair.ca, ticket).await.unwrap();
    let (reached, finish) = pair.gate();
    let scheduled = pair
        .a
        .dispatch_ready(&HashMap::from([(pair.ib, pair.ca.clone())]))
        .pop()
        .unwrap();
    let service = pair.a.clone();
    let connection = pair.ca.clone();
    let sender = tokio::spawn(async move { service.execute_queued(scheduled, connection).await });
    tokio::time::timeout(Duration::from_secs(5), reached)
        .await
        .unwrap()
        .unwrap();
    assert!(
        pair.a
            .automatic_attempt_exists(&id, pair.ib, pair.ca.stable_id())
    );
    assert!(
        !pair
            .a
            .automatic_attempt_exists(&id, pair.ib, lost_transport)
    );
    assert!(
        !pair
            .a
            .automatic_attempt_exists(&id, pair.ia, pair.ca.stable_id())
    );
    release.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "fresh active sender must be reused: {result:?}"
    );
    assert_eq!(
        pair.b.task(id.clone()).await.unwrap().state(),
        TaskState::Transferring
    );
    finish.send(()).unwrap();
    sender.await.unwrap().unwrap();
    wait_state(&pair.b, &id, TaskState::Completed).await;
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Completed
    );
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    assert!(pair.a.automatic_sends.lock().unwrap().is_empty());
    for service in [&pair.a, &pair.b] {
        service.wait_previous_attempt(&id).await.unwrap();
        let measurements = service.measurements.lock().unwrap();
        let sample = measurements.sample(&id, Instant::now()).unwrap();
        assert_eq!(sample.attempts, 2);
        assert_eq!(sample.automatic, 1);
    }
    pair.shutdown().await;
}
#[tokio::test]
async fn cancelled_or_expired_receive_rpc_leaves_a_manually_recoverable_task() {
    for control in 0..3 {
        let mut pair = Pair::new().await;
        pair.a.set_auto_resume(true);
        pair.b.set_auto_resume(true);
        let (id, _) = pair.select(4 * 1024 * 1024).await;
        let (reached, _release) = pair.gate();
        let sender = pair.sender(id.clone());
        reached.await.unwrap();
        pair.a.network_lost(pair.ib);
        pair.b.network_lost(pair.ia);
        pair.ca.close(0u32.into(), b"fixture loss");
        assert!(sender.await.unwrap().is_err());
        pair.cb.closed().await;
        while pair.tasks.join_next().await.is_some() {}
        pair.a.settle_network_loss(pair.ib).await.unwrap();
        pair.b.settle_network_loss(pair.ia).await.unwrap();
        pair.reconnect().await;
        // Keep a real authenticated connection while the RPC responder is
        // stalled. Cancellation must work without an Offer or network error.
        pair.tasks.abort_all();
        while pair.tasks.join_next().await.is_some() {}
        let mut ticket = pair
            .b
            .recovery
            .lock()
            .unwrap()
            .due(
                Instant::now() + Duration::from_secs(5),
                |_, _| true,
                |_| false,
            )
            .1
            .pop()
            .unwrap();
        if control == 2 {
            ticket.deadline = Some(Instant::now() + Duration::from_secs(2));
        }
        let service = pair.b.clone();
        let connection = pair.cb.clone();
        let request = tokio::spawn(async move { service.auto_recover(&connection, ticket).await });
        wait_state(&pair.b, &id, TaskState::Queued).await;
        match control {
            0 => pair.b.set_auto_resume(false),
            1 => pair.b.pause_task(id.clone()).await.unwrap(),
            _ => {}
        }
        tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            pair.b.task(id.clone()).await.unwrap().state(),
            if control == 1 {
                TaskState::Paused
            } else {
                TaskState::Interrupted
            }
        );
        assert!(pair.b.ui_snapshot().unwrap().tasks[0].can_continue());
        assert!(!pair.root.join("b-receive/文件.bin").exists());
        pair.shutdown().await;
    }
}
#[tokio::test]
async fn automatic_offer_can_deliver_when_the_first_offer_never_reached_receiver() {
    let pair = Pair::new().await;
    pair.a.set_auto_resume(true);
    let (id, bytes) = pair.select(4096).await;
    pair.a
        .recovery
        .lock()
        .unwrap()
        .enroll(id.clone(), pair.ib, TaskDirection::Send);
    pair.a.network_lost(pair.ib);
    pair.a
        .store({
            let id = id.clone();
            move |store| disk::transition(store, &id, TaskState::Interrupted)
        })
        .await
        .unwrap();
    let ticket = pair
        .a
        .recovery
        .lock()
        .unwrap()
        .due(
            Instant::now() + Duration::from_secs(2),
            |_, _| true,
            |_| false,
        )
        .1
        .pop()
        .unwrap();
    pair.a.auto_recover(&pair.ca, ticket).await.unwrap();
    let scheduled = pair
        .a
        .dispatch_ready(&HashMap::from([(pair.ib, pair.ca.clone())]))
        .pop()
        .unwrap();
    pair.a
        .execute_queued(scheduled, pair.ca.clone())
        .await
        .unwrap();
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Completed
    );
    assert_eq!(pair.b.task(id).await.unwrap().state(), TaskState::Completed);
    pair.shutdown().await;
}
#[tokio::test]
async fn automatic_recovery_cannot_override_pause_restart_disabled_preference_or_changed_source() {
    for scenario in 0..9 {
        let mut pair = Pair::new().await;
        pair.a.set_auto_resume(true);
        pair.b.set_auto_resume(true);
        let (id, _) = pair.select(4 * 1024 * 1024).await;
        let (reached, _release) = pair.gate();
        let sender = pair.sender(id.clone());
        tokio::time::timeout(Duration::from_secs(5), reached)
            .await
            .unwrap()
            .unwrap();
        pair.a.network_lost(pair.ib);
        pair.b.network_lost(pair.ia);
        pair.ca.close(0u32.into(), b"fixture loss");
        assert!(sender.await.unwrap().is_err());
        pair.cb.closed().await;
        while pair.tasks.join_next().await.is_some() {}
        pair.a.settle_network_loss(pair.ib).await.unwrap();
        pair.b.settle_network_loss(pair.ia).await.unwrap();
        let ticket = pair
            .a
            .recovery
            .lock()
            .unwrap()
            .due(
                Instant::now() + Duration::from_secs(2),
                |_, _| true,
                |_| false,
            )
            .1
            .pop()
            .unwrap();
        match scenario {
            0 => pair.b.pause_task(id.clone()).await.unwrap(),
            1 => {
                let replacement = TransferService::new(
                    TaskStore::open(&pair.root.join("dummy/tasks.json"))
                        .unwrap()
                        .0,
                    pair.root.join("b-receive"),
                );
                drop(std::mem::replace(&mut pair.b, replacement));
                let store = TaskStore::open(&pair.root.join("b-state/tasks.json"))
                    .unwrap()
                    .0;
                pair.b = TransferService::new(store, pair.root.join("b-receive"));
                pair.b.set_auto_resume(true);
            }
            2 => pair.a.set_auto_resume(false),
            3 => pair.a.pause_task(id.clone()).await.unwrap(),
            4 => fs::write(pair.root.join("文件.bin"), b"source changed").unwrap(),
            6 => pair
                .a
                .revoke_authorization(
                    pair.ib,
                    crate::node::core::remote_auth::RemoteAuthorization::default(),
                )
                .await
                .unwrap(),
            7 => fs::remove_file(pair.root.join("文件.bin")).unwrap(),
            8 => fs::create_dir(pair.root.join("b-receive/文件.bin")).unwrap(),
            _ => {}
        }
        pair.reconnect().await;
        pair.a.auto_recover(&pair.ca, ticket).await.unwrap();
        if scenario == 5 {
            pair.a.set_auto_resume(false);
        }
        let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
        let scheduled = pair.a.dispatch_ready(&connections);
        if scenario == 2 || scenario == 3 || scenario == 6 {
            assert!(scheduled.is_empty());
        }
        for send in scheduled {
            assert!(pair.a.execute_queued(send, pair.ca.clone()).await.is_err());
        }
        if scenario == 8 {
            assert!(pair.root.join("b-receive/文件.bin").is_dir());
        } else {
            assert!(!pair.root.join("b-receive/文件.bin").exists());
        }
        assert_ne!(
            pair.a.task(id.clone()).await.unwrap().state(),
            TaskState::Completed
        );
        assert_ne!(
            pair.b.task(id.clone()).await.unwrap().state(),
            TaskState::Completed
        );
        if scenario == 0 {
            assert_eq!(
                pair.b.task(id.clone()).await.unwrap().state(),
                TaskState::Paused
            );
        }
        if scenario == 3 {
            assert_eq!(
                pair.a.task(id.clone()).await.unwrap().state(),
                TaskState::Paused
            );
        }
        if scenario == 4 {
            let record = pair.a.task(id.clone()).await.unwrap();
            assert_eq!(record.state(), TaskState::Failed);
            assert_eq!(
                record.diagnostic().unwrap().code(),
                TaskErrorCode::SourceChanged
            );
        }
        assert!(pair.a.auto_due(|_, _| true, |_| false).1.is_empty());
        pair.shutdown().await;
    }
}
#[tokio::test]
async fn file_limits_bound_real_upload_and_download_without_affecting_receipts() {
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(64 * 1024).await;
    pair.a
        .set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 64,
            download_kib: 0,
        })
        .unwrap();
    let start = Instant::now();
    pair.sender(id.clone()).await.unwrap().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(850));
    assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Completed);
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    pair.a.set_file_limits(Default::default()).unwrap();
    pair.b
        .set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 0,
            download_kib: 64,
        })
        .unwrap();
    let (id, _) = pair.select(64 * 1024).await;
    let start = Instant::now();
    pair.sender(id.clone()).await.unwrap().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(850));
    assert_eq!(pair.b.task(id).await.unwrap().state(), TaskState::Completed);
    pair.shutdown().await;
}
#[tokio::test]
async fn file_limits_heartbeat_keeps_slow_receiver_alive_past_frame_timeout() {
    let pair = Pair::new().await;
    pair.b
        .set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 0,
            download_kib: 1,
        })
        .unwrap();
    let (id, _) = pair.select(32 * 1024).await;
    let start = Instant::now();
    pair.sender(id.clone()).await.unwrap().unwrap();
    assert!(start.elapsed() > IDLE_TIMEOUT);
    assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Completed);
    pair.shutdown().await;
}
#[tokio::test]
async fn file_limits_slow_upload_uses_byte_progress_instead_of_whole_frame_timeout() {
    let pair = Pair::new().await;
    pair.a
        .set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 1,
            download_kib: 0,
        })
        .unwrap();
    let (id, _) = pair.select(32 * 1024).await;
    let start = Instant::now();
    pair.sender(id.clone()).await.unwrap().unwrap();
    assert!(start.elapsed() > IDLE_TIMEOUT);
    assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Completed);
    pair.shutdown().await;
}
#[tokio::test]
async fn file_limits_runtime_disable_releases_upload_and_download_waits() {
    for download in [false, true] {
        let pair = Pair::new().await;
        let service = if download { &pair.b } else { &pair.a };
        service
            .set_file_limits(crate::node::core::bandwidth::FileLimits {
                upload_kib: if download { 0 } else { 1 },
                download_kib: if download { 1 } else { 0 },
            })
            .unwrap();
        let (id, _) = pair.select(64 * 1024).await;
        let sender = pair.sender(id.clone());
        wait_state(&pair.b, &id, TaskState::Transferring).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!sender.is_finished());
        service.set_file_limits(Default::default()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), sender)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Completed);
        pair.shutdown().await;
    }
}

#[tokio::test]
async fn file_limits_pause_and_runtime_unlimit_wake_in_flight_tasks() {
    for receiver_pause in [false, true] {
        let pair = Pair::new().await;
        pair.a
            .set_file_limits(crate::node::core::bandwidth::FileLimits {
                upload_kib: 1,
                download_kib: 0,
            })
            .unwrap();
        let (id, _) = pair.select(8 * 1024 * 1024).await;
        let sender = pair.sender(id.clone());
        wait_state(&pair.b, &id, TaskState::Transferring).await;
        if receiver_pause {
            pair.b.pause(&id).unwrap();
        } else {
            pair.a.pause(&id).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), sender)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            pair.a.task(id.clone()).await.unwrap().state(),
            TaskState::Paused
        );
        pair.a.set_file_limits(Default::default()).unwrap();
        pair.a
            .send_file(&pair.ca, pair.ib, id.clone())
            .await
            .unwrap();
        assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Completed);
        pair.shutdown().await;
    }
}

#[tokio::test]
async fn file_limits_pause_during_budget_wait_is_not_consumed_by_the_limiter() {
    let pair = Pair::new().await;
    pair.a
        .set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 1,
            download_kib: 0,
        })
        .unwrap();
    let (id, _) = pair.select(8 * 1024 * 1024).await;
    let sender = pair.sender(id.clone());
    wait_state(&pair.b, &id, TaskState::Transferring).await;
    // Ensure the pause arrives during an active rate wait rather than
    // before the first chunk. No more than a few bytes can pass at 1KiB/s.
    tokio::time::sleep(Duration::from_millis(150)).await;
    pair.a.pause_task(id.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), sender)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Paused
    );
    assert_eq!(pair.b.task(id).await.unwrap().state(), TaskState::Paused);
    pair.shutdown().await;
}
#[tokio::test]
async fn file_limits_reject_peer_without_flow_capability() {
    let pair = Pair::new().await;
    pair.a.set_peer_flow_support(pair.ib, false);
    pair.a
        .set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 1,
            download_kib: 0,
        })
        .unwrap();
    let (id, _) = pair.select(64 * 1024).await;
    let error = pair.sender(id.clone()).await.unwrap().unwrap_err();
    assert!(error.to_string().contains("对端不支持"));
    pair.a.set_file_limits(Default::default()).unwrap();
    pair.a.send_file(&pair.ca, pair.ib, id).await.unwrap();
    pair.shutdown().await;
}

async fn wait_state(service: &TransferService, id: &TaskId, state: TaskState) {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = service.subscribe();
        loop {
            if service
                .task(id.clone())
                .await
                .is_ok_and(|r| r.state() == state)
            {
                break;
            }
            events.changed().await.unwrap();
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "waiting {state:?}, actual {:?}",
        service
            .task(id.clone())
            .await
            .map(|record| (record.state(), record.diagnostic()))
    );
}
#[tokio::test]
async fn authenticated_single_file_and_empty_file_complete_with_durable_receipts() {
    let pair = Pair::new().await;
    for size in [2 * 1024 * 1024, 0] {
        let (id, bytes) = pair.select(size).await;
        if size == 0 {
            fs::remove_file(pair.root.join("b-receive/文件.bin")).unwrap();
        }
        tokio::time::timeout(
            Duration::from_secs(10),
            pair.a.send_file(&pair.ca, pair.ib, id.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        wait_state(&pair.b, &id, TaskState::Completed).await;
        assert_eq!(
            fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
            bytes
        );
        for service in [&pair.a, &pair.b] {
            let record = service.task(id.clone()).await.unwrap();
            assert_eq!(record.state(), TaskState::Completed);
            assert!(record.file_details().unwrap().receipt_committed);
            assert_eq!(
                record.progress_hint().unwrap().verified_bytes(),
                size as u64
            );
        }
    }
    let reverse = pair.root.join("返回.bin");
    let reverse_bytes = vec![91; 300 * 1024];
    fs::write(&reverse, &reverse_bytes).unwrap();
    let reverse_id = pair.b.select_file(pair.ia, reverse).await.unwrap();
    pair.b
        .send_file(&pair.cb, pair.ia, reverse_id.clone())
        .await
        .unwrap();
    wait_state(&pair.a, &reverse_id, TaskState::Completed).await;
    assert_eq!(
        fs::read(pair.root.join("a-receive/返回.bin")).unwrap(),
        reverse_bytes
    );
    pair.shutdown().await;
}

#[tokio::test]
async fn deleting_completed_receive_file_removes_only_received_copy_and_history() {
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(256 * 1024).await;
    let source = pair.root.join("文件.bin");
    let received = pair.root.join("b-receive/文件.bin");
    pair.a
        .send_file(&pair.ca, pair.ib, id.clone())
        .await
        .unwrap();
    wait_state(&pair.b, &id, TaskState::Completed).await;

    assert!(pair.a.delete_completed_receive_file(&id).is_err());
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert!(received.exists());

    pair.b.delete_completed_receive_file(&id).unwrap();
    assert!(!received.exists());
    assert!(pair.b.task(id.clone()).await.is_err());
    assert!(pair.a.task(id).await.is_ok());
    assert_eq!(fs::read(&source).unwrap(), bytes);
    pair.shutdown().await;
}
#[tokio::test]
async fn sender_receiver_and_simultaneous_pause_then_id_only_continue() {
    for side in 0..3 {
        let pair = Pair::new().await;
        let (id, bytes) = pair.select(4 * 1024 * 1024).await;
        let (reached, release) = pair.gate();
        let sender = pair.sender(id.clone());
        tokio::time::timeout(Duration::from_secs(10), reached)
            .await
            .unwrap()
            .unwrap();
        if side != 1 {
            pair.a.pause(&id).unwrap();
            pair.a.pause(&id).unwrap();
        }
        if side != 0 {
            pair.b.pause(&id).unwrap();
        }
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), sender)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        wait_state(&pair.a, &id, TaskState::Paused).await;
        wait_state(&pair.b, &id, TaskState::Paused).await;
        let receiver = pair.b.task(id.clone()).await.unwrap();
        let download = disk::open_download(&receiver).unwrap();
        assert!(download.bitmap().count_set() > 0);
        assert!(!download.is_complete());
        drop(download);
        pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
        wait_state(&pair.a, &id, TaskState::Completed).await;
        wait_state(&pair.b, &id, TaskState::Completed).await;
        assert_eq!(
            fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
            bytes
        );
        pair.shutdown().await;
    }
}
#[tokio::test]
async fn pause_racing_last_chunk_preserves_completed_receipt() {
    let pair = Pair::new().await;
    let (id, _) = pair.select(33).await;
    let (reached, release) = pair.gate();
    let sender = pair.sender(id.clone());
    tokio::time::timeout(Duration::from_secs(10), reached)
        .await
        .unwrap()
        .unwrap();
    pair.a.pause(&id).unwrap();
    pair.b.pause(&id).unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), sender)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    wait_state(&pair.a, &id, TaskState::Completed).await;
    wait_state(&pair.b, &id, TaskState::Completed).await;
    pair.shutdown().await;
}

// This test function is also the isolated child-process entrypoint. The parent
// below executes this exact test only; no production binary/test switches exist.
#[test]
fn process_worker_entry() {
    let Some(root) = std::env::var_os("P2P_DESKTOP_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let role = std::env::var("P2P_DESKTOP_TEST_ROLE").unwrap();
    let mode = std::env::var("P2P_DESKTOP_TEST_MODE").unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let identity =
                Identity::load_or_create(&root.join(format!("{role}-state/identity.bin"))).unwrap();
            let other = if role == "a" { "b" } else { "a" };
            let peer = Identity::load_or_create(&root.join(format!("{other}-state/identity.bin")))
                .unwrap()
                .node_id();
            let (store, _) =
                TaskStore::open(&root.join(format!("{role}-state/tasks.json"))).unwrap();
            let service = TransferService::new(store, root.join(format!("{role}-receive")));
            let endpoint = if role == "a" {
                client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap()
            } else {
                server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap()
            };
            let connection = if role == "b" {
                fs::write(
                    root.join("receiver-address"),
                    endpoint.local_addr().unwrap().to_string(),
                )
                .unwrap();
                let connection = endpoint.accept().await.unwrap().await.unwrap();
                let binding = ChannelBinding::from_connection(&connection).unwrap();
                let (mut send, mut recv) = connection.accept_bi().await.unwrap();
                assert_eq!(
                    handshake_responder(&mut send, &mut recv, &identity, &binding)
                        .await
                        .unwrap()
                        .peer_node_id,
                    peer
                );
                send.finish().unwrap();
                crate::node::core::wire::negotiate(
                    &connection,
                    false,
                    crate::node::core::wire::capabilities(true, true, true),
                )
                .await
                .unwrap();
                connection
            } else {
                let addr = fs::read_to_string(root.join("receiver-address"))
                    .unwrap()
                    .parse()
                    .unwrap();
                let connection = crate::transport::quic::connect(&endpoint, addr, "p2pfile")
                    .await
                    .unwrap();
                let binding = ChannelBinding::from_connection(&connection).unwrap();
                let (mut send, mut recv) = connection.open_bi().await.unwrap();
                assert_eq!(
                    handshake_initiator(&mut send, &mut recv, &identity, &binding)
                        .await
                        .unwrap()
                        .peer_node_id,
                    peer
                );
                send.finish().unwrap();
                crate::node::core::wire::negotiate(
                    &connection,
                    true,
                    crate::node::core::wire::capabilities(true, true, true),
                )
                .await
                .unwrap();
                connection
            };
            let mut helpers = JoinSet::new();
            if role == "b" && mode == "first" {
                let (reached_tx, reached) = tokio::sync::oneshot::channel();
                let (release, release_rx) = tokio::sync::oneshot::channel();
                *service.checkpoint_gate.lock().unwrap() = Some((reached_tx, release_rx));
                let root = root.clone();
                helpers.spawn(async move {
                    reached.await.unwrap();
                    fs::write(root.join("checkpoint-reached"), b"durable bitmap nonempty").unwrap();
                    while !root.join("release").exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    let _ = release.send(());
                });
            }
            let stop_root = root.clone();
            let stop_connection = connection.clone();
            helpers.spawn(async move {
                while !stop_root.join("stop").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                stop_connection.close(0u32.into(), b"surviving process graceful stop");
            });
            if role == "a" {
                let id = if mode == "first" {
                    let id = service
                        .select_file(peer, root.join("source.bin"))
                        .await
                        .unwrap();
                    fs::write(root.join("task-id"), id.as_str()).unwrap();
                    id
                } else {
                    TaskId::parse(&fs::read_to_string(root.join("task-id")).unwrap()).unwrap()
                };
                let result = service.send_file(&connection, peer, id.clone()).await;
                if mode == "resume" {
                    result.unwrap();
                    assert_eq!(
                        service.task(id).await.unwrap().state(),
                        TaskState::Completed
                    );
                    fs::write(root.join("sender-complete"), b"receipt").unwrap();
                } else {
                    assert!(root.join("stop").exists());
                }
                connection.close(0u32.into(), b"sender done");
            } else {
                let _ = service.serve_peer(connection.clone(), peer).await;
                if mode == "resume" {
                    let id =
                        TaskId::parse(&fs::read_to_string(root.join("task-id")).unwrap()).unwrap();
                    assert_eq!(
                        service.task(id).await.unwrap().state(),
                        TaskState::Completed
                    );
                }
            }
            helpers.abort_all();
            while helpers.join_next().await.is_some() {}
            service.interrupt_all().await.unwrap();
            endpoint.close(0u32.into(), b"child done");
            endpoint.wait_idle().await;
        });
}

struct ChildProcess(std::process::Child);
impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child(root: &std::path::Path, role: &str, mode: &str) -> ChildProcess {
    let log = fs::File::create(root.join(format!("{role}-{mode}.log"))).unwrap();
    ChildProcess(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!(
                    "{}::process_worker_entry",
                    module_path!().split_once("::").unwrap().1
                ),
                "--nocapture",
            ])
            .env("P2P_DESKTOP_TEST_ROOT", root)
            .env("P2P_DESKTOP_TEST_ROLE", role)
            .env("P2P_DESKTOP_TEST_MODE", mode)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    )
}
fn await_file(path: &std::path::Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "process milestone missing: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn await_success(child: &mut ChildProcess, root: &std::path::Path, role: &str, mode: &str) {
    let start = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "child failed: {}",
                fs::read_to_string(root.join(format!("{role}-{mode}.log"))).unwrap()
            );
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "child exit deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn real_process_kill_of_either_endpoint_resumes_verified_durable_chunks() {
    use std::io::Write;
    for killed in ["a", "b"] {
        let root = std::env::temp_dir().join(format!(
            "p2p-desktop-process-kill-{killed}-{}",
            rand::random::<u128>()
        ));
        fs::create_dir(&root).unwrap();
        for role in ["a", "b"] {
            crate::node::core::config::ensure_private_app_dir(&root.join(format!("{role}-state")))
                .unwrap();
            fs::create_dir(root.join(format!("{role}-receive"))).unwrap();
            Identity::load_or_create(&root.join(format!("{role}-state/identity.bin"))).unwrap();
        }
        let mut source = fs::File::create(root.join("source.bin")).unwrap();
        let chunk = vec![73; 1024 * 1024];
        for _ in 0..70 {
            source.write_all(&chunk).unwrap();
        }
        source.sync_all().unwrap();
        drop(source);
        let mut receiver = child(&root, "b", "first");
        await_file(&root.join("receiver-address"));
        let mut sender = child(&root, "a", "first");
        await_file(&root.join("checkpoint-reached"));
        let victim = if killed == "a" {
            &mut sender
        } else {
            &mut receiver
        };
        victim.0.kill().unwrap();
        assert!(!victim.0.wait().unwrap().success());
        fs::write(root.join("release"), b"release survivor").unwrap();
        fs::write(root.join("stop"), b"graceful survivor").unwrap();
        if killed == "a" {
            await_success(&mut receiver, &root, "b", "first");
        } else {
            await_success(&mut sender, &root, "a", "first");
        }
        let id = TaskId::parse(&fs::read_to_string(root.join("task-id")).unwrap()).unwrap();
        let (store, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
        let record = store.task(&id).unwrap();
        assert_eq!(record.state(), TaskState::Interrupted);
        let download = disk::open_download(&record).unwrap();
        let durable = download.bitmap().count_set();
        assert!(durable > 0);
        assert!(!download.is_complete());
        drop(download);
        drop(store);
        for name in ["receiver-address", "checkpoint-reached", "release", "stop"] {
            fs::remove_file(root.join(name)).unwrap();
        }
        let mut receiver = child(&root, "b", "resume");
        await_file(&root.join("receiver-address"));
        let mut sender = child(&root, "a", "resume");
        await_success(&mut sender, &root, "a", "resume");
        await_success(&mut receiver, &root, "b", "resume");
        let expected = crate::transfer::chunker::manifest_from_path(
            &root.join("source.bin"),
            crate::protocol::manifest::DEFAULT_CHUNK_SIZE,
        )
        .unwrap();
        let actual = crate::transfer::chunker::manifest_from_path(
            &root.join("b-receive/source.bin"),
            expected.chunk_size,
        )
        .unwrap();
        assert_eq!(actual, expected);
        for role in ["a", "b"] {
            let (store, _) =
                TaskStore::open(&root.join(format!("{role}-state/tasks.json"))).unwrap();
            assert_eq!(store.list().len(), 1);
            assert_eq!(store.task(&id).unwrap().state(), TaskState::Completed);
        }
        eprintln!(
            "process hard-kill {killed}: resume from {durable} durable chunks; 70 MiB manifest and both receipts verified"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn lost_completed_frame_replays_receipt_without_republishing() {
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(256 * 1024).await;
    pair.b
        .drop_completion
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        pair.a
            .send_file(&pair.ca, pair.ib, id.clone())
            .await
            .is_err()
    );
    wait_state(&pair.a, &id, TaskState::Interrupted).await;
    wait_state(&pair.b, &id, TaskState::Completed).await;
    let before = pair.b.task(id.clone()).await.unwrap().updated_at_unix_ms();
    pair.a
        .send_file(&pair.ca, pair.ib, id.clone())
        .await
        .unwrap();
    wait_state(&pair.a, &id, TaskState::Completed).await;
    assert_eq!(pair.b.task(id).await.unwrap().updated_at_unix_ms(), before);
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read_dir(pair.root.join("b-receive"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().unwrap().is_file())
            .count(),
        1
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn pause_timeout_retains_download_and_marks_interrupted() {
    let pair = Pair::new().await;
    let (id, _) = pair.select(4 * 1024 * 1024).await;
    let (reached, release) = pair.gate();
    let sender = pair.sender(id.clone());
    tokio::time::timeout(Duration::from_secs(10), reached)
        .await
        .unwrap()
        .unwrap();
    pair.a.pause(&id).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(8), sender)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("暂停确认超时"));
    release.send(()).unwrap();
    wait_state(&pair.a, &id, TaskState::Interrupted).await;
    wait_state(&pair.b, &id, TaskState::Interrupted).await;
    let record = pair.b.task(id).await.unwrap();
    let download = disk::open_download(&record).unwrap();
    assert!(download.bitmap().count_set() > 0);
    assert!(download.temp_path().exists());
    drop(download);
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_pending_request_cancellation_supersession_and_capacity_are_bounded() {
    let pair = Pair::new().await;
    let source = pair.root.join("目录");
    fs::create_dir(&source).unwrap();
    let first = TaskId::generate();
    pair.a.begin_preview(pair.ib, first.clone()).unwrap();
    let cancellation = pair.a.preview_requests.lock().unwrap()[&pair.ib]
        .cancellation
        .clone();
    let second = TaskId::generate();
    pair.a.begin_preview(pair.ib, second.clone()).unwrap();
    assert!(cancellation.check().is_err());
    assert!(
        pair.a
            .preview_directory_request(&pair.ca, pair.ib, source.clone(), first)
            .await
            .is_err()
    );
    pair.a.cancel_preview(&second);
    assert!(
        pair.a
            .preview_directory_request(&pair.ca, pair.ib, source, second)
            .await
            .is_err()
    );
    for _ in 0..8 {
        pair.a
            .begin_preview(Identity::generate().node_id(), TaskId::generate())
            .unwrap();
    }
    assert!(pair.a.begin_preview(pair.ib, TaskId::generate()).is_err());
    for request in pair.a.preview_requests.lock().unwrap().values_mut() {
        request.created = Instant::now() - crate::node::core::incremental::PREVIEW_LIFETIME;
    }
    pair.a.begin_preview(pair.ib, TaskId::generate()).unwrap();
    assert_eq!(pair.a.preview_requests.lock().unwrap().len(), 1);
    assert!(pair.a.snapshot().await.unwrap().is_empty());
    assert!(pair.b.snapshot().await.unwrap().is_empty());
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_retry_can_skip_now_identical_target_and_clean_existing_partial() {
    let pair = Pair::new().await;
    let source = pair.root.join("目录");
    fs::create_dir(&source).unwrap();
    let bytes = vec![19; 300_000];
    fs::write(source.join("a.bin"), &bytes).unwrap();
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source)
        .await
        .unwrap();
    let ids = pair
        .a
        .confirm_directory(&pair.ca, pair.ib, preview.id)
        .await
        .unwrap();
    let file = pair
        .a
        .snapshot()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.file_details().is_some())
        .unwrap();
    let details = file.file_details().unwrap().clone();
    let id = file.task_id().clone();
    let group = file.group_id().cloned();
    let root = pair.root.join("b-receive");
    let peer = pair.ia;
    let received = pair
        .b
        .store(move |store| {
            let record = disk::accept_entry(
                store,
                peer,
                &id,
                &root,
                details.relative_path,
                details.manifest,
                group,
            )?;
            disk::transition(store, &id, TaskState::Transferring)?;
            disk::transition(store, &id, TaskState::Interrupted)?;
            Ok(record)
        })
        .await
        .unwrap();
    let mut download = disk::open_download(&received).unwrap();
    download.write_chunk(0, &bytes[..256 * 1024]).unwrap();
    download.checkpoint().unwrap();
    let part = download.temp_path().to_path_buf();
    assert!(part.exists());
    drop(download);
    fs::create_dir(pair.root.join("b-receive/目录")).unwrap();
    fs::write(pair.root.join("b-receive/目录/a.bin"), &bytes).unwrap();
    pair.a.send_selection(&pair.ca, pair.ib, ids).await.unwrap();
    assert!(
        pair.b
            .task(file.task_id().clone())
            .await
            .unwrap()
            .file_details()
            .unwrap()
            .skipped_existing
    );
    assert!(!part.exists());
    assert!(!part.with_file_name("data.bitmap").exists());
    assert_eq!(
        fs::read_dir(pair.root.join("b-receive/目录"))
            .unwrap()
            .count(),
        1
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_preview_and_transfer_preserve_identical_files_and_back_up_changes() {
    let pair = Pair::new().await;
    let source = pair.root.join("目录");
    let target = pair.root.join("b-receive/目录");
    fs::create_dir_all(source.join("空目录")).unwrap();
    fs::create_dir_all(&target).unwrap();
    for (name, bytes) in [
        ("same.txt", &b"same"[..]),
        ("changed.txt", &b"new"[..]),
        ("added.txt", &b"added"[..]),
        ("empty.txt", &b""[..]),
    ] {
        fs::write(source.join(name), bytes).unwrap();
    }
    fs::write(target.join("same.txt"), b"same").unwrap();
    fs::write(target.join("changed.txt"), b"old").unwrap();
    fs::write(target.join("empty.txt"), b"").unwrap();
    fs::write(target.join("extra.txt"), b"keep").unwrap();
    let identity =
        crate::storage::secure_fs::identity(&std::fs::File::open(target.join("same.txt")).unwrap())
            .unwrap();
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source)
        .await
        .unwrap();
    assert_eq!(
        (
            preview.added,
            preview.changed,
            preview.unchanged,
            preview.directories,
            preview.transfer_bytes,
            preview.saved_bytes
        ),
        (1, 1, 2, 2, 8, 4)
    );
    assert!(pair.a.snapshot().await.unwrap().is_empty());
    assert!(pair.b.snapshot().await.unwrap().is_empty());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 4);
    let ids = pair
        .a
        .confirm_directory(&pair.ca, pair.ib, preview.id)
        .await
        .unwrap();
    pair.a
        .send_selection(&pair.ca, pair.ib, ids.clone())
        .await
        .unwrap();
    for id in &ids {
        wait_state(&pair.b, id, TaskState::Completed).await;
    }
    let received = pair.b.snapshot().await.unwrap();
    assert_eq!(
        received
            .iter()
            .filter(|r| r.file_details().is_some_and(|d| d.skipped_existing))
            .count(),
        2
    );
    assert_eq!(
        crate::storage::secure_fs::identity(&std::fs::File::open(target.join("same.txt")).unwrap())
            .unwrap(),
        identity
    );
    assert!(target.join("空目录").is_dir());
    assert_eq!(fs::read(target.join("extra.txt")).unwrap(), b"keep");
    assert_eq!(fs::read(target.join("changed.txt")).unwrap(), b"new");
    assert_eq!(fs::read(target.join("added.txt")).unwrap(), b"added");
    let backups: Vec<_> = fs::read_dir(&target)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("changed+")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), b"old");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 7);
    fs::create_dir(pair.root.join("reopened")).unwrap();
    fs::copy(
        pair.root.join("b-state/tasks.json"),
        pair.root.join("reopened/tasks.json"),
    )
    .unwrap();
    let (reopened, _) = TaskStore::open(&pair.root.join("reopened/tasks.json")).unwrap();
    assert_eq!(
        reopened
            .list()
            .iter()
            .filter(|r| r.file_details().is_some_and(|d| d.skipped_existing))
            .count(),
        2
    );
    let detail = crate::node::core::task_details::project(
        &received,
        &crate::node::core::task_details::Selection::Group(received[0].group_id().unwrap().clone()),
        |_| 0.,
        |_| None,
        |_| false,
        system_time_unix_ms().unwrap(),
    );
    assert_eq!(detail.unwrap().saved_bytes, 4);
    drop(reopened);
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_cancel_expiry_peer_and_connection_binding_never_admit_tasks() {
    let mut pair = Pair::new().await;
    let source = pair.root.join("目录");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), b"a").unwrap();
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source.clone())
        .await
        .unwrap();
    pair.a.cancel_preview(&preview.id);
    assert!(
        pair.a
            .confirm_directory(&pair.ca, pair.ib, preview.id)
            .await
            .is_err()
    );
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source.clone())
        .await
        .unwrap();
    assert!(
        pair.a
            .confirm_directory(&pair.ca, pair.ia, preview.id)
            .await
            .is_err()
    );
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source.clone())
        .await
        .unwrap();
    pair.a
        .previews
        .lock()
        .unwrap()
        .get_mut(&preview.id)
        .unwrap()
        .created = Instant::now() - crate::node::core::incremental::PREVIEW_LIFETIME;
    assert!(
        pair.a
            .confirm_directory(&pair.ca, pair.ib, preview.id)
            .await
            .is_err()
    );
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source.clone())
        .await
        .unwrap();
    pair.reconnect().await;
    assert!(
        pair.a
            .confirm_directory(&pair.ca, pair.ib, preview.id)
            .await
            .is_err()
    );
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source)
        .await
        .unwrap();
    pair.a.interrupt_all().await.unwrap();
    assert!(
        pair.a
            .confirm_directory(&pair.ca, pair.ib, preview.id)
            .await
            .is_err()
    );
    assert!(pair.a.snapshot().await.unwrap().is_empty());
    assert!(pair.b.snapshot().await.unwrap().is_empty());
    assert_eq!(
        fs::read_dir(pair.root.join("b-receive")).unwrap().count(),
        0
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_rechecks_receiver_and_refuses_changed_source_after_preview() {
    let pair = Pair::new().await;
    let source = pair.root.join("目录");
    let target = pair.root.join("b-receive/目录");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    for name in ["receiver.txt", "sender.txt"] {
        fs::write(source.join(name), b"same").unwrap();
        fs::write(target.join(name), b"same").unwrap();
    }
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source.clone())
        .await
        .unwrap();
    assert_eq!(preview.unchanged, 2);
    fs::write(target.join("receiver.txt"), b"diff").unwrap();
    fs::write(source.join("sender.txt"), b"diff").unwrap();
    let ids = pair
        .a
        .confirm_directory(&pair.ca, pair.ib, preview.id)
        .await
        .unwrap();
    let error = pair
        .a
        .send_selection(&pair.ca, pair.ib, ids)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("源文件内容已变化"));
    assert_eq!(fs::read(target.join("receiver.txt")).unwrap(), b"same");
    assert_eq!(fs::read(target.join("sender.txt")).unwrap(), b"same");
    assert!(
        !pair
            .b
            .snapshot()
            .await
            .unwrap()
            .iter()
            .any(|r| r.file_details().is_some_and(|d| d.skipped_existing))
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_lost_receipt_replays_same_task_without_backup_or_payload() {
    let pair = Pair::new().await;
    let source = pair.root.join("目录");
    let target = pair.root.join("b-receive/目录");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(source.join("same.txt"), b"same").unwrap();
    fs::write(target.join("same.txt"), b"same").unwrap();
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source)
        .await
        .unwrap();
    let ids = pair
        .a
        .confirm_directory(&pair.ca, pair.ib, preview.id)
        .await
        .unwrap();
    let file = pair
        .a
        .snapshot()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.file_details().is_some())
        .unwrap();
    for id in &ids {
        if id != file.task_id() {
            pair.a
                .send_file(&pair.ca, pair.ib, id.clone())
                .await
                .unwrap();
        }
    }
    pair.b
        .drop_completion
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        pair.a
            .send_file(&pair.ca, pair.ib, file.task_id().clone())
            .await
            .is_err()
    );
    wait_state(&pair.b, file.task_id(), TaskState::Completed).await;
    assert!(
        pair.b
            .task(file.task_id().clone())
            .await
            .unwrap()
            .file_details()
            .unwrap()
            .skipped_existing
    );
    pair.a
        .resume(&pair.ca, pair.ib, file.task_id().clone())
        .await
        .unwrap();
    assert!(
        pair.a
            .task(file.task_id().clone())
            .await
            .unwrap()
            .file_details()
            .unwrap()
            .skipped_existing
    );
    assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
    assert!(!pair.root.join("b-receive/.p2p-desktop").exists());
    pair.shutdown().await;
}
#[tokio::test]
async fn incremental_preview_without_capability_uses_explicit_full_send() {
    let pair = Pair::new().await;
    pair.a.set_peer_incremental_support(pair.ib, false);
    pair.b.set_peer_incremental_support(pair.ia, false);
    let source = pair.root.join("目录");
    let target = pair.root.join("b-receive/目录");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(source.join("same.txt"), b"same").unwrap();
    fs::write(target.join("same.txt"), b"same").unwrap();
    let preview = pair
        .a
        .preview_directory(&pair.ca, pair.ib, source)
        .await
        .unwrap();
    assert!(!preview.supported);
    assert_eq!(
        (
            preview.unchanged,
            preview.transfer_bytes,
            preview.saved_bytes
        ),
        (0, 4, 0)
    );
    let ids = pair
        .a
        .confirm_directory(&pair.ca, pair.ib, preview.id)
        .await
        .unwrap();
    pair.a.send_selection(&pair.ca, pair.ib, ids).await.unwrap();
    assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
    assert!(!pair.a.snapshot().await.unwrap().iter().any(|r| {
        r.file_details()
            .is_some_and(|d| d.incremental || d.skipped_existing)
    }));
    pair.shutdown().await;
}
#[tokio::test]
async fn reading_snapshots_does_not_emit_progress_events() {
    let pair = Pair::new().await;
    let mut changed = pair.a.subscribe();
    pair.a.snapshot().await.unwrap();
    assert!(!changed.has_changed().unwrap());
    changed.borrow_and_update();
    pair.shutdown().await;
}
#[tokio::test]
async fn publication_failure_never_completes_and_receiver_can_retry_after_conflict_removed() {
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(300 * 1024).await;
    let target = pair.root.join("b-receive/文件.bin");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("previous.bin"), b"previous user data").unwrap();
    assert!(
        pair.a
            .send_file(&pair.ca, pair.ib, id.clone())
            .await
            .is_err()
    );
    wait_state(&pair.b, &id, TaskState::Failed).await;
    for service in [&pair.a, &pair.b] {
        let record = service.task(id.clone()).await.unwrap();
        assert_eq!(record.state(), TaskState::Failed);
        assert!(!record.file_details().unwrap().receipt_committed);
    }
    assert_eq!(
        fs::read(target.join("previous.bin")).unwrap(),
        b"previous user data"
    );
    fs::remove_dir_all(&target).unwrap();
    pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
    wait_state(&pair.a, &id, TaskState::Completed).await;
    wait_state(&pair.b, &id, TaskState::Completed).await;
    assert_eq!(fs::read(&target).unwrap(), bytes);
    pair.shutdown().await;
}
#[test]
fn queued_old_epoch_store_write_cannot_reactivate_an_interrupted_task() {
    let root = std::env::temp_dir().join(format!("p2p-transfer-epoch-{}", rand::random::<u128>()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("receive")).unwrap();
    let (mut store, _) = TaskStore::open(&root.join("state/tasks.json")).unwrap();
    let path = root.join("source.bin");
    fs::write(&path, b"source").unwrap();
    let id = disk::select_file(&mut store, Identity::generate().node_id(), path)
        .unwrap()
        .task_id()
        .clone();
    let service = TransferService::new(store, root.join("receive"));
    tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).enable_all().build().unwrap().block_on(async {
            let (release,held)=std::sync::mpsc::channel();let (started,ready)=tokio::sync::oneshot::channel();
            let blocker=tokio::task::spawn_blocking(move||{started.send(()).unwrap();held.recv().unwrap();});ready.await.unwrap();
            let mut old=Box::pin(service.state(&id,TaskState::Connecting));
            tokio::select! {biased; result=&mut old=>panic!("worker should be queued: {result:?}"), _=std::future::ready(())=>{}}
            let mut interrupt=Box::pin(service.interrupt_all());
            tokio::select! {biased; result=&mut interrupt=>panic!("interrupt worker should be queued: {result:?}"), _=std::future::ready(())=>{}}
            release.send(()).unwrap();blocker.await.unwrap();assert!(old.await.unwrap_err().to_string().contains("旧传输会话"));interrupt.await.unwrap();assert_eq!(service.task(id.clone()).await.unwrap().state(),TaskState::Interrupted);
        });
    drop(service);
    fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn receiver_continue_reports_previously_detected_source_change() {
    let pair = Pair::new().await;
    let (id, _) = pair.select(4 * 1024 * 1024).await;
    let (reached, release) = pair.gate();
    let sender = pair.sender(id.clone());
    tokio::time::timeout(Duration::from_secs(10), reached)
        .await
        .unwrap()
        .unwrap();
    pair.a.pause(&id).unwrap();
    release.send(()).unwrap();
    sender.await.unwrap().unwrap();
    wait_state(&pair.b, &id, TaskState::Paused).await;
    fs::write(pair.root.join("文件.bin"), b"different source").unwrap();
    let error = pair
        .a
        .resume(&pair.ca, pair.ib, id.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("变化"));
    wait_state(&pair.a, &id, TaskState::Failed).await;
    let error = pair
        .b
        .resume(&pair.cb, pair.ia, id.clone())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("源文件内容已变化"),
        "unexpected receiver diagnostic: {error}"
    );
    let record = pair.b.task(id).await.unwrap();
    assert_eq!(record.state(), TaskState::Failed);
    assert_eq!(
        record.diagnostic().unwrap().code(),
        TaskErrorCode::SourceChanged
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn authenticated_directory_preserves_structure_empty_entries_and_collision_backups() {
    let pair = Pair::new().await;
    let source = pair.root.join("目录");
    fs::create_dir_all(source.join("nested/空目录")).unwrap();
    fs::write(source.join("nested/空文件.txt"), []).unwrap();
    fs::write(source.join("中文.txt"), b"new selected content").unwrap();
    fs::create_dir(pair.root.join("b-receive/目录")).unwrap();
    fs::write(pair.root.join("b-receive/目录/中文.txt"), b"old content").unwrap();
    let ids = pair.a.select_directory(pair.ib, source).await.unwrap();
    assert_eq!(ids.len(), 5);
    pair.a
        .send_selection(&pair.ca, pair.ib, ids.clone())
        .await
        .unwrap();
    for id in &ids {
        wait_state(&pair.a, id, TaskState::Completed).await;
        wait_state(&pair.b, id, TaskState::Completed).await;
    }
    assert!(pair.root.join("b-receive/目录/nested/空目录").is_dir());
    assert_eq!(
        fs::read(pair.root.join("b-receive/目录/nested/空文件.txt")).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        fs::read(pair.root.join("b-receive/目录/中文.txt")).unwrap(),
        b"new selected content"
    );
    let backups = fs::read_dir(pair.root.join("b-receive/目录"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("中文+")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), b"old content");
    let records = pair.b.snapshot().await.unwrap();
    assert!(
        records
            .iter()
            .all(|r| r.group_id() == records[0].group_id())
    );
    use crate::node::core::task_details::Selection;
    let selection = Selection::Group(records[0].group_id().unwrap().clone());
    for service in [&pair.a, &pair.b] {
        service.select_detail(Some(selection.clone()));
        let detail = service.ui_snapshot().unwrap().detail.unwrap();
        assert_eq!(detail.members.len(), 5);
        assert_eq!(detail.counts.files, 2);
        assert_eq!(detail.counts.directories, 3);
        assert_eq!(detail.counts.completed, 5);
        assert_eq!(detail.total, b"new selected content".len() as u64);
        for record in service.snapshot().await.unwrap() {
            let target = service
                .reveal_task(&Selection::Task(record.task_id().clone()))
                .unwrap();
            assert!(target.exists());
        }
    }
    assert_eq!(
        pair.a.reveal_task(&selection).unwrap(),
        pair.root.join("目录")
    );
    assert_eq!(
        pair.b.reveal_task(&selection).unwrap(),
        pair.root.join("b-receive")
    );
    // Completed task replay does not create another backup or another record.
    pair.a.send_selection(&pair.ca, pair.ib, ids).await.unwrap();
    assert_eq!(pair.b.snapshot().await.unwrap().len(), 5);
    assert_eq!(
        fs::read_dir(pair.root.join("b-receive/目录"))
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .starts_with("中文+"))
            .count(),
        1
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn rejected_directory_scan_leaves_no_partial_persistent_group() {
    let pair = Pair::new().await;
    let source = pair.root.join("invalid");
    fs::create_dir(&source).unwrap();
    // Windows treats CON.txt as a device rather than creating a file.
    // This real directory entry is illegal to the portable protocol on all OSes.
    fs::create_dir(source.join(".p2p-desktop")).unwrap();
    assert!(pair.a.select_directory(pair.ib, source).await.is_err());
    assert!(pair.a.snapshot().await.unwrap().is_empty());
    pair.shutdown().await;
}
#[tokio::test]
async fn directory_group_keeps_original_receive_root_after_settings_change() {
    let pair = Pair::new().await;
    let source = pair.root.join("selected-dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), b"bound to original root").unwrap();
    let ids = pair.a.select_directory(pair.ib, source).await.unwrap();
    assert_eq!(ids.len(), 2);
    pair.a
        .send_file(&pair.ca, pair.ib, ids[0].clone())
        .await
        .unwrap();
    let changed_root = pair.root.join("changed-root");
    fs::create_dir(&changed_root).unwrap();
    pair.b.set_receive_root(changed_root.clone());
    pair.a
        .send_file(&pair.ca, pair.ib, ids[1].clone())
        .await
        .unwrap();
    assert_eq!(
        fs::read(pair.root.join("b-receive/selected-dir/file.txt")).unwrap(),
        b"bound to original root"
    );
    assert_eq!(fs::read_dir(changed_root).unwrap().count(), 0);
    for record in pair.b.snapshot().await.unwrap() {
        assert_eq!(record.local_path(), pair.root.join("b-receive"));
        let target = pair
            .b
            .reveal_task(&crate::node::core::task_details::Selection::Task(
                record.task_id().clone(),
            ))
            .unwrap();
        assert!(target.starts_with(pair.root.join("b-receive")));
    }
    pair.shutdown().await;
}
#[tokio::test]
async fn reserved_but_unstarted_pause_cancels_nonce_and_preserves_manual_retry() {
    let pair = Pair::new().await;
    let (id, _) = pair.select(64 * 1024).await;
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    assert!(pair.a.dispatch_ready(&HashMap::new()).is_empty());
    assert_eq!(pair.a.queue_metrics().pending, 1);
    let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
    let old = pair.a.dispatch_ready(&connections).pop().unwrap();
    assert_eq!(pair.a.queue_metrics().active_files, 1);
    pair.a.pause_task(id.clone()).await.unwrap();
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Paused
    );
    assert_eq!(pair.a.queue_metrics().active_files, 0);
    // A new attempt may be reserved before the old future even starts.
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    let retry = pair.a.dispatch_ready(&connections).pop().unwrap();
    assert!(pair.a.execute_queued(old, pair.ca.clone()).await.is_err());
    assert_eq!(pair.a.queue_metrics().active_files, 1);
    pair.a.execute_queued(retry, pair.ca.clone()).await.unwrap();
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Completed
    );
    assert_eq!(pair.b.task(id).await.unwrap().state(), TaskState::Completed);
    pair.shutdown().await;
}

#[tokio::test]
async fn real_queue_runs_one_two_three_files_on_the_same_authenticated_peer() {
    for limit in 1..=3 {
        let pair = Pair::new().await;
        pair.a.set_send_limit(limit).unwrap();
        let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
        let mut ids = Vec::new();
        for index in 0..4 {
            let path = pair.root.join(format!("queued-{index}.bin"));
            fs::write(&path, vec![index as u8; 96 * 1024]).unwrap();
            ids.push(pair.a.select_file(pair.ib, path).await.unwrap());
        }
        pair.a.enqueue_tasks(pair.ib, ids.clone()).await.unwrap();
        let first = pair.a.dispatch_ready(&connections);
        assert_eq!(first.len(), limit as usize);
        assert_eq!(pair.a.queue_metrics().pending, 4 - limit as usize);
        let mut running = JoinSet::new();
        for scheduled in first {
            let service = pair.a.clone();
            let connection = pair.ca.clone();
            running.spawn(async move { service.execute_queued(scheduled, connection).await });
        }
        while let Some(result) = running.join_next().await {
            result.unwrap().unwrap();
            for scheduled in pair.a.dispatch_ready(&connections) {
                let service = pair.a.clone();
                let connection = pair.ca.clone();
                running.spawn(async move { service.execute_queued(scheduled, connection).await });
            }
            assert!(pair.a.queue_metrics().active_files <= limit as usize);
        }
        for id in ids {
            assert!(pair.a.task(id.clone()).await.unwrap().receipt_committed());
            assert!(pair.b.task(id).await.unwrap().receipt_committed());
        }
        assert_eq!(pair.a.queue_metrics().pending, 0);
        assert_eq!(pair.a.queue_metrics().active_files, 0);
        assert!(pair.a.set_send_limit(0).is_err());
        assert!(pair.a.set_send_limit(4).is_err());
        pair.shutdown().await;
    }
}

#[tokio::test]
async fn independent_receive_cap_reports_busy_before_creating_a_task_and_is_retryable() {
    let pair = Pair::new().await;
    let mut receive_guards = Vec::new();
    for _ in 0..3 {
        receive_guards.push(
            pair.b
                .claim(
                    pair.ia,
                    &TaskId::generate(),
                    TaskDirection::Receive,
                    false,
                    None,
                    pair.cb.stable_id(),
                )
                .unwrap(),
        );
    }
    // Receiving directory metadata has its own budget, as does sending.
    let metadata = pair
        .b
        .claim(
            pair.ia,
            &TaskId::generate(),
            TaskDirection::Receive,
            true,
            None,
            pair.cb.stable_id(),
        )
        .unwrap();
    assert!(
        pair.b
            .claim(
                pair.ia,
                &TaskId::generate(),
                TaskDirection::Receive,
                false,
                None,
                pair.cb.stable_id(),
            )
            .is_err()
    );
    let (id, bytes) = pair.select(64 * 1024).await;
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
    let scheduled = pair.a.dispatch_ready(&connections).pop().unwrap();
    assert!(
        pair.a
            .execute_queued(scheduled, pair.ca.clone())
            .await
            .is_err()
    );
    let rejected = pair.a.task(id.clone()).await.unwrap();
    assert_eq!(rejected.state(), TaskState::Failed);
    assert_eq!(
        rejected.diagnostic().unwrap().code(),
        TaskErrorCode::PeerBusy
    );
    assert!(can_continue(&rejected));
    assert!(pair.b.snapshot().await.unwrap().is_empty());
    assert!(!pair.root.join("b-receive/文件.bin").exists());
    receive_guards.pop();
    drop(metadata);
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    let scheduled = pair.a.dispatch_ready(&connections).pop().unwrap();
    pair.a
        .execute_queued(scheduled, pair.ca.clone())
        .await
        .unwrap();
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    drop(receive_guards);
    pair.shutdown().await;
}

#[tokio::test]
async fn queued_pause_and_invalid_batch_leave_no_unowned_or_rebound_task() {
    let pair = Pair::new().await;
    let (id, _) = pair.select(1024).await;
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    pair.a.pause_task(id.clone()).await.unwrap();
    assert_eq!(pair.a.queue_metrics().pending, 0);
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Paused
    );
    assert!(
        pair.a
            .enqueue_tasks(pair.ib, vec![id.clone(), TaskId::generate()])
            .await
            .is_err()
    );
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Paused
    );
    assert!(
        pair.a
            .enqueue_tasks(pair.ia, vec![id.clone()])
            .await
            .is_err()
    );
    let view = pair.a.presentation().await.unwrap();
    assert_eq!(view.bytes_per_second[&id], 0.0);
    assert_eq!(view.queue.pending, 0);
    pair.shutdown().await;
}

#[tokio::test]
async fn directory_group_counts_partial_failure_without_claiming_complete() {
    let pair = Pair::new().await;
    let source = pair.root.join("group");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("good.bin"), vec![7; 3000]).unwrap();
    fs::write(source.join("missing.bin"), vec![8; 4000]).unwrap();
    let ids = pair
        .a
        .select_directory(pair.ib, source.clone())
        .await
        .unwrap();
    let group = pair
        .a
        .task(ids[0].clone())
        .await
        .unwrap()
        .group_id()
        .unwrap()
        .clone();
    fs::remove_file(source.join("missing.bin")).unwrap();
    pair.a.enqueue_tasks(pair.ib, ids).await.unwrap();
    let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
    let mut failures = 0;
    loop {
        let scheduled = pair.a.dispatch_ready(&connections);
        if scheduled.is_empty() {
            break;
        }
        for scheduled in scheduled {
            failures += usize::from(
                pair.a
                    .execute_queued(scheduled, pair.ca.clone())
                    .await
                    .is_err(),
            );
        }
    }
    assert_eq!(failures, 1);
    let view = pair.a.presentation().await.unwrap();
    let aggregate = &view.groups[&group];
    assert_eq!(aggregate.children, 3);
    assert_eq!(aggregate.completed, 2);
    assert_eq!(aggregate.failed, 1);
    assert_eq!(aggregate.total_bytes, 7000);
    assert_eq!(aggregate.confirmed_bytes, 3000);
    assert!(!aggregate.complete);
    assert_eq!(
        fs::read(pair.root.join("b-receive/group/good.bin")).unwrap(),
        vec![7; 3000]
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn blocked_file_does_not_stop_ready_file_and_pause_releases_pending_slot() {
    let pair = Pair::new().await;
    pair.a.set_send_limit(2).unwrap();
    let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
    let (slow, _) = pair.select(4 * 1024 * 1024).await;
    let mut others = Vec::new();
    for index in 0..2 {
        let path = pair.root.join(format!("independent-{index}.bin"));
        fs::write(&path, vec![index as u8; 96 * 1024]).unwrap();
        others.push(pair.a.select_file(pair.ib, path).await.unwrap());
    }
    pair.a
        .enqueue_tasks(
            pair.ib,
            vec![slow.clone(), others[0].clone(), others[1].clone()],
        )
        .await
        .unwrap();
    let (reached, release) = pair.gate();
    // Start only the first selected executor, so the injected disk gate
    // deterministically belongs to it rather than a second racing stream.
    let mut selected = pair.a.dispatch_ready(&connections);
    let first = selected.remove(0);
    let service = pair.a.clone();
    let conn = pair.ca.clone();
    let blocked = tokio::spawn(async move { service.execute_queued(first, conn).await });
    tokio::time::timeout(Duration::from_secs(10), reached)
        .await
        .unwrap()
        .unwrap();
    pair.a
        .execute_queued(selected.pop().unwrap(), pair.ca.clone())
        .await
        .unwrap();
    assert_eq!(
        pair.a.task(others[0].clone()).await.unwrap().state(),
        TaskState::Completed
    );
    assert!(!blocked.is_finished());
    pair.a.pause_task(slow.clone()).await.unwrap();
    release.send(()).unwrap();
    blocked.await.unwrap().unwrap();
    assert_eq!(
        pair.a.task(slow.clone()).await.unwrap().state(),
        TaskState::Paused
    );
    assert_eq!(pair.a.queue_metrics().active_files, 0);
    let pending = pair.a.dispatch_ready(&connections).pop().unwrap();
    assert_eq!(pending.entry.id, others[1]);
    pair.a
        .execute_queued(pending, pair.ca.clone())
        .await
        .unwrap();
    let presentation = pair.a.presentation().await.unwrap();
    assert_eq!(presentation.bytes_per_second[&slow], 0.0);
    assert_eq!(pair.a.task(slow).await.unwrap().state(), TaskState::Paused);
    pair.shutdown().await;
}

#[tokio::test]
async fn bounded_event_overflow_is_repaired_by_authoritative_lifecycle_snapshot() {
    let pair = Pair::new().await;
    let path = pair.root.join("snapshot.bin");
    fs::write(&path, b"snapshot").unwrap();
    let peer = pair.ib;
    let ids = pair
        .a
        .store(move |store| {
            let records = (0..140)
                .map(|_| {
                    disk::scan_selected_file(
                        peer,
                        path.clone(),
                        &crate::node::core::files::ScanCancellation::default(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let ids = store.create_selection(records).map_err(disk::local_error)?;
            for id in &ids {
                disk::transition(store, id, TaskState::Queued)?;
            }
            Ok(ids)
        })
        .await
        .unwrap();
    pair.a.interrupt_all().await.unwrap();
    let view = pair.a.presentation().await.unwrap();
    assert!(view.resync_required);
    assert_eq!(view.events.len(), 256);
    assert_eq!(view.tasks.len(), ids.len());
    assert!(
        view.tasks
            .iter()
            .all(|task| task.state() == TaskState::Interrupted)
    );
    assert_eq!(view.queue.pending, 0);
    let next = pair.a.presentation().await.unwrap();
    assert!(!next.resync_required);
    assert!(next.events.is_empty());
    assert_eq!(next.tasks.len(), ids.len());
    pair.shutdown().await;
}
#[tokio::test]
async fn pause_after_durable_queued_before_worker_return_cannot_be_lost() {
    let pair = Pair::new().await;
    let (id, _) = pair.select(1024).await;
    let (reached_tx, reached) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    *pair.a.admission_gate.lock().unwrap() = Some((reached_tx, release_rx));
    let service = pair.a.clone();
    let task = id.clone();
    let peer = pair.ib;
    let worker = tokio::spawn(async move { service.enqueue_tasks(peer, vec![task]).await });
    tokio::time::timeout(Duration::from_secs(10), reached)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Queued
    );
    let pause = pair.a.pause_task(id.clone()).await;
    release.send(()).unwrap();
    let admission = worker.await.unwrap();
    let record = pair.a.task(id).await.unwrap();
    let metrics = pair.a.queue_metrics();
    pair.shutdown().await;
    assert!(pause.is_ok(), "visible Queued task lost pause: {pause:?}");
    admission.unwrap();
    assert_eq!(record.state(), TaskState::Paused);
    assert_eq!(metrics.pending, 0);
    assert_eq!(metrics.active_files, 0);
}
#[tokio::test]
async fn fresh_file_and_directory_selection_are_pauseable_before_worker_returns() {
    for directory in [false, true] {
        let pair = Pair::new().await;
        let source = pair.root.join(if directory {
            "selected-dir"
        } else {
            "selected.bin"
        });
        if directory {
            fs::create_dir(&source).unwrap();
            fs::create_dir(source.join("empty")).unwrap();
            fs::write(source.join("child.bin"), b"child").unwrap();
        } else {
            fs::write(&source, b"file").unwrap();
        }
        let (reached_tx, reached) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *pair.a.admission_gate.lock().unwrap() = Some((reached_tx, release_rx));
        let service = pair.a.clone();
        let peer = pair.ib;
        let worker = tokio::spawn(async move {
            if directory {
                service.select_directory(peer, source).await
            } else {
                service.select_file(peer, source).await.map(|id| vec![id])
            }
        });
        tokio::time::timeout(Duration::from_secs(10), reached)
            .await
            .unwrap()
            .unwrap();
        let records = pair.a.snapshot().await.unwrap();
        assert_eq!(records.len(), if directory { 3 } else { 1 });
        assert_eq!(pair.a.queue_metrics().pending, records.len());
        for record in &records {
            assert_eq!(record.state(), TaskState::Queued);
            pair.a.pause_task(record.task_id().clone()).await.unwrap();
        }
        release.send(()).unwrap();
        let ids = worker.await.unwrap().unwrap();
        assert_eq!(ids.len(), records.len());
        for id in ids {
            assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Paused);
        }
        assert_eq!(pair.a.queue_metrics().pending, 0);
        assert_eq!(pair.a.queue_metrics().active_files, 0);
        assert!(pair.b.snapshot().await.unwrap().is_empty());
        pair.shutdown().await;
    }
}

#[tokio::test]
async fn full_queue_retains_new_selection_as_manually_retryable_interrupted_record() {
    let pair = Pair::new().await;
    // Model the full pending budget without doing 4096 unnecessary fsyncs.
    // The selected record and its subsequent QUIC retry are real.
    pair.a
        .sender_queue
        .lock()
        .unwrap()
        .enqueue(
            (0..crate::node::core::queue::MAX_QUEUED)
                .map(|_| crate::node::core::queue::QueueEntry {
                    id: TaskId::generate(),
                    peer: pair.ib,
                    metadata: false,
                })
                .collect(),
        )
        .unwrap();
    let path = pair.root.join("capacity.bin");
    fs::write(&path, b"capacity").unwrap();
    assert!(pair.a.select_file(pair.ib, path).await.is_err());
    let records = pair.a.snapshot().await.unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.state(), TaskState::Interrupted);
    assert!(can_continue(record));
    assert_eq!(
        pair.a.queue_metrics().pending,
        crate::node::core::queue::MAX_QUEUED
    );
    let id = record.task_id().clone();
    pair.a.sender_queue.lock().unwrap().clear();
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
    let scheduled = pair.a.dispatch_ready(&connections).pop().unwrap();
    pair.a
        .execute_queued(scheduled, pair.ca.clone())
        .await
        .unwrap();
    assert!(pair.a.task(id.clone()).await.unwrap().receipt_committed());
    assert!(pair.b.task(id).await.unwrap().receipt_committed());
    assert_eq!(
        fs::read(pair.root.join("b-receive/capacity.bin")).unwrap(),
        b"capacity"
    );
    pair.shutdown().await;
}
#[test]
fn cancelling_selection_while_its_commit_is_queued_cannot_create_a_ghost_task() {
    let root =
        std::env::temp_dir().join(format!("p2p-cancel-selection-{}", rand::random::<u128>()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("receive")).unwrap();
    let source = root.join("source.bin");
    fs::write(&source, b"cancelled source").unwrap();
    let (store, _) = TaskStore::open(&root.join("state/tasks.json")).unwrap();
    let service = TransferService::new(store, root.join("receive"));
    tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).enable_all().build().unwrap().block_on(async {
            let (release_first, held_first) = std::sync::mpsc::channel();
            let (started_first, ready_first) = tokio::sync::oneshot::channel();
            let first = tokio::task::spawn_blocking(move || {started_first.send(()).unwrap(); held_first.recv().unwrap();});
            ready_first.await.unwrap();
            let peer = Identity::generate().node_id();
            let mut selection = Box::pin(service.select_file(peer, source));
            tokio::select! {biased; result=&mut selection=>panic!("scan must be queued: {result:?}"), _=std::future::ready(())=>{}}
            // FIFO in the single blocking worker: scan first, then this barrier,
            // then the store commit submitted when we poll the completed scan.
            let (release_second, held_second) = std::sync::mpsc::channel();
            let (started_second, ready_second) = tokio::sync::oneshot::channel();
            let second = tokio::task::spawn_blocking(move || {started_second.send(()).unwrap(); held_second.recv().unwrap();});
            release_first.send(()).unwrap(); ready_second.await.unwrap();
            tokio::select! {biased; result=&mut selection=>panic!("commit must be queued: {result:?}"), _=std::future::ready(())=>{}}
            drop(selection);
            release_second.send(()).unwrap(); first.await.unwrap(); second.await.unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            assert!(service.snapshot().await.unwrap().is_empty(), "cancelled worker created a task after its future was dropped");
            assert_eq!(service.queue_metrics().pending, 0);
        });
    drop(service);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn continue_after_paused_receipt_waits_for_previous_owned_slot_cleanup() {
    tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).enable_all().build().unwrap().block_on(async {
            let pair = Pair::new().await;
            let (id, bytes) = pair.select(4 * 1024 * 1024).await;
            let (data_reached, data_release) = pair.gate();
            let (cleanup_reached_tx, cleanup_reached) = tokio::sync::oneshot::channel();
            let (cleanup_release, cleanup_release_rx) = tokio::sync::oneshot::channel();
            *pair.a.source_cleanup_gate.lock().unwrap() = Some((cleanup_reached_tx, cleanup_release_rx));
            let sender = pair.sender(id.clone());
            tokio::time::timeout(Duration::from_secs(10), data_reached).await.unwrap().unwrap();
            pair.a.pause_task(id.clone()).await.unwrap(); data_release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(10), cleanup_reached).await.unwrap().unwrap();
            assert_eq!(pair.a.task(id.clone()).await.unwrap().state(), TaskState::Paused);
            wait_state(&pair.b, &id, TaskState::Paused).await;
            assert_eq!(pair.a.queue_metrics().active_files, 1);
            let mut continuation = Box::pin(pair.a.enqueue_tasks(pair.ib, vec![id.clone()]));
            tokio::select! {biased; result=&mut continuation=>panic!("continuation must await old cleanup: {result:?}"), _=std::future::ready(())=>{}}
            // Single blocking worker ensures the first store/read job completes
            // before this barrier, without a timing/sleep assumption.
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            tokio::select! {biased; result=&mut continuation=>panic!("continuation lost behind old active slot: {result:?}"), _=std::future::ready(())=>{}}
            cleanup_release.send(()).unwrap(); sender.await.unwrap().unwrap();
            continuation.await.unwrap();
            assert_eq!(pair.a.queue_metrics().pending, 1);
            assert_eq!(pair.a.queue_metrics().active_files, 0);
            let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
            let scheduled = pair.a.dispatch_ready(&connections).pop().unwrap();
            assert_eq!(scheduled.entry.id, id);
            pair.a.execute_queued(scheduled, pair.ca.clone()).await.unwrap();
            assert!(pair.a.task(id.clone()).await.unwrap().receipt_committed());
            assert!(pair.b.task(id).await.unwrap().receipt_committed());
            assert_eq!(fs::read(pair.root.join("b-receive/文件.bin")).unwrap(), bytes);
            pair.shutdown().await;
        });
}
#[tokio::test]
async fn speed_uses_authenticated_connection_receiver_result_and_never_creates_file_tasks() {
    use crate::node::core::protocol::SpeedDirection;
    use crate::node::core::speed::SpeedStatus;
    let pair = Pair::new_inner(true).await;
    for direction in [SpeedDirection::Upload, SpeedDirection::Download] {
        let report = pair.a.start_speed(pair.ib, direction, 600).await.unwrap();
        assert_eq!(report.status, SpeedStatus::Completed);
        assert!(report.bytes > 0);
        assert!(report.elapsed > Duration::ZERO);
        let mut changed = pair.b.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if pair.b.speed_snapshots().get(&pair.ia).is_some_and(|s| {
                    s.test_id == report.test_id && s.status == SpeedStatus::Completed
                }) {
                    break;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let other = pair.b.speed_snapshots()[&pair.ia].clone();
        assert_eq!(other.test_id, report.test_id);
        assert_eq!(other.bytes, report.bytes);
        assert_eq!(other.elapsed, report.elapsed);
        assert_eq!(other.status, SpeedStatus::Completed);
        assert!(pair.a.snapshot().await.unwrap().is_empty());
        assert!(pair.b.snapshot().await.unwrap().is_empty());
        assert_eq!(
            fs::read_dir(pair.root.join("a-receive")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read_dir(pair.root.join("b-receive")).unwrap().count(),
            0
        );
    }
    assert!(
        pair.a
            .start_speed(pair.ib, SpeedDirection::Upload, 3000)
            .await
            .is_err()
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn speed_simultaneous_requests_have_one_grant_and_one_busy() {
    use crate::node::core::protocol::SpeedDirection;
    let pair = Pair::new_inner(true).await;
    let (a, b) = tokio::join!(
        pair.a.start_speed(pair.ib, SpeedDirection::Upload, 30),
        pair.b.start_speed(pair.ia, SpeedDirection::Download, 30)
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "a={a:?} b={b:?}"
    );
    let error = a.err().or_else(|| b.err()).unwrap().to_string();
    assert!(error.contains("进行"), "{error}");
    assert!(pair.ca.close_reason().is_none());
    assert!(pair.cb.close_reason().is_none());
    pair.a
        .start_speed(pair.ib, SpeedDirection::Download, 60)
        .await
        .unwrap();
    pair.shutdown().await;
}
#[tokio::test]
async fn speed_and_file_activity_exclude_each_other_without_silent_pause() {
    use crate::node::core::protocol::SpeedDirection;
    let pair = Pair::new_inner(true).await;
    let (id, _) = pair.select(1024).await;
    let connections = HashMap::from([(pair.ib, pair.ca.clone())]);
    let scheduled = pair.a.dispatch_ready(&connections).pop().unwrap();
    assert!(
        pair.a
            .start_speed(pair.ib, SpeedDirection::Upload, 30)
            .await
            .is_err()
    );
    assert_eq!(
        pair.a.task(id.clone()).await.unwrap().state(),
        TaskState::Queued
    );
    assert_eq!(pair.a.queue_metrics().active_files, 1);
    pair.a.pause_task(id.clone()).await.unwrap();
    drop(scheduled);
    pair.a.speed_peers.lock().unwrap()[&pair.ib].set_test_duration(Duration::from_secs(2));
    let service = pair.a.clone();
    let peer = pair.ib;
    let job =
        tokio::spawn(async move { service.start_speed(peer, SpeedDirection::Upload, 30).await });
    let mut changes = pair.a.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pair.a.activity.lock().speed_active(pair.ib)
                && pair.b.activity.lock().speed_active(pair.ia)
            {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let count = pair.a.snapshot().await.unwrap().len();
    let source = pair.root.join("reject.bin");
    fs::write(&source, b"reject").unwrap();
    assert!(pair.a.select_file(pair.ib, source).await.is_err());
    assert_eq!(pair.a.snapshot().await.unwrap().len(), count);
    assert!(pair.a.dispatch_ready(&connections).is_empty());
    assert_eq!(pair.a.task(id).await.unwrap().state(), TaskState::Paused);
    let test = pair.a.speed_snapshots()[&pair.ib].test_id.clone();
    pair.a.cancel_speed(pair.ib, &test).unwrap();
    assert!(job.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        while pair.b.activity.lock().speed_active(pair.ia) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(pair.ca.close_reason().is_none());
    pair.shutdown().await;
}
#[tokio::test]
async fn speed_cancel_and_stale_uni_token_do_not_pollute_next_test_or_file_transfer() {
    use crate::node::core::protocol::{SpeedControl, SpeedDirection};
    use crate::node::core::speed::SpeedStatus;
    for (direction, remote_cancel) in [
        (SpeedDirection::Upload, false),
        (SpeedDirection::Download, true),
    ] {
        let pair = Pair::new_inner(true).await;
        pair.a.speed_peers.lock().unwrap()[&pair.ib].set_test_duration(Duration::from_secs(2));
        pair.b.speed_peers.lock().unwrap()[&pair.ia].set_test_duration(Duration::from_secs(2));
        let service = pair.a.clone();
        let peer = pair.ib;
        let job = tokio::spawn(async move { service.start_speed(peer, direction, 600).await });
        let mut changes = pair.a.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if pair
                    .a
                    .speed_snapshots()
                    .get(&pair.ib)
                    .is_some_and(|s| s.bytes > 0)
                {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let old = pair.a.speed_peers.lock().unwrap()[&pair.ib]
            .test_lease()
            .unwrap();
        if remote_cancel {
            pair.b.cancel_speed(pair.ia, &old.test_id).unwrap();
        } else {
            pair.a.cancel_speed(pair.ib, &old.test_id).unwrap();
        }
        assert!(
            job.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("已取消")
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !pair.a.activity.lock().speed_active(pair.ib)
                    && !pair.b.activity.lock().speed_active(pair.ia)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            pair.a.speed_snapshots()[&pair.ib].status,
            SpeedStatus::Cancelled
        );
        assert_eq!(pair.a.speed_snapshots()[&pair.ib].bytes_per_second, 0.0);
        pair.a.speed_peers.lock().unwrap()[&pair.ib].set_test_duration(Duration::from_millis(200));
        pair.b.speed_peers.lock().unwrap()[&pair.ia].set_test_duration(Duration::from_millis(200));
        let service = pair.a.clone();
        let peer = pair.ib;
        let next =
            tokio::spawn(
                async move { service.start_speed(peer, SpeedDirection::Upload, 30).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if pair
                    .b
                    .speed_snapshots()
                    .get(&pair.ia)
                    .is_some_and(|s| s.test_id != old.test_id && s.status == SpeedStatus::Running)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut stale = pair.ca.open_uni().await.unwrap();
        protocol::write(
            &mut stale,
            &Frame {
                request_id: 1,
                message: Message::Speed(SpeedControl::Granted(old.clone())),
            },
        )
        .await
        .unwrap();
        let _ = stale.write_all(b"old payload must not be consumed").await;
        let _ = stale.finish();
        let report = next.await.unwrap().unwrap();
        assert_ne!(report.test_id, old.test_id);
        assert_eq!(report.status, SpeedStatus::Completed);
        assert!(pair.ca.close_reason().is_none());
        assert!(pair.cb.close_reason().is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            while pair.b.activity.lock().speed_active(pair.ia) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (id, bytes) = pair.select(4096).await;
        pair.a
            .send_file(&pair.ca, pair.ib, id.clone())
            .await
            .unwrap();
        assert!(pair.a.task(id.clone()).await.unwrap().receipt_committed());
        assert!(pair.b.task(id).await.unwrap().receipt_committed());
        assert_eq!(
            fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
            bytes
        );
        pair.shutdown().await;
    }
}
#[tokio::test]
async fn speed_remote_busy_and_invalid_uni_headers_preserve_connection_and_next_lease() {
    use crate::node::core::protocol::{SpeedControl, SpeedDirection};
    let pair = Pair::new_inner(true).await;
    let peer_file = pair.b.activity.file(pair.ia).unwrap();
    let error = pair
        .a
        .start_speed(pair.ib, SpeedDirection::Upload, 30)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("进行"), "{error}");
    drop(peer_file);
    tokio::time::timeout(Duration::from_secs(5), async {
        while pair.a.activity.lock().speed_active(pair.ib) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pair.a.speed_peers.lock().unwrap()[&pair.ib].set_test_duration(Duration::from_millis(300));
    pair.b.speed_peers.lock().unwrap()[&pair.ia].set_test_duration(Duration::from_millis(300));
    let (data_reached, data_release) = pair.a.speed_peers.lock().unwrap()[&pair.ib].gate_data();
    let service = pair.a.clone();
    let peer = pair.ib;
    let job =
        tokio::spawn(async move { service.start_speed(peer, SpeedDirection::Upload, 60).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while pair.b.speed_peers.lock().unwrap()[&pair.ia]
            .test_lease()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), data_reached)
        .await
        .unwrap()
        .unwrap();
    let mut forged = pair.b.speed_peers.lock().unwrap()[&pair.ia]
        .test_lease()
        .unwrap();
    forged.stream_token[0] ^= 1;
    let mut bad = pair.ca.open_uni().await.unwrap();
    protocol::write(
        &mut bad,
        &Frame {
            request_id: 1,
            message: Message::Speed(SpeedControl::Granted(forged)),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), bad.stopped())
            .await
            .unwrap()
            .unwrap(),
        Some(5u32.into())
    );
    let mut oversized = pair.ca.open_uni().await.unwrap();
    oversized.write_all(&4097u32.to_le_bytes()).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), oversized.stopped())
            .await
            .unwrap()
            .unwrap(),
        Some(5u32.into())
    );
    data_release.send(()).unwrap();
    job.await.unwrap().unwrap();
    assert!(pair.ca.close_reason().is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        while pair.b.activity.lock().speed_active(pair.ia) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pair.a
        .start_speed(pair.ib, SpeedDirection::Download, 30)
        .await
        .unwrap();
    assert!(pair.a.snapshot().await.unwrap().is_empty());
    assert!(pair.b.snapshot().await.unwrap().is_empty());
    pair.shutdown().await;
}
#[tokio::test]
async fn next_speed_request_waits_for_completed_peer_lease_cleanup() {
    use crate::node::core::protocol::SpeedDirection;
    let pair = Pair::new_inner(true).await;
    let (cleanup_reached, cleanup_release) =
        pair.b.speed_peers.lock().unwrap()[&pair.ia].gate_cleanup();
    let first = pair
        .a
        .start_speed(pair.ib, SpeedDirection::Upload, 30)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), cleanup_reached)
        .await
        .unwrap()
        .unwrap();
    assert!(pair.b.activity.lock().speed_active(pair.ia));
    let waiting = pair.b.speed_peers.lock().unwrap()[&pair.ia].gate_wait();
    let service = pair.a.clone();
    let peer = pair.ib;
    let mut next = tokio::spawn(async move {
        service
            .start_speed(peer, SpeedDirection::Download, 60)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result=&mut next=>panic!("new request lost behind completed old lease: {result:?}"),
            reached=waiting=>reached.unwrap(),
        }
    })
    .await
    .unwrap();
    assert!(!next.is_finished());
    cleanup_release.send(()).unwrap();
    let second = next.await.unwrap().unwrap();
    assert_ne!(second.test_id, first.test_id);
    assert_eq!(second.direction, SpeedDirection::Download);
    pair.shutdown().await;
}
#[tokio::test]
async fn task_details_attempt_cleanup_fences_a_replacement_claim() {
    let pair = Pair::new().await;
    let id = TaskId::generate();
    let (old, _) = pair
        .b
        .claim(
            pair.ia,
            &id,
            TaskDirection::Receive,
            false,
            None,
            pair.cb.stable_id(),
        )
        .unwrap();
    // Pause exactly after the old attempt begins retiring its statistics.
    let (reached, wait) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *pair.b.statistics_cleanup_gate.lock().unwrap() = Some((reached, resume));
    let old = std::thread::spawn(move || drop(old));
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .unwrap()
        .unwrap();
    let fenced = matches!(
        pair.b.active.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    );
    release.send(()).unwrap();
    old.join().unwrap();
    assert!(
        fenced,
        "replacement can overtake the old attempt's statistics cleanup"
    );
    let (new, _) = pair
        .b
        .claim(
            pair.ia,
            &id,
            TaskDirection::Receive,
            false,
            None,
            pair.cb.stable_id(),
        )
        .unwrap();
    {
        let measurements = pair.b.measurements.lock().unwrap();
        let now = Instant::now();
        let sample = measurements.sample(&id, now).unwrap();
        assert_eq!(sample.attempts, 2);
        assert!(
            measurements
                .sample(&id, now + Duration::from_secs(1))
                .unwrap()
                .elapsed
                > sample.elapsed
        );
    }
    drop(new);
    pair.shutdown().await;
}
#[tokio::test]
async fn task_details_follow_real_pause_retry_receipts_and_history_removal() {
    use crate::node::core::task_details::Selection;
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(8 * 1024 * 1024).await;
    let selected = Selection::Task(id.clone());
    pair.a.select_detail(Some(selected.clone()));
    pair.b.select_detail(Some(selected.clone()));
    let (reached, release) = pair.gate();
    let sender = pair.sender(id.clone());
    reached.await.unwrap();
    assert_eq!(
        pair.b.reveal_task(&selected).unwrap(),
        pair.root.join("b-receive")
    );
    pair.b.pause_task(id.clone()).await.unwrap();
    release.send(()).unwrap();
    sender.await.unwrap().unwrap();
    wait_state(&pair.b, &id, TaskState::Paused).await;
    for service in [&pair.a, &pair.b] {
        service.wait_previous_attempt(&id).await.unwrap();
        let detail = service.ui_snapshot().unwrap().detail.unwrap();
        assert_eq!(detail.rate, 0.);
        assert_eq!(detail.eta, None);
        assert_eq!(detail.run.attempts, 1);
        let measurements = service.measurements.lock().unwrap();
        let now = Instant::now();
        assert_eq!(
            measurements.sample(&id, now).unwrap().elapsed,
            measurements
                .sample(&id, now + Duration::from_secs(100))
                .unwrap()
                .elapsed
        );
    }
    pair.b.resume(&pair.cb, pair.ia, id.clone()).await.unwrap();
    wait_state(&pair.b, &id, TaskState::Completed).await;
    for service in [&pair.a, &pair.b] {
        service.wait_previous_attempt(&id).await.unwrap();
        let before = service.snapshot().await.unwrap();
        let changed = *service.changed.borrow();
        let detail = service.ui_snapshot().unwrap().detail.unwrap();
        assert_eq!(detail.confirmed, bytes.len() as u64);
        assert_eq!(detail.run.attempts, 2);
        assert_eq!(detail.run.retries, 1);
        assert_eq!(detail.run.automatic, 0);
        assert_eq!(detail.rate, 0.);
        assert_eq!(detail.eta, None);
        assert!(service.reveal_task(&selected).unwrap().is_file());
        assert_eq!(*service.changed.borrow(), changed);
        assert_eq!(service.snapshot().await.unwrap(), before);
    }
    let destination = pair.root.join("b-receive/文件.bin");
    assert_eq!(pair.b.reveal_task(&selected).unwrap(), destination);
    pair.b.remove_completed_task(&id).unwrap();
    assert!(pair.b.ui_snapshot().unwrap().detail.is_none());
    assert!(pair.b.reveal_task(&selected).is_err());
    assert_eq!(fs::read(destination).unwrap(), bytes);
    pair.shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn task_details_locator_rejects_replaced_source_parent_and_receive_leaf_links() {
    use crate::node::core::task_details::Selection;
    let pair = Pair::new().await;
    let folder = pair.root.join("source");
    fs::create_dir(&folder).unwrap();
    let path = folder.join("data.bin");
    fs::write(&path, b"original").unwrap();
    let id = pair.a.select_file(pair.ib, path.clone()).await.unwrap();
    let selected = Selection::Task(id.clone());
    assert_eq!(pair.a.reveal_task(&selected).unwrap(), path);
    pair.sender(id.clone()).await.unwrap().unwrap();
    wait_state(&pair.b, &id, TaskState::Completed).await;
    pair.b.wait_previous_attempt(&id).await.unwrap();
    let received = pair.b.reveal_task(&selected).unwrap();
    let outside = pair.root.join("outside.bin");
    fs::write(&outside, b"untouched").unwrap();
    fs::remove_file(&received).unwrap();
    std::os::unix::fs::symlink(&outside, &received).unwrap();
    assert!(pair.b.reveal_task(&selected).is_err());
    fs::rename(&folder, pair.root.join("old-source")).unwrap();
    std::os::unix::fs::symlink(pair.root.join("old-source"), &folder).unwrap();
    assert!(pair.a.reveal_task(&selected).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"untouched");
    pair.shutdown().await;
}
#[tokio::test]
async fn ui_projection_tracks_real_pause_and_receipts_without_rebinding_or_false_completion() {
    let pair = Pair::new().await;
    let (id, bytes) = pair.select(4096).await;
    let snapshot = pair.a.ui_snapshot().unwrap();
    let row = snapshot.tasks.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.peer, pair.ib);
    assert_eq!(row.state, TaskState::Queued);
    assert_eq!(row.confirmed, 0);
    assert!(row.can_pause());
    pair.a.pause_task(id.clone()).await.unwrap();
    let snapshot = pair.a.ui_snapshot().unwrap();
    let row = snapshot.tasks.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.state, TaskState::Paused);
    assert_eq!(row.peer, pair.ib);
    assert_eq!(row.rate, 0.);
    assert!(row.can_continue());
    assert!(!row.can_pause());
    pair.a
        .enqueue_tasks(pair.ib, vec![id.clone()])
        .await
        .unwrap();
    let scheduled = pair
        .a
        .dispatch_ready(&HashMap::from([(pair.ib, pair.ca.clone())]))
        .pop()
        .unwrap();
    pair.a
        .execute_queued(scheduled, pair.ca.clone())
        .await
        .unwrap();
    for service in [&pair.a, &pair.b] {
        let snapshot = service.ui_snapshot().unwrap();
        let row = snapshot.tasks.iter().find(|r| r.id == id).unwrap();
        assert_eq!(row.state, TaskState::Completed);
        assert_eq!(row.confirmed, bytes.len() as u64);
        assert_eq!(row.percent(), 100.);
        assert_eq!(row.rate, 0.);
        assert!(!row.can_pause());
        assert!(!row.can_continue());
    }
    assert_eq!(
        fs::read(pair.root.join("b-receive/文件.bin")).unwrap(),
        bytes
    );
    pair.shutdown().await;
}
#[tokio::test]
async fn speed_revocation_uses_lease_owner_and_preserves_reverse_password_right() {
    use crate::node::core::protocol::SpeedDirection;
    use crate::node::core::remote_auth::{AuthorizationGrant, RemoteAuthorization};
    use crate::node::core::speed::SpeedStatus;
    for revoke_request_direction in [false, true] {
        let pair = Pair::new_inner(true).await;
        pair.a.speed_peers.lock().unwrap()[&pair.ib].set_test_duration(Duration::from_millis(300));
        pair.b.speed_peers.lock().unwrap()[&pair.ia].set_test_duration(Duration::from_millis(300));
        let (reached, release) = pair.a.speed_peers.lock().unwrap()[&pair.ib].gate_data();
        let service = pair.a.clone();
        let peer = pair.ib;
        let job =
            tokio::spawn(
                async move { service.start_speed(peer, SpeedDirection::Upload, 30).await },
            );
        tokio::time::timeout(Duration::from_secs(5), reached)
            .await
            .unwrap()
            .unwrap();
        let rights = RemoteAuthorization {
            inbound: AuthorizationGrant::password(!revoke_request_direction),
            outbound: AuthorizationGrant::password(true),
        };
        pair.b.revoke_authorization(pair.ia, rights).await.unwrap();
        if revoke_request_direction {
            // Cancellation can already have dropped the gated data future.
            let _ = release.send(());
        } else {
            release.send(()).unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap();
        if revoke_request_direction {
            assert!(
                result.is_err()
                    || result.is_ok_and(|report| report.status != SpeedStatus::Completed)
            );
        } else {
            assert_eq!(result.unwrap().status, SpeedStatus::Completed);
        }
        assert!(pair.ca.close_reason().is_none());
        pair.shutdown().await;
    }
}
