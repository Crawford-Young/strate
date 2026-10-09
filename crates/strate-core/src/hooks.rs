//! The optional receiver for Claude Code `http` hooks. Off unless
//! [`HookReceiver::start`] runs; strate never edits the user's
//! settings.json (the README has the hook snippet).
//!
//! It binds 127.0.0.1 only and serves one connection at a time on a worker
//! thread: POST only, a capped body, an optional bearer token compared in
//! constant time. A hook is synchronous for Claude Code, so a good request
//! is answered `204` with an empty body (no decision) before anything else
//! happens; the parsed event then goes into a bounded channel, and a full
//! channel drops it rather than wait. SubagentStart, SubagentStop,
//! PermissionRequest and TeammateIdle become [`HookEvent`]s; any other
//! event is answered `204` and ignored. A malformed request gets `400`.
//!
//! The HTTP is hand-rolled on `std::net`: one request per connection from a
//! local client needs no more, and it keeps a server dependency out.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::panic::resume_unwind;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Sender, TrySendError, bounded};
use serde::Deserialize;

pub use crossbeam_channel::Receiver;

/// One accepted hook event and when it arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    /// Unix ms at receipt.
    pub received_ms: i64,
    pub event: HookEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    SubagentStart {
        session_id: String,
        agent_id: String,
        agent_type: Option<String>,
    },
    SubagentStop {
        session_id: String,
        agent_id: String,
        agent_type: Option<String>,
    },
    /// `agent_id` is set when a subagent asks.
    PermissionRequest {
        session_id: String,
        agent_id: Option<String>,
        tool_name: Option<String>,
    },
    TeammateIdle {
        session_id: String,
        agent_id: Option<String>,
        teammate_name: Option<String>,
    },
}

/// The payload as posted; unknown fields are ignored.
#[derive(Deserialize)]
#[serde(tag = "hook_event_name")]
enum Wire {
    SubagentStart {
        session_id: String,
        agent_id: String,
        agent_type: Option<String>,
    },
    SubagentStop {
        session_id: String,
        agent_id: String,
        agent_type: Option<String>,
    },
    PermissionRequest {
        session_id: String,
        agent_id: Option<String>,
        tool_name: Option<String>,
    },
    TeammateIdle {
        session_id: String,
        agent_id: Option<String>,
        teammate_name: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// Parses a hook body: `Ok(None)` for an event strate does not use.
fn parse(body: &[u8]) -> serde_json::Result<Option<HookEvent>> {
    Ok(match serde_json::from_slice(body)? {
        Wire::SubagentStart {
            session_id,
            agent_id,
            agent_type,
        } => Some(HookEvent::SubagentStart {
            session_id,
            agent_id,
            agent_type,
        }),
        Wire::SubagentStop {
            session_id,
            agent_id,
            agent_type,
        } => Some(HookEvent::SubagentStop {
            session_id,
            agent_id,
            agent_type,
        }),
        Wire::PermissionRequest {
            session_id,
            agent_id,
            tool_name,
        } => Some(HookEvent::PermissionRequest {
            session_id,
            agent_id,
            tool_name,
        }),
        Wire::TeammateIdle {
            session_id,
            agent_id,
            teammate_name,
        } => Some(HookEvent::TeammateIdle {
            session_id,
            agent_id,
            teammate_name,
        }),
        Wire::Other => None,
    })
}

#[derive(Debug, Clone)]
pub struct HookOptions {
    /// Port on 127.0.0.1; 0 lets the OS pick (see
    /// [`HookReceiver::local_addr`]).
    pub port: u16,
    /// When set, every request needs `Authorization: Bearer <token>`.
    pub token: Option<String>,
    /// Largest body accepted; a bigger one gets `413`.
    pub max_body: usize,
    /// Events buffered before new ones are dropped.
    pub channel_events: usize,
    /// How long a client may take to send its request.
    pub read_timeout: Duration,
}

impl Default for HookOptions {
    fn default() -> Self {
        Self {
            port: 0,
            token: None,
            max_body: 1024 * 1024,
            channel_events: 256,
            read_timeout: Duration::from_secs(2),
        }
    }
}

/// A running receiver. Dropping it stops the worker, as
/// [`HookReceiver::stop`] does.
pub struct HookReceiver {
    addr: SocketAddr,
    stopping: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl HookReceiver {
    /// Binds 127.0.0.1 and serves on a worker thread. Fails when the port
    /// cannot be bound.
    pub fn start(options: HookOptions) -> io::Result<(Self, Receiver<Hook>)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port))?;
        let addr = listener.local_addr()?;
        let (out, hooks) = bounded(options.channel_events);
        let stopping = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let server = Server {
            options,
            out,
            dropped: dropped.clone(),
        };
        let flag = stopping.clone();
        let worker = thread::Builder::new()
            .name("strate-hooks".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Ok(stream) = stream {
                        server.serve(stream);
                    }
                }
            })?;
        let receiver = Self {
            addr,
            stopping,
            dropped,
            worker: Some(worker),
        };
        Ok((receiver, hooks))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Events dropped because the channel was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops serving and closes the port; events already queued stay
    /// receivable.
    pub fn stop(mut self) {
        if let Some(worker) = self.shutdown() {
            worker.join().unwrap_or_else(|panic| resume_unwind(panic));
        }
    }

    /// Ends the worker's accept loop once; `None` when already stopped (a
    /// wake connect to a closed port takes seconds on Windows).
    fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        let worker = self.worker.take()?;
        self.stopping.store(true, Ordering::SeqCst);
        // Wakes the blocking accept so the worker sees the flag.
        let _ = TcpStream::connect(self.addr);
        Some(worker)
    }
}

