//! Bounded clipboard history with immutable encrypted payloads and an encrypted manifest.
//! Atomic manifest replacement is the commit point; files outside that manifest are never
//! restored. Only encrypted bytes reach disk, and unavailable keys disable persistence.

use super::Payload;
use super::payload::{MAX_IMAGE, history_limit};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{AeadInOut, KeyInit},
};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

const MAX_ENTRIES: usize = 100;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_MANIFEST: usize = 16 * 1024;
const HEADER: &[u8; 8] = b"SHOJIMF1";
const PAYLOAD_HEADER: &[u8; 8] = b"SHOJIPL1";
const SNAPSHOT: &str = "history.enc";
const KEY_ACQUISITION_NAME: &str = "org.shoji_bar.ClipboardHistoryKey";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub id: u64,
    pub payload: Payload,
    pub pinned: bool,
}

struct DirectoryLock(File);

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // fork/dup shares the open-file description. Closing only our descriptor
        // can leave its lock alive until an unrelated child reaches exec.
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
}

struct Storage {
    directory: PathBuf,
    _directory: DirectoryLock,
    key: Zeroizing<[u8; 32]>,
    stored: HashMap<u64, [u8; 24]>,
    #[cfg(test)]
    fail_directory_sync_after: Option<usize>,
}

pub(crate) struct History {
    entries: Vec<Entry>,
    next_id: u64,
    storage: Option<Storage>,
    error: Option<String>,
    entry_limit: usize,
    byte_limit: usize,
}

impl History {
    /// Does no I/O and is suitable for previews, without reporting a persistence failure.
    pub fn memory() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            storage: None,
            error: None,
            entry_limit: MAX_ENTRIES,
            byte_limit: MAX_BYTES,
        }
    }

    /// Call on the history worker, never the UI thread. Keyring access has a total deadline
    /// and never unlocks collections or invokes Secret Service prompts.
    pub fn open(directory: PathBuf) -> Self {
        Self::open_with_key(directory, |existing| {
            futures_lite::future::block_on(futures_lite::future::race(
                acquire_key(existing),
                async {
                    async_io::Timer::after(Duration::from_secs(3)).await;
                    Err("Secret Service timed out".to_owned())
                },
            ))
        })
    }

    fn open_with_key(
        directory: PathBuf,
        key: impl FnOnce(bool) -> Result<Zeroizing<[u8; 32]>, String>,
    ) -> Self {
        let mut history = Self::memory();
        let opened = (|| -> Result<Storage, String> {
            let directory_file = private_directory(&directory).map_err(storage_error)?;
            rustix::fs::flock(
                &directory_file,
                rustix::fs::FlockOperation::NonBlockingLockExclusive,
            )
            .map_err(|_| "Clipboard history is already open in another process".to_owned())?;
            let directory_file = DirectoryLock(directory_file);
            // Even an orphaned encrypted payload or temporary file forbids creating a
            // replacement key. A crash before the first manifest must not lose its key.
            let existing = fs::read_dir(&directory)
                .map_err(storage_error)?
                .next()
                .transpose()
                .map_err(storage_error)?
                .is_some();
            let key = key(existing)?;
            Ok(Storage {
                directory,
                _directory: directory_file,
                key,
                stored: HashMap::new(),
                #[cfg(test)]
                fail_directory_sync_after: None,
            })
        })();
        match opened {
            Ok(mut storage) => match storage.load() {
                Ok(Some((entries, next_id))) => {
                    history.entries = entries;
                    history.next_id = next_id;
                    history.storage = Some(storage);
                }
                Ok(None) => history.storage = Some(storage),
                // Keep unreadable/tampered history intact, including on future captures.
                Err(error) => history.error = Some(format!("Memory-only clipboard: {error}")),
            },
            Err(error) => history.error = Some(format!("Memory-only clipboard: {error}")),
        }
        history
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn capture(&mut self, payload: Payload) -> bool {
        if payload.sensitive {
            return false;
        }
        if let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.payload.same_content(&payload))
        {
            let first_unpinned = self.entries.partition_point(|entry| entry.pinned);
            if self.entries[position].pinned || position == first_unpinned {
                return false;
            }
            let entry = self.entries.remove(position);
            self.entries.insert(first_unpinned, entry);
            self.persist();
            return true;
        }
        if !payload.valid_for_history() {
            return false;
        }

        // Plan eviction first: a fully pinned history must not lose anything on rejection.
        let pinned_bytes: usize = self
            .entries
            .iter()
            .filter(|entry| entry.pinned)
            .map(|entry| entry.payload.bytes.len())
            .sum();
        let pinned_count = self.entries.iter().filter(|entry| entry.pinned).count();
        if pinned_count >= self.entry_limit || pinned_bytes + payload.bytes.len() > self.byte_limit
        {
            return false;
        }
        let Some(next_id) = self.next_id.checked_add(1) else {
            self.error = Some("Clipboard history ID space exhausted".to_owned());
            return false;
        };
        let mut bytes: usize = self
            .entries
            .iter()
            .map(|entry| entry.payload.bytes.len())
            .sum();
        while self.entries.len() >= self.entry_limit
            || bytes + payload.bytes.len() > self.byte_limit
        {
            // Pinned entries are the prefix, so this can only remove an unpinned entry.
            let removed = self.entries.pop().expect("eviction requires an entry");
            bytes -= removed.payload.bytes.len();
        }
        self.entries.insert(
            pinned_count,
            Entry {
                id: self.next_id,
                payload,
                pinned: false,
            },
        );
        self.next_id = next_id;
        self.persist();
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let Some(position) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.entries.remove(position);
        self.persist();
        true
    }

    pub fn toggle_pin(&mut self, id: u64) -> bool {
        let Some(position) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        let mut entry = self.entries.remove(position);
        entry.pinned = !entry.pinned;
        // Pin newest first, unpin to the front of recent history.
        let position = if entry.pinned {
            0
        } else {
            self.entries.partition_point(|entry| entry.pinned)
        };
        self.entries.insert(position, entry);
        self.persist();
        true
    }

    fn persist(&mut self) {
        if let Some(storage) = &mut self.storage {
            self.error = storage
                .save(&self.entries, self.next_id)
                .err()
                .map(|error| format!("Clipboard persistence error: {error}"));
        }
    }
}

