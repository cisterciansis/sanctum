// Headless check of Session pairing + bidirectional messaging (live Hippius).
// Run: creds env set, `cargo run -p sanctum-desktop --example pairtest`
use sanctum_core::demo::Session;

fn main() {
    std::env::set_var("SANCTUM_ROLE", "alice");
    let mut alice = Session::from_env().expect("alice session");
    std::env::set_var("SANCTUM_ROLE", "bob");
    let mut bob = Session::from_env().expect("bob session");

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

    alice.send("hi bob — alice").expect("alice send");
    bob.send("hi alice — bob").expect("bob send");

    let wait = |s: &Session, who: &str| {
        let t = std::time::Instant::now();
        loop {
            let got = s.poll();
            if !got.is_empty() {
                println!("{who} received: {got:?} in {:.1}s", t.elapsed().as_secs_f32());
                return got;
            }
            assert!(t.elapsed().as_secs() < 30, "{who} receive timed out");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };
    let a = wait(&alice, "alice");
    let b = wait(&bob, "bob");
    assert_eq!(a, vec!["hi alice — bob".to_string()]);
    assert_eq!(b, vec!["hi bob — alice".to_string()]);
    println!("PAIRTEST OK");
}
