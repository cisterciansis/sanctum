// Headless check of Session pairing, messaging, receipts, reactions (live
// Hippius). Run: creds env set, `cargo run -p sanctum-desktop --example pairtest`
use sanctum_core::demo::{Incoming, Session};

fn texts(items: &[Incoming]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|i| match i {
            Incoming::Message { id, text } => Some((id.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

fn main() {
    // Isolated roles so live alice/bob clients can't race the test.
    std::env::set_var("SANCTUM_ROLE", "carole");
    std::env::set_var("SANCTUM_PEER", "dave");
    let mut alice = Session::from_env().expect("carole session");
    std::env::set_var("SANCTUM_ROLE", "dave");
    std::env::set_var("SANCTUM_PEER", "carole");
    let mut bob = Session::from_env().expect("dave session");

    let t0 = std::time::Instant::now();
    loop {
        let a = alice.pair();
        let b = bob.pair();
        if a && b {
            break;
        }
        assert!(t0.elapsed().as_secs() < 30, "pairing timed out");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!("paired in {:.1}s", t0.elapsed().as_secs_f32());

    // Drain leftovers from any earlier aborted run.
    let _ = alice.poll();
    let _ = bob.poll();

    let alice_key = alice.send("w-alice-1", "hi bob — alice").expect("alice send");
    assert!(
        !alice.is_delivered(&alice_key).unwrap_or(true),
        "must not read as delivered while still in the drop"
    );
    bob.send("w-bob-1", "hi alice — bob").expect("bob send");

    let wait = |s: &Session, who: &str| {
        let t = std::time::Instant::now();
        loop {
            let got = s.poll();
            if !got.is_empty() {
                println!("{who} received {} item(s) in {:.1}s", got.len(), t.elapsed().as_secs_f32());
                return got;
            }
            assert!(t.elapsed().as_secs() < 30, "{who} receive timed out");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };

    let a = wait(&alice, "alice");
    assert_eq!(texts(&a), vec![("w-bob-1".to_string(), "hi alice — bob".to_string())]);

    let b = wait(&bob, "bob");
    assert_eq!(texts(&b), vec![("w-alice-1".to_string(), "hi bob — alice".to_string())]);

    // Bob's poll deleted alice's object → the receipt flips.
    let t = std::time::Instant::now();
    while !alice.is_delivered(&alice_key).expect("receipt check") {
        assert!(t.elapsed().as_secs() < 15, "receipt timed out");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!("delivery receipt confirmed in {:.1}s", t.elapsed().as_secs_f32());

    // Reaction round-trip: alice hearts bob's message.
    alice.react("w-bob-1", "❤️").expect("react");
    let t = std::time::Instant::now();
    loop {
        let got = bob.poll();
        if let Some(Incoming::Reaction { ref_id, emoji }) =
            got.iter().find(|i| matches!(i, Incoming::Reaction { .. }))
        {
            assert_eq!(ref_id, "w-bob-1");
            assert_eq!(emoji, "❤️");
            println!("reaction round-trip in {:.1}s", t.elapsed().as_secs_f32());
            break;
        }
        assert!(t.elapsed().as_secs() < 30, "reaction timed out");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!("PAIRTEST OK");
}