fn storage_error(error: io::Error) -> String {
    // Never include clipboard content in diagnostics.
    format!("Clipboard storage: {error}")
}

fn private_directory(path: &Path) -> io::Result<File> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::DIRECTORY).bits() as i32)
        .open(path)?;
    if directory.metadata()?.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "history directory has another owner",
        ));
    }
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

impl Storage {
    fn load(&mut self) -> Result<Option<(Vec<Entry>, u64)>, String> {
        let manifest = match self.read_encrypted(SNAPSHOT, HEADER, HEADER, MAX_MANIFEST)? {
            Some((manifest, _)) => manifest,
            None => return Ok(None),
        };
        let (records, next_id) = decode(&manifest)?;
        let mut entries: Vec<Entry> = Vec::with_capacity(records.len());
        let mut stored = HashMap::with_capacity(records.len());
        for record in records {
            let associated = payload_associated(record.id);
            let (bytes, nonce) = self
                .read_encrypted(
                    &payload_file(record.id),
                    PAYLOAD_HEADER,
                    &associated,
                    record.length + 48,
                )?
                .ok_or_else(|| "Clipboard payload is missing".to_owned())?;
            // Bind the manifest to the exact payload generation, not only its ID: an
            // uncommitted orphan's ID may legitimately be reused after a process crash.
            if nonce != record.nonce {
                return Err("Clipboard payload authentication binding failed".to_owned());
            }
            if bytes.len() != record.length {
                return Err("Invalid clipboard payload length".to_owned());
            }
            let payload = Payload {
                mime: record.mime,
                bytes: Arc::from(bytes.as_slice()),
                sensitive: false,
            };
            if !payload.valid_for_history()
                || entries
                    .iter()
                    .any(|entry| entry.payload.same_content(&payload))
            {
                return Err("Invalid clipboard payload".to_owned());
            }
            stored.insert(record.id, nonce);
            entries.push(Entry {
                id: record.id,
                payload,
                pinned: record.pinned,
            });
        }
        self.stored = stored;
        Ok(Some((entries, next_id)))
    }

