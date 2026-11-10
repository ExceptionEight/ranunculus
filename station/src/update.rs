//! The client replaced by the server's own: a server of another version sends it instead of a welcome (the
//! protocol's `update`), because the two only talk when they are of one version. The file is checked by the SHA-256
//! the server named over the same connection (TLS, the server's certificate pinned), tried — it must run here and
//! say the version promised — and only then put in this program's place. The supervisor starts it.
use futures_util::StreamExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// more than any client will ever be: a wrong `size` must not fill the memory
const SIZE_MAX: u64 = 64 << 20;
/// A client that was offered and did not pass: not downloaded again every few seconds.
static REJECTED: Mutex<Option<String>> = Mutex::new(None);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `file` as a program at `path`, if it runs and says it is the client of `version`.
fn tried(file: &[u8], path: &Path, version: &str) -> Result<(), String> {
    let e = |e: std::io::Error| format!("{}: {e}", path.display());
    std::fs::write(path, file).map_err(e)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(e)?;
    let out = std::process::Command::new(path).arg("version").output().map_err(e)?;
    let said = String::from_utf8_lossy(&out.stdout);
    if said.starts_with(&format!("ranunculus-station {version} ")) {
        Ok(())
    } else {
        Err(format!("it does not say it is version {version}: «{}»", said.trim()))
    }
}

/// `file` (checked already) in the place of `me`: tried next to it, then moved over it in one step.
fn put(file: &[u8], me: &Path, version: &str) -> Result<(), String> {
    let new = me.with_extension("new");
    let done = tried(file, &new, version)
        .and_then(|()| std::fs::rename(&new, me).map_err(|e| format!("{}: {e}", me.display())));
    if done.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    done
}

/// Takes the client the server sends after its `update` (`size` bytes in binary messages). → Ok: it is in this
/// program's place, and this process should end.
pub async fn take<S>(rx: &mut S, version: &str, size: u64, sha256: &str) -> Result<(), String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    if REJECTED.lock().unwrap().as_deref() == Some(sha256) {
        return Err("the same file as before".into());
    }
    if size == 0 || size > SIZE_MAX {
        return Err(format!("{size} bytes is no client"));
    }
    let mut file = Vec::with_capacity(size as usize);
    while (file.len() as u64) < size {
        match tokio::time::timeout(Duration::from_secs(15), rx.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => file.extend_from_slice(&b),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Ok(Some(Ok(other))) => return Err(format!("expected the file, got {other:?}")),
            Ok(Some(Err(e))) => return Err(e.to_string()),
            Ok(None) => return Err(format!("the link closed at {} of {size} bytes", file.len())),
            Err(_) => return Err(format!("no data for 15 s at {} of {size} bytes", file.len())),
        }
    }
    let got = hex(ring::digest::digest(&ring::digest::SHA256, &file).as_ref());
    let done = if file.len() as u64 != size || got != sha256.to_lowercase() {
        Err("the file does not match its SHA-256".to_string())
    } else {
        std::env::current_exe().map_err(|e| e.to_string()).and_then(|me| put(&file, &me, version))
    };
    if done.is_err() {
        *REJECTED.lock().unwrap() = Some(sha256.to_string());
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in client: a script that says its version as the real one does.
    fn client(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho \"ranunculus-station {version} (proto 4)\"\n").into_bytes()
    }

    #[test]
    fn a_new_client_takes_the_place_only_if_it_runs_and_is_the_one_promised() {
        let dir = std::env::temp_dir().join(format!("station-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let me = dir.join("ranunculus-station");
        std::fs::write(&me, client("1.0.0")).unwrap();
        // another version than promised, and something that does not run at all: the old client stays
        assert!(put(&client("1.5.0"), &me, "2.0.0").unwrap_err().contains("does not say"));
        assert!(put(b"not a program", &me, "2.0.0").is_err());
        assert_eq!(
            (std::fs::read(&me).unwrap(), dir.join("ranunculus-station.new").exists()),
            (client("1.0.0"), false)
        );
        // the one promised: in its place, runnable
        put(&client("2.0.0"), &me, "2.0.0").unwrap();
        assert_eq!(std::fs::read(&me).unwrap(), client("2.0.0"));
        assert_eq!(std::fs::metadata(&me).unwrap().permissions().mode() & 0o777, 0o755);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_file_is_checked_before_anything_is_tried() {
        let file = client("2.0.0");
        let sha = hex(ring::digest::digest(&ring::digest::SHA256, &file).as_ref());
        let messages = |f: &[u8]| {
            futures_util::stream::iter(f.chunks(7).map(|c| Ok(Message::Binary(c.to_vec().into()))).collect::<Vec<_>>())
        };
        // cut short, and another file under the same name: refused
        let e = take(&mut messages(&file[..10]), "2.0.0", file.len() as u64, &sha).await.unwrap_err();
        assert!(e.contains("closed at 10"), "{e}");
        let other = client("6.6.6");
        let wrong = "00".repeat(32);
        assert!(take(&mut messages(&other), "2.0.0", other.len() as u64, &wrong)
            .await
            .unwrap_err()
            .contains("SHA-256"));
        // offered again: not even read
        assert!(take(&mut messages(&other), "2.0.0", other.len() as u64, &wrong)
            .await
            .unwrap_err()
            .contains("the same file"));
        assert!(take(&mut messages(&file), "2.0.0", 0, &sha).await.is_err());
    }
}
