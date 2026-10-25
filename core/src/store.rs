//! Encrypted local store: contacts + message history in SQLCipher, with the
//! database key and the identity key held in the OS keychain (macOS Keychain
//! via `keyring`). Nothing sensitive touches plain files.

use crate::{Error, Identity};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use rusqlite::Connection;

const SERVICE: &str = "chat.sanctum";

fn keychain(name: &str) -> Result<keyring::Entry, Error> {
    keyring::Entry::new(SERVICE, name).map_err(|e| Error::Crypto(e.to_string()))
}

/// Dev mode: DB key in a 0600 file so unsigned rebuilds don't re-prompt.
/// Migrates the existing keychain key on first use (one last prompt).
fn dev_keyfile_secret(dir: &str) -> Result<String, Error> {
    use std::os::unix::fs::PermissionsExt;
    let path = format!("{dir}/db.key");
    if let Ok(s) = std::fs::read_to_string(&path) {
        return Ok(s.trim().to_string());
    }
    // Preserve the key an existing database was encrypted with.
    let key = keychain_secret("db-key")?;
    std::fs::write(&path, &key).map_err(|e| Error::Crypto(e.to_string()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(key)
}

/// Fetch-or-create a random secret in the keychain.
fn keychain_secret(name: &str) -> Result<String, Error> {
    let entry = keychain(name)?;
    match entry.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) => {
            let mut bytes = [0u8; 32];
            use rand_core::RngCore;
            rand_core::OsRng.fill_bytes(&mut bytes);
            let secret = B64.encode(bytes);
            entry.set_password(&secret).map_err(|e| Error::Crypto(e.to_string()))?;
            Ok(secret)
        }
        Err(e) => Err(Error::Crypto(e.to_string())),
    }
}



#[derive(Clone, Debug, Default)]
pub struct Contact {
    pub id: i64,
    pub name: String,
    /// Bucket we poll (peer writes into it).
    pub bucket_in: String,
    /// Bucket we write (peer polls it).
    pub bucket_out: String,
    pub access_key: String,
    pub secret: String,
    /// Peer identity public key, b64url; empty until the handshake lands.
    pub peer_pk: String,
    pub endpoint: String,
    /// Pending invite link (shown until the peer joins), else empty.
    pub invite_link: String,
    /// Local-only marks (see `Mark`).
    pub verified: bool,
    pub blocked: bool,
    pub favorite: bool,
    pub key_changed: bool,
    /// Safety number (`crate::safety_number`); filled in by the worker,
    /// empty until the peer's key is known. Not stored.
    pub safety: String,
}

/// Per-contact flags kept on this device only.
#[derive(Clone, Copy, Debug)]
pub enum Mark {
    /// The safety number was checked in person.
    Verified,
    /// Their messages are dropped unseen.
    Blocked,
    /// Listed first.
    Favorite,
    /// Someone wrote under a different identity key (refused).
    KeyChanged,
}

