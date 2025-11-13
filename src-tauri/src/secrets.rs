#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
#[cfg(test)]
use parking_lot::Mutex;
use rand::rngs::OsRng;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, info, warn};

use crate::errors::{AppError, AppResult};

const KEY_LENGTH: usize = 64;

#[derive(Clone)]
pub struct SecretVault {
    service_name: String,
    backend: SecretBackend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretLifecycle {
    Retrieved,
    Created,
    Rotated,
}

impl SecretLifecycle {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretLifecycle::Retrieved => "retrieved",
            SecretLifecycle::Created => "created",
            SecretLifecycle::Rotated => "rotated",
        }
    }
}

#[derive(Clone)]
pub struct SecretMaterial {
    secret: SecretString,
    lifecycle: SecretLifecycle,
}

impl SecretMaterial {
    fn new(secret: SecretString, lifecycle: SecretLifecycle) -> Self {
        Self { secret, lifecycle }
    }

    pub fn secret(&self) -> &SecretString {
        &self.secret
    }

    #[allow(dead_code)]
    pub fn into_secret(self) -> SecretString {
        self.secret
    }

    pub fn lifecycle(&self) -> SecretLifecycle {
        self.lifecycle
    }
}

#[derive(Clone)]
enum SecretBackend {
    Keyring,
    #[cfg(test)]
    Memory(Arc<Mutex<HashMap<String, SecretString>>>),
}

impl SecretVault {
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            backend: SecretBackend::Keyring,
        }
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            service_name: "in-memory".to_string(),
            backend: SecretBackend::Memory(Arc::new(Mutex::new(HashMap::new()))),
        }
    }

    pub fn ensure(&self, account: &str) -> AppResult<SecretMaterial> {
        if let Some(secret) = self.try_get(account)? {
            debug!(
                target: "secret_vault",
                service = %self.service_name,
                account,
                "loaded secret from secure backend"
            );
            return Ok(SecretMaterial::new(secret, SecretLifecycle::Retrieved));
        }
        let secret = self.generate_secret();
        self.store(account, &secret)?;
        info!(
            target: "secret_vault",
            service = %self.service_name,
            account,
            "created new secret in secure backend"
        );
        Ok(SecretMaterial::new(secret, SecretLifecycle::Created))
    }

    pub fn rotate(&self, account: &str) -> AppResult<SecretMaterial> {
        let secret = self.generate_secret();
        self.store(account, &secret)?;
        warn!(
            target: "secret_vault",
            service = %self.service_name,
            account,
            "rotated secret material"
        );
        Ok(SecretMaterial::new(secret, SecretLifecycle::Rotated))
    }

    #[allow(dead_code)]
    pub fn delete(&self, account: &str) -> AppResult<()> {
        match &self.backend {
            SecretBackend::Keyring => {
                let entry = keyring::Entry::new(&self.service_name, account)?;
                match entry.delete_password() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(err) => Err(AppError::from(err)),
                }
            }
            #[cfg(test)]
            SecretBackend::Memory(store) => {
                store.lock().remove(account);
                Ok(())
            }
        }
    }

    pub fn has(&self, account: &str) -> AppResult<bool> {
        self.try_get(account).map(|secret| secret.is_some())
    }

    fn try_get(&self, account: &str) -> AppResult<Option<SecretString>> {
        match &self.backend {
            SecretBackend::Keyring => {
                let entry = keyring::Entry::new(&self.service_name, account)?;
                match entry.get_password() {
                    Ok(value) => Ok(Some(SecretString::new(value.into()))),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(err) => Err(AppError::from(err)),
                }
            }
            #[cfg(test)]
            SecretBackend::Memory(store) => Ok(store.lock().get(account).cloned()),
        }
    }

    fn store(&self, account: &str, secret: &SecretString) -> AppResult<()> {
        match &self.backend {
            SecretBackend::Keyring => {
                let entry = keyring::Entry::new(&self.service_name, account)?;
                entry.set_password(secret.expose_secret())?;
                Ok(())
            }
            #[cfg(test)]
            SecretBackend::Memory(store) => {
                store.lock().insert(account.to_string(), secret.clone());
                Ok(())
            }
        }
    }

    fn generate_secret(&self) -> SecretString {
        let mut bytes = vec![0_u8; KEY_LENGTH];
        OsRng.fill_bytes(&mut bytes);
        let encoded = STANDARD_NO_PAD.encode(bytes);
        SecretString::new(encoded.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensures_secret_is_persisted() {
        let vault = SecretVault::in_memory();
        let first = vault.ensure("db-key").unwrap();
        let second = vault.ensure("db-key").unwrap();

        assert_eq!(
            first.secret().expose_secret(),
            second.secret().expose_secret()
        );
        assert!(vault.has("db-key").unwrap());
        assert_eq!(first.lifecycle(), SecretLifecycle::Created);
        assert_eq!(second.lifecycle(), SecretLifecycle::Retrieved);
    }

    #[test]
    fn rotate_replaces_existing_secret() {
        let vault = SecretVault::in_memory();
        let initial = vault.ensure("db-key").unwrap();
        let rotated = vault.rotate("db-key").unwrap();

        assert_ne!(
            initial.secret().expose_secret(),
            rotated.secret().expose_secret()
        );
        assert_eq!(rotated.lifecycle(), SecretLifecycle::Rotated);
    }
}
