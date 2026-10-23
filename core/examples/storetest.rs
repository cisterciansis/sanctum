// Headless check of the encrypted store + keychain: identity round-trip,
// contact + message persistence across a reopen.
use sanctum_core::store::{identity, Contact, Store, StoredMsg};

fn main() {
    let id1 = identity().expect("identity");
    let id2 = identity().expect("identity again");
    assert_eq!(id1.public().as_bytes(), id2.public().as_bytes(), "identity stable");

    let store = Store::open().expect("open store");
    let cid = store
        .add_contact(&Contact {
            id: 0,
            name: "storetest".into(),
            bucket_in: "b-in".into(),
            bucket_out: "b-out".into(),
            access_key: "hip_test".into(),
            secret: "s".into(),
            peer_pk: String::new(),
            endpoint: "https://s3.hippius.com".into(),
            invite_link: "sanctum://v2/test".into(),
        })
        .expect("add contact");
    store
        .add_msg(
            cid,
            &StoredMsg {
                wire_id: "w1".into(),
                mine: true,
                text: "persisted".into(),
                file_path: None,
                ts_ms: sanctum_core::store::chrono_now_ms(),
                delivered: false,
                reaction: None,
            },
        )
        .expect("add msg");
    store.set_delivered(cid, "w1").expect("delivered");
    store.set_reaction(cid, "w1", "❤️").expect("reaction");
    drop(store);

    // Reopen: everything must still be there, decrypted via keychain key.
    let store = Store::open().expect("reopen");
    let c = store.contacts().expect("contacts").into_iter().find(|c| c.id == cid).expect("row");
    assert_eq!(c.name, "storetest");
    assert_eq!(c.invite_link, "sanctum://v2/test");
    let msgs = store.messages(cid).expect("messages");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].delivered);
    assert_eq!(msgs[0].reaction.as_deref(), Some("❤️"));

    // Cleanup everything the test created (contact row included).
    store.delete_contact(cid).expect("del contact");
    println!("STORETEST OK — encrypted persistence verified");
}
