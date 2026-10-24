//! Embedded Tor (Arti) for platforms with no Tor daemon to point at (iOS).
//!
//! `proxy_url = "tor"` starts it on first use: Arti runs on its own runtime and
//! serves a loopback SOCKS5 port that the ordinary ureq socks path dials, so
//! timeouts and errors behave exactly as with an external proxy. The port
//! demands a random per-launch password, so other local apps can't ride our
//! circuits.

use crate::Error;
use arti_client::config::TorClientConfigBuilder;
use arti_client::status::BootstrapStatus;
use arti_client::TorClient;
use std::io;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Budget for the SOCKS greeting/auth/request bytes (not the Tor connect).
const HANDSHAKE: Duration = Duration::from_secs(10);

struct Bridge {
    url: String,
    status: Box<dyn Fn() -> BootstrapStatus + Send + Sync>,
    _rt: tokio::runtime::Runtime,
}

static BRIDGE: OnceLock<Result<Bridge, String>> = OnceLock::new();

/// `socks5://` URL of the bridge, starting Tor on the first call.
pub fn socks_url() -> Result<String, Error> {
    match BRIDGE.get_or_init(start) {
        Ok(b) => Ok(b.url.clone()),
        Err(e) => Err(Error::Transport(format!("Tor failed to start: {e}"))),
    }
}

/// (ready, human-readable progress); None until something asked for Tor.
pub fn status() -> Option<(bool, String)> {
    Some(match BRIDGE.get()? {
        Ok(b) => {
            let s = (b.status)();
            if s.ready_for_traffic() {
                (true, "Tor connected".into())
            } else if s.blocked().is_some() {
                (false, format!("Tor {s}"))
            } else {
                (false, format!("Connecting to Tor… {}%", (s.as_frac() * 100.0) as u32))
            }
        }
        Err(e) => (false, format!("Tor failed to start: {e}")),
    })
}

fn start() -> Result<Bridge, String> {
    let dir = format!("{}/arti", crate::data_dir().ok_or("HOME not set")?);
    let mut cfg = TorClientConfigBuilder::from_directories(format!("{dir}/state"), format!("{dir}/cache"));
    if cfg!(target_os = "ios") {
        // The app sandbox is the isolation boundary there; Arti's Unix
        // ownership checks on container directories add nothing.
        cfg.storage().permissions().dangerously_trust_everyone();
    }
    let cfg = cfg.build().map_err(|e| e.to_string())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let _ctx = rt.enter();
    let tor = TorClient::builder().config(cfg).create_unbootstrapped().map_err(|e| e.to_string())?;
    let listener = rt.block_on(TcpListener::bind("127.0.0.1:0")).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let pass = format!("{:032x}", rand::random::<u128>());

    // Bootstrap now so it progresses before the first request; failures show
    // in status(), and connect() retries on demand.
    let t = tor.clone();
    rt.spawn(async move { let _ = t.bootstrap().await; });
    let (t, p) = (tor.clone(), pass.clone());
    rt.spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut s, _)) => {
                    let (t, p) = (t.clone(), p.clone());
                    tokio::spawn(async move {
                        let Ok(Ok(Some((host, port)))) = tokio::time::timeout(HANDSHAKE, handshake(&mut s, &p)).await else {
                            return;
                        };
                        match t.connect((host.as_str(), port)).await {
                            Ok(mut tor_stream) => {
                                if reply(&mut s, 0).await.is_ok() {
                                    let _ = tokio::io::copy_bidirectional(&mut s, &mut tor_stream).await;
                                }
                            }
                            Err(_) => { let _ = reply(&mut s, 4).await; } // host unreachable
                        }
                    });
                }
                // e.g. out of fds: back off instead of spinning.
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    });

    Ok(Bridge {
        // Username unused; ureq needs one to send the password.
        url: format!("socks5://sanctum:{pass}@127.0.0.1:{port}"),
        status: Box::new(move || tor.bootstrap_status()),
        _rt: rt,
    })
}

/// SOCKS5 (RFC 1928) after username/password auth (RFC 1929): the CONNECT
/// target, or None if the client was refused.
async fn handshake(s: &mut TcpStream, pass: &str) -> io::Result<Option<(String, u16)>> {
    // Greeting: VER NMETHODS METHODS. Only username/password (2) is offered.
    let [ver, n] = read::<2>(s).await?;
    let methods = read_vec(s, n).await?;
    if ver != 5 || !methods.contains(&2) {
        s.write_all(&[5, 0xff]).await?;
        return Ok(None);
    }
    s.write_all(&[5, 2]).await?;

    // VER ULEN UNAME PLEN PASSWD; the username is ignored.
    let [_, ulen] = read::<2>(s).await?;
    read_vec(s, ulen).await?;
    let [plen] = read::<1>(s).await?;
    let got = read_vec(s, plen).await?;
    let ok = got.len() == pass.len() && got.iter().zip(pass.bytes()).fold(0, |acc, (a, b)| acc | (a ^ b)) == 0;
    s.write_all(&[1, !ok as u8]).await?;
    if !ok {
        return Ok(None);
    }

    // VER CMD RSV ATYP. CONNECT to a domain only: ureq passes hostnames
    // through, so DNS happens at the exit, never locally.
    let [ver, cmd, _, atyp] = read::<4>(s).await?;
    if ver != 5 || cmd != 1 || atyp != 3 {
        reply(s, if cmd != 1 { 7 } else { 8 }).await?;
        return Ok(None);
    }
    let [len] = read::<1>(s).await?;
    let host = String::from_utf8(read_vec(s, len).await?).map_err(|_| io::ErrorKind::InvalidData)?;
    let port = u16::from_be_bytes(read::<2>(s).await?);
    Ok(Some((host, port)))
}

async fn reply(s: &mut TcpStream, code: u8) -> io::Result<()> {
    s.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

async fn read<const N: usize>(s: &mut TcpStream) -> io::Result<[u8; N]> {
    let mut b = [0; N];
    s.read_exact(&mut b).await?;
    Ok(b)
}

async fn read_vec(s: &mut TcpStream, n: u8) -> io::Result<Vec<u8>> {
    let mut b = vec![0; n as usize];
    s.read_exact(&mut b).await?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_checks_password_and_parses_target() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            for (pass, want) in [("secret", Some(("s3.hippius.com".to_string(), 443))), ("wrong!", None)] {
                let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = l.local_addr().unwrap();
                let client = tokio::spawn(async move {
                    let mut c = TcpStream::connect(addr).await.unwrap();
                    let mut m = vec![5, 2, 0, 2, 1, 1, b'u', pass.len() as u8];
                    m.extend(pass.as_bytes());
                    m.extend([5, 1, 0, 3, 14]);
                    m.extend(b"s3.hippius.com");
                    m.extend(443u16.to_be_bytes());
                    c.write_all(&m).await.unwrap();
                    c
                });
                let (mut s, _) = l.accept().await.unwrap();
                assert_eq!(handshake(&mut s, "secret").await.unwrap(), want);
                drop(client.await);
            }
        });
    }
}
