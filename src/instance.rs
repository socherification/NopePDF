//! Single-instance support: the first process holds an OS file lock and listens
//! on a localhost socket; later launches forward their file paths to it and exit.
//! The port and a random token are stored next to the lock in the user's config
//! directory (owner-only permissions on Unix), so only processes of the same user
//! can talk to the listener.

use std::collections::hash_map::RandomState;
use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Upper bound for one forwarded message (token + file paths).
const MAX_MESSAGE: usize = 64 * 1024;
/// Time a client gets to deliver its whole message.
const MESSAGE_DEADLINE: Duration = Duration::from_secs(2);

pub struct Primary {
    _lock: File,
    listener: TcpListener,
    token: String,
}

pub enum Startup {
    /// We are the main instance (or single-instance setup failed and we run standalone).
    Primary(Option<Primary>),
    /// Files were handed to the running instance; this process should exit.
    Forwarded,
}

fn dir() -> Option<PathBuf> {
    let d = dirs::config_dir()?.join("NopePDF");
    std::fs::create_dir_all(&d).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    Some(d)
}

/// Writes a file only the current user can read (the token must stay private).
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(contents.as_bytes())
}

fn random_token() -> String {
    let a = RandomState::new().build_hasher().finish();
    let mut h = RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    format!("{a:016x}{:016x}", h.finish())
}

pub fn start(files: &[PathBuf]) -> Startup {
    let Some(dir) = dir() else { return Startup::Primary(None) };
    let Ok(lock) = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("instance.lock")) else {
        return Startup::Primary(None);
    };
    match lock.try_lock() {
        Ok(()) => Startup::Primary(become_primary(lock, &dir)),
        Err(std::fs::TryLockError::WouldBlock) => {
            if forward(&dir, files) {
                Startup::Forwarded
            } else {
                Startup::Primary(None)
            }
        }
        Err(_) => Startup::Primary(None),
    }
}

fn become_primary(lock: File, dir: &std::path::Path) -> Option<Primary> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = random_token();
    let tmp = dir.join("instance.port.tmp");
    write_private(&tmp, &format!("{port} {token}")).ok()?;
    std::fs::rename(&tmp, dir.join("instance.port")).ok()?;
    Some(Primary { _lock: lock, listener, token })
}

/// Send paths to the running instance, retrying while it starts up.
fn forward(dir: &std::path::Path, files: &[PathBuf]) -> bool {
    #[cfg(windows)]
    unsafe {
        // Let the running instance bring its window to the foreground.
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    for _ in 0..40 {
        if let Some((port, token)) = std::fs::read_to_string(dir.join("instance.port"))
            .ok()
            .and_then(|s| s.split_once(' ').map(|(p, t)| (p.trim().parse::<u16>().ok(), t.trim().to_string())))
            .and_then(|(p, t)| p.map(|p| (p, t)))
        {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
                let mut msg = format!("{token}\n");
                for f in files {
                    msg.push_str(&f.to_string_lossy());
                    msg.push('\n');
                }
                if s.write_all(msg.as_bytes()).is_ok() {
                    let _ = s.shutdown(std::net::Shutdown::Write);
                    // Wait for the acknowledgement so we know the token matched.
                    let mut ack = String::new();
                    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                    if BufReader::new(&s).read_line(&mut ack).is_ok() && ack.trim() == "ok" {
                        return true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Reads one message with a size limit and a deadline for the whole message, so a
/// misbehaving local client cannot exhaust memory or block the listener.
fn read_message(mut stream: &TcpStream) -> Option<String> {
    let deadline = Instant::now() + MESSAGE_DEADLINE;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) if buf.len() + n <= MAX_MESSAGE => buf.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
    String::from_utf8(buf).ok()
}

/// Accept forwarded launches; each message is the list of files to open
/// (possibly empty: just bring the window to the front).
pub fn serve(p: Primary, ctx: eframe::egui::Context) -> mpsc::Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            let Primary { _lock, listener, token } = p;
            for stream in listener.incoming().flatten() {
                let Some(msg) = read_message(&stream) else { continue };
                let mut lines = msg.lines();
                if lines.next() != Some(token.as_str()) {
                    continue;
                }
                let files: Vec<PathBuf> = lines.filter(|l| !l.is_empty()).map(PathBuf::from).collect();
                // Acknowledge only once the window has the files, so a launch during
                // shutdown falls back to opening its own window.
                if tx.send(files).is_err() {
                    break;
                }
                let _ = (&stream).write_all(
                    b"ok
",
                );
                ctx.request_repaint();
            }
            drop(_lock);
        })
        .expect("spawn instance listener");
    rx
}
