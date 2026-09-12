//! Adversarial tests for the SQLite local store and its anchor invariants.
//!
//! These attack the seams the design docs call out (`docs/local-store-plan.md`,
//! `docs/object-envelopes.md` "Client verification and rollback limits"):
//! revision-anchor behaviour under hostile or corrupt inputs, transaction
//! atomicity, concurrent writers, hydration robustness, and sweep semantics.
//! Every test asserts the documented contract; where the store diverges the
//! test fails and the divergence is the finding.

use std::sync::Arc;

use clipper_core::models::{
    ClipboardMeta, OBJECT_ENVELOPE_SIGNATURE_BYTES, ObjectEnvelopeBody, ObjectEnvelopeOperation,
    ObjectEnvelopePayload,
};
use rusqlite::params;

use super::sqlite;
use super::*;
use crate::api_client::{encrypt_clipboard_meta, encrypt_clipboard_payload};

const TEST_KEY: [u8; 32] = [7; 32];
const TEST_DEVICE_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn item(id: &str, text: &str, created_at: &str) -> DecryptedClipboardItem {
    DecryptedClipboardItem {
        id: id.into(),
        text: text.into(),
        mime_type: "text/plain".into(),
        payload_size: text.len() as i64,
        created_at: created_at.into(),
        source_device_id: TEST_DEVICE_ID.into(),
    }
}

fn encrypted_clipboard_at(
    item: &DecryptedClipboardItem,
    payload: &[u8],
    revision: u64,
    parent_hash: Option<[u8; crypto::SHA256_BYTES]>,
    operation: ObjectEnvelopeOperation,
) -> EncryptedInlineObject {
    let object_id = item.id.parse().expect("object id");
    let payload_id = uuid::Uuid::now_v7().into();
    let source_device_id = item.source_device_id.parse().expect("device id");
    let aad_body = ObjectEnvelopeBody {
        object_id,
        object_type: ObjectKind::Clipboard,
        envelope_version: crypto::OBJECT_ENVELOPE_VERSION,
        revision,
        parent_hash,
        source_device_id,
        created_at: item.created_at.clone(),
        operation,
        meta_nonce: Vec::new(),
        sha256_meta_ciphertext: Vec::new(),
        payloads: vec![ObjectEnvelopePayload {
            id: payload_id,
            nonce: Vec::new(),
            ciphertext_size: 0,
            sha256_ciphertext: Vec::new(),
        }],
    };
    let meta = ClipboardMeta {
        mime_type: item.mime_type.clone(),
        size: Some(payload.len() as i64),
    };
    let (meta_nonce, meta_ciphertext) =
        encrypt_clipboard_meta(&meta, &TEST_KEY, &aad_body).expect("meta encrypt");
    let (payload_nonce, payload_ciphertext) =
        encrypt_clipboard_payload(payload, &TEST_KEY, &aad_body, payload_id).expect("payload encrypt");
    let envelope_payload = ObjectEnvelopePayload {
        id: payload_id,
        nonce: payload_nonce.clone(),
        ciphertext_size: payload_ciphertext.len() as i64,
        sha256_ciphertext: crypto::sha256(&payload_ciphertext).to_vec(),
    };
    let envelope_body = ObjectEnvelopeBody {
        meta_nonce: meta_nonce.clone(),
        sha256_meta_ciphertext: crypto::sha256(&meta_ciphertext).to_vec(),
        payloads: vec![envelope_payload.clone()],
        ..aad_body
    };
    EncryptedInlineObject {
        object: EncryptedObject {
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadDescriptor {
                id: payload_id,
                nonce: payload_nonce,
                ciphertext_size: payload_ciphertext.len() as i64,
                sha256_ciphertext: crypto::sha256(&payload_ciphertext).to_vec(),
            }],
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.clone(),
            envelope: ObjectEnvelope {
                body: envelope_body,
                signature: vec![0; OBJECT_ENVELOPE_SIGNATURE_BYTES],
            },
        },
        payload_ciphertext,
    }
}

