//! One credential per bounded slot: Windows limits each UTF-16 password blob
//! to 2560 bytes. Combining several accounts in one entry exceeds that limit.
use super::{IpcFailure, failure};
use information_native_acquire::google_import::{GoogleCredentials, GoogleService};

const ACCOUNT_SLOTS: usize = 8;

pub(super) trait CredentialSlots {
    fn read(&self, slot: usize) -> Result<Option<String>, IpcFailure>;
    fn write(&self, slot: usize, value: &str) -> Result<(), IpcFailure>;
    fn remove(&self, slot: usize) -> Result<(), IpcFailure>;
}

pub(super) struct SystemSlots<'a> {
    project_id: &'a str,
    service: GoogleService,
}

fn keyring_failure(error: &keyring::Error) -> IpcFailure {
    if matches!(error, keyring::Error::TooLong(..)) {
        failure("This account credential exceeds the system credential store's entry limit.")
    } else {
        failure(
            "The system credential store is unavailable. No credential was written to a project file.",
        )
    }
}

impl SystemSlots<'_> {
    fn entry(&self, slot: usize) -> Result<keyring::Entry, IpcFailure> {
        let service = match self.service {
            GoogleService::Gmail => "gmail",
            GoogleService::Drive => "drive",
        };
        keyring::Entry::new(
            "com.delysis.loom.connected-import",
            &format!("{}:{service}:{slot}", self.project_id),
        )
        .map_err(|error| keyring_failure(&error))
    }
}

impl CredentialSlots for SystemSlots<'_> {
    fn read(&self, slot: usize) -> Result<Option<String>, IpcFailure> {
        match self.entry(slot)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_failure(&error)),
        }
    }

    fn write(&self, slot: usize, value: &str) -> Result<(), IpcFailure> {
        self.entry(slot)?
            .set_password(value)
            .map_err(|error| keyring_failure(&error))
    }

    fn remove(&self, slot: usize) -> Result<(), IpcFailure> {
        match self.entry(slot)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_failure(&error)),
        }
    }
}

pub(super) struct AccountStore<S> {
    slots: S,
    service: GoogleService,
}

impl<'a> AccountStore<SystemSlots<'a>> {
    pub(super) fn new(project_id: &'a str, service: GoogleService) -> Self {
        Self {
            slots: SystemSlots {
                project_id,
                service,
            },
            service,
        }
    }
}

impl<S: CredentialSlots> AccountStore<S> {
    fn read(&self) -> Result<Vec<(usize, GoogleCredentials)>, IpcFailure> {
        let mut accounts = Vec::new();
        for slot in 0..ACCOUNT_SLOTS {
            let Some(value) = self.slots.read(slot)? else {
                continue;
            };
            let credential: GoogleCredentials = serde_json::from_str(&value)
                .map_err(|_| failure("The stored account credential is invalid."))?;
            if credential.service != self.service || credential.account_email.is_empty() {
                return Err(failure(
                    "The stored account scope or identity does not match this import.",
                ));
            }
            accounts.push((slot, credential));
        }
        Ok(accounts)
    }

    pub(super) fn list(&self) -> Result<Vec<GoogleCredentials>, IpcFailure> {
        Ok(self
            .read()?
            .into_iter()
            .map(|(_, account)| account)
            .collect())
    }

    pub(super) fn save(&self, credential: &GoogleCredentials) -> Result<(), IpcFailure> {
        if credential.service != self.service || credential.account_email.is_empty() {
            return Err(failure(
                "The account scope or identity does not match this import.",
            ));
        }
        let accounts = self.read()?;
        let slot = accounts.iter()
            .find(|(_, account)| account.account_email == credential.account_email)
            .map(|(slot, _)| *slot)
            .or_else(|| (0..ACCOUNT_SLOTS).find(|slot| !accounts.iter().any(|(used, _)| slot == used)))
            .ok_or_else(|| failure("Disconnect an account before adding another; this project supports eight accounts per service."))?;
        let encoded = serde_json::to_string(credential)
            .map_err(|_| failure("The account credential could not be encoded."))?;
        // Replace only this account. A failed write leaves other slots intact.
        self.slots.write(slot, &encoded)
    }

    pub(super) fn disconnect(&self, email: &str) -> Result<(), IpcFailure> {
        for (slot, account) in self.read()? {
            if account.account_email == email {
                self.slots.remove(slot)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct WindowsSizedSlots(RefCell<[Option<String>; ACCOUNT_SLOTS]>);

    impl CredentialSlots for WindowsSizedSlots {
        fn read(&self, slot: usize) -> Result<Option<String>, IpcFailure> {
            Ok(self.0.borrow()[slot].clone())
        }
        fn write(&self, slot: usize, value: &str) -> Result<(), IpcFailure> {
            if value.encode_utf16().count() * 2 > 2560 {
                return Err(failure("Windows credential size exceeded."));
            }
            self.0.borrow_mut()[slot] = Some(value.to_owned());
            Ok(())
        }
        fn remove(&self, slot: usize) -> Result<(), IpcFailure> {
            self.0.borrow_mut()[slot] = None;
            Ok(())
        }
    }

    fn credential(index: usize) -> GoogleCredentials {
        GoogleCredentials {
            client_id: "desktop.apps.googleusercontent.com".into(),
            client_secret: "synthetic-client-secret".into(),
            refresh_token: "x".repeat(512),
            account_email: format!("writer-{index}@example.test"),
            service: GoogleService::Gmail,
        }
    }

    #[test]
    fn eight_accounts_fit_individual_windows_entries_and_disconnect_is_isolated() {
        let accounts: Vec<_> = (0..ACCOUNT_SLOTS).map(credential).collect();
        assert!(
            serde_json::to_string(&accounts)
                .unwrap()
                .encode_utf16()
                .count()
                * 2
                > 2560
        );
        let store = AccountStore {
            slots: WindowsSizedSlots::default(),
            service: GoogleService::Gmail,
        };
        for account in &accounts {
            store.save(account).unwrap();
        }
        assert_eq!(store.list().unwrap().len(), ACCOUNT_SLOTS);
        assert!(store.save(&credential(8)).is_err());
        let mut refreshed = credential(3);
        refreshed.refresh_token = "replacement-token".into();
        store.save(&refreshed).unwrap();
        assert_eq!(store.list().unwrap()[3].refresh_token, "replacement-token");
        store.disconnect(&credential(2).account_email).unwrap();
        store.save(&credential(8)).unwrap();
        let accounts = store.list().unwrap();
        assert_eq!(accounts.len(), ACCOUNT_SLOTS);
        assert!(
            accounts
                .iter()
                .any(|account| account.account_email == "writer-8@example.test")
        );
        assert!(
            !accounts
                .iter()
                .any(|account| account.account_email == "writer-2@example.test")
        );
        assert_eq!(accounts[3].refresh_token, "replacement-token");
    }

    #[test]
    fn failed_replacement_and_wrong_service_preserve_existing_accounts() {
        let store = AccountStore {
            slots: WindowsSizedSlots::default(),
            service: GoogleService::Gmail,
        };
        store.save(&credential(0)).unwrap();
        let mut changed = credential(0);
        changed.refresh_token = "x".repeat(2560);
        assert!(store.save(&changed).is_err());
        changed = credential(0);
        changed.service = GoogleService::Drive;
        assert!(store.save(&changed).is_err());
        assert_eq!(
            store.list().unwrap()[0].refresh_token,
            credential(0).refresh_token
        );
    }
}