    fn save(&mut self, entries: &[Entry], next_id: u64) -> Result<(), String> {
        let mut wrote_payload = false;
        let mut stored = HashMap::with_capacity(entries.len());
        for entry in entries {
            if let Some(nonce) = self.stored.get(&entry.id) {
                stored.insert(entry.id, *nonce);
                continue;
            }
            // Original payloads are immutable for an ID. Only a new capture needs this
            // copy/encryption; reorder, pin, remove and later captures reuse existing files.
            let mut bytes = Zeroizing::new(Vec::with_capacity(entry.payload.bytes.len() + 16));
            bytes.extend_from_slice(&entry.payload.bytes);
            let nonce = self.write_encrypted(
                &payload_file(entry.id),
                PAYLOAD_HEADER,
                &payload_associated(entry.id),
                bytes,
            )?;
            stored.insert(entry.id, nonce);
            wrote_payload = true;
        }
        if wrote_payload {
            // Make new payload names durable before committing references to them.
            self.sync_directory()?;
        }
        self.write_encrypted(
            SNAPSHOT,
            HEADER,
            HEADER,
            encode_manifest(entries, next_id, &stored),
        )?;
        // Replacement already installed a new manifest even if durability confirmation
        // fails. Retries must never rewrite payload generations that it now references.
        self.stored = stored;
        self.sync_directory()?;

        // Only the committed manifest controls restoration. Cleanup is deliberately after
        // commit, so a failed manifest write cannot delete the previous manifest's data.
        let mut removed = false;
        for entry in fs::read_dir(&self.directory).map_err(storage_error)? {
            let entry = entry.map_err(storage_error)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let orphan = payload_id(name).is_some_and(|id| !self.stored.contains_key(&id));
            let stale_temporary = name.starts_with(".history-") && name.ends_with(".tmp");
            if orphan || stale_temporary {
                fs::remove_file(entry.path()).map_err(storage_error)?;
                removed = true;
            }
        }
        if removed {
            self.sync_directory()?;
        }
        Ok(())
    }

    fn sync_directory(&mut self) -> Result<(), String> {
        #[cfg(test)]
        if let Some(remaining) = &mut self.fail_directory_sync_after {
            if *remaining == 0 {
                self.fail_directory_sync_after = None;
                return Err("Injected directory synchronization failure".to_owned());
            }
            *remaining -= 1;
        }
        self._directory.0.sync_all().map_err(storage_error)
    }

    fn read_encrypted(
        &self,
        name: &str,
        header: &[u8; 8],
        associated: &[u8],
        maximum: usize,
    ) -> Result<Option<(Zeroizing<Vec<u8>>, [u8; 24])>, String> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(self.directory.join(name))
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_error(error)),
        };
        let metadata = file.metadata().map_err(storage_error)?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
        {
            return Err("Unsafe clipboard file ownership or type".to_owned());
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(storage_error)?;
        if metadata.len() > maximum as u64 || metadata.len() < 48 {
            return Err("Invalid clipboard encrypted file length".to_owned());
        }
        let mut envelope = [0; 32];
        file.read_exact(&mut envelope).map_err(storage_error)?;
        if &envelope[..8] != header {
            return Err("Invalid clipboard encrypted file".to_owned());
        }
        let nonce: [u8; 24] = envelope[8..].try_into().expect("24-byte nonce");
        let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize - 32));
        file.take(maximum as u64 - 32 + 1)
            .read_to_end(&mut bytes)
            .map_err(storage_error)?;
        if bytes.len() > maximum - 32 {
            return Err("Clipboard encrypted file exceeds limit".to_owned());
        }
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref()).expect("32-byte key");
        cipher
            .decrypt_in_place(&XNonce::from(nonce), associated, &mut *bytes)
            .map_err(|_| "Clipboard file authentication failed".to_owned())?;
        Ok(Some((bytes, nonce)))
    }

    fn write_encrypted(
        &self,
        name: &str,
        header: &[u8; 8],
        associated: &[u8],
        mut bytes: Zeroizing<Vec<u8>>,
    ) -> Result<[u8; 24], String> {
        let mut nonce = [0; 24];
        getrandom::fill(&mut nonce).map_err(|_| "Secure randomness unavailable".to_owned())?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref()).expect("32-byte key");
        cipher
            .encrypt_in_place(&XNonce::from(nonce), associated, &mut *bytes)
            .map_err(|_| "Clipboard encryption failed".to_owned())?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".history-")
            .suffix(".tmp")
            .tempfile_in(&self.directory)
            .map_err(storage_error)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(storage_error)?;
        temporary.write_all(header).map_err(storage_error)?;
        temporary.write_all(&nonce).map_err(storage_error)?;
        temporary.write_all(&bytes).map_err(storage_error)?;
        temporary.as_file().sync_all().map_err(storage_error)?;
        temporary
            .persist(self.directory.join(name))
            .map_err(|error| storage_error(error.error))?;
        Ok(nonce)
    }
}