fn head_of(encrypted: &EncryptedInlineObject) -> LocalHead {
    LocalHead {
        revision: encrypted.object.envelope.body.revision,
        parent_hash: crypto::object_envelope_parent_hash(&encrypted.object.envelope.body)
            .expect("parent hash"),
    }
}

fn successor(
    item: &DecryptedClipboardItem,
    previous: &EncryptedInlineObject,
    text: &str,
) -> EncryptedInlineObject {
    let revision = previous.object.envelope.body.revision + 1;
    encrypted_clipboard_at(
        item,
        text.as_bytes(),
        revision,
        Some(head_of(previous).parent_hash),
        ObjectEnvelopeOperation::Revise,
    )
}

fn new_store(tmp: &tempfile::TempDir) -> LocalStore {
    let store = LocalStore::new(tmp.path());
    store.set_profile("profile-a".into());
    store
}

fn revision_error_text(error: &LocalStoreError) -> String {
    error.to_string()
}

// ── Anchor invariants ──

#[tokio::test]
async fn served_revision_older_than_accepted_anchor_is_rejected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "11111111-1111-4111-8111-111111111111",
        "two",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let rev2 = successor(&entry, &rev1, "two");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 2, 2, 10)
        .await
        .expect("persist revision two");

    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 3, 3, 10)
        .await
        .expect_err("revision 1 must be rejected once revision 2 is the anchor");
    assert!(
        revision_error_text(&error).contains("rolls back"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn different_body_at_the_accepted_revision_is_rejected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "22222222-2222-4222-8222-222222222222",
        "body A",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let rev2_a = successor(&entry, &rev1, "body A");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"body A", &rev2_a, 2, 2, 10)
        .await
        .expect("persist revision two body A");

    // Same revision number, same parent, different content.
    let rev2_b = encrypted_clipboard_at(
        &entry,
        b"body B",
        2,
        Some(head_of(&rev1).parent_hash),
        ObjectEnvelopeOperation::Revise,
    );
    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"body B", &rev2_b, 3, 3, 10)
        .await
        .expect_err("a different body at the accepted revision must be rejected");
    assert!(
        revision_error_text(&error).contains("changes the already accepted revision"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn successor_with_wrong_parent_hash_is_rejected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "33333333-3333-4333-8333-333333333333",
        "one",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect("persist revision one");

    let bad = encrypted_clipboard_at(
        &entry,
        b"two",
        2,
        Some([7; crypto::SHA256_BYTES]),
        ObjectEnvelopeOperation::Revise,
    );
    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &bad, 2, 2, 10)
        .await
        .expect_err("a successor naming the wrong parent must be rejected");
    assert!(
        revision_error_text(&error).contains("does not chain"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn skipped_link_is_accepted_as_documented() {
    // The client never saw revision 2, so revision 3 cannot be parent-checked.
    // docs/object-envelopes.md: the local anchor prevents rollback of history
    // this device accepted; it cannot prove the server showed every revision.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "44444444-4444-4444-8444-444444444444",
        "three",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect("persist revision one");

    // A revision-3 body naming a parent this device never held, chaining over
    // a gap the local store cannot verify.
    let rev2_never_held = successor(&entry, &rev1, "never held");
    let rev3 = encrypted_clipboard_at(
        &entry,
        b"three",
        3,
        Some(head_of(&rev2_never_held).parent_hash),
        ObjectEnvelopeOperation::Revise,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"three", &rev3, 2, 2, 10)
        .await
        .expect("a discontinuous jump forward is accepted by design");
    assert_eq!(store.local_head(&entry.id).await.expect("head"), Some(head_of(&rev3)));
}

#[tokio::test]
async fn event_stream_delete_requires_two_newer_revisions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let generation = store.start_generation().await;
    let entry = item(
        "55555555-5555-4555-8555-555555555555",
        "two",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let rev2 = successor(&entry, &rev1, "two");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 2, 2, 10)
        .await
        .expect("persist revision two");

    // A delete learned only from the event stream: no tombstone body, so the
    // retained head is the last visible revision and any revival must be at
    // least two steps newer.
    store
        .apply_live_delete(ObjectKind::Clipboard, &entry.id, 3, generation, 10)
        .await
        .expect("live delete")
        .expect("current generation");

    let rev3 = successor(&entry, &rev2, "three");
    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"three", &rev3, 4, 4, 10)
        .await
        .expect_err("head+1 must not revive an observed-delete object");
    assert!(
        revision_error_text(&error).contains("retained delete marker"),
        "unexpected error: {error}"
    );

    // Even a body that correctly chains to the retained head is not enough.
    let rev4 = successor(&entry, &rev3, "four");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"four", &rev4, 5, 5, 10)
        .await
        .expect("head+2 is the minimum for an observed-delete revival");
    assert_eq!(store.local_head(&entry.id).await.expect("head"), Some(head_of(&rev4)));
}

