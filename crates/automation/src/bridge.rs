//! Client for the desktop app's JSON-lines control protocol
//! (`docs/control-protocol.md`): one JSON request per line, replies matched
//! by `id`. Keeps one connection and reconnects on failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::AutomationError;
use crate::budgets::MAX_RESPONSE_BYTES;
use crate::security::{AUTH_METHOD, MAX_REQUEST_BYTES, validate_token};

type Conn = (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf);

pub struct BridgeClient {
    addr: String,
    token: String,
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl BridgeClient {
    /// `addr` such as `127.0.0.1:7878`. Only loopback addresses are accepted,
    /// matching the server, which binds to loopback only.
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Result<Self, AutomationError> {
        let addr = addr.into();
        let token = token.into();
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(AutomationError::BadRequest(format!("bridge address must be loopback, got `{addr}`")));
        }
        validate_token(&token)?;
        Ok(BridgeClient { addr, token: token.to_ascii_lowercase(), conn: Mutex::new(None), next_id: AtomicU64::new(1), timeout: Duration::from_secs(60) })
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Call a control method; returns its `result` or the app's error.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, AutomationError> {
        let mut guard = self.conn.lock().await;
        // One retry with a fresh connection (the app may have restarted).
        for attempt in 0..2 {
            if guard.is_none() {
                let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
                    .await
                    .map_err(|_| AutomationError::Bridge(format!("timed out connecting to {}", self.addr)))?
                    .map_err(|e| {
                        AutomationError::Bridge(format!(
                            "cannot connect to {} ({e}); start the app with `photocraft --control <port>` and matching control credentials",
                            self.addr
                        ))
                    })?;
                let (r, w) = s.into_split();
                let mut conn = (BufReader::new(r), w);
                let auth_id = self.next_id.fetch_add(1, Ordering::Relaxed);
                let auth = tokio::time::timeout(Duration::from_secs(5), exchange(&mut conn, auth_id, AUTH_METHOD, &json!({"token": self.token})))
                    .await
                    .map_err(|_| AutomationError::Bridge("control authentication timed out".into()))??;
                auth?;
                *guard = Some(conn);
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let Some(conn) = guard.as_mut() else {
                return Err(AutomationError::Bridge(format!("not connected to {}", self.addr)));
            };
            match tokio::time::timeout(self.timeout, exchange(conn, id, method, &params)).await {
                Ok(Ok(Err(error @ AutomationError::BadRequest(_)))) => {
                    // An oversized frame leaves unread bytes. Drop this connection and
                    // report the budget failure without retrying a possibly completed edit.
                    *guard = None;
                    return Err(error);
                }
                Ok(Ok(v)) => return v,
                Ok(Err(e)) if attempt == 0 => {
                    *guard = None;
                    let _ = e;
                }
                Ok(Err(e)) => {
                    *guard = None;
                    return Err(e);
                }
                Err(_) => {
                    *guard = None;
                    return Err(AutomationError::Bridge(format!("`{method}` timed out after {:?}", self.timeout)));
                }
            }
        }
        Err(AutomationError::Bridge("unreachable".into()))
    }
}

/// Outer `Err` = transport failure (retryable); inner = app-level result.
async fn exchange(conn: &mut Conn, id: u64, method: &str, params: &Value) -> Result<Result<Value, AutomationError>, AutomationError> {
    let mut line = serde_json::to_string(&json!({"id": id, "method": method, "params": params})).map_err(|e| AutomationError::Other(e.to_string()))?;
    line.push('\n');
    if line.len() > MAX_REQUEST_BYTES {
        return Ok(Err(AutomationError::BadRequest(format!("request is {} bytes; maximum is {MAX_REQUEST_BYTES}", line.len()))));
    }
    let io = |e: std::io::Error| AutomationError::Bridge(e.to_string());
    conn.1.write_all(line.as_bytes()).await.map_err(io)?;
    conn.1.flush().await.map_err(io)?;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Read raw bytes so truncation inside a UTF-8 character still reports the
        // budget error instead of a retryable decoding/transport error.
        let n = (&mut conn.0).take((MAX_RESPONSE_BYTES + 1) as u64).read_until(b'\n', &mut buf).await.map_err(io)?;
        if n > MAX_RESPONSE_BYTES {
            return Ok(Err(AutomationError::BadRequest(format!("bridge response exceeds {MAX_RESPONSE_BYTES} bytes; operation may have completed"))));
        }
        if n == 0 {
            return Err(AutomationError::Bridge("connection closed by the app".into()));
        }
        let Ok(v) = serde_json::from_slice::<Value>(&buf) else {
            continue;
        };
        if v.get("id").and_then(Value::as_u64) != Some(id) {
            continue; // stale reply from an earlier, timed-out request
        }
        return Ok(if v.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(AutomationError::App(v.get("error").and_then(Value::as_str).unwrap_or("unknown error").to_owned()))
        });
    }
}