fn payload_file(id: u64) -> String {
    format!("entry-{id:016x}.enc")
}

fn payload_id(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("entry-")?.strip_suffix(".enc")?;
    if digits.len() != 16
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

fn payload_associated(id: u64) -> [u8; 16] {
    let mut associated = [0; 16];
    associated[..8].copy_from_slice(PAYLOAD_HEADER);
    associated[8..].copy_from_slice(&id.to_le_bytes());
    associated
}

struct Record {
    id: u64,
    pinned: bool,
    mime: String,
    length: usize,
    nonce: [u8; 24],
}

// The manifest is bounded metadata only; original bytes live in immutable authenticated
// payload files. Its ordering and references are the sole authority on reload.
fn encode_manifest(
    entries: &[Entry],
    next_id: u64,
    stored: &HashMap<u64, [u8; 24]>,
) -> Zeroizing<Vec<u8>> {
    let capacity = 12
        + entries
            .iter()
            .map(|entry| 41 + entry.payload.mime.len())
            .sum::<usize>();
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity + 16));
    bytes.extend_from_slice(&next_id.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        bytes.extend_from_slice(&entry.id.to_le_bytes());
        bytes.push(u8::from(entry.pinned));
        bytes.extend_from_slice(&(entry.payload.mime.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(entry.payload.bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(
            stored
                .get(&entry.id)
                .expect("payload written before manifest"),
        );
        bytes.extend_from_slice(entry.payload.mime.as_bytes());
    }
    bytes
}

fn decode(mut bytes: &[u8]) -> Result<(Vec<Record>, u64), String> {
    fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Result<&'a [u8], String> {
        if length > bytes.len() {
            return Err("Truncated clipboard manifest".to_owned());
        }
        let (head, tail) = bytes.split_at(length);
        *bytes = tail;
        Ok(head)
    }
    fn number(bytes: &mut &[u8]) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            take(bytes, 4)?.try_into().expect("four bytes"),
        ))
    }
    let next_id = u64::from_le_bytes(take(&mut bytes, 8)?.try_into().expect("eight bytes"));
    let count = number(&mut bytes)? as usize;
    if count > MAX_ENTRIES || next_id == 0 {
        return Err("Invalid clipboard history limits".to_owned());
    }
    let mut records = Vec::with_capacity(count);
    let mut ids = HashSet::with_capacity(count);
    let mut total = 0;
    let mut unpinned = false;
    for _ in 0..count {
        let id = u64::from_le_bytes(take(&mut bytes, 8)?.try_into().expect("eight bytes"));
        let pinned = match take(&mut bytes, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err("Invalid pin flag".to_owned()),
        };
        let mime_len = number(&mut bytes)? as usize;
        let length = number(&mut bytes)? as usize;
        let nonce = take(&mut bytes, 24)?.try_into().expect("24-byte nonce");
        total += length;
        if mime_len > 64
            || length == 0
            || length > MAX_IMAGE
            || total > MAX_BYTES
            || id == 0
            || id >= next_id
            || !ids.insert(id)
            || (pinned && unpinned)
        {
            return Err("Invalid clipboard history record".to_owned());
        }
        unpinned |= !pinned;
        let mime = std::str::from_utf8(take(&mut bytes, mime_len)?)
            .map_err(|_| "Invalid clipboard MIME".to_owned())?
            .to_owned();
        if !history_limit(&mime).is_some_and(|limit| length <= limit) {
            return Err("Invalid clipboard payload type".to_owned());
        }
        records.push(Record {
            id,
            pinned,
            mime,
            length,
            nonce,
        });
    }
    if !bytes.is_empty() {
        return Err("Trailing clipboard manifest data".to_owned());
    }
    Ok((records, next_id))
}

async fn lock_key_acquisition(
    connection: &zbus::Connection,
) -> Result<(), Box<dyn std::error::Error>> {
    // The key namespace is global to the session, not the chosen XDG state directory.
    // Never queue or replace another initializer: an in-progress empty item is not ours
    // to recover until its creator has completed or its bus connection has closed.
    let reply = connection
        .request_name_with_flags(
            KEY_ACQUISITION_NAME,
            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
        )
        .await?;
    if reply != zbus::fdo::RequestNameReply::PrimaryOwner {
        return Err("Clipboard encryption key initialization is already in progress".into());
    }
    Ok(())
}