impl Mark {
    fn column(self) -> &'static str {
        match self {
            Mark::Verified => "verified",
            Mark::Blocked => "blocked",
            Mark::Favorite => "favorite",
            Mark::KeyChanged => "key_changed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct StoredMsg {
    pub wire_id: String,
    pub mine: bool,
    pub text: String,
    pub file_path: Option<String>,
    pub ts_ms: i64,
    pub delivered: bool,
    pub reaction: Option<String>,
    /// Group messages: which seat sent it (None for direct chats / own).
    pub sender_seat: Option<i64>,
    /// When it was read: by the peer for own messages (read receipts), by
    /// us for received ones. None = unread.
    pub read_ms: Option<i64>,
    /// TAO payment card (JSON `tao::PayCard`); `text` is its plain summary.
    pub pay: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the encrypted store at ~/.sanctum/sanctum.db.
    pub fn open() -> Result<Self, Error> {
        let dir = crate::data_dir().ok_or_else(|| Error::Crypto("no HOME".into()))?;
        std::fs::create_dir_all(&dir).map_err(|e| Error::Crypto(e.to_string()))?;
        let conn = Connection::open(format!("{dir}/sanctum.db"))
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let key = if crate::demo::Settings::load().dev_keyfile {
            dev_keyfile_secret(&dir)?
        } else {
            keychain_secret("db-key")?
        };
        conn.pragma_update(None, "key", &key).map_err(|e| Error::Crypto(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS contacts (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                bucket_in TEXT NOT NULL,
                bucket_out TEXT NOT NULL,
                access_key TEXT NOT NULL,
                secret TEXT NOT NULL,
                peer_pk TEXT NOT NULL DEFAULT '',
                endpoint TEXT NOT NULL,
                invite_link TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY,
                contact_id INTEGER NOT NULL REFERENCES contacts(id),
                wire_id TEXT NOT NULL,
                mine INTEGER NOT NULL,
                text TEXT NOT NULL,
                file_path TEXT,
                ts_ms INTEGER NOT NULL,
                delivered INTEGER NOT NULL DEFAULT 0,
                reaction TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_contact ON messages(contact_id, ts_ms);
            CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS groups (
                id INTEGER PRIMARY KEY,
                gid TEXT NOT NULL,
                name TEXT NOT NULL,
                my_seat INTEGER NOT NULL,
                size INTEGER NOT NULL,
                access_key TEXT NOT NULL,
                secret TEXT NOT NULL,
                endpoint TEXT NOT NULL,
                invite_links TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS group_members (
                group_id INTEGER NOT NULL,
                seat INTEGER NOT NULL,
                pk TEXT NOT NULL,
                PRIMARY KEY (group_id, seat)
            );",
        )
        .map_err(|e| Error::Crypto(e.to_string()))?;
        // Group conversations store messages under negative ids that have no
        // contacts row; the bundled SQLite enforces FKs by default, which
        // silently rejected every group message insert.
        conn.pragma_update(None, "foreign_keys", "OFF")
            .map_err(|e| Error::Crypto(e.to_string()))?;
        // Additive migration for pre-group databases; harmless when present.
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN sender_seat INTEGER", []);
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN read_ms INTEGER", []);
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN pay TEXT", []);
        for mark in [Mark::Verified, Mark::Blocked, Mark::Favorite, Mark::KeyChanged] {
            let sql = format!("ALTER TABLE contacts ADD COLUMN {} INTEGER NOT NULL DEFAULT 0", mark.column());
            let _ = conn.execute(&sql, []);
        }
        Ok(Self { conn })
    }

    /// The user's long-term identity, kept inside the encrypted DB so the
    /// keychain holds exactly one item (the DB key) — one prompt, not two.
    /// Migrates a legacy keychain-stored identity on first call.
    pub fn identity(&self) -> Result<Identity, Error> {
        let existing: Option<String> = self
            .conn
            .query_row("SELECT v FROM kv WHERE k = 'identity'", [], |r| r.get(0))
            .ok();
        if let Some(s) = existing {
            return Identity::from_b64(&s);
        }
        let id = match keychain("identity").and_then(|e| {
            e.get_password().map_err(|e| Error::Crypto(e.to_string()))
        }) {
            Ok(s) => Identity::from_b64(&s)?, // migrate legacy entry
            Err(_) => Identity::generate(),
        };
        self.conn
            .execute(
                "INSERT INTO kv (k, v) VALUES ('identity', ?1)",
                rusqlite::params![id.to_b64()],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(id)
    }

    pub fn add_contact(&self, c: &Contact) -> Result<i64, Error> {
        self.conn
            .execute(
                "INSERT INTO contacts (name, bucket_in, bucket_out, access_key, secret, peer_pk, endpoint, invite_link)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![c.name, c.bucket_in, c.bucket_out, c.access_key, c.secret, c.peer_pk, c.endpoint, c.invite_link],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_peer_pk(&self, contact_id: i64, pk: &str) -> Result<(), Error> {
        self.conn
            .execute("UPDATE contacts SET peer_pk = ?1, invite_link = '' WHERE id = ?2", rusqlite::params![pk, contact_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// A contact's v2 ratchet state (opaque blob), keyed by its inbox bucket.
    pub fn ratchet(&self, bucket_in: &str) -> Option<String> {
        self.conn
            .query_row("SELECT v FROM kv WHERE k = 'ratchet/' || ?1", [bucket_in], |r| r.get(0))
            .ok()
    }

    pub fn set_ratchet(&self, bucket_in: &str, blob: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO kv (k, v) VALUES ('ratchet/' || ?1, ?2)",
                rusqlite::params![bucket_in, blob],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Remove a contact, its entire history and its ratchet keys.
    pub fn delete_contact(&self, contact_id: i64) -> Result<(), Error> {
        self.conn
            .execute(
"DELETE FROM kv WHERE k =
                 'ratchet/' || (SELECT bucket_in FROM contacts WHERE id = ?1)",
                rusqlite::params![contact_id],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        self.conn
            .execute("DELETE FROM messages WHERE contact_id = ?1", rusqlite::params![contact_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        self.conn
            .execute("DELETE FROM contacts WHERE id = ?1", rusqlite::params![contact_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    pub fn rename_contact(&self, contact_id: i64, name: &str) -> Result<(), Error> {
        self.conn
            .execute("UPDATE contacts SET name = ?1 WHERE id = ?2", rusqlite::params![name, contact_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Set a mark. Verifying settles a key-change warning; a key change
    /// voids verification.
    pub fn set_mark(&self, contact_id: i64, mark: Mark, on: bool) -> Result<(), Error> {
        let also = match (mark, on) {
            (Mark::Verified, true) => ", key_changed = 0",
            (Mark::KeyChanged, true) => ", verified = 0",
            _ => "",
        };
        let sql = format!("UPDATE contacts SET {} = ?1{also} WHERE id = ?2", mark.column());
        self.conn
            .execute(&sql, rusqlite::params![on, contact_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    pub fn contacts(&self) -> Result<Vec<Contact>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, bucket_in, bucket_out, access_key, secret, peer_pk, endpoint, invite_link, verified, blocked, favorite, key_changed FROM contacts ORDER BY id")
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Contact {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    bucket_in: r.get(2)?,
                    bucket_out: r.get(3)?,
                    access_key: r.get(4)?,
                    secret: r.get(5)?,
                    peer_pk: r.get(6)?,
                    endpoint: r.get(7)?,
                    invite_link: r.get(8)?,
                    verified: r.get(9)?,
                    blocked: r.get(10)?,
                    favorite: r.get(11)?,
                    key_changed: r.get(12)?,
                    safety: String::new(),
                })
            })
            .map_err(|e| Error::Crypto(e.to_string()))?;
        rows.collect::<Result<_, _>>().map_err(|e| Error::Crypto(e.to_string()))
    }

    pub fn add_msg(&self, contact_id: i64, m: &StoredMsg) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO messages (contact_id, wire_id, mine, text, file_path, ts_ms, delivered, reaction, sender_seat, pay)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![contact_id, m.wire_id, m.mine, m.text, m.file_path, m.ts_ms, m.delivered, m.reaction, m.sender_seat, m.pay],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Replace a message's text and payment card (payment status changes).
    pub fn set_msg(&self, contact_id: i64, wire_id: &str, text: &str, pay: Option<&str>) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE messages SET text = ?3, pay = ?4 WHERE contact_id = ?1 AND wire_id = ?2",
                rusqlite::params![contact_id, wire_id, text, pay],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Small key/value records inside the encrypted DB (wallet, settings).
    pub fn kv_get(&self, k: &str) -> Option<String> {
        self.conn.query_row("SELECT v FROM kv WHERE k = ?1", [k], |r| r.get(0)).ok()
    }

    pub fn kv_set(&self, k: &str, v: &str) -> Result<(), Error> {
        self.conn
            .execute("INSERT OR REPLACE INTO kv (k, v) VALUES (?1, ?2)", rusqlite::params![k, v])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    pub fn set_delivered(&self, contact_id: i64, wire_id: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE messages SET delivered = 1 WHERE contact_id = ?1 AND wire_id = ?2",
                rusqlite::params![contact_id, wire_id],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    pub fn set_reaction(&self, contact_id: i64, wire_id: &str, emoji: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE messages SET reaction = ?1 WHERE contact_id = ?2 AND wire_id = ?3",
                rusqlite::params![emoji, contact_id, wire_id],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Mark every unread received message in a chat as read; returns their
    /// wire ids (oldest first) so a receipt can name them.
    pub fn mark_incoming_read(&self, contact_id: i64) -> Result<Vec<String>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT wire_id FROM messages
                 WHERE contact_id = ?1 AND mine = 0 AND read_ms IS NULL ORDER BY ts_ms, id",
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let ids = stmt
            .query_map([contact_id], |r| r.get(0))
            .and_then(|rows| rows.collect::<Result<Vec<String>, _>>())
            .map_err(|e| Error::Crypto(e.to_string()))?;
        self.conn
            .execute(
                "UPDATE messages SET read_ms = ?2
                 WHERE contact_id = ?1 AND mine = 0 AND read_ms IS NULL",
                rusqlite::params![contact_id, chrono_now_ms()],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(ids)
    }

    /// Record the peer's read receipt for one of *our* messages (never theirs).
    /// Returns whether it newly applied.
    pub fn set_read(&self, contact_id: i64, wire_id: &str, ts_ms: i64) -> Result<bool, Error> {
        self.conn
            .execute(
                "UPDATE messages SET read_ms = ?3, delivered = 1
                 WHERE contact_id = ?1 AND wire_id = ?2 AND mine = 1 AND read_ms IS NULL",
                rusqlite::params![contact_id, wire_id, ts_ms],
            )
            .map(|n| n > 0)
            .map_err(|e| Error::Crypto(e.to_string()))
    }

    /// (mine, file_path) of one message, if it exists.
    pub fn msg_meta(&self, contact_id: i64, wire_id: &str) -> Option<(bool, Option<String>)> {
        self.conn
            .query_row(
                "SELECT mine, file_path FROM messages WHERE contact_id = ?1 AND wire_id = ?2",
                rusqlite::params![contact_id, wire_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    pub fn delete_msg(&self, contact_id: i64, wire_id: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM messages WHERE contact_id = ?1 AND wire_id = ?2",
                rusqlite::params![contact_id, wire_id],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Clear a chat's local history: everything, or only messages older
    /// than `before_ms` when given.
    pub fn clear_history(&self, contact_id: i64, before_ms: Option<i64>) -> Result<usize, Error> {
        match before_ms {
            Some(ts) => self
                .conn
                .execute(
                    "DELETE FROM messages WHERE contact_id = ?1 AND ts_ms < ?2",
                    rusqlite::params![contact_id, ts],
                )
                .map_err(|e| Error::Crypto(e.to_string())),
            None => self
                .conn
                .execute("DELETE FROM messages WHERE contact_id = ?1", rusqlite::params![contact_id])
                .map_err(|e| Error::Crypto(e.to_string())),
        }
    }

    /// Delete messages older than `secs` (disappearing messages). Returns
    /// what went, (chat, wire id, saved file), so views and files follow.
    pub fn purge_older_than(&self, secs: u64) -> Result<Vec<(i64, String, Option<String>)>, Error> {
        let cutoff = chrono_now_ms() - (secs as i64) * 1000;
        let mut stmt = self
            .conn
            .prepare("SELECT contact_id, wire_id, file_path FROM messages WHERE ts_ms < ?1")
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let gone = stmt
            .query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| Error::Crypto(e.to_string()))?;
        if !gone.is_empty() {
            self.conn
                .execute("DELETE FROM messages WHERE ts_ms < ?1", rusqlite::params![cutoff])
                .map_err(|e| Error::Crypto(e.to_string()))?;
        }
        Ok(gone)
    }

    pub fn messages(&self, contact_id: i64) -> Result<Vec<StoredMsg>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT wire_id, mine, text, file_path, ts_ms, delivered, reaction, sender_seat, read_ms, pay
                 FROM messages WHERE contact_id = ?1 ORDER BY ts_ms, id",
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let rows = stmt
            .query_map([contact_id], |r| {
                Ok(StoredMsg {
                    wire_id: r.get(0)?,
                    mine: r.get(1)?,
                    text: r.get(2)?,
                    file_path: r.get(3)?,
                    ts_ms: r.get(4)?,
                    delivered: r.get(5)?,
                    reaction: r.get(6)?,
                    sender_seat: r.get(7)?,
                    read_ms: r.get(8)?,
                    pay: r.get(9)?,
                })
            })
            .map_err(|e| Error::Crypto(e.to_string()))?;
        rows.collect::<Result<_, _>>().map_err(|e| Error::Crypto(e.to_string()))
    }
}

#[derive(Clone, Debug)]
pub struct Group {
    pub id: i64,
    pub gid: String,
    pub name: String,
    pub my_seat: i64,
    pub size: i64,
    pub access_key: String,
    pub secret: String,
    pub endpoint: String,
    /// Creator keeps the un-shared seat links (newline-joined) until peers join.
    pub invite_links: String,
    /// seat -> pk (b64), for seats whose hello has landed.
    pub members: Vec<(i64, String)>,
}

impl Store {
    pub fn add_group(&self, g: &Group) -> Result<i64, Error> {
        self.conn
            .execute(
                "INSERT INTO groups (gid, name, my_seat, size, access_key, secret, endpoint, invite_links)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![g.gid, g.name, g.my_seat, g.size, g.access_key, g.secret, g.endpoint, g.invite_links],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn groups(&self) -> Result<Vec<Group>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, gid, name, my_seat, size, access_key, secret, endpoint, invite_links FROM groups ORDER BY id")
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Group {
                    id: r.get(0)?,
                    gid: r.get(1)?,
                    name: r.get(2)?,
                    my_seat: r.get(3)?,
                    size: r.get(4)?,
                    access_key: r.get(5)?,
                    secret: r.get(6)?,
                    endpoint: r.get(7)?,
                    invite_links: r.get(8)?,
                    members: Vec::new(),
                })
            })
            .map_err(|e| Error::Crypto(e.to_string()))?;
        let mut groups: Vec<Group> =
            rows.collect::<Result<_, _>>().map_err(|e| Error::Crypto(e.to_string()))?;
        for g in &mut groups {
            let mut stmt = self
                .conn
                .prepare("SELECT seat, pk FROM group_members WHERE group_id = ?1")
                .map_err(|e| Error::Crypto(e.to_string()))?;
            g.members = stmt
                .query_map([g.id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| Error::Crypto(e.to_string()))?
                .collect::<Result<_, _>>()
                .map_err(|e| Error::Crypto(e.to_string()))?;
        }
        Ok(groups)
    }

    pub fn set_group_member(&self, group_id: i64, seat: i64, pk: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO group_members (group_id, seat, pk) VALUES (?1, ?2, ?3)",
                rusqlite::params![group_id, seat, pk],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    pub fn rename_group(&self, group_id: i64, name: &str) -> Result<(), Error> {
        self.conn
            .execute("UPDATE groups SET name = ?1 WHERE id = ?2", rusqlite::params![name, group_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    pub fn delete_group(&self, group_id: i64) -> Result<(), Error> {
        self.conn
            .execute("DELETE FROM messages WHERE contact_id = ?1", rusqlite::params![-group_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        self.conn
            .execute("DELETE FROM group_members WHERE group_id = ?1", rusqlite::params![group_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        self.conn
            .execute("DELETE FROM groups WHERE id = ?1", rusqlite::params![group_id])
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }
}

/// Emergency wipe: the DB key leaves the keychain (crypto-shredding the
/// SQLCipher file even where flash keeps old blocks), then everything under
/// data_dir() goes: database, settings, dev key file, received files.
/// Ratchet state and the identity live in the database. Local only.
pub fn wipe() -> Result<(), Error> {
    let mut first_err = None;
    for name in ["db-key", "identity"] {
        let deleted = keychain(name).and_then(|e| match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Crypto(e.to_string())),
        });
        if let Err(e) = deleted {
            first_err = first_err.or(Some(e));
        }
    }
    if let Some(dir) = crate::data_dir() {
        // The retiring worker may drop a journal file in mid-delete: retry.
        let gone = (0..3).any(|_| match std::fs::remove_dir_all(&dir) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        });
        if !gone {
            first_err = first_err.or(Some(Error::Crypto(format!("could not delete {dir}"))));
        }
    }
    first_err.map_or(Ok(()), Err)
}

pub fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
