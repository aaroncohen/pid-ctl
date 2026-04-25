//! Unix domain socket control interface for `pid-ctl`.
//!
//! Provides a non-blocking listener that a running controller loop can poll
//! each tick to service operator commands (status, set, reset, hold, resume,
//! save) without interrupting the control cadence.

use crate::app::defaults::SOCKET_IO_TIMEOUT;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Maximum request payload size in bytes.
const MAX_REQUEST_BYTES: u64 = 4096;

/// Result of probing an existing socket path on disk.
#[derive(Debug, PartialEq, Eq)]
pub enum ProbeResult {
    /// The path does not exist.
    NoFile,
    /// A socket file exists but no process is listening (safe to clean up).
    Stale,
    /// A socket file exists and a process is actively listening.
    AlreadyRunning,
    /// The path exists but is not a Unix socket.
    NotASocket,
}

/// Inbound request from an operator or tooling.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "cmd")]
pub enum Request {
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "set")]
    Set { param: String, value: f64 },
    #[serde(rename = "reset")]
    Reset,
    #[serde(rename = "hold")]
    Hold,
    #[serde(rename = "resume")]
    Resume,
    #[serde(rename = "save")]
    Save,
}

/// Outbound response to a control request.
///
/// Uses `#[serde(untagged)]` — variant order matters for deserialization but
/// since we only serialize responses, the ordering is cosmetic.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Response {
    Status {
        ok: bool,
        iter: u64,
        pv: f64,
        sp: f64,
        err: f64,
        kp: f64,
        ki: f64,
        kd: f64,
        cv: f64,
        i_acc: f64,
    },
    Set {
        ok: bool,
        param: String,
        old: f64,
        new: f64,
    },
    Reset {
        ok: bool,
        i_acc_before: f64,
    },
    Ack {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ErrorUnknownCommand {
        ok: bool,
        error: String,
        available: Vec<String>,
    },
    ErrorUnknownParam {
        ok: bool,
        error: String,
        settable: Vec<String>,
    },
}

/// Errors returned by socket operations.
#[derive(Debug)]
#[allow(clippy::module_name_repetitions)]
pub enum SocketError {
    /// An underlying I/O failure.
    Io(std::io::Error),
    /// Another pid-ctl instance is already listening on this path.
    AlreadyRunning,
    /// The path exists but is not a Unix domain socket.
    NotASocket,
    /// JSON protocol error (parse failure, oversized request, etc.).
    Protocol(String),
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "socket I/O error: {source}"),
            Self::AlreadyRunning => {
                write!(f, "another pid-ctl instance is already listening")
            }
            Self::NotASocket => write!(f, "path exists but is not a socket"),
            Self::Protocol(msg) => write!(f, "socket protocol error: {msg}"),
        }
    }
}

impl Error for SocketError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::AlreadyRunning | Self::NotASocket | Self::Protocol(_) => None,
        }
    }
}

impl From<std::io::Error> for SocketError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

/// Probes an existing socket path to determine its liveness.
#[must_use]
pub fn probe_existing(path: &Path) -> ProbeResult {
    if !path.exists() {
        return ProbeResult::NoFile;
    }

    // Check metadata to see if it's a socket at all.
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_socket() {
                return ProbeResult::NotASocket;
            }
        }
        Err(_) => return ProbeResult::NoFile,
    }

    // Try to connect — if it succeeds, someone is listening.
    match UnixStream::connect(path) {
        Ok(_) => ProbeResult::AlreadyRunning,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => ProbeResult::Stale,
        Err(_) => ProbeResult::Stale,
    }
}

/// Non-blocking Unix domain socket listener for servicing operator commands.
///
/// Removes the socket file on drop.
#[allow(clippy::module_name_repetitions)]
pub struct SocketListener {
    listener: UnixListener,
    path: PathBuf,
}