async fn acquire_key(existing: bool) -> Result<Zeroizing<[u8; 32]>, String> {
    async fn acquire(
        existing: bool,
        connection: &zbus::Connection,
    ) -> Result<Zeroizing<[u8; 32]>, Box<dyn std::error::Error>> {
        let service = secret_service::SecretService::connect_with_existing(
            secret_service::EncryptionType::Dh,
            connection.clone(),
        )
        .await?;
        let attributes = HashMap::from([
            ("application", "shoji-bar-4"),
            ("purpose", "clipboard-history-v1"),
        ]);
        let found = service.search_items(attributes.clone()).await?;
        if !found.locked.is_empty() {
            return Err("Clipboard encryption key is locked".into());
        }
        if found.unlocked.len() > 1 {
            return Err("Multiple clipboard encryption keys found".into());
        }
        if let Some(item) = found.unlocked.first() {
            let secret = Zeroizing::new(item.get_secret().await?);
            // Recover interrupted first-time creation, but never replace a key once any
            // encrypted snapshot exists (even an unreadable or corrupted snapshot).
            if secret.is_empty() && !existing {
                let mut key = Zeroizing::new([0; 32]);
                getrandom::fill(key.as_mut()).map_err(|_| "Secure randomness unavailable")?;
                item.set_secret(key.as_ref(), "application/octet-stream")
                    .await?;
                return Ok(key);
            }
            if secret.len() != 32 {
                return Err("Invalid clipboard encryption key".into());
            }
            let mut key = Zeroizing::new([0; 32]);
            key.copy_from_slice(&secret);
            return Ok(key);
        }
        if existing {
            return Err("Encryption key missing for existing clipboard history".into());
        }
        let collection = service.get_default_collection().await?;
        if collection.is_locked().await? {
            return Err("Secret Service collection is locked".into());
        }
        let mut key = Zeroizing::new([0; 32]);
        getrandom::fill(key.as_mut()).map_err(|_| "Secure randomness unavailable")?;

        // The crate's create_item executes prompts automatically. Create an EMPTY item via
        // D-Bus instead, never call Prompt, then set the key over the crate's DH session.
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.secrets",
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service",
        )
        .await?;
        let (_, session): (zbus::zvariant::OwnedValue, zbus::zvariant::OwnedObjectPath) = proxy
            .call("OpenSession", &("plain", zbus::zvariant::Value::from("")))
            .await?;
        let result = async {
            let proxy = zbus::Proxy::new(
                &connection,
                "org.freedesktop.secrets",
                collection.collection_path.as_str(),
                "org.freedesktop.Secret.Collection",
            )
            .await?;
            let properties = HashMap::from([
                (
                    "org.freedesktop.Secret.Item.Label",
                    zbus::zvariant::Value::from("Shoji clipboard history encryption key"),
                ),
                (
                    "org.freedesktop.Secret.Item.Attributes",
                    zbus::zvariant::Value::from(zbus::zvariant::Dict::from(attributes)),
                ),
            ]);
            let empty = (
                &session,
                Vec::<u8>::new(),
                Vec::<u8>::new(),
                "application/octet-stream",
            );
            let (item, prompt): (
                zbus::zvariant::OwnedObjectPath,
                zbus::zvariant::OwnedObjectPath,
            ) = proxy
                .call("CreateItem", &(properties, empty, false))
                .await?;
            if prompt.as_str() != "/" || item.as_str() == "/" {
                return Err::<(), Box<dyn std::error::Error>>(
                    "Secret Service requires an interactive prompt".into(),
                );
            }
            service
                .get_item_by_path(item)
                .await?
                .set_secret(key.as_ref(), "application/octet-stream")
                .await?;
            Ok(())
        }
        .await;
        if let Ok(proxy) = zbus::Proxy::new(
            &connection,
            "org.freedesktop.secrets",
            session.as_str(),
            "org.freedesktop.Secret.Session",
        )
        .await
        {
            let _: Result<(), _> = proxy.call("Close", &()).await;
        }
        result?;
        Ok(key)
    }
    let result = async {
        let connection = zbus::connection::Builder::session()?
            .method_timeout(Duration::from_secs(2))
            .build()
            .await?;
        lock_key_acquisition(&connection).await?;
        let result = acquire(existing, &connection).await;
        // Closing the dedicated connection releases the session-wide lock on both
        // success and failure, after every key lookup/initialization operation finishes.
        let _ = connection.close().await;
        result
    }
    .await;
    result
        .map_err(|error: Box<dyn std::error::Error>| format!("Secret Service unavailable: {error}"))
}

#[cfg(test)]
mod tests;
