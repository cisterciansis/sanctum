// Live three-member group test: mint mesh -> two accepts -> mutual seat
// discovery -> fan-out messages all directions -> reaction -> cleanup.
// Needs hippius_api_token in ~/.sanctum/settings.json.
use sanctum_core::contact::{accept_group, create_group, GroupItem, GroupLive};
use sanctum_core::provision::Provisioner;
use sanctum_core::Identity;

fn drain_for<F: FnMut(&GroupItem)>(gl: &GroupLive, secs: u64, mut f: F) {
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < secs {
        for item in gl.poll() {
            f(&item);
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
}

fn main() {
    let p = Provisioner::from_settings().expect("token");
    let alice = Identity::generate();
    let bob = Identity::generate();
    let carol = Identity::generate();

    let t0 = std::time::Instant::now();
    let (mut g0, links) = create_group(&p, &alice, "trio", 3).expect("create");
    g0.id = 1;
    println!("group minted in {:.1}s ({} seat links)", t0.elapsed().as_secs_f32(), links.len());

    let mut gb = accept_group(&links[0], &bob).expect("bob accept");
    gb.id = 2;
    gb.members.push((1, {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bob.public().as_bytes())
    }));
    let mut gc = accept_group(&links[1], &carol).expect("carol accept");
    gc.id = 3;
    gc.members.push((2, {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(carol.public().as_bytes())
    }));

    let mut la = GroupLive::new(&g0, &alice).expect("alice live");
    let mut lb = GroupLive::new(&gb, &bob).expect("bob live");
    let mut lc = GroupLive::new(&gc, &carol).expect("carol live");
    lb.announce_all();
    lc.announce_all();

    // Discovery rounds: register peers, reply to newcomers.
    let t = std::time::Instant::now();
    for _round in 0..30 {
        for (livei, name) in [(&mut la, "alice"), (&mut lb, "bob"), (&mut lc, "carol")] {
            let items = livei.poll();
            for item in items {
                if let GroupItem::NewPeer { seat, pk } = item {
                    if livei.register_peer(seat, &pk) {
                        println!("{name} discovered seat {seat}");
                        livei.announce_to(&[seat]);
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
        if t.elapsed().as_secs() > 40 {
            break;
        }
    }

    la.send("w-a", "hello group — from alice (seat 0)").expect("alice send");
    lb.send("w-b", "bob here (seat 1)").expect("bob send");

    let mut carol_got = 0;
    drain_for(&lc, 12, |item| {
        if let GroupItem::Message { seat, text, .. } = item {
            println!("carol received from seat {seat}: {text}");
            carol_got += 1;
        }
    });
    assert!(carol_got >= 2, "carol should receive from both alice and bob, got {carol_got}");

    let mut alice_got = 0;
    drain_for(&la, 8, |item| {
        if let GroupItem::Message { seat, text, .. } = item {
            println!("alice received from seat {seat}: {text}");
            alice_got += 1;
        }
    });
    assert!(alice_got >= 1, "alice should receive bob's message");

    lc.react("w-a", "❤️").expect("carol react");
    let mut saw_react = false;
    drain_for(&la, 10, |item| {
        if let GroupItem::Reaction { ref_id, emoji } = item {
            println!("alice saw reaction {emoji} on {ref_id}");
            saw_react = true;
        }
    });
    assert!(saw_react, "reaction should reach alice");

    // Cleanup the mesh + tokens.
    for i in 0..3 {
        for j in 0..3 {
            if i != j {
                let _ = p.delete_bucket(&format!("g-{}-{}-{}", g0.gid, i, j));
            }
        }
    }
    let n = p.revoke_sub_tokens_by_label("sanctum-group").unwrap_or(0);
    println!("cleanup: mesh deleted, {n} tokens revoked");
    println!("GROUPTEST OK");
}
