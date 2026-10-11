use super::*;
use presage::{
    libsignal_service::{
        protocol::{Direction, IdentityKeyStore, PrivateKey},
        push_service::DEFAULT_DEVICE_ID,
    },
    store::{StateStore, Store},
};
use presage_store_sqlite::{OnNewIdentity, SqliteConnectOptions};

const LOCAL: &str = "02c43ab9-83fa-469c-a76a-25f3647affc1";
const ALICE: &str = "898b371e-15e8-4588-a5cb-d841f641933e";
const BOB: &str = "5398824b-1228-43c0-af6c-c42319e578a2";

fn key(seed: u8) -> IdentityKeyPair {
    let private = PrivateKey::deserialize(&[seed; 32]).unwrap();
    IdentityKeyPair::new(IdentityKey::new(private.public_key().unwrap()), private)
}

struct Fixture {
    directory: tempfile::TempDir,
    store: SqliteStore,
    database: SqlitePool,
    protocol_database: SqlitePool,
}

impl Fixture {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, database, protocol_database) = Self::connect(directory.path()).await;
        let registration: RegistrationData = serde_json::from_str(&serde_json::json!({
            "signal_servers": "Production", "device_name": "fixture", "phone_number": presage::libsignal_service::prelude::phonenumber::parse(None, "+14155550100").unwrap(),
            "uuid": LOCAL, "pni": "68e1ff55-cb1c-4e91-b9fe-68996b332761", "password": "fixture",
            "device_id": 2, "registration_id": 1, "pni_registration_id": 2,
            "profile_key": base64::engine::general_purpose::STANDARD.encode([1; 32]),
        }).to_string()).unwrap();
        store.save_registration_data(&registration).await.unwrap();
        store.set_aci_identity_key_pair(key(1)).await.unwrap();
        store.set_pni_identity_key_pair(key(2)).await.unwrap();
        let fixture = Self {
            directory,
            store,
            database,
            protocol_database,
        };
        fixture.trust(ALICE, 3).await;
        fixture.trust(BOB, 4).await;
        fixture
    }

    async fn connect(directory: &std::path::Path) -> (SqliteStore, SqlitePool, SqlitePool) {
        let options = SqliteConnectOptions::new()
            .filename(directory.join("signal.db"))
            .create_if_missing(true)
            .pragma("key", "'fixture-only'");
        let protocol_database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(options.clone())
            .await
            .unwrap();
        let store = SqliteStore::open_with_pool(protocol_database.clone(), OnNewIdentity::Reject)
            .await
            .unwrap();
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        initialize(&database).await.unwrap();
        (store, database, protocol_database)
    }

    async fn reopen(self) -> Self {
        let Self {
            directory,
            store,
            database,
            protocol_database,
        } = self;
        protocol_database.close().await;
        database.close().await;
        drop(store);
        let (store, database, protocol_database) = Self::connect(directory.path()).await;
        Self {
            directory,
            store,
            database,
            protocol_database,
        }
    }

    async fn close(self) {
        self.protocol_database.close().await;
        self.database.close().await;
    }

    async fn trust(&self, person: &str, seed: u8) {
        let aci = ServiceId::from(Aci::from(person.parse::<Uuid>().unwrap()));
        let address = aci.to_protocol_address(*DEFAULT_DEVICE_ID);
        for mut store in [
            self.store.aci_protocol_store(),
            self.store.pni_protocol_store(),
        ] {
            store
                .save_identity(&address, key(seed).identity_key())
                .await
                .unwrap();
        }
    }

    async fn trusts(&self, seed: u8) -> bool {
        let aci = ServiceId::from(Aci::from(ALICE.parse::<Uuid>().unwrap()));
        let address = aci.to_protocol_address(*DEFAULT_DEVICE_ID);
        for store in [
            self.store.aci_protocol_store(),
            self.store.pni_protocol_store(),
        ] {
            if !store
                .is_trusted_identity(&address, key(seed).identity_key(), Direction::Sending)
                .await
                .unwrap()
            {
                return false;
            }
        }
        true
    }

    async fn review(&self, seed: u8) -> IdentityReview {
        inspect_saved(
            &self.database,
            ALICE,
            Some(key(seed).identity_key().serialize().into_vec()),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn add_sessions(&self) {
        for person in [ALICE, BOB] {
            for namespace in ["aci", "pni"] {
                // Opaque fixtures are deliberately never decoded; this checks
                // retirement against Presage's actual installed SQL schema.
                sqlx::query("INSERT INTO sessions(address, device_id, identity, record) VALUES (?, 1, ?, X'01')")
                    .bind(person).bind(namespace).execute(&self.database).await.unwrap();
                sqlx::query("INSERT INTO sender_keys(address, device_id, identity, distribution_id, record) VALUES (?, 1, ?, ?, X'01')")
                    .bind(person).bind(namespace).bind(LOCAL).execute(&self.database).await.unwrap();
            }
        }
    }

    async fn session_count(&self, table: &str, person: &str) -> i64 {
        let query = match table {
            "sessions" => "SELECT count(*) FROM sessions WHERE address = ?",
            "sender_keys" => "SELECT count(*) FROM sender_keys WHERE address = ?",
            _ => panic!("Unexpected fixture table"),
        };
        sqlx::query_scalar(query)
            .bind(person)
            .fetch_one(&self.database)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_review_never_trusts_a_key_until_approved_and_applied_after_reopen() {
    let fixture = Fixture::open().await;
    fixture.add_sessions().await;
    let review = fixture.review(5).await;
    assert_eq!(review.state, IdentityState::Changed);
    assert!(fixture.trusts(3).await);
    assert!(!fixture.trusts(5).await);
    let pending = approve_saved(&fixture.database, ALICE, &review.review_id)
        .await
        .unwrap();
    assert_eq!(pending.state, IdentityState::Pending);
    assert!(
        fixture.trusts(3).await,
        "approval does not race the live receiver"
    );
    let fixture = fixture.reopen().await;
    assert_eq!(
        inspect_saved(&fixture.database, ALICE, None)
            .await
            .unwrap()
            .unwrap()
            .state,
        IdentityState::Pending
    );
    apply_approved(&fixture.database).await.unwrap();
    assert!(fixture.trusts(5).await);
    assert!(!fixture.trusts(3).await);
    for table in ["sessions", "sender_keys"] {
        assert_eq!(fixture.session_count(table, ALICE).await, 0);
        assert_eq!(fixture.session_count(table, BOB).await, 2);
    }
    let verified = inspect_saved(&fixture.database, ALICE, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(verified.state, IdentityState::Verified);
    assert_eq!(verified.safety_number, review.safety_number);
    assert_eq!(
        approve_saved(&fixture.database, ALICE, &review.review_id)
            .await
            .unwrap()
            .state,
        IdentityState::Verified
    );
    apply_approved(&fixture.database).await.unwrap();
    assert_eq!(fixture.review(6).await.state, IdentityState::Changed);
    assert!(
        approve_saved(&fixture.database, ALICE, &review.review_id)
            .await
            .is_err()
    );
    assert!(fixture.trusts(5).await);
    let retained_store = fixture.store.clone();
    fixture.close().await;
    assert!(
        retained_store.load_registration_data().await.is_err(),
        "an upstream store clone cannot keep SQLite work alive after shutdown"
    );
}

#[tokio::test]
async fn refresh_and_key_changes_invalidate_exact_reviews() {
    let fixture = Fixture::open().await;
    let first = fixture.review(5).await;
    let second = fixture.review(5).await;
    assert_ne!(first.review_id, second.review_id);
    assert_eq!(first.safety_number, second.safety_number);
    assert!(
        approve_saved(&fixture.database, ALICE, &first.review_id)
            .await
            .is_err()
    );
    fixture.trust(ALICE, 6).await;
    assert!(
        approve_saved(&fixture.database, ALICE, &second.review_id)
            .await
            .is_err()
    );
    let review = fixture.review(5).await;
    approve_saved(&fixture.database, ALICE, &review.review_id)
        .await
        .unwrap();
    fixture
        .store
        .set_aci_identity_key_pair(key(7))
        .await
        .unwrap();
    apply_approved(&fixture.database).await.unwrap();
    assert!(
        fixture.trusts(6).await,
        "a local-key change cannot extend an earlier approval"
    );
    let replacement = inspect_saved(&fixture.database, ALICE, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replacement.state, IdentityState::Unverified);
    assert_ne!(replacement.safety_number, review.safety_number);
    assert!(
        approve_saved(&fixture.database, ALICE, &review.review_id)
            .await
            .is_err()
    );
    fixture.close().await;
}

#[tokio::test]
async fn acceptance_rolls_back_the_identity_sessions_and_approval_together() {
    let fixture = Fixture::open().await;
    fixture.add_sessions().await;
    let review = fixture.review(5).await;
    approve_saved(&fixture.database, ALICE, &review.review_id)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_retirement BEFORE DELETE ON sender_keys BEGIN SELECT RAISE(ABORT, 'fixture disk failure'); END")
        .execute(&fixture.database).await.unwrap();
    assert!(apply_approved(&fixture.database).await.is_err());
    assert!(fixture.trusts(3).await);
    assert_eq!(fixture.session_count("sessions", ALICE).await, 2);
    assert_eq!(
        inspect_saved(&fixture.database, ALICE, None)
            .await
            .unwrap()
            .unwrap()
            .state,
        IdentityState::Pending
    );
    sqlx::query("DROP TRIGGER fail_retirement")
        .execute(&fixture.database)
        .await
        .unwrap();
    apply_approved(&fixture.database).await.unwrap();
    assert!(fixture.trusts(5).await);
    fixture.close().await;
}

#[tokio::test]
async fn counterpart_numbers_match_and_key_or_aci_changes_do_not() {
    let fixture = Fixture::open().await;
    let alice = fixture.review(3).await;
    let mut connection = fixture.database.acquire().await.unwrap();
    let mut opposite = load(&mut connection, ALICE).await.unwrap().unwrap();
    opposite.context.local_aci = ALICE.into();
    opposite.context.local_key = key(3).identity_key().serialize().into_vec();
    opposite.candidate = key(1).identity_key().serialize().into_vec();
    let counterpart = present(LOCAL, &opposite).unwrap();
    assert_eq!(alice.safety_number, counterpart.safety_number);
    assert_eq!(alice.safety_number.len(), 60);
    assert!(
        alice
            .safety_number
            .bytes()
            .all(|digit| digit.is_ascii_digit())
    );
    assert_ne!(
        alice.qr_code, counterpart.qr_code,
        "Signal QR codes retain directional keys"
    );
    assert_ne!(
        alice.safety_number,
        present(BOB, &opposite).unwrap().safety_number
    );
    let event = serde_json::to_value(alice).unwrap();
    assert!(event.get("local_key").is_none());
    assert!(event.get("candidate").is_none());
    drop(connection);
    fixture.close().await;
}

#[tokio::test]
async fn group_membership_scopes_reviews_and_changed_keys_block_a_new_send() {
    use presage::{
        libsignal_service::{groups_v2::Role, prelude::ProfileKey},
        model::groups::{Group, Member},
    };
    let fixture = Fixture::open().await;
    let group = Group {
        title: "Friends".into(),
        avatar: String::new(),
        disappearing_messages_timer: None,
        access_control: None,
        revision: 1,
        pending_members: vec![],
        requesting_members: vec![],
        invite_link_password: vec![],
        description: None,
        members: [ALICE, BOB]
            .into_iter()
            .map(|id| Member {
                aci: Aci::from(id.parse::<Uuid>().unwrap()),
                role: Role::Default,
                profile_key: ProfileKey::create([0; 32]),
                joined_at_revision: 1,
                label: None,
                label_emoji: None,
            })
            .collect(),
    };
    let group_key = [1; 32];
    let old_snapshot = serde_json::to_string(&group).unwrap();
    fixture.store.save_group(group_key, group).await.unwrap();
    let conversation = messages::id(&Thread::Group(group_key));
    let people = members(&fixture.store, &conversation).await.unwrap();
    assert_eq!(people.len(), 2);
    assert!(recipient(&people, Some(LOCAL)).is_err());
    assert!(recipient(&people, None).unwrap().is_none());
    ensure_send_allowed(&fixture.store, &fixture.database, &conversation)
        .await
        .unwrap();
    let review = fixture.review(5).await;
    assert!(
        ensure_send_allowed(&fixture.store, &fixture.database, &conversation)
            .await
            .is_err()
    );
    assert!(fixture.trusts(3).await);
    approve_saved(&fixture.database, ALICE, &review.review_id)
        .await
        .unwrap();
    assert!(
        ensure_send_allowed(&fixture.store, &fixture.database, &conversation)
            .await
            .is_err()
    );
    apply_approved(&fixture.database).await.unwrap();
    ensure_send_allowed(&fixture.store, &fixture.database, &conversation)
        .await
        .unwrap();
    let mut group = fixture.store.group(group_key).await.unwrap().unwrap();
    group
        .members
        .retain(|member| ServiceId::from(member.aci).service_id_string() == BOB);
    group.revision += 1;
    fixture.store.save_group(group_key, group).await.unwrap();
    // A slower, earlier server read must not restore a member after the
    // receiver has already saved a newer membership revision.
    let stale: Group = serde_json::from_str(&old_snapshot).unwrap();
    fixture.store.save_group(group_key, stale).await.unwrap();
    assert!(
        recipient(
            &members(&fixture.store, &conversation).await.unwrap(),
            Some(ALICE)
        )
        .is_err()
    );
    fixture.close().await;
}