impl Drop for HookReceiver {
    fn drop(&mut self) {
        if let Some(worker) = self.shutdown() {
            let _panicked = worker.join();
        }
    }
}

struct Server {
    options: HookOptions,
    out: Sender<Hook>,
    dropped: Arc<AtomicU64>,
}

/// Longest request line plus headers accepted.
const MAX_HEAD: u64 = 16 * 1024;
/// Input read and discarded after answering, and for how long at most.
const MAX_DRAIN: u64 = 1024 * 1024;
const DRAIN_FOR: Duration = Duration::from_millis(100);

impl Server {
    fn serve(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(self.options.read_timeout));
        let _ = stream.set_write_timeout(Some(self.options.read_timeout));
        let mut writer = match stream.try_clone() {
            Ok(writer) => writer,
            Err(_) => return,
        };
        let (status, event) = match self.read(stream) {
            Ok(event) => (204, event),
            Err(status) => (status, None),
        };
        let reason = match status {
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            405 => "Method Not Allowed",
            411 => "Length Required",
            _ => "Content Too Large",
        };
        let _ = write!(
            writer,
            "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = writer.flush();
        if let Some(event) = event {
            let hook = Hook {
                received_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64),
                event,
            };
            if let Err(TrySendError::Full(_)) = self.out.try_send(hook) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Closing with unread input (a refused body) would reset the
        // connection and could cost the client the answer: end the answer,
        // then drain briefly before closing.
        let _ = writer.shutdown(Shutdown::Write);
        let _ = writer.set_read_timeout(Some(DRAIN_FOR));
        let _ = io::copy(&mut writer.take(MAX_DRAIN), &mut io::sink());
    }

    /// Reads and checks one request; `Err` is the status to answer.
    fn read(&self, stream: TcpStream) -> Result<Option<HookEvent>, u16> {
        let mut head = BufReader::new(stream.take(MAX_HEAD));
        let mut line = String::new();
        if head.read_line(&mut line).map_err(|_| 400u16)? == 0 {
            return Err(400);
        }
        let request = line.clone();
        let mut parts = request.split_whitespace();
        let (method, _target, version) = (parts.next(), parts.next(), parts.next());
        if !version.is_some_and(|v| v.starts_with("HTTP/")) {
            return Err(400);
        }
        let mut length = None;
        let mut authorization = None;
        loop {
            line.clear();
            if head.read_line(&mut line).map_err(|_| 400u16)? == 0 {
                return Err(400);
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').ok_or(400u16)?;
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.parse::<usize>().map_err(|_| 400u16)?);
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                return Err(411);
            }
        }
        if method != Some("POST") {
            return Err(405);
        }
        if let Some(token) = &self.options.token {
            let given = authorization
                .as_deref()
                .and_then(|a| a.strip_prefix("Bearer "))
                .unwrap_or_default();
            if !constant_time_eq(given.as_bytes(), token.as_bytes()) {
                return Err(401);
            }
        }
        let length = length.ok_or(411u16)?;
        if length > self.options.max_body {
            return Err(413);
        }
        let mut body = vec![0; length];
        // What the head reader buffered past the headers comes first.
        let buffered = head.buffer().len().min(length);
        body[..buffered].copy_from_slice(&head.buffer()[..buffered]);
        head.consume(buffered);
        head.into_inner()
            .into_inner()
            .read_exact(&mut body[buffered..])
            .map_err(|_| 400u16)?;
        parse(&body).map_err(|_| 400)
    }
}

/// Equal-length inputs take the same time whatever their contents. Only the
/// length can leak, which says nothing useful about a random token.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_only_when_identical() {
        assert!(constant_time_eq(b"lorem", b"lorem"));
        assert!(!constant_time_eq(b"lorem", b"lorex"));
        assert!(!constant_time_eq(b"lorem", b"lore"));
        assert!(constant_time_eq(b"", b""));
    }
}