#[tokio::test]
async fn not_found_absence_keeps_the_anchor_and_allows_the_same_head() {
    // The 404 path (`remove_absent_object`) rather than the sweep path.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let generation = store.start_generation().await;
    let entry = item(
        "66666666-6666-4666-8666-666666666666",
        "two",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let rev2 = successor(&entry, &rev1, "two");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 2, 2, 10)
        .await
        .expect("persist revision two");

    store
        .remove_absent_object(&entry.id, generation, 10)
        .await
        .expect("mark absent")
        .expect("current generation");

    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 3, 3, 10)
        .await
        .expect_err("an older revision must stay forbidden after absence");
    assert!(
        revision_error_text(&error).contains("rolls back"),
        "unexpected error: {error}"
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 4, 4, 10)
        .await
        .expect("the same accepted head may reappear after a 404");
}

// ── Anchor survival ──

#[tokio::test]
async fn signed_tombstone_chains_the_restore_and_rejects_wrong_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "77777777-7777-4777-8777-777777777777",
        "one",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect("persist revision one");

    let tombstone = LocalHead {
        revision: 2,
        parent_hash: [42; crypto::SHA256_BYTES],
    };
    store
        .apply_local_tombstone(ObjectKind::Clipboard, &entry.id, 2, tombstone, 10)
        .await
        .expect("tombstone");
    assert_eq!(
        store.local_head(&entry.id).await.expect("head"),
        Some(tombstone),
        "a signed tombstone is the exact durable head"
    );

    let wrong_parent = encrypted_clipboard_at(
        &entry,
        b"restored",
        3,
        Some([9; crypto::SHA256_BYTES]),
        ObjectEnvelopeOperation::Revise,
    );
    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"restored", &wrong_parent, 3, 3, 10)
        .await
        .expect_err("a restore must chain to the tombstone head");
    assert!(
        revision_error_text(&error).contains("does not chain"),
        "unexpected error: {error}"
    );

    let restore = encrypted_clipboard_at(
        &entry,
        b"restored",
        3,
        Some(tombstone.parent_hash),
        ObjectEnvelopeOperation::Revise,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"restored", &restore, 3, 3, 10)
        .await
        .expect("the restore naming the tombstone head is accepted");
}

#[tokio::test]
async fn undecryptable_payload_bytes_keep_the_anchor() {
    // A payload row whose bytes no longer match the signed descriptor: not a
    // missing row (covered elsewhere) but corrupt in place.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "88888888-8888-4888-8888-888888888888",
        "two",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let rev2 = successor(&entry, &rev1, "two");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 2, 2, 10)
        .await
        .expect("persist revision two");

    store
        .with_database(|connection| {
            connection.execute(
                "UPDATE object_payloads SET ciphertext = x'0001020304' WHERE object_id = ?1",
                params![&entry.id],
            )?;
            Ok(())
        })
        .await
        .expect("corrupt the payload row");

    let restarted = new_store(&tmp);
    let visible = restarted
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("hydrate with a corrupt payload row");
    assert!(
        visible.clipboard_items.is_empty(),
        "content that cannot be verified must not be displayed"
    );

    let error = restarted
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 3, 3, 10)
        .await
        .expect_err("a corrupt cache entry must not forfeit the anchor");
    assert!(
        revision_error_text(&error).contains("rolls back"),
        "unexpected error: {error}"
    );
    restarted
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 4, 4, 10)
        .await
        .expect("the refetched head is the one the anchor already accepted");
}

