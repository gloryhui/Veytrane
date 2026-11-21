//! Actual CLI TLS signaling and direct loopback services.
use p2p_file::{identity::Identity, net::family::bind_udp};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn ready(process: &mut Process, log: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = fs::read_to_string(log).unwrap();
        if text.contains(marker) {
            return;
        }
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "process stopped: {text}"
        );
        assert!(Instant::now() < deadline, "CLI readiness timeout: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn run(command: &mut Command, log: &Path) -> Process {
    let output = fs::File::create(log).unwrap();
    Process(
        command
            .env("RUST_LOG", "info")
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    )
}
#[cfg(feature = "standalone-files")]
fn finished(process: &mut Process, log: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            return status.success();
        }
        assert!(
            Instant::now() < deadline,
            "CLI completion timeout: {}",
            fs::read_to_string(log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn signal_tls_files(root: &Path) -> (PathBuf, PathBuf) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let cert_path = root.join("server.pem");
    let key_path = root.join("server.key");
    fs::write(&cert_path, cert.pem()).unwrap();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&key_path)
        .unwrap()
        .write_all(key.serialize_pem().as_bytes())
        .unwrap();
    (cert_path, key_path)
}
fn reserve_udp() -> SocketAddr {
    let socket = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    address
}
fn start_signal_server(root: &Path, cert: &Path, key: &Path) -> (SocketAddr, Process, PathBuf) {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let signal = socket.local_addr().unwrap();
    drop(socket);
    let log = root.join("signal.log");
    let mut server = run(
        Command::new(env!("CARGO_BIN_EXE_p2p_file"))
            .args([
                "signal-server",
                "--listen",
                &signal.to_string(),
                "--tls-cert",
            ])
            .arg(cert)
            .arg("--tls-key")
            .arg(key)
            .arg("--short-id-db")
            .arg(root.join("ids.sqlite3")),
        &log,
    );
    ready(&mut server, &log, "信令服务器已启动");
    (signal, server, log)
}
fn reserve_tcp() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}
fn spawn_echo() -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let echo = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "TCP echo was never reached");
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("accept local echo connection: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buffer = [0u8; 8192];
        loop {
            let read = match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("echo read failed: {error}"),
            };
            stream.write_all(&buffer[..read]).unwrap();
        }
    });
    (address, echo)
}
#[cfg(feature = "standalone-files")]
#[test]
fn signaling_tls_cli_server_serve_push_verify_names_and_preserve_file_transfer() {
    let root = Directory(std::env::temp_dir().join(format!(
        "p2p-cli-signal-tls-{:032x}",
        rand::random::<u128>()
    )));
    fs::create_dir(&root.0).unwrap();
    let received = root.0.join("received");
    fs::create_dir(&received).unwrap();
    let (cert_path, key_path) = signal_tls_files(&root.0);
    let (signal, mut server, signal_log) = start_signal_server(&root.0, &cert_path, &key_path);
    let binary = env!("CARGO_BIN_EXE_p2p_file");
    let sender = Identity::generate();
    let receiver = Identity::generate();
    let sender_key = root.0.join("sender.key");
    let receiver_key = root.0.join("receiver.key");
    sender.save(&sender_key).unwrap();
    receiver.save(&receiver_key).unwrap();
    let receiver_udp = reserve_udp();
    let receiver_log = root.0.join("receiver.log");
    let mut receiver_process = run(
        Command::new(binary)
            .arg("--key-file")
            .arg(receiver_key)
            .args([
                "serve",
                "--signal",
                &signal.to_string(),
                "--signal-tls",
                "--signal-ca-file",
            ])
            .arg(&cert_path)
            .args([
                "--signal-server-name",
                "localhost",
                "--allow",
                &sender.node_id().to_hex(),
                "--ip-family",
                "ipv4-only",
                "--port",
                &receiver_udp.port().to_string(),
                "--advertise-only",
                "--advertise",
                &receiver_udp.to_string(),
                "--recv-dir",
            ])
            .arg(&received),
        &receiver_log,
    );
    ready(&mut receiver_process, &receiver_log, "常驻等待中");
    let source = root.0.join("tls-transfer.bin");
    let data = vec![69u8; 160_000];
    fs::write(&source, &data).unwrap();
    let sender_udp = reserve_udp();
    let push = |name: &str| {
        let mut command = Command::new(binary);
        command
            .arg("--key-file")
            .arg(&sender_key)
            .args([
                "push",
                "--signal",
                &signal.to_string(),
                "--signal-tls",
                "--signal-ca-file",
            ])
            .arg(&cert_path)
            .args([
                "--signal-server-name",
                name,
                "--peer",
                &receiver.node_id().to_hex(),
                "--ip-family",
                "ipv4-only",
                "--port",
                &sender_udp.port().to_string(),
                "--advertise-only",
                "--advertise",
                &sender_udp.to_string(),
                "--chunk-size",
                "65536",
            ])
            .arg(&source);
        command
    };
    let bad_log = root.0.join("wrong-name.log");
    let mut failed = run(&mut push("wrong.invalid"), &bad_log);
    assert!(!finished(&mut failed, &bad_log));
    assert!(
        fs::read_to_string(&bad_log)
            .unwrap()
            .contains("证书/主机名校验失败")
    );
    assert!(!received.join("tls-transfer.bin").exists());
    let good_log = root.0.join("push.log");
    let mut sent = run(&mut push("localhost"), &good_log);
    assert!(
        finished(&mut sent, &good_log),
        "push: {}\nserve: {}\nsignal: {}",
        fs::read_to_string(&good_log).unwrap(),
        fs::read_to_string(&receiver_log).unwrap(),
        fs::read_to_string(&signal_log).unwrap()
    );
    assert_eq!(fs::read(received.join("tls-transfer.bin")).unwrap(), data);
    assert!(server.0.try_wait().unwrap().is_none());
    assert!(receiver_process.0.try_wait().unwrap().is_none());
    drop((sent, failed, receiver_process, server));
    eprintln!(
        "CLI_TLS_PROOF verified hostname rejection and TLS-only signaling with authenticated QUIC file transfer"
    );
}