impl SocketListener {
    /// Binds a new listener at `path` with the given file `mode` (e.g. `0o600`).
    ///
    /// # Errors
    ///
    /// Returns [`SocketError`] when the path is occupied by a live listener,
    /// is not a socket, or the bind/permissions call fails.
    pub fn bind(path: &Path, mode: u32) -> Result<Self, SocketError> {
        match probe_existing(path) {
            ProbeResult::NoFile => {}
            ProbeResult::Stale => {
                std::fs::remove_file(path)?;
            }
            ProbeResult::AlreadyRunning => return Err(SocketError::AlreadyRunning),
            ProbeResult::NotASocket => return Err(SocketError::NotASocket),
        }

        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;

        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// Attempts to accept and service a single pending connection.
    ///
    /// Returns `Ok(None)` when no connection is waiting (`WouldBlock`),
    /// `Ok(Some(()))` when a request was serviced, or `Err` on I/O failure.
    ///
    /// # Errors
    ///
    /// Returns [`SocketError`] on accept or stream I/O failures.
    pub fn try_service_one<F>(&self, handler: F) -> Result<Option<()>, SocketError>
    where
        F: FnOnce(Request) -> Response,
    {
        let (mut stream, _addr) = match self.listener.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(SocketError::Io(e)),
            Ok(pair) => pair,
        };

        stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT))?;

        let mut buf = Vec::new();
        let bytes_read = (&stream)
            .take(MAX_REQUEST_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(SocketError::Io)?;

        if bytes_read as u64 > MAX_REQUEST_BYTES {
            // `take` stops after MAX_REQUEST_BYTES+1 bytes; any further bytes the peer
            // sent remain in the socket buffer. On Linux, closing the stream with
            // unread inbound data can RST the connection, so the client sees
            // ConnectionReset instead of the error JSON — drain before writing.
            let mut drain = Vec::new();
            let _ = stream.read_to_end(&mut drain);
            let _ = write_response(
                &stream,
                &Response::Ack {
                    ok: false,
                    error: Some(String::from("request too large")),
                },
            );
            return Ok(Some(()));
        }

        let Ok(request) = serde_json::from_slice::<Request>(&buf) else {
            let _ = write_response(
                &stream,
                &Response::ErrorUnknownCommand {
                    ok: false,
                    error: String::from("failed to parse request"),
                    available: available_commands(),
                },
            );
            return Ok(Some(()));
        };

        let response = handler(request);
        let _ = write_response(&stream, &response);

        Ok(Some(()))
    }

    /// Removes the socket file from disk.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Sends a single request to a running controller and reads the response.
///
/// # Errors
///
/// Returns [`SocketError`] on connection, I/O, or protocol failures.
pub fn client_request(path: &Path, req: &Request) -> Result<Response, SocketError> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    let payload = serde_json::to_string(req).map_err(|e| SocketError::Protocol(e.to_string()))?;
    stream
        .write_all(payload.as_bytes())
        .map_err(SocketError::Io)?;
    stream.shutdown(Shutdown::Write)?;

    let mut response_buf = String::new();
    stream
        .read_to_string(&mut response_buf)
        .map_err(SocketError::Io)?;

    serde_json::from_str(&response_buf).map_err(|e| SocketError::Protocol(e.to_string()))
}

fn write_response(mut stream: &UnixStream, response: &Response) -> Result<(), SocketError> {
    let mut json =
        serde_json::to_string(response).map_err(|e| SocketError::Protocol(e.to_string()))?;
    json.push('\n');
    stream.write_all(json.as_bytes()).map_err(SocketError::Io)?;
    Ok(())
}