#[tokio::test]
async fn cache_row_eviction_keeps_the_anchor() {
    // Delete the cache row (not the whole table): the anchor must remain and
    // the deleted object's ordering guard must still work.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "99999999-9999-4999-8999-999999999999",
        "one",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect("persist");
    let head = head_of(&rev1);

    store
        .with_database(|connection| {
            connection.execute("DELETE FROM objects WHERE id = ?1", params![&entry.id])?;
            Ok(())
        })
        .await
        .expect("evict the cache row");

    let restarted = new_store(&tmp);
    // `local_head` intentionally surfaces only signed tombstones; the retained
    // Absent anchor shows up as the object's marker record instead.
    let marker = restarted
        .stored_object_record(&entry.id)
        .await
        .expect("marker after eviction")
        .expect("a marker after eviction");
    match &marker {
        StoredObjectRecord::Deleted(marker) => {
            let anchor = marker.revision_anchor.expect("the anchor must survive");
            assert_eq!(anchor.head, head, "the anchor must record the accepted head");
        }
        other => panic!("expected a deleted marker, got {other:?}"),
    }
    // A different body at the accepted revision proves the anchor survived:
    // it must be rejected, where a forgotten anchor would accept anything.
    let different_body = encrypted_clipboard_at(
        &entry,
        b"forgery",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let error = restarted
        .persist_local_clipboard_present_encrypted(&entry, b"forgery", &different_body, 2, 2, 10)
        .await
        .expect_err("a different body at the accepted revision must stay forbidden");
    assert!(
        revision_error_text(&error).contains("changes the already accepted revision"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn uncommitted_transaction_leaves_no_trace_and_prior_data_survives() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "aaaaaaaa-1111-4111-8111-111111111111",
        "committed",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"committed",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"committed", &rev1, 1, 1, 10)
        .await
        .expect("persist");

    // Simulated crash mid-write: a transaction that never commits.
    store
        .with_database(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO objects (id, kind, pending, seen_generation, event_seq, created_seq, content)
                 VALUES ('aaaaaaaa-2222-4222-8222-222222222222', 'clipboard', 1, NULL, 9, 9, NULL)",
                [],
            )?;
            drop(transaction);
            Ok(())
        })
        .await
        .expect("crash simulation");

    let ghost = store
        .stored_object_record("aaaaaaaa-2222-4222-8222-222222222222")
        .await
        .expect("read after crash");
    assert!(
        ghost.is_none(),
        "an uncommitted insert must not survive a dropped connection"
    );
    let visible = store
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("hydrate after crash");
    assert_eq!(visible.clipboard_items.len(), 1);
    assert_eq!(visible.clipboard_items[0].text, "committed");

    // The committed row must also survive a full close/reopen of the database.
    drop(store);
    let reopened = new_store(&tmp);
    let visible = reopened
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("hydrate after reopen");
    assert_eq!(visible.clipboard_items.len(), 1);
}

// ── Transactions and concurrent writers ──

