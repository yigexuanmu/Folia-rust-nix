// Platform sessions: the system keyring when it is usable, otherwise a file sealed with `systemd-creds`.
//
// The original kept sessions in the keyring only, which silently did nothing on machines without a
// Secret Service. A sealed file keeps the same promise (never plain text, never in settings or logs)
// while surviving a restart; the keyring stays the first choice whenever it answers.

use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::OmniError;

/// Where the kept sessions actually live, decided once per run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The system keyring, unlocked by the session: the strongest of the three.
    Keyring,
    /// A file under the app's data directory, encrypted with `systemd-creds --user` (host bound).
    Sealed,
    /// Neither is usable: sessions cannot be kept, and the interface has to say so.
    Unavailable,
}

impl Backend {
    /// The text form used in logs; never includes an entry name or a secret.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::Sealed => "sealed-file",
            Self::Unavailable => "unavailable",
        }
    }
}

pub struct Store {
    service: String,
    /// Where sealed files go; set by the app before the first session is read or written.
    sealed_dir: Mutex<Option<PathBuf>>,
    backend: OnceLock<Backend>,
}

impl Store {
    pub fn new(service: String) -> Self {
        Self { service, sealed_dir: Mutex::new(None), backend: OnceLock::new() }
    }

    /// Tells the store where sealed files may live. Ignored once the backend has been decided.
    pub fn seal_in(&self, dir: PathBuf) {
        if self.backend.get().is_none() {
            *self.sealed_dir.lock() = Some(dir);
        }
    }

    pub fn backend(&self) -> Backend {
        *self.backend.get_or_init(|| self.detect())
    }

    fn detect(&self) -> Backend {
        // `store_status` initialises the platform store once and reports whether it came up.
        if keyring::Entry::store_status().is_ok() && keyring::Entry::new(&self.service, "backend-probe").is_ok() {
            return Backend::Keyring;
        }
        if self.sealed_dir.lock().is_some() && sealed::tool().is_some() {
            return Backend::Sealed;
        }
        Backend::Unavailable
    }

    pub fn load(&self, provider: &str) -> Result<Option<String>, OmniError> {
        match self.backend() {
            Backend::Keyring => match self.entry(provider)?.get_password() {
                Ok(value) => Ok(Some(value)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(err) => Err(err.into()),
            },
            Backend::Sealed => self.load_sealed(provider),
            Backend::Unavailable => Ok(None),
        }
    }

    pub fn save(&self, provider: &str, value: &str) -> Result<(), OmniError> {
        match self.backend() {
            Backend::Keyring => {
                self.entry(provider)?.set_password(value)?;
                // A sealed copy from an earlier run would be a second copy of the same secret.
                self.delete_sealed(provider);
                tracing::info!(provider, backend = "keyring", "session kept");
                Ok(())
            }
            Backend::Sealed => self.save_sealed(provider, value),
            Backend::Unavailable => Err(OmniError::Session("no keyring and no systemd-creds".to_owned())),
        }
    }

    pub fn delete(&self, provider: &str) -> Result<(), OmniError> {
        let keyring = match self.backend() {
            Backend::Keyring => match self.entry(provider)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(OmniError::from(err)),
            },
            Backend::Sealed | Backend::Unavailable => Ok(()),
        };
        self.delete_sealed(provider);
        keyring
    }

    fn entry(&self, provider: &str) -> Result<keyring::Entry, OmniError> {
        Ok(keyring::Entry::new(&self.service, &format!("{provider}-session"))?)
    }

    fn sealed_path(&self, provider: &str) -> Option<PathBuf> {
        self.sealed_dir.lock().as_ref().map(|dir| dir.join(format!("{provider}.session")))
    }

    /// The `--name` a sealed file is bound to; it is also the value checked when decrypting.
    fn sealed_name(&self, provider: &str) -> String {
        format!("{}-{provider}-session", self.service)
    }

    fn load_sealed(&self, provider: &str) -> Result<Option<String>, OmniError> {
        let (Some(tool), Some(path)) = (sealed::tool(), self.sealed_path(provider)) else { return Ok(None) };
        match sealed::read(tool, &self.sealed_name(provider), &path) {
            // A file from another host, or one that lost its key, is the same as no session at all.
            Ok(value) => Ok(value),
            Err(err) => {
                tracing::warn!(provider, error = %err, "sealed session unreadable");
                Ok(None)
            }
        }
    }

    fn save_sealed(&self, provider: &str, value: &str) -> Result<(), OmniError> {
        let (Some(tool), Some(path)) = (sealed::tool(), self.sealed_path(provider)) else {
            return Err(OmniError::Session("sealed sessions have no directory".to_owned()));
        };
        sealed::write(tool, &self.sealed_name(provider), &path, value)
            .map_err(|err| OmniError::Session(err.to_string()))?;
        tracing::info!(provider, backend = "sealed-file", "session kept");
        Ok(())
    }

    fn delete_sealed(&self, provider: &str) {
        if let Some(path) = self.sealed_path(provider) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => tracing::warn!(provider, error = %err, "sealed session not removed"),
            }
        }
    }
}

/// Sealed files: `systemd-creds --user` encrypts against the host key, so the secret is never on
/// disk in the clear and a copied file is useless elsewhere. Only this user's processes can open it,
/// which is exactly as strong as the keyring on a single-user machine and weaker than it on a shared one.
#[cfg(unix)]
mod sealed {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    pub fn tool() -> Option<&'static str> {
        static TOOL: OnceLock<Option<&'static str>> = OnceLock::new();
        *TOOL.get_or_init(|| {
            let usable = Command::new("systemd-creds")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            usable.then_some("systemd-creds")
        })
    }

    pub fn write(tool: &str, name: &str, path: &Path, value: &str) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut child = Command::new(tool)
            .args(["encrypt", "--user", "--name"])
            .arg(name)
            .arg("-")
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(value.as_bytes())?;
        }
        let finished = child.wait_with_output()?;
        if !finished.status.success() {
            let detail = String::from_utf8_lossy(&finished.stderr).trim().to_owned();
            return Err(std::io::Error::other(format!("{tool} encrypt: {detail}")));
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }

    pub fn read(tool: &str, name: &str, path: &Path) -> std::io::Result<Option<String>> {
        if !path.exists() {
            return Ok(None);
        }
        let finished = Command::new(tool)
            .args(["decrypt", "--user", "--name"])
            .arg(name)
            .arg(path)
            .arg("-")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !finished.status.success() {
            let detail = String::from_utf8_lossy(&finished.stderr).trim().to_owned();
            return Err(std::io::Error::other(format!("{tool} decrypt: {detail}")));
        }
        Ok(Some(String::from_utf8_lossy(&finished.stdout).into_owned()))
    }
}

#[cfg(not(unix))]
mod sealed {
    use std::path::Path;

    pub fn tool() -> Option<&'static str> {
        None
    }

    pub fn write(_tool: &str, _name: &str, _path: &Path, _value: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("sealed sessions need systemd-creds"))
    }

    pub fn read(_tool: &str, _name: &str, _path: &Path) -> std::io::Result<Option<String>> {
        Ok(None)
    }
}
