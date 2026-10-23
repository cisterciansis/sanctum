// Headless host for the iOS demo: mints an invite, waits for the phone's
// hello, greets it, then echoes anything the phone sends.
use sanctum_core::contact::{create_invite, ChatItem, ContactSession};
use sanctum_core::provision::Provisioner;
use sanctum_core::Identity;

fn main() {
    let p = Provisioner::from_settings().expect("needs hippius_api_token");
    let me = Identity::generate();
    let (contact, link) = create_invite(&p, &me, "iPhone").expect("invite");
    println!("INVITE:{link}");
    let mut sess = ContactSession::new(&contact, &me).expect("session");
    let t0 = std::time::Instant::now();
    loop {
        for item in sess.poll() {
            match item {
                ChatItem::Hello { pk } => {
                    sess.set_peer_pk(&pk).expect("pk");
                    println!("HELLO received — phone joined");
                    sess.send(&ulid::Ulid::new().to_string(), "welcome to sanctum on iphone — sent from the host over hippius")
                        .expect("send");
                    println!("GREETING sent");
                }
                ChatItem::Message { text, .. } => println!("PHONE SAYS: {text}"),
                _ => {}
            }
        }
        if t0.elapsed().as_secs() > 300 {
            println!("timeout");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}