#[tokio::test]
async fn payload_write_rejected_for_a_marker_leaves_no_partial_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let entry = item(
        "bbbbbbbb-1111-4111-8111-111111111111",
        "one",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect("persist");
    store
        .apply_local_delete(ObjectKind::Clipboard, &entry.id, 2, 10)
        .await
        .expect("delete");

    // The storage layer refuses a payload on a non-present record. Whatever it
    // refuses must leave no partial state behind.
    let marker = store.stored_object_record(&entry.id).await.expect("marker read");
    let Some(StoredObjectRecord::Deleted(marker)) = marker else {
        panic!("expected a deleted marker");
    };
    let error = store
        .with_database(|connection| {
            sqlite::write_record(connection, &StoredObjectRecord::Deleted(marker), Some(b"nope"))
        })
        .await
        .expect_err("a payload on a deleted marker must be refused");
    assert!(
        error.to_string().contains("held object"),
        "unexpected error: {error}"
    );

    let restarted = new_store(&tmp);
    assert!(
        matches!(
            restarted.stored_object_record(&entry.id).await.expect("record"),
            Some(StoredObjectRecord::Deleted(_))
        ),
        "the refused write must not have disturbed the marker"
    );
    assert!(
        restarted
            .stored_object_payload_ciphertext(&entry.id)
            .await
            .expect("payload lookup")
            .is_none(),
        "the refused write must not have left a payload row"
    );
}

#[tokio::test]
async fn concurrent_writers_to_one_object_never_leave_torn_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(new_store(&tmp));
    let entry = item(
        "bbbbbbbb-2222-4222-8222-222222222222",
        "rev 1",
        "2026-01-01T00:00:00+00:00",
    );

    // Eight properly chained revisions, all racing to land.
    let mut revisions = Vec::new();
    let mut parent = None;
    for revision in 1..=8u64 {
        let text = format!("rev {revision}");
        let encrypted = encrypted_clipboard_at(
            &entry,
            text.as_bytes(),
            revision,
            parent,
            if revision == 1 {
                ObjectEnvelopeOperation::Create
            } else {
                ObjectEnvelopeOperation::Revise
            },
        );
        parent = Some(head_of(&encrypted).parent_hash);
        revisions.push((text, encrypted));
    }

    let mut handles = Vec::new();
    for (offset, (text, encrypted)) in revisions.into_iter().enumerate() {
        let store = Arc::clone(&store);
        let entry = entry.clone();
        let bytes = text.clone().into_bytes();
        handles.push(tokio::spawn(async move {
            (
                text,
                store
                    .persist_local_clipboard_present_encrypted(
                        &entry,
                        &bytes,
                        &encrypted,
                        100 + offset as i64,
                        100 + offset as i64,
                        10,
                    )
                    .await,
            )
        }));
    }

    let mut succeeded = Vec::new();
    for handle in handles {
        let (text, result) = handle.await.expect("writer task");
        if let Ok(visible) = result {
            succeeded.push((text, visible));
        }
    }
    assert!(!succeeded.is_empty(), "at least the genesis write must land");

    // Whatever revision won, the record, its payload, and the memory copy must
    // all describe that same revision, never a torn mix.
    let head = store.local_head(&entry.id).await.expect("head").expect("a head");
    let record = store
        .stored_object_record(&entry.id)
        .await
        .expect("record")
        .expect("a record");
    let StoredObjectRecord::Present(present) = record else {
        panic!("expected a present record");
    };
    assert_eq!(
        local_head_from_present(&present).expect("stored head").revision,
        head.revision,
        "the stored record must agree with the anchor"
    );
    let payload = store
        .stored_object_payload_ciphertext(&entry.id)
        .await
        .expect("payload")
        .expect("a payload");
    let descriptor = single_payload(present_encrypted_object(&present).expect("encrypted"))
        .expect("one payload");
    assert_eq!(
        crypto::sha256(&payload).as_slice(),
        descriptor.sha256_ciphertext.as_slice(),
        "the cached payload must match the stored descriptor"
    );
    let plaintext = store
        .clipboard_payload(&entry.id, &TEST_KEY)
        .await
        .expect("decrypt")
        .expect("plaintext");
    // Rebuild visible state from the database to probe what a restart sees.
    let visible = store
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("visible state");
    assert_eq!(
        visible.clipboard_items.first().map(|c| c.text.as_str()),
        Some(String::from_utf8_lossy(&plaintext).as_ref()),
        "memory and database must describe the same revision"
    );
}

// ── Reconciliation-shaped inputs ──

