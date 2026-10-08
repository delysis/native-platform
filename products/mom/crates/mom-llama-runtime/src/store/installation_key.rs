//! Installation-key arbitration. The OS insert is create-only; a concurrent
//! winner is read back, never overwritten by this process's candidate.
use anyhow::{Result, anyhow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyCreation {
    Created,
    AlreadyExists,
}

fn decode_key(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("Keychain key is not 32 bytes"))
}

fn load_or_create(
    existing_store: bool,
    mut read: impl FnMut() -> Result<Option<Vec<u8>>>,
    generate: impl FnOnce() -> Result<[u8; 32]>,
    create: impl FnOnce(&[u8; 32]) -> Result<KeyCreation>,
) -> Result<[u8; 32]> {
    if existing_store && let Some(key) = read()? {
        return decode_key(key);
    }
    // A fresh database needs only create-only insertion. An orphaned existing
    // credential or a concurrent creator is recovered on duplicate, never rotated.
    let candidate = generate()?;
    match create(&candidate)? {
        KeyCreation::Created => Ok(candidate),
        KeyCreation::AlreadyExists => decode_key(
            read()?.ok_or_else(|| anyhow!("Keychain key disappeared after concurrent creation"))?,
        ),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn load_or_create_macos_key(account: &str, existing_store: bool) -> Result<[u8; 32]> {
    use security_framework::os::macos::passwords::find_generic_password;
    const ITEM_NOT_FOUND: i32 = -25300;
    load_or_create(
        existing_store,
        || {
            trace_keychain_request("read", account, "begin");
            // Read through the same file-based Keychain API used for insertion.
            // This safe wrapper makes one SecKeychainFindGenericPassword call
            // and returns the password together with the item; no second read.
            let result = find_generic_password(None, super::KEYCHAIN_SERVICE, account);
            trace_keychain_request("read", account, "end");
            match result {
                Ok((key, _item)) => Ok(Some(key.to_owned())),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
                Err(error) => Err(error.into()),
            }
        },
        || {
            let mut key = [0; 32];
            getrandom::fill(&mut key)
                .map_err(|error| anyhow!("store key generation failed: {error}"))?;
            Ok(key)
        },
        |key| create_macos_key(account, key),
    )
}

#[cfg(target_os = "macos")]
fn create_macos_key(account: &str, key: &[u8; 32]) -> Result<KeyCreation> {
    use security_framework::os::macos::keychain::SecKeychain;
    const DUPLICATE_ITEM: i32 = -25299;
    // Preserve the default file-based keychain and service/account identity.
    // Create-only insertion must never update an existing encryption key.
    trace_keychain_request("create", account, "begin");
    let result =
        SecKeychain::default()?.add_generic_password(super::KEYCHAIN_SERVICE, account, key);
    trace_keychain_request("create", account, "end");
    match result {
        Ok(()) => Ok(KeyCreation::Created),
        Err(error) if error.code() == DUPLICATE_ITEM => Ok(KeyCreation::AlreadyExists),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
fn trace_keychain_request(operation: &str, account: &str, phase: &str) {
    if std::env::var("LLAMA_NATIVE_KIT_KEYCHAIN_TRACE").as_deref() == Ok("1") {
        // Only an opaque account prefix and request boundaries. Never key bytes,
        // passwords, returned data, or the user's filesystem path.
        eprintln!(
            "mom-keychain pid={} account={} operation={operation} phase={phase}",
            std::process::id(),
            account.get(..12).unwrap_or(account)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    #[test]
    fn fresh_store_creates_without_requesting_a_password_read() -> Result<()> {
        let key = load_or_create(
            false,
            || panic!("fresh creation must not request a credential read"),
            || Ok([9; 32]),
            |candidate| {
                assert_eq!(candidate, &[9; 32]);
                Ok(KeyCreation::Created)
            },
        )?;
        assert_eq!(key, [9; 32]);
        Ok(())
    }

    #[test]
    fn fresh_store_reuses_existing_key_without_overwriting_it() -> Result<()> {
        let reads = Cell::new(0);
        let key = load_or_create(
            false,
            || {
                reads.set(reads.get() + 1);
                Ok(Some(vec![7; 32]))
            },
            || Ok([9; 32]),
            |_| Ok(KeyCreation::AlreadyExists),
        )?;
        assert_eq!(key, [7; 32]);
        assert_eq!(reads.get(), 1);
        Ok(())
    }

    #[test]
    fn existing_key_never_generates_or_creates() -> Result<()> {
        let key = load_or_create(
            true,
            || Ok(Some(vec![7; 32])),
            || panic!("existing key must not generate"),
            |_| panic!("existing key must not be replaced"),
        )?;
        assert_eq!(key, [7; 32]);
        Ok(())
    }

    #[test]
    fn successful_creation_returns_the_published_candidate() -> Result<()> {
        let calls = Cell::new(0);
        let key = load_or_create(
            true,
            || Ok(None),
            || Ok([3; 32]),
            |candidate| {
                calls.set(calls.get() + 1);
                assert_eq!(candidate, &[3; 32]);
                Ok(KeyCreation::Created)
            },
        )?;
        assert_eq!(key, [3; 32]);
        assert_eq!(calls.get(), 1);
        Ok(())
    }

    #[test]
    fn concurrent_absence_returns_one_winning_key_to_both_creators() {
        let published = Mutex::new(None::<Vec<u8>>);
        let (observed, observations) = mpsc::channel();
        let (release_first, first_ready) = mpsc::channel();
        let (release_second, second_ready) = mpsc::channel();
        std::thread::scope(|scope| {
            let run = |candidate: [u8; 32], ready: mpsc::Receiver<()>| {
                let mut first_read = true;
                load_or_create(
                    true,
                    || {
                        let value = published.lock().expect("fixture key slot").clone();
                        if first_read {
                            first_read = false;
                            observed.send(()).expect("coordinator is live");
                            ready
                                .recv_timeout(Duration::from_secs(5))
                                .expect("both callers must observe absence before either creates");
                        }
                        Ok(value)
                    },
                    || Ok(candidate),
                    |key| {
                        let mut value = published.lock().expect("fixture key slot");
                        if value.is_some() {
                            Ok(KeyCreation::AlreadyExists)
                        } else {
                            *value = Some(key.to_vec());
                            Ok(KeyCreation::Created)
                        }
                    },
                )
                .expect("creator resolves the winning key")
            };
            let first = scope.spawn(move || run([1; 32], first_ready));
            let second = scope.spawn(move || run([2; 32], second_ready));
            for _ in 0..2 {
                observations
                    .recv_timeout(Duration::from_secs(5))
                    .expect("creator reached absent read");
            }
            release_first.send(()).expect("first caller is live");
            release_second.send(()).expect("second caller is live");
            let first = first.join().expect("first creator joined");
            let second = second.join().expect("second creator joined");
            assert_eq!(first, second, "the losing candidate must never escape");
            assert_eq!(
                published.lock().expect("key slot").as_deref(),
                Some(first.as_slice())
            );
        });
    }

    #[test]
    fn duplicate_with_missing_winner_does_not_retry_creation() {
        let calls = Cell::new(0);
        let error = load_or_create(
            true,
            || Ok(None),
            || Ok([3; 32]),
            |_| {
                calls.set(calls.get() + 1);
                Ok(KeyCreation::AlreadyExists)
            },
        )
        .expect_err("missing winner fails closed");
        assert_eq!(
            error.to_string(),
            "Keychain key disappeared after concurrent creation"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn invalid_existing_and_winning_keys_are_rejected_without_replacement() {
        for length in [0, 31, 33] {
            let error = load_or_create(
                true,
                || Ok(Some(vec![7; length])),
                || panic!("invalid existing key must not rotate"),
                |_| panic!("invalid existing key must not be replaced"),
            )
            .expect_err("invalid existing key");
            assert_eq!(error.to_string(), "Keychain key is not 32 bytes");

            let mut reads = 0;
            let error = load_or_create(
                true,
                || {
                    reads += 1;
                    Ok((reads > 1).then(|| vec![7; length]))
                },
                || Ok([3; 32]),
                |_| Ok(KeyCreation::AlreadyExists),
            )
            .expect_err("invalid winning key");
            assert_eq!(error.to_string(), "Keychain key is not 32 bytes");
            assert_eq!(reads, 2);
        }
    }

    #[test]
    fn read_generation_creation_and_winner_errors_are_preserved() {
        let read = load_or_create(
            true,
            || Err(anyhow!("read denied")),
            || panic!("denial must not generate"),
            |_| panic!("denial must not create"),
        )
        .expect_err("read denial");
        assert_eq!(read.to_string(), "read denied");
        let random = load_or_create(
            true,
            || Ok(None),
            || Err(anyhow!("randomness unavailable")),
            |_| panic!("failed generation must not create"),
        )
        .expect_err("generation failure");
        assert_eq!(random.to_string(), "randomness unavailable");
        let calls = Cell::new(0);
        let create = load_or_create(
            true,
            || {
                calls.set(calls.get() + 1);
                Ok(None)
            },
            || Ok([3; 32]),
            |_| Err(anyhow!("create denied")),
        )
        .expect_err("create denial");
        assert_eq!(create.to_string(), "create denied");
        assert_eq!(calls.get(), 1, "only a duplicate permits winner lookup");
        let mut reads = 0;
        let winner = load_or_create(
            true,
            || {
                reads += 1;
                if reads == 1 {
                    Ok(None)
                } else {
                    Err(anyhow!("winner read denied"))
                }
            },
            || Ok([3; 32]),
            |_| Ok(KeyCreation::AlreadyExists),
        )
        .expect_err("winner read denial");
        assert_eq!(winner.to_string(), "winner read denied");
        assert_eq!(reads, 2);
    }
}
