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



#[derive(Clone, Debug)]
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
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the encrypted store at ~/.sanctum/sanctum.db.
    pub fn open() -> Result<Self, Error> {
        let dir = std::env::var("HOME")
            .map(|h| format!("{h}/.sanctum"))
            .map_err(|_| Error::Crypto("no HOME".into()))?;
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
            CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )
        .map_err(|e| Error::Crypto(e.to_string()))?;
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

    /// Remove a contact and its entire history.
    pub fn delete_contact(&self, contact_id: i64) -> Result<(), Error> {
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

    pub fn contacts(&self) -> Result<Vec<Contact>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, bucket_in, bucket_out, access_key, secret, peer_pk, endpoint, invite_link FROM contacts ORDER BY id")
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
                })
            })
            .map_err(|e| Error::Crypto(e.to_string()))?;
        rows.collect::<Result<_, _>>().map_err(|e| Error::Crypto(e.to_string()))
    }

    pub fn add_msg(&self, contact_id: i64, m: &StoredMsg) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO messages (contact_id, wire_id, mine, text, file_path, ts_ms, delivered, reaction)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![contact_id, m.wire_id, m.mine, m.text, m.file_path, m.ts_ms, m.delivered, m.reaction],
            )
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

    pub fn delete_msg(&self, contact_id: i64, wire_id: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM messages WHERE contact_id = ?1 AND wire_id = ?2",
                rusqlite::params![contact_id, wire_id],
            )
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Delete messages older than `secs` (disappearing messages).
    pub fn purge_older_than(&self, secs: u64) -> Result<usize, Error> {
        let cutoff = chrono_now_ms() - (secs as i64) * 1000;
        self.conn
            .execute("DELETE FROM messages WHERE ts_ms < ?1", rusqlite::params![cutoff])
            .map_err(|e| Error::Crypto(e.to_string()))
    }

    pub fn messages(&self, contact_id: i64) -> Result<Vec<StoredMsg>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT wire_id, mine, text, file_path, ts_ms, delivered, reaction
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
                })
            })
            .map_err(|e| Error::Crypto(e.to_string()))?;
        rows.collect::<Result<_, _>>().map_err(|e| Error::Crypto(e.to_string()))
    }
}

pub fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