#[test]
fn signaling_tls_cli_tunnel_carries_loopback_tcp_echo() {
    let root = Directory(std::env::temp_dir().join(format!(
        "p2p-cli-signal-tunnel-{:032x}",
        rand::random::<u128>()
    )));
    fs::create_dir(&root.0).unwrap();
    let (cert_path, key_path) = signal_tls_files(&root.0);
    let (signal, mut server, _) = start_signal_server(&root.0, &cert_path, &key_path);
    let binary = env!("CARGO_BIN_EXE_p2p_file");
    let tunnel_identity = Identity::generate();
    let serve_identity = Identity::generate();
    let tunnel_key = root.0.join("tunnel.key");
    let serve_key = root.0.join("serve.key");
    tunnel_identity.save(&tunnel_key).unwrap();
    serve_identity.save(&serve_key).unwrap();
    let (echo_addr, echo) = spawn_echo();
    let serve_udp = reserve_udp();
    let serve_log = root.0.join("serve.log");
    let mut serve = run(
        Command::new(binary)
            .arg("--key-file")
            .arg(&serve_key)
            .args([
                "serve",
                "--signal",
                &signal.to_string(),
                "--signal-tls",
                "--signal-ca-file",
            ])
            .arg(&cert_path)
            .args([
                "--signal-server-name",
                "localhost",
                "--allow",
                &tunnel_identity.node_id().to_hex(),
                "--forward",
                &echo_addr.to_string(),
                "--ip-family",
                "ipv4-only",
                "--port",
                &serve_udp.port().to_string(),
                "--advertise-only",
                "--advertise",
                &serve_udp.to_string(),
                "--punch-attempts",
                "10",
            ]),
        &serve_log,
    );
    ready(&mut serve, &serve_log, "常驻等待中");
    let listen = reserve_tcp();
    let tunnel_udp = reserve_udp();
    let tunnel_log = root.0.join("tunnel.log");
    let mut tunnel = run(
        Command::new(binary)
            .arg("--key-file")
            .arg(&tunnel_key)
            .args([
                "tunnel",
                "--signal",
                &signal.to_string(),
                "--signal-tls",
                "--signal-ca-file",
            ])
            .arg(&cert_path)
            .args([
                "--signal-server-name",
                "localhost",
                "--peer",
                &serve_identity.node_id().to_hex(),
                "--listen",
                &listen.to_string(),
                "--to",
                &echo_addr.to_string(),
                "--ip-family",
                "ipv4-only",
                "--port",
                &tunnel_udp.port().to_string(),
                "--advertise-only",
                "--advertise",
                &tunnel_udp.to_string(),
                "--wait",
                "15",
                "--punch-attempts",
                "10",
            ]),
        &tunnel_log,
    );
    ready(&mut tunnel, &tunnel_log, "本地转发已就绪");
    let mut stream = TcpStream::connect_timeout(&listen, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let payload = vec![0x5au8; 32 * 1024];
    stream.write_all(&payload).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut echoed = vec![0u8; payload.len()];
    stream.read_exact(&mut echoed).unwrap();
    assert_eq!(echoed, payload);
    assert!(server.0.try_wait().unwrap().is_none());
    assert!(serve.0.try_wait().unwrap().is_none());
    assert!(tunnel.0.try_wait().unwrap().is_none());
    echo.join().unwrap();
    drop((stream, tunnel, serve, server));
}

#[cfg(not(feature = "standalone-files"))]
#[test]
fn standalone_without_files_hides_send_recv_and_push() {
    let output = Command::new(env!("CARGO_BIN_EXE_p2p_file"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "p2p_file --help failed");
    let help = String::from_utf8_lossy(&output.stdout);
    for name in ["send", "recv", "push"] {
        let listed = help.lines().any(|line| {
            let line = line.trim_start();
            line.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with("  "))
        });
        assert!(
            !listed,
            "standalone build without file commands exposed {name}:\n{help}"
        );
    }
}
