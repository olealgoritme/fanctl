//! The service's control socket. `fanctl set` / `auto` and the live view send it one line,
//! `set <control> <pct>` or `auto <control>`, and get `ok` or `error ...` back, so a fan can be
//! changed while the curves run. The socket is root:sudo (or wheel) 0660: members of the admin
//! group may change fans without sudo.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

pub const SOCK: &str = "/run/fanctl.sock";

/// gid of the first of these groups that exists.
fn admin_gid() -> Option<u32> {
    let g = std::fs::read_to_string("/etc/group").ok()?;
    ["sudo", "wheel", "admin"].iter().find_map(|want| {
        g.lines().find_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let gid = f.nth(1)?.parse().ok()?;
            (name == *want).then_some(gid)
        })
    })
}

pub fn listen() -> Option<UnixListener> {
    let _ = std::fs::remove_file(SOCK);
    let l = UnixListener::bind(SOCK).map_err(|e| eprintln!("fanctl: {SOCK}: {e}")).ok()?;
    l.set_nonblocking(true).ok()?;
    let path = std::ffi::CString::new(SOCK).ok()?;
    let gid = admin_gid().unwrap_or(0);
    unsafe {
        libc::chown(path.as_ptr(), 0, gid);
        libc::chmod(path.as_ptr(), if gid == 0 { 0o600 } else { 0o660 });
    }
    Some(l)
}

/// Every request waiting on the socket: `handle(line)` gives the reply.
pub fn serve(l: &UnixListener, mut handle: impl FnMut(&str) -> String) {
    while let Ok((s, _)) = l.accept() {
        let _ = s.set_nonblocking(false);
        let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
        let mut line = String::new();
        let mut r = BufReader::new(&s);
        if r.read_line(&mut line).is_ok() && !line.trim().is_empty() {
            let reply = handle(line.trim());
            let _ = (&s).write_all(format!("{reply}\n").as_bytes());
        }
    }
}

/// Whether this process may talk to the service.
pub fn reachable() -> bool {
    let Ok(p) = std::ffi::CString::new(SOCK) else { return false };
    unsafe { libc::access(p.as_ptr(), libc::W_OK) == 0 }
}

pub fn send(cmd: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(SOCK).map_err(|e| format!("{SOCK}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.write_all(format!("{cmd}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply).map_err(|e| e.to_string())?;
    let reply = reply.trim().to_string();
    match reply.strip_prefix("error ") {
        Some(e) => Err(e.to_string()),
        None => Ok(reply),
    }
}
