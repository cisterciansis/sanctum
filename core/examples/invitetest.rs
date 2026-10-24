// Live integration test of the real contact flow (SPEC-V2 §4b):
// mint invite (2 buckets + 2 capabilities) -> accept -> hello handshake ->
// message both ways -> receipt -> cleanup.
// Needs hippius_api_token in ~/.sanctum/settings.json.

use sanctum_core::contact::{accept_invite, create_invite, ChatItem, ContactSession};
use sanctum_core::provision::Provisioner;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sanctum_core::Identity;

fn drain_until<F: Fn(&ChatItem) -> bool>(s: &mut ContactSession, what: &str, f: F) -> ChatItem {
    let t = std::time::Instant::now();
    loop {
        for item in s.poll() {
            if f(&item) {
                println!("{what} in {:.1}s", t.elapsed().as_secs_f32());
                return item;
            }
        }
        assert!(t.elapsed().as_secs() < 45, "{what} timed out");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn main() {
    let p = Provisioner::from_settings().expect("needs hippius_api_token in settings");
    let alice = Identity::generate();
    let bob = Identity::generate();

    let t0 = std::time::Instant::now();
    let (contact_a, link, mut sess_a) = create_invite(&p, &alice, "Bob").expect("create invite");
    println!("invite minted in {:.1}s ({} chars)", t0.elapsed().as_secs_f32(), link.len());

    let (_, mut sess_b) = accept_invite(&link, &bob, "Alice").expect("accept invite");

    // Alice waits for Bob's hello (the PQXDH handshake), learns his key.
    let hello = drain_until(&mut sess_a, "hello received", |i| matches!(i, ChatItem::Hello { .. }));
    let ChatItem::Hello { pk } = hello else { unreachable!() };
    assert_eq!(pk, URL_SAFE_NO_PAD.encode(bob.public().as_bytes()));
    sess_a.set_peer_pk(&pk).expect("set peer pk");
    assert!(sess_a.ready() && sess_b.ready());

    let key_a = sess_a.send("w1", "hi bob, real invite flow").expect("alice send");
    sess_b.send("w2", "hi alice, no account needed").expect("bob send");

    let got_b = drain_until(&mut sess_b, "bob received", |i| matches!(i, ChatItem::Message { .. }));
    let ChatItem::Message { text, .. } = got_b else { unreachable!() };
    assert_eq!(text, "hi bob, real invite flow");
    let got_a = drain_until(&mut sess_a, "alice received", |i| matches!(i, ChatItem::Message { .. }));
    let ChatItem::Message { text, .. } = got_a else { unreachable!() };
    assert_eq!(text, "hi alice, no account needed");

    let t = std::time::Instant::now();
    while !sess_a.is_delivered(&key_a).expect("receipt") {
        assert!(t.elapsed().as_secs() < 15, "receipt timed out");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!("receipt confirmed in {:.1}s", t.elapsed().as_secs_f32());

    // Cleanup: retiring both buckets leaves the test's capabilities with
    // nothing to reach. (Revoking by label would also kill real chats'.)
    for b in [&contact_a.bucket_in, &contact_a.bucket_out] {
        p.delete_bucket(b).expect("delete bucket");
    }
    println!("INVITETEST OK — full contact lifecycle verified");
}
