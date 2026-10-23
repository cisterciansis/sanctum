//! sanctum CLI: keygen, invite, send, recv — plus `demo` (live E2E round-trip).
//! Plain std::env::args; a real arg parser arrives with real surface area.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use sanctum_core::{invite::Invite, open, seal, transport::Mailbox, Identity};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    let result = match cmd {
        "keygen" => keygen(),
        "send" => send(&args),
        "recv" => recv(&args),
        "demo" => demo(),
        _ => {
            eprintln!(
                "usage: sanctum <command>\n\
                 \x20 keygen                          print a new identity (secret b64)\n\
                 \x20 send <invite-link> <sender-secret-b64> <text>\n\
                 \x20 recv <recipient-secret-b64> <bucket> <access-key> <secret>\n\
                 \x20 demo                            live round-trip (env: see experiment/hippius.env)"
            );
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn keygen() -> Result<(), String> {
    let id = Identity::generate();
    println!("secret: {}", id.to_b64());
    println!("public: {}", B64.encode(id.public().as_bytes()));
    Ok(())
}

fn send(args: &[String]) -> Result<(), String> {
    let [link, sk, text] = &args[2..] else {
        return Err("usage: sanctum send <invite-link> <sender-secret-b64> <text>".into());
    };
    let invite = Invite::from_link(link).map_err(|e| e.to_string())?;
    let sender = Identity::from_b64(sk).map_err(|e| e.to_string())?;
    let pk_bytes: [u8; 32] = B64
        .decode(&invite.pk)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "bad recipient key in invite")?;
    let sealed = seal(&sender, &pk_bytes.into(), text.as_bytes()).map_err(|e| e.to_string())?;
    let mbox = Mailbox::new(&invite.e, &invite.b, &invite.k, &invite.s).map_err(|e| e.to_string())?;
    let key = format!("m/{}", ulid::Ulid::new());
    mbox.put(&key, &sealed).map_err(|e| e.to_string())?;
    println!("sent {key}");
    Ok(())
}

fn recv(args: &[String]) -> Result<(), String> {
    let [sk, bucket, ak, secret] = &args[2..] else {
        return Err("usage: sanctum recv <recipient-secret-b64> <bucket> <access-key> <secret>".into());
    };
    let me = Identity::from_b64(sk).map_err(|e| e.to_string())?;
    let mbox = Mailbox::new(sanctum_core::transport::DEFAULT_ENDPOINT, bucket, ak, secret)
        .map_err(|e| e.to_string())?;
    for key in mbox.list("m/").map_err(|e| e.to_string())? {
        let raw = mbox.get(&key).map_err(|e| e.to_string())?;
        match open(&me, &raw) {
            Ok((from, pt)) => {
                println!("[{key}] from {}: {}", &B64.encode(from.as_bytes())[..8], String::from_utf8_lossy(&pt));
                mbox.delete(&key).map_err(|e| e.to_string())?;
            }
            Err(e) => eprintln!("[{key}] undecryptable ({e}) — left in place"),
        }
    }
    Ok(())
}

/// Live round-trip over Hippius using the experiment credentials:
/// Bob (sub-token only, no account) -> Alice (bucket owner).
fn demo() -> Result<(), String> {
    let env = |k: &str| std::env::var(k).map_err(|_| format!("missing env {k}"));
    let bucket = std::env::var("SANCTUM_BUCKET").unwrap_or_else(|_| "owner".into());

    println!("== Sanctum live demo: dead-drop E2E over Hippius ==");
    let alice = Identity::generate();
    let bob = Identity::generate();

    let invite = Invite {
        b: bucket.clone(),
        k: env("SENDER_KEY")?,
        s: env("SENDER_SECRET")?,
        pk: B64.encode(alice.public().as_bytes()),
        e: sanctum_core::transport::DEFAULT_ENDPOINT.into(),
    };
    let link = invite.to_link();
    println!("1. Alice's invite link ({} chars):\n   {}...", link.len(), &link[..60]);

    let text = format!("hello alice — sent {}", ulid::Ulid::new());
    let sealed = seal(&bob, &alice.public(), text.as_bytes()).map_err(|e| e.to_string())?;
    let bob_box = Mailbox::new(&invite.e, &invite.b, &invite.k, &invite.s).map_err(|e| e.to_string())?;
    let key = format!("m/{}", ulid::Ulid::new());
    let t0 = std::time::Instant::now();
    bob_box.put(&key, &sealed).map_err(|e| e.to_string())?;
    println!("2. Bob (no account, invite capability only) deposited {key}");

    let alice_box = Mailbox::new(
        sanctum_core::transport::DEFAULT_ENDPOINT,
        &bucket,
        &env("OWNER_KEY")?,
        &env("OWNER_SECRET")?,
    )
    .map_err(|e| e.to_string())?;
    loop {
        let keys = alice_box.list("m/").map_err(|e| e.to_string())?;
        if let Some(k) = keys.iter().find(|k| **k == key) {
            let raw = alice_box.get(k).map_err(|e| e.to_string())?;
            let (from, pt) = open(&alice, &raw).map_err(|e| e.to_string())?;
            assert_eq!(from.as_bytes(), bob.public().as_bytes(), "sender key mismatch");
            println!(
                "3. Alice received+verified from Bob in {:.2}s: \"{}\"",
                t0.elapsed().as_secs_f32(),
                String::from_utf8_lossy(&pt)
            );
            alice_box.delete(k).map_err(|e| e.to_string())?;
            println!("4. Deleted from drop. Round-trip complete.");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        if t0.elapsed().as_secs() > 30 {
            return Err("timed out waiting for message visibility".into());
        }
    }
    Ok(())
}