#[tokio::test]
async fn repeated_snapshot_page_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let generation = store.start_generation().await;
    let first = item(
        "cccccccc-1111-4111-8111-111111111111",
        "first",
        "2026-01-01T00:00:00+00:00",
    );
    let second = item(
        "cccccccc-2222-4222-8222-222222222222",
        "second",
        "2026-01-02T00:00:00+00:00",
    );
    // Build each snapshot body once: a repeated page serves identical bytes.
    let encrypted = [
        (
            &first,
            encrypted_clipboard_at(
                &first,
                first.text.as_bytes(),
                1,
                None,
                ObjectEnvelopeOperation::Create,
            ),
        ),
        (
            &second,
            encrypted_clipboard_at(
                &second,
                second.text.as_bytes(),
                1,
                None,
                ObjectEnvelopeOperation::Create,
            ),
        ),
    ];
    for (entry, encrypted) in &encrypted {
        store
            .persist_snapshot_clipboard_present_encrypted(
                entry,
                entry.text.as_bytes(),
                encrypted,
                1,
                generation,
                10,
            )
            .await
            .expect("snapshot persist")
            .expect("current generation");
    }
    // The same page arriving again must not duplicate or disturb anything.
    for (entry, encrypted) in &encrypted {
        store
            .persist_snapshot_clipboard_present_encrypted(
                entry,
                entry.text.as_bytes(),
                encrypted,
                1,
                generation,
                10,
            )
            .await
            .expect("repeat snapshot persist")
            .expect("current generation");
    }
    let visible = store
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("visible state");
    assert_eq!(visible.clipboard_items.len(), 2);
}

#[tokio::test]
async fn snapshot_item_at_lower_revision_than_local_is_rejected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let generation = store.start_generation().await;
    let entry = item(
        "cccccccc-3333-4333-8333-333333333333",
        "two",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let rev2 = successor(&entry, &rev1, "two");
    store
        .persist_local_clipboard_present_encrypted(&entry, b"two", &rev2, 2, 2, 10)
        .await
        .expect("persist revision two locally");

    // A reconciliation snapshot that serves the older revision again. The item
    // is skipped rather than installed, and the pass is not failed for it: a
    // page built before the head advanced is an ordinary interleave.
    assert!(
        store
            .persist_snapshot_clipboard_present_encrypted(&entry, b"one", &rev1, 1, generation, 10)
            .await
            .expect("a stale page item must not fail the pass")
            .is_none(),
        "a snapshot below the local anchor must not be installed"
    );
    // A write this device makes itself still reports the refusal.
    let error = store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect_err("a local write below the anchor must be rejected");
    assert!(
        revision_error_text(&error).contains("rolls back"),
        "unexpected error: {error}"
    );

    // The newer local copy must be untouched.
    assert_eq!(store.local_head(&entry.id).await.expect("head"), Some(head_of(&rev2)));
}

// ── Hydration robustness ──

#[tokio::test]
async fn one_malformed_content_row_must_not_drop_the_other_objects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let good = item(
        "dddddddd-1111-4111-8111-111111111111",
        "good",
        "2026-01-01T00:00:00+00:00",
    );
    let bad = item(
        "dddddddd-2222-4222-8222-222222222222",
        "bad",
        "2026-01-02T00:00:00+00:00",
    );
    for entry in [&good, &bad] {
        let encrypted = encrypted_clipboard_at(
            entry,
            entry.text.as_bytes(),
            1,
            None,
            ObjectEnvelopeOperation::Create,
        );
        store
            .persist_local_clipboard_present_encrypted(
                entry,
                entry.text.as_bytes(),
                &encrypted,
                1,
                1,
                10,
            )
            .await
            .expect("persist");
    }

    // One row's content stops being parseable, from a stray write or a
    // truncated restore. Hydration must keep serving everything else.
    store
        .with_database(|connection| {
            connection.execute(
                "UPDATE objects SET content = ?2 WHERE id = ?1",
                params![&bad.id, vec![0x00u8, 0xff, 0x00, 0xff]],
            )?;
            Ok(())
        })
        .await
        .expect("corrupt one content row");

    let restarted = new_store(&tmp);
    let visible = restarted
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("hydration must survive one malformed row");
    assert_eq!(
        visible.clipboard_items.len(),
        1,
        "the healthy object must survive its neighbour's corruption"
    );
    assert_eq!(visible.clipboard_items[0].text, "good");
}

