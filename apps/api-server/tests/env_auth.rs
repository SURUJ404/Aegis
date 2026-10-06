//! Environment-driven configuration of the standalone api-server.
//!
//! Bug R: env overrides (and therefore `.env` files via dotenvy) were only
//! applied when a `--config` TOML file was supplied — with built-in defaults
//! `API_BIND`/`API_TOKEN` were silently ignored.
//!
//! Bug Q: even when `cfg.api.token` was populated, `main` never passed it to
//! `ApiState`, so the documented `API_TOKEN` bearer auth could not activate.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base_port() -> u16 {
    21_000 + (std::process::id() as u16 % 1_500)
}

fn start(port: u16, envs: &[(&str, String)], args: &[&str]) -> Server {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_api-server"));
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.args(args).stdout(Stdio::null()).stderr(Stdio::null());
    let child = cmd.spawn().expect("failed to spawn api-server");
    Server(child)
}

fn wait_for_bind(server: &mut Server, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if let Ok(Some(status)) = server.0.try_wait() {
            panic!("api-server exited before binding 127.0.0.1:{port}: {status}");
        }
        if Instant::now() > deadline {
            panic!("api-server never bound 127.0.0.1:{port} — env override ignored?");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn http_status(port: u16, path: &str, bearer: Option<&str>) -> u16 {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write request");
    let mut buf = String::new();
    stream.read_to_string(&mut buf).expect("read response");
    buf.split_whitespace()
        .nth(1)
        .expect("status line")
        .parse()
        .expect("numeric status")
}

#[test]
fn api_bind_env_is_honored_without_config_file() {
    let port = base_port() + 1;
    let mut srv = start(port, &[("API_BIND", format!("127.0.0.1:{port}"))], &[]);
    wait_for_bind(&mut srv, port);
    assert_eq!(http_status(port, "/healthz", None), 200);
}

#[test]
fn api_token_env_enables_auth_when_config_file_present() {
    let port = base_port() + 2;
    let cfg_path = std::env::temp_dir().join(format!("aegis-auth-test-{}.toml", std::process::id()));
    std::fs::write(&cfg_path, "mode = \"paper\"\n").expect("write temp config");

    let mut srv = start(
        port,
        &[
            ("API_TOKEN", "s3cret-env-token".to_string()),
            ("LQ_CONFIG", cfg_path.display().to_string()),
        ],
        &["--bind", &format!("127.0.0.1:{port}")],
    );
    wait_for_bind(&mut srv, port);

    // Without the bearer token the protected route must refuse.
    assert_eq!(http_status(port, "/api/v1/state", None), 401);
    // Wrong token also refuses.
    assert_eq!(http_status(port, "/api/v1/state", Some("wrong")), 401);
    // Correct token passes.
    assert_eq!(http_status(port, "/api/v1/state", Some("s3cret-env-token")), 200);
    // Liveness stays open.
    assert_eq!(http_status(port, "/healthz", None), 200);

    let _ = std::fs::remove_file(&cfg_path);
}
