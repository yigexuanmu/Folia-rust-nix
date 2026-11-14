// Session storage: the sealed-file backend is the one that works when no keyring answers.
//
// Tests never touch a real keyring: when one is available they step aside rather than write entries
// into the developer's own keychain.

use std::os::unix::fs::PermissionsExt;

use folia_omni::session::{Backend, Store};

/// A store whose backend is the sealed file, or `None` when this machine has a keyring or lacks
/// `systemd-creds` — either way there is nothing here to check.
fn sealed_store(dir: &std::path::Path) -> Option<Store> {
    let store = Store::new("Folia-rust-test".to_owned());
    store.seal_in(dir.to_path_buf());
    (store.backend() == Backend::Sealed).then_some(store)
}

#[test]
fn sealed_session_round_trip_leaves_a_private_file() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("session");
    let Some(store) = sealed_store(&dir) else { return };

    assert_eq!(store.load("netease").unwrap(), None, "nothing kept yet");

    store.save("netease", "music_u=secret").unwrap();
    let file = dir.join("netease.session");
    assert!(file.exists(), "sealed file written");
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the sealed file must stay private");
    let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "its directory too");

    assert_eq!(store.load("netease").unwrap().as_deref(), Some("music_u=secret"));

    store.delete("netease").unwrap();
    assert!(!file.exists(), "signing out removes the file");
    assert_eq!(store.load("netease").unwrap(), None);
}

/// A file that lost its key (another host) or was tampered with is treated as no session at all.
#[test]
fn an_unreadable_sealed_file_reads_as_no_session() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("session");
    let Some(store) = sealed_store(&dir) else { return };
    store.save("qq", "credential=secret").unwrap();

    std::fs::write(dir.join("qq.session"), b"not a sealed credential").unwrap();
    assert_eq!(store.load("qq").unwrap(), None);
}

/// Without a keyring and without `systemd-creds` the store says so instead of pretending.
#[test]
fn nothing_available_keeps_nothing_and_says_why() {
    let store = Store::new("Folia-rust-test-bare".to_owned());
    if store.backend() != Backend::Unavailable {
        return;
    }
    assert_eq!(store.load("netease").unwrap(), None);
    assert!(store.save("netease", "music_u=secret").is_err(), "saving must fail loudly");
    assert!(store.delete("netease").is_ok(), "deleting what cannot exist is fine");
}