#[tokio::test]
async fn live_row_with_null_content_must_not_brick_refetch_or_sweep() {
    // `live_records` explicitly anticipates a held row with no content and
    // skips it. The same shape read through `read_record` is an error, and
    // every persist/sweep path reads through `read_record`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let other = item(
        "dddddddd-3333-4333-8333-333333333333",
        "other",
        "2026-01-01T00:00:00+00:00",
    );
    let broken = item(
        "dddddddd-4444-4444-8444-444444444444",
        "broken",
        "2026-01-02T00:00:00+00:00",
    );
    let broken_rev = encrypted_clipboard_at(
        &broken,
        broken.text.as_bytes(),
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    let other_rev = encrypted_clipboard_at(
        &other,
        other.text.as_bytes(),
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    // The refetch below has to bring back the body the anchor accepted; a
    // different body at the same revision is an equivocation and is refused.
    for (entry, encrypted) in [(&other, &other_rev), (&broken, &broken_rev)] {
        store
            .persist_local_clipboard_present_encrypted(
                entry,
                entry.text.as_bytes(),
                encrypted,
                1,
                1,
                10,
            )
            .await
            .expect("persist");
    }

    store
        .with_database(|connection| {
            connection.execute(
                "UPDATE objects SET content = NULL WHERE id = ?1",
                params![&broken.id],
            )?;
            Ok(())
        })
        .await
        .expect("null out one content row");

    let restarted = new_store(&tmp);
    let visible = restarted
        .hydrate_ciphertext_cache(&TEST_KEY, 10)
        .await
        .expect("hydrate");
    assert_eq!(visible.clipboard_items.len(), 1);
    assert_eq!(visible.clipboard_items[0].text, "other");

    // A sweep of the kind must still run.
    let generation = restarted.start_generation().await;
    restarted
        .sweep_kind(ObjectKind::Clipboard, generation, 100, 10)
        .await
        .expect("sweep");

    // The refetch of the broken object (same accepted head) must land.
    restarted
        .persist_local_clipboard_present_encrypted(
            &broken,
            broken.text.as_bytes(),
            &broken_rev,
            2,
            2,
            10,
        )
        .await
        .expect("refetching the broken object must replace its row");
}

#[tokio::test]
async fn corrupt_anchor_row_must_not_brick_every_later_event_for_that_object() {
    // The anchor table is the security ledger; a damaged row for one object
    // must downgrade that object's protection, not turn every subsequent read
    // of it into a hard error.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = new_store(&tmp);
    let generation = store.start_generation().await;
    let entry = item(
        "dddddddd-5555-4555-8555-555555555555",
        "one",
        "2026-01-01T00:00:00+00:00",
    );
    let rev1 = encrypted_clipboard_at(
        &entry,
        b"one",
        1,
        None,
        ObjectEnvelopeOperation::Create,
    );
    store
        .persist_local_clipboard_present_encrypted(&entry, b"one", &rev1, 1, 1, 10)
        .await
        .expect("persist");
    store
        .apply_live_delete(ObjectKind::Clipboard, &entry.id, 2, generation, 10)
        .await
        .expect("delete");

    store
        .with_database(|connection| {
            connection.execute(
                "UPDATE object_anchors SET parent_hash = x'00' WHERE object_id = ?1",
                params![&entry.id],
            )?;
            Ok(())
        })
        .await
        .expect("corrupt the anchor row");

    let restarted = new_store(&tmp);
    // A later create event for the same id arrives. This must not fail; the
    // worst acceptable outcome is re-fetching with reduced rollback protection.
    restarted
        .mark_pending_create(ObjectKind::Clipboard, &entry.id, 50, restarted.start_generation().await)
        .await
        .expect("a create event after anchor corruption must not error");
}