fn available_commands() -> Vec<String> {
    ["status", "set", "reset", "hold", "resume", "save"]
        .iter()
        .map(|s| String::from(*s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn test_request_deserialization() {
        let status: Request = serde_json::from_str(r#"{"cmd":"status"}"#).unwrap();
        assert_eq!(status, Request::Status);

        let set: Request =
            serde_json::from_str(r#"{"cmd":"set","param":"kp","value":1.5}"#).unwrap();
        assert_eq!(
            set,
            Request::Set {
                param: String::from("kp"),
                value: 1.5,
            }
        );

        let reset: Request = serde_json::from_str(r#"{"cmd":"reset"}"#).unwrap();
        assert_eq!(reset, Request::Reset);

        let hold: Request = serde_json::from_str(r#"{"cmd":"hold"}"#).unwrap();
        assert_eq!(hold, Request::Hold);

        let resume: Request = serde_json::from_str(r#"{"cmd":"resume"}"#).unwrap();
        assert_eq!(resume, Request::Resume);

        let save: Request = serde_json::from_str(r#"{"cmd":"save"}"#).unwrap();
        assert_eq!(save, Request::Save);
    }

    #[test]
    fn test_response_serialization() {
        let status = Response::Status {
            ok: true,
            iter: 42,
            pv: 65.0,
            sp: 70.0,
            err: -5.0,
            kp: 1.0,
            ki: 0.1,
            kd: 0.01,
            cv: 55.0,
            i_acc: 3.2,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains(r#""ok":true"#));
        assert!(json.contains(r#""iter":42"#));

        let set = Response::Set {
            ok: true,
            param: String::from("kp"),
            old: 1.0,
            new: 2.0,
        };
        let json = serde_json::to_string(&set).unwrap();
        assert!(json.contains(r#""param":"kp""#));
        assert!(json.contains(r#""old":1.0"#));

        let reset = Response::Reset {
            ok: true,
            i_acc_before: 5.5,
        };
        let json = serde_json::to_string(&reset).unwrap();
        assert!(json.contains(r#""i_acc_before":5.5"#));

        let ack = Response::Ack {
            ok: true,
            error: None,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert_eq!(json, r#"{"ok":true}"#);

        let err_cmd = Response::ErrorUnknownCommand {
            ok: false,
            error: String::from("unknown command"),
            available: vec![String::from("status"), String::from("set")],
        };
        let json = serde_json::to_string(&err_cmd).unwrap();
        assert!(json.contains(r#""available""#));

        let err_param = Response::ErrorUnknownParam {
            ok: false,
            error: String::from("unknown param"),
            settable: vec![String::from("kp"), String::from("ki")],
        };
        let json = serde_json::to_string(&err_param).unwrap();
        assert!(json.contains(r#""settable""#));
    }

    #[test]
    fn test_probe_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.sock");
        assert_eq!(probe_existing(&path), ProbeResult::NoFile);
    }

    #[test]
    fn test_probe_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.sock");

        // Bind a listener then drop it, leaving a stale socket file.
        {
            let _listener = UnixListener::bind(&path).unwrap();
        }

        assert!(path.exists());
        assert_eq!(probe_existing(&path), ProbeResult::Stale);
    }

    #[test]
    fn test_probe_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("active.sock");

        let listener = UnixListener::bind(&path).unwrap();
        // Keep listener alive while probing.
        assert_eq!(probe_existing(&path), ProbeResult::AlreadyRunning);
        drop(listener);
    }

    #[test]
    fn test_bind_and_service_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctl.sock");

        let listener = SocketListener::bind(&path, 0o600).unwrap();

        // Connect as a client.
        let mut client = UnixStream::connect(&path).unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();

        let req = serde_json::to_string(&Request::Status).unwrap();
        client.write_all(req.as_bytes()).unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        // Service the connection.
        let result = listener.try_service_one(|r| {
            assert_eq!(r, Request::Status);
            Response::Ack {
                ok: true,
                error: None,
            }
        });
        assert!(result.unwrap().is_some());

        // Read the response on the client side.
        let mut resp_buf = String::new();
        client.read_to_string(&mut resp_buf).unwrap();
        let resp: Response = serde_json::from_str(resp_buf.trim()).unwrap();
        assert_eq!(
            resp,
            Response::Ack {
                ok: true,
                error: None
            }
        );
    }

    #[test]
    fn test_client_request_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctl.sock");

        let listener = UnixListener::bind(&path).unwrap();

        let path_clone = path.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();

            let mut buf = String::new();
            let mut reader = std::io::BufReader::new(&stream);
            reader.read_to_string(&mut buf).unwrap();
            let _req: Request = serde_json::from_str(&buf).unwrap();

            let resp = Response::Ack {
                ok: true,
                error: None,
            };
            let mut json = serde_json::to_string(&resp).unwrap();
            json.push('\n');
            (&stream).write_all(json.as_bytes()).unwrap();
        });

        let resp = client_request(&path_clone, &Request::Status).unwrap();
        assert_eq!(
            resp,
            Response::Ack {
                ok: true,
                error: None
            }
        );

        server.join().unwrap();
    }

    #[test]
    fn test_drop_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drop.sock");

        {
            let _listener = SocketListener::bind(&path, 0o600).unwrap();
            assert!(path.exists());
        }
        // After drop, file should be gone.
        assert!(!path.exists());
    }

    #[test]
    fn test_oversized_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.sock");

        let listener = SocketListener::bind(&path, 0o600).unwrap();

        let mut client = UnixStream::connect(&path).unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();

        // Write more than 4096 bytes.
        let oversized = "x".repeat(5000);
        client.write_all(oversized.as_bytes()).unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        // Service should succeed but return error response.
        let result = listener.try_service_one(|_| {
            panic!("handler should not be called for oversized request");
        });
        assert!(result.unwrap().is_some());

        // Client should get an error response.
        let mut resp_buf = String::new();
        client.read_to_string(&mut resp_buf).unwrap();
        let resp: serde_json::Value = serde_json::from_str(resp_buf.trim()).unwrap();
        assert_eq!(resp["ok"], false);
    }
}
