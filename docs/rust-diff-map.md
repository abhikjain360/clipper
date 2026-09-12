# Rust diff map for the scheduler PR

Generated from `git diff main...HEAD` on 2026-09-12. For each changed Rust file: the functions whose bodies contain added or removed lines, with the function's current line number and the number of changed lines inside it (added plus removed). New files list their functions. Test code is listed separately. Use it with [scheduler-rust-review-guide.md](scheduler-rust-review-guide.md) to read only the changed functions of the three large existing files. Regenerate it after any commit that moves code.

## crates/api-types/src/lib.rs

+333 −14 lines; 6 non-test functions touched, 7 test functions touched.

- (top level) (195 changed lines)
- `default_message` (line 841, 3 changed lines)
- `http_status` (line 907, 3 changed lines)
- `validate_revision_link` (line 1027, 19 changed lines)
- `validate_payload_count_for_operation` (line 1052, 15 changed lines)
- `validate_unique_envelope_payload_ids` (line 1068, 2 changed lines)
- tests touched: `payload`, `body`, `the_three_legal_shapes_validate`, `a_genesis_revision_may_not_claim_a_parent`, `a_later_revision_must_carry_one`, `create_and_revision_number_must_agree`, `only_a_tombstone_may_be_empty`

## crates/app-types/src/lib.rs

+173 −9 lines; 1 non-test functions touched, 0 test functions touched.

- (top level) (182 changed lines)

## crates/client/src/api_client.rs

+242 −69 lines; 43 non-test functions touched, 2 test functions touched.

- (top level) (51 changed lines)
- `deadline_get` (line 71, 3 changed lines)
- `deadline_post` (line 75, 3 changed lines)
- `deadline_delete` (line 79, 3 changed lines)
- `login_prepare` (line 262, 12 changed lines)
- `login_finish` (line 311, 3 changed lines)
- `websocket_ticket` (line 362, 3 changed lines)
- `register_prepare` (line 388, 3 changed lines)
- `register_finish` (line 428, 3 changed lines)
- `logout` (line 467, 3 changed lines)
- `validate_session` (line 488, 3 changed lines)
- `list_devices` (line 503, 3 changed lines)
- `remove_device` (line 516, 3 changed lines)
- `object_init` (line 532, 14 changed lines)
- `object_revise` (line 555, 7 changed lines)
- `object_upload_payload` (line 574, 4 changed lines)
- `object_complete` (line 594, 3 changed lines)
- `list_objects` (line 614, 3 changed lines)
- `get_object` (line 649, 3 changed lines)
- `get_object_revision` (line 665, 16 changed lines)
- `download_object_revision_payload` (line 682, 32 changed lines)
- `download_object_payload` (line 715, 4 changed lines)
- `delete_object` (line 745, 3 changed lines)
- `create_collab_doc` (line 770, 3 changed lines)
- `get_collab_doc_meta` (line 785, 3 changed lines)
- `list_collab_docs` (line 802, 3 changed lines)
- `rename_collab_doc` (line 817, 4 changed lines)
- `delete_collab_doc` (line 839, 3 changed lines)
- `default_tls_config` (line 862, 2 changed lines)
- `with_deadline` (line 889, 10 changed lines)
- `with_transfer_deadline` (line 903, 10 changed lines)
- `sanitized_server_message` (line 1004, 15 changed lines)
- `api_error_from_response` (line 1020, 3 changed lines)
- `encrypt_clipboard_meta` (line 1092, 4 changed lines)
- `decrypt_clipboard_meta` (line 1105, 4 changed lines)
- `encrypt_clipboard_payload` (line 1118, 4 changed lines)
- `decrypt_clipboard_payload` (line 1130, 4 changed lines)
- `encrypt_file_meta_bytes` (line 1142, 4 changed lines)
- `decrypt_file_meta_bytes` (line 1155, 4 changed lines)
- `encrypt_file_blob_bytes` (line 1172, 4 changed lines)
- `decrypt_file_blob_bytes` (line 1184, 4 changed lines)
- `error_response` (line 1343, 5 changed lines)
- `from` (line 1383, 9 changed lines)
- tests touched: `envelope_body`, `server_messages_are_stripped_and_clamped`

## crates/client/src/calendar_import.rs

New file, 641 lines, 11 functions, 0 test functions.

- `recurrence_engine` (27), `ready_sources` (136), `calendar_source` (160), `save_calendar_source` (173), `sync_calendar_source` (199), `load_import_event` (404), `purge_import_object` (446), `cleanup_calendar_imports` (544), `remove_calendar_imports` (595), `is_import_file` (613), `check_record_size` (630)

## crates/client/src/engine.rs

+2082 −133 lines; 69 non-test functions touched, 16 test functions touched.

- (top level) (361 changed lines)
- `validate_snapshot_page` (line 65, 30 changed lines)
- `try_new_with_data_dir` (line 175, 6 changed lines)
- `login_with_platform` (line 248, 1 changed lines)
- `register_with_platform` (line 310, 1 changed lines)
- `resume_with_platform` (line 387, 5 changed lines)
- `finish_auth` (line 453, 20 changed lines)
- `logout` (line 552, 5 changed lines)
- `clear_local_session` (line 572, 18 changed lines)
- `end_refused_session` (line 594, 7 changed lines)
- `remove_device` (line 630, 9 changed lines)
- `send_clipboard_payload` (line 661, 10 changed lines)
- `clipboard_payload` (line 857, 2 changed lines)
- `copy_to_local` (line 891, 2 changed lines)
- `submit_single_payload_object` (line 920, 12 changed lines)
- `finish_single_payload_object` (line 946, 9 changed lines)
- `upload_file_bytes` (line 1025, 11 changed lines)
- `download_file_bytes` (line 1144, 29 changed lines)
- `retain_downloaded_file` (line 1190, 29 changed lines)
- `delete_file` (line 1260, 35 changed lines)
- `create_schedule_item` (line 1305, 4 changed lines)
- `create_schedule_record` (line 1316, 7 changed lines)
- `write_schedule_record` (line 1331, 131 changed lines)
- `write_tombstone` (line 1470, 57 changed lines)
- `start_actual` (line 1532, 36 changed lines)
- `stop_actual` (line 1574, 4 changed lines)
- `stop_actual_inner` (line 1579, 32 changed lines)
- `actuals_between` (line 1613, 46 changed lines)
- `running_actual_id` (line 1661, 11 changed lines)
- `update_schedule_item` (line 1677, 55 changed lines)
- `check_revision_advance` (line 1754, 6 changed lines)
- `local_head` (line 1762, 8 changed lines)
- `delete_schedule_object` (line 1777, 5 changed lines)
- `tombstone_schedule_object` (line 1783, 18 changed lines)
- `expand_schedule` (line 1808, 137 changed lines)
- `next_alarms` (line 1953, 70 changed lines)
- `add_calendar_source` (line 2025, 37 changed lines)
- `snapshot_schedule` (line 2064, 71 changed lines)
- `decrypt_schedule_object_item` (line 2136, 49 changed lines)
- `rename_collab_doc` (line 2220, 5 changed lines)
- `publish_visible_state` (line 2285, 31 changed lines)
- `start_reconciliation` (line 2324, 14 changed lines)
- `handle_ws_text` (line 2370, 24 changed lines)
- `snapshot_files` (line 2455, 9 changed lines)
- `snapshot_clipboard` (line 2554, 27 changed lines)
- `keep_held_revision` (line 2630, 19 changed lines)
- `decrypt_clipboard_object_item_with_api` (line 2731, 3 changed lines)
- `handle_updated_object_event` (line 2825, 32 changed lines)
- `materialize_object` (line 2907, 38 changed lines)
- `materialize_collab` (line 3012, 5 changed lines)
- `encrypted_clipboard_from_init` (line 3557, 4 changed lines)
- `parse_calendar_feed` (line 3569, 8 changed lines)
- `fetch_calendar_feed` (line 3595, 57 changed lines)
- `parse_instant` (line 3659, 8 changed lines)
- `encrypted_object_from_revise` (line 3688, 19 changed lines)
- `object_envelope_body_for_aad` (line 3736, 9 changed lines)
- `revision` (line 3778, 6 changed lines)
- `parent_hash` (line 3785, 6 changed lines)
- `operation` (line 3792, 7 changed lines)
- `object_envelope_body` (line 3802, 14 changed lines)
- `verify_object_list_item_envelope` (line 3827, 15 changed lines)
- `verify_payload_hash` (line 3907, 8 changed lines)
- `stopped_span` (line 3922, 7 changed lines)
- `check_upload_plaintext_size` (line 3934, 5 changed lines)
- `session_refused` (line 4022, 3 changed lines)
- `source_record` (line 4482, 13 changed lines)
- `encrypted_schedule_object` (line 4496, 69 changed lines)
- `history_cache_eviction_clears_stale_entries_without_losing_the_new_read` (line 4570, 68 changed lines)
- `logout_clears_history_cache_and_advances_the_epoch_offline` (line 4642, 31 changed lines)
- tests touched: `snapshot_pages_must_advance_inside_the_watermark`, `calendar_fetch_errors_do_not_expose_the_private_url`, `calendar_fetch_bounds_chunked_bodies_before_reading_to_end`, `envelope_payload`, `signed_item_with_payload_count`, `signed_item_with_version`, `historical_reads_require_the_exact_pinned_signed_body`, `envelope_verification_accepts_initial_format_and_rejects_unknown_versions`, `visible_state`, `open_session`, `stopping_a_timer_clamps_a_clock_that_runs_behind`, `logout_fences_sync_writes_that_are_still_in_flight`, `a_late_view_does_not_replace_a_newer_one`, `only_a_refused_token_ends_the_session`, `a_refused_session_is_torn_down_once`, `an_oversized_upload_is_refused_before_encryption`

## crates/client/src/lib.rs

+1 −0 lines; 1 non-test functions touched, 0 test functions touched.

- (top level) (1 changed lines)

## crates/client/src/local_store.rs

+2531 −363 lines; 80 non-test functions touched, 28 test functions touched.

- (top level) (431 changed lines)
- `new` (line 297, 3 changed lines)
- `mark_snapshot_seen` (line 327, 12 changed lines)
- `keep_retained_revision` (line 346, 12 changed lines)
- `refresh_seen_generation` (line 361, 14 changed lines)
- `persist_local_clipboard_present_encrypted` (line 380, 2 changed lines)
- `persist_snapshot_clipboard_present_encrypted` (line 406, 28 changed lines)
- `persist_local_schedule_present_encrypted` (line 443, 20 changed lines)
- `persist_snapshot_schedule_present_encrypted` (line 468, 31 changed lines)
- `persist_schedule_present_encrypted_inner` (line 504, 40 changed lines)
- `import_file_object` (line 547, 18 changed lines)
- `import_file_ciphertext` (line 570, 16 changed lines)
- `cache_import_file_ciphertext` (line 588, 25 changed lines)
- `persist_snapshot_file_present_encrypted` (line 636, 24 changed lines)
- `hydrate_ciphertext_cache` (line 724, 10 changed lines)
- `mark_pending_update` (line 779, 34 changed lines)
- `apply_local_delete` (line 814, 2 changed lines)
- `apply_local_tombstone` (line 830, 20 changed lines)
- `apply_live_delete` (line 851, 2 changed lines)
- `remove_absent_object` (line 889, 2 changed lines)
- `persist_clipboard_present_encrypted_inner` (line 952, 7 changed lines)
- `persist_file_present_encrypted_inner` (line 996, 2 changed lines)
- `persist_collab_present_inner` (line 1040, 17 changed lines)
- `mark_pending_create_inner` (line 1103, 15 changed lines)
- `apply_delete_inner` (line 1164, 65 changed lines)
- `sweep_kind_inner` (line 1249, 40 changed lines)
- `mark_record_absent` (line 1282, 28 changed lines)
- `discard_unreadable_cache_entry` (line 1320, 10 changed lines)
- `mark_object_absent_inner` (line 1332, 19 changed lines)
- `recent_clipboard_items_inner` (line 1352, 8 changed lines)
- `file_items_inner` (line 1364, 6 changed lines)
- `collab_items_inner` (line 1368, 6 changed lines)
- `decrypt_stored_object_record_preview` (line 1384, 6 changed lines)
- `decrypt_schedule_record` (line 1414, 31 changed lines)
- `schedule_items_inner` (line 1452, 17 changed lines)
- `calendar_sources_inner` (line 1470, 21 changed lines)
- `running_actual_inner` (line 1496, 38 changed lines)
- `local_head` (line 1544, 10 changed lines)
- `validate_incoming_revision` (line 1561, 9 changed lines)
- `validate_encrypted_revision_advance` (line 1577, 44 changed lines)
- `schedule_records_with_ids` (line 1622, 14 changed lines)
- `schedule_records_with_heads` (line 1640, 10 changed lines)
- `decrypt_present_clipboard_payload` (line 1702, 2 changed lines)
- `visible_state_inner` (line 1745, 16 changed lines)
- `load_or_create_device_signing_identity_inner` (line 1775, 10 changed lines)
- `load_device_signing_identity_inner` (line 1810, 4 changed lines)
- `write_device_identity` (line 1833, 2 changed lines)
- `sweep_orphaned_temp_files` (line 1849, 23 changed lines)
- `with_database` (line 1858, 30 changed lines)
- `open_database` (line 1878, 6 changed lines)
- `discard_legacy_file_store` (line 1892, 13 changed lines)
- `stored_object_record` (line 1909, 8 changed lines)
- `write_stored_object_record` (line 1917, 7 changed lines)
- `write_stored_object_record_with_payload` (line 1930, 18 changed lines)
- `stored_object_payload_ciphertext` (line 1939, 13 changed lines)
- `live_stored_object_records` (line 1947, 33 changed lines)
- `stale_stored_object_ids` (line 1952, 61 changed lines)
- `discard_cached_payload` (line 1969, 4 changed lines)
- `remove_payloads_for_object` (line 1974, 12 changed lines)
- `remove_stored_object_record_and_payloads` (line 1979, 13 changed lines)
- `database_path` (line 1994, 4 changed lines)
- `legacy_object_dir` (line 2000, 1 changed lines)
- `legacy_clipboard_dir` (line 2004, 9 changed lines)
- `write_browser_device_identity` (line 2077, 2 changed lines)
- `object_payload_ciphertext_key` (line 2297, 4 changed lines)
- `local_head_from_present` (line 2351, 10 changed lines)
- `revision_anchor_for_record` (line 2362, 10 changed lines)
- `present_revision_anchor` (line 2378, 11 changed lines)
- `validate_revision_against_head` (line 2390, 35 changed lines)
- `revision_anchor_error` (line 2426, 5 changed lines)
- `decrypt_file_record` (line 2455, 8 changed lines)
- `schedule_item_view_from_record` (line 2490, 12 changed lines)
- `verify_payload_ciphertext` (line 2580, 5 changed lines)
- `clipboard_display_text` (line 2599, 1 changed lines)
- `is_text_mime_type` (line 2624, 4 changed lines)
- `top_level_mime_type` (line 2628, 7 changed lines)
- `normalized_clipboard_mime_type` (line 2638, 8 changed lines)
- `device_identity_record_aad` (line 2670, 21 changed lines)
- `encrypted_device_identity_record` (line 2692, 14 changed lines)
- `device_identity_from_record` (line 2714, 19 changed lines)
- tests touched: `encrypted_clipboard`, `encrypted_clipboard_at`, `file_item`, `encrypted_file_at`, `collab_item`, `encrypts_device_identity_at_rest`, `tampered_device_identity_record`, `rejects_device_identity_record_with_a_substituted_device_id`, `rejects_device_identity_record_with_a_malformed_device_id`, `rejects_device_identity_record_copied_from_another_profile`, `hydrate_sweeps_orphaned_temp_files`, `opening_the_store_discards_the_file_based_one_it_replaces`, `restricts_cache_permissions_and_does_not_store_plaintext`, `delete_marker_keeps_revision_anchor_across_sweeps_and_restart`, `snapshot_absence_retains_head_without_forcing_a_new_revision`, `locally_signed_tombstone_is_a_durable_exact_head`, `hydration_keeps_the_anchor_when_cached_content_cannot_be_read`, `wiping_the_cache_leaves_every_anchor_standing`, `a_deleted_object_leaves_nothing_for_hydration_to_read`, `dropping_an_object_reclaims_its_cached_payload`, `a_stale_snapshot_page_skips_one_item_and_keeps_going`, `an_equivocating_snapshot_body_is_skipped`, `a_held_collab_doc_does_not_break_the_view`, `a_collab_listing_cannot_erase_an_encrypted_objects_anchor`, `a_chainless_record_cannot_take_an_anchor_away`, `a_late_local_tombstone_cannot_lower_the_anchor`, `validating_an_incoming_revision_honours_an_absent_anchor`, `validating_an_incoming_revision_honours_an_observed_delete_anchor`

## crates/client/src/local_store/adversarial_tests.rs

New file, 1087 lines, 23 functions, 0 test functions.

- `item` (24), `encrypted_clipboard_at` (35), `head_of` (105), `successor` (113), `new_store` (128), `revision_error_text` (134), `served_revision_older_than_accepted_anchor_is_rejected` (141), `different_body_at_the_accepted_revision_is_rejected` (167), `successor_with_wrong_parent_hash_is_rejected` (201), `skipped_link_is_accepted_as_documented` (233), `event_stream_delete_requires_two_newer_revisions` (271), `not_found_absence_keeps_the_anchor_and_allows_the_same_head` (319), `signed_tombstone_chains_the_restore_and_rejects_wrong_parent` (359), `undecryptable_payload_bytes_keep_the_anchor` (417), `cache_row_eviction_keeps_the_anchor` (470), `uncommitted_transaction_leaves_no_trace_and_prior_data_survives` (528), `payload_write_rejected_for_a_marker_leaves_no_partial_record` (591), `concurrent_writers_to_one_object_never_leave_torn_state` (655), `repeated_snapshot_page_is_idempotent` (772), `snapshot_item_at_lower_revision_than_local_is_rejected` (846), `one_malformed_content_row_must_not_drop_the_other_objects` (893), `live_row_with_null_content_must_not_brick_refetch_or_sweep` (954), `corrupt_anchor_row_must_not_brick_every_later_event_for_that_object` (1041)

## crates/client/src/local_store/sqlite.rs

New file, 743 lines, 21 functions, 0 test functions.

- `open` (79), `create_private_file_if_missing` (120), `read_record` (142), `unreadable_cache_row` (214), `live_records` (233), `stale_object_ids` (337), `write_record` (362), `read_payload` (453), `delete_payload` (470), `forget_object` (488), `anchor_has_chain_position` (509), `upsert_object` (524), `upsert_anchor` (562), `write_departed_anchor` (598), `carry_anchor` (620), `read_anchor_row` (636), `damaged_anchor_dropped` (680), `revision_anchor` (696), `object_kind` (718), `anchor_kind_text` (725), `anchor_kind_from_text` (733)

## crates/client/src/schedule.rs

New file, 416 lines, 20 functions, 6 test functions.

- `kind` (40), `meta` (50), `as_item` (58), `as_source` (65), `as_ingested` (72), `planned_title` (80), `encrypt_schedule_meta` (92), `decrypt_schedule_meta` (105), `encrypt_schedule_payload` (118), `decrypt_schedule_payload` (132), `item_view` (146), `actual_view` (182), `occurrence_key` (213), `parse_occurrence_key` (227), `occurrence_view` (248), `ingested_as_series` (273), `to_rfc3339` (284), `source_view` (291), `redact_url` (320), `zone_or_utc` (335)

## crates/client/src/schedule_context.rs

New file, 374 lines, 9 functions, 1 test functions.

- `revision_ref` (11), `series` (22), `invalid` (30), `verify_pin` (34), `schedule_revision` (52), `effective_overrides` (128), `validate_plan_context` (179), `actual_title` (246), `recorded_plan` (266)

## crates/client/src/schedule_integration_tests.rs

New file, 1345 lines, 6 functions, 0 test functions.

- `drop` (20), `wait_for` (26), `server_command` (39), `live_schedule_revisions_timers_feeds_and_two_devices` (54), `imported_source_readiness_requires_a_complete_active_batch` (1011), `exercise_revision_aware_plans` (1105)

## crates/core/src/crypto.rs

+634 −38 lines; 19 non-test functions touched, 20 test functions touched.

- (top level) (174 changed lines)
- `opaque_ksf_params` (line 57, 11 changed lines)
- `opaque_ksf` (line 71, 7 changed lines)
- `object_envelope_body_bytes` (line 145, 2 changed lines)
- `object_envelope_parent_hash` (line 156, 5 changed lines)
- `domain_separated` (line 174, 6 changed lines)
- `sign_object_envelope_body` (line 182, 5 changed lines)
- `sign_device_login_proof_body` (line 193, 3 changed lines)
- `verify_object_envelope_signature` (line 204, 10 changed lines)
- `verify_device_login_proof_signature` (line 219, 3 changed lines)
- `object_meta_aad` (line 250, 4 changed lines)
- `object_payload_aad` (line 255, 6 changed lines)
- `object_aad` (line 273, 62 changed lines)
- `opaque_new_server_setup` (line 477, 2 changed lines)
- `opaque_client_register_start` (line 496, 8 changed lines)
- `opaque_client_register_finish` (line 518, 8 changed lines)
- `opaque_client_login_start` (line 607, 8 changed lines)
- `opaque_client_login_finish` (line 634, 8 changed lines)
- `opaque_server_login_start` (line 684, 2 changed lines)
- tests touched: `test_opaque_ksf_parameters_are_pinned`, `uuid_str`, `payload`, `body`, `bound_fields`, `unbound_fields`, `meta_ciphertext_will_not_open_under_a_changed_bound_field`, `payload_ciphertext_will_not_open_under_a_changed_bound_field`, `unbound_fields_leave_the_aad_byte_identical`, `the_body_signature_covers_what_the_aad_omits`, `meta_and_payload_aads_are_domain_separated`, `a_payload_ciphertext_cannot_be_moved_to_a_sibling`, `every_operation_variant_is_covered_by_a_mutation`, `parent_hash_changes_with_every_field_of_the_parent`, `envelope_body`, `proof_body`, `sign_raw`, `an_envelope_signature_needs_the_envelope_domain`, `a_login_proof_signature_needs_the_login_proof_domain`, `the_domain_is_a_prefix_of_the_canonical_body`

## crates/daemon-types/src/ipc_path.rs

+95 −8 lines; 6 non-test functions touched, 0 test functions touched.

- (top level) (16 changed lines)
- `ensure_private_socket_dir` (line 40, 44 changed lines)
- `unique_base` (line 260, 10 changed lines)
- `existing_0755_directory_is_rejected` (line 272, 14 changed lines)
- `freshly_created_directory_is_0700` (line 288, 10 changed lines)
- `existing_0700_directory_is_accepted` (line 300, 9 changed lines)

## crates/daemon-types/src/protocol.rs

+84 −2 lines; 2 non-test functions touched, 0 test functions touched.

- (top level) (82 changed lines)
- `state_changed` (line 367, 4 changed lines)

## crates/daemon/src/clients.rs

+26 −2 lines; 1 non-test functions touched, 2 test functions touched.

- `broadcast` (line 41, 12 changed lines)
- tests touched: `(top level)`, `broadcast_keeps_slow_clients_on_full`

## crates/daemon/src/handler.rs

+150 −6 lines; 12 non-test functions touched, 0 test functions touched.

- (top level) (25 changed lines)
- `handle_connection` (line 46, 10 changed lines)
- `dispatch_command` (line 389, 25 changed lines)
- `cmd_create_schedule_item` (line 747, 10 changed lines)
- `cmd_update_schedule_item` (line 758, 13 changed lines)
- `cmd_delete_schedule_object` (line 772, 10 changed lines)
- `cmd_expand_schedule` (line 783, 13 changed lines)
- `cmd_start_actual` (line 797, 10 changed lines)
- `cmd_stop_actual` (line 808, 10 changed lines)
- `cmd_actuals_between` (line 819, 10 changed lines)
- `cmd_add_calendar_source` (line 830, 10 changed lines)
- `cmd_sync_calendar_source` (line 841, 10 changed lines)

## crates/daemon/src/main.rs

+70 −7 lines; 6 non-test functions touched, 0 test functions touched.

- (top level) (15 changed lines)
- `try_acquire_ipc_slot` (line 213, 6 changed lines)
- `run` (line 238, 30 changed lines)
- `connection_slots_reject_over_cap` (line 437, 8 changed lines)
- `connection_slots_free_on_drop` (line 447, 11 changed lines)
- `handshake_timeout_mirrors_ws_hello_timeout` (line 460, 7 changed lines)

## crates/daemon/src/protocol.rs

+10 −7 lines; 1 non-test functions touched, 0 test functions touched.

- (top level) (17 changed lines)

## crates/fs-txn/src/lib.rs

+8 −12 lines; 1 non-test functions touched, 0 test functions touched.

- `write_new` (line 54, 20 changed lines)

## crates/mobile-uniffi/src/lib.rs

+98 −1 lines; 5 non-test functions touched, 1 test functions touched.

- (top level) (28 changed lines)
- `decode_resume_key` (line 43, 11 changed lines)
- `session_resume_material` (line 158, 10 changed lines)
- `resume` (line 169, 31 changed lines)
- `next_alarms` (line 227, 10 changed lines)
- tests touched: `resume_keys_require_exactly_32_decoded_bytes`

## crates/schedule/src/alarm.rs

New file, 200 lines, 4 functions, 9 test functions.

- `at_start` (27), `minutes_before` (31), `lead` (37), `plan_alarms` (66)

## crates/schedule/src/engine.rs

New file, 530 lines, 19 functions, 0 test functions.

- `occurrences` (46), `overlapping_occurrences` (59), `next_after` (85), `maximum_lookback` (106), `span_lookback` (120), `default` (150), `new` (159), `with_max_candidates` (163), `with_imported_rules` (172), `rule_spans` (181), `insert` (291), `merge` (303), `lookup` (307), `is_empty` (311), `effective_zone` (393), `recurrence_id` (411), `span_at` (433), `rrule_line` (458), `ical_weekday` (505)

## crates/schedule/src/ingest.rs

New file, 976 lines, 37 functions, 0 test functions.

- `new` (44), `default` (50), `fmt` (56), `contains_event` (91), `belongs_to_import` (139), `derive_id` (151), `parse_ics` (192), `parse_imported_recurrence_rules` (238), `partition_masters_and_overrides` (265), `parse_calendar` (295), `validate_calendar_envelope` (324), `validate_rrule_counts` (350), `unfold_content_lines` (395), `event_from_component` (412), `local` (466), `is_floating` (470), `timed_start` (474), `instant` (486), `span_from` (494), `positive_days` (554), `duration_minutes` (561), `all_day_duration` (568), `ical_duration_minutes` (582), `duration_property` (599), `recurrence_overrides` (617), `make_override` (697), `recurrence_id_for` (712), `span_at` (734), `override_count_within_limit` (751), `recurrence_values_count` (770), `recurrence_times` (783), `text_property` (807), `rrule_text` (821), `date_time_property` (845), `property` (855), `feed_time_from_entry` (865), `feed_time_from_partial` (876)

## crates/schedule/src/item.rs

New file, 210 lines, 4 functions, 0 test functions.

- `new` (26), `default` (32), `fmt` (38), `overrides_compatible_with` (84)

## crates/schedule/src/lib.rs

New file, 41 lines, 0 functions, 0 test functions.

-

## crates/schedule/src/recurrence.rs

New file, 533 lines, 23 functions, 0 test functions.

- `from_imported_rule` (40), `new` (59), `as_str` (98), `until_wall_clock` (115), `until_value_wall_clock` (127), `every` (159), `each` (168), `ending` (176), `try_from` (243), `from_start` (252), `from_end` (256), `check` (260), `as_ical` (269), `last` (322), `serialize` (351), `deserialize` (362), `serde_weekday_name` (428), `serde_weekday_from_name` (440), `just` (465), `weekdays` (470), `contains` (481), `iter` (485), `after` (511)

## crates/schedule/src/recurrence/imported_rule.rs

New file, 301 lines, 7 functions, 6 test functions.

- `convert` (17), `cadence` (30), `has_any` (146), `parse_positive` (150), `parse_plain_weekday` (154), `parse_month_day` (167), `parse_monthly_weekday` (176)

## crates/schedule/src/summary.rs

New file, 282 lines, 8 functions, 5 test functions.

- `summary` (19), `phrase` (63), `time_summary` (95), `weekly_phrase` (118), `weekday_list` (125), `weekday_name` (134), `month_name` (146), `ordinal` (164)

## crates/schedule/src/time.rs

New file, 250 lines, 14 functions, 0 test functions.

- `local` (36), `resolve` (45), `local_start` (114), `from_minutes` (134), `minutes` (140), `as_time_delta` (144), `try_from` (167), `new` (173), `start` (180), `end` (184), `contains` (188), `overlaps` (193), `resolve_local` (203), `one_local` (228)

## crates/schedule/tests/adversarial.rs

New file, 1709 lines, 72 functions, 0 test functions.

- `local` (21), `utc` (25), `window` (31), `expansion` (35), `local_midnight_window` (43), `timed_item` (53), `daily` (67), `expand` (71), `in_zone` (79), `floating_0230_spring_forward_berlin` (93), `floating_0230_spring_forward_new_york` (107), `zoned_0230_fall_back_berlin_takes_earlier_instant` (122), `zoned_0130_fall_back_new_york_takes_earlier_instant` (138), `all_day_on_fall_back_day_is_25_hours` (153), `two_all_day_blocks_over_fall_back_are_49_hours` (168), `weekly_zoned_series_keeps_wall_clock_across_spring_forward` (184), `weekly_zoned_series_keeps_wall_clock_across_fall_back` (219), `series_starting_inside_the_gap_still_emits_the_gap_day` (256), `floating_series_starting_in_gap_expands_in_every_zone` (289), `floating_series_on_a_skipped_date_shifts_to_the_next_valid_instant` (328), `all_day_series_on_a_skipped_date_reports_a_zero_length_day` (370), `all_day_series_keeps_every_date_across_a_midnight_gap` (404), `a_floating_gap_identity_is_the_same_for_every_observer` (445), `monthly_30th_skips_february` (487), `monthly_29th_skips_non_leap_february` (509), `monthly_31st_every_two_months` (547), `yearly_feb29_every_two_years` (583), `count_includes_the_first_occurrence` (612), `until_is_inclusive_of_the_boundary_instant` (641), `until_compares_instants_not_wall_clock` (680), `fortnightly_weeks_start_on_monday_across_year_boundary` (711), `cancelled` (748), `rescheduled` (757), `utc_span` (770), `floating_cancellation_in_dst_gap_matches_in_any_zone` (787), `override_moving_an_occurrence_out_of_the_window` (818), `override_moving_an_occurrence_within_the_window` (848), `duplicate_overrides_for_one_identity_yield_at_most_one_occurrence` (882), `a_rescheduled_override_for_an_ungenerated_identity_adds_an_occurrence` (911), `a_rescheduled_override_adds_an_occurrence_to_a_one_off` (948), `cancellation_for_an_unknown_identity_changes_nothing` (977), `occurrence_starting_exactly_at_window_end_is_excluded` (1001), `occurrence_starting_exactly_at_window_start_is_included` (1020), `occurrence_ending_exactly_at_window_start_is_excluded_everywhere` (1039), `overnight_event_needs_overlap_expansion` (1070), `multi_day_all_day_overlaps_each_covered_window` (1096), `window_before_series_start_is_empty` (1124), `zero_length_windows_are_rejected` (1144), `import_id` (1170), `an_imported_all_day_rule_with_a_date_until_includes_the_last_day` (1177), `an_imported_rule_with_a_utc_until_keeps_the_matching_day` (1231), `single_candidate_with_limit_one_is_complete` (1279), `two_candidates_with_limit_one_errors` (1299), `very_old_daily_series_hits_the_historical_scan_limit` (1321), `alarmed_daily_utc` (1343), `alarm_lead_before_the_window_still_rings` (1363), `alarm_with_passed_lead_is_dropped` (1382), `cancelled_occurrence_raises_no_alarm` (1400), `rescheduled_occurrence_raises_one_ordered_alarm` (1416), `json_round_trip` (1452), `postcard_round_trip` (1462), `sample_item` (1472), `every_public_time_shape_round_trips` (1499), `every_public_recurrence_shape_round_trips` (1525), `every_public_item_and_alarm_shape_round_trips` (1579), `postcard_supported_shapes_round_trip` (1620), `internally_tagged_enums_are_json_only` (1634), `zero_duration_is_rejected_on_deserialize` (1648), `zero_interval_is_rejected_on_deserialize` (1660), `zero_count_is_rejected_on_deserialize` (1678), `zero_day_all_day_is_rejected_on_deserialize` (1686), `bad_zone_is_rejected_on_deserialize` (1697)

## crates/schedule/tests/behaviour.rs

New file, 1096 lines, 42 functions, 0 test functions.

- `local` (15), `utc` (19), `daily_at` (25), `expand` (39), `window` (49), `floating_follows_the_observer` (57), `zoned_stays_put_wherever_it_is_read` (104), `a_floating_override_matches_in_any_zone` (144), `cancelled_occurrence_is_skipped` (185), `rescheduled_occurrence_reports_its_override` (215), `override_can_move_an_occurrence_into_the_window` (258), `overrides_for_other_items_are_ignored` (296), `all_day_spans_whole_local_days` (323), `one_off_items_expand_to_themselves` (350), `a_half_hour_dst_gap_keeps_the_position_within_the_gap` (389), `a_skipped_civil_date_shifts_by_the_full_transition` (406), `a_narrow_window_does_not_count_decades_of_series_history` (423), `a_dense_old_rule_hits_the_history_scan_bound` (441), `an_impossible_finite_raw_rule_is_empty` (470), `an_overnight_occurrence_overlaps_the_next_days_window` (500), `a_multi_day_occurrence_overlaps_a_window_across_dst` (534), `a_longer_override_can_overlap_from_before_the_window` (563), `expansion_limit_errors_rather_than_truncating` (595), `overlap_expansion_keeps_the_candidate_limit` (617), `exactly_the_candidate_limit_is_complete` (642), `an_empty_window_is_rejected` (661), `invalid_domain_values_cannot_be_constructed` (673), `the_wire_format_is_self_describing` (699), `an_item_without_a_reference_or_an_alarm_deserializes` (742), `an_imported_rule_serializes_only_its_reference` (764), `an_imported_rule_without_its_snapshot_fails_clearly` (790), `a_raw_rule_cannot_smuggle_a_second_property` (825), `a_non_ascii_rule_is_refused_rather_than_crashing` (851), `every_until_value_kind_validates` (872), `descriptive_edits_preserve_override_applicability` (887), `structural_edits_require_reconsidering_overrides` (897), `historical_plan_retains_original_identity_and_resolved_override_after_travel` (933), `time_ranges_validate_construction_and_deserialization` (982), `time_ranges_use_half_open_boundaries` (1001), `day_ordinals_serialize_in_the_current_wire_shape` (1018), `out_of_range_day_ordinals_fail_to_deserialize` (1035), `out_of_range_weekday_ordinals_fail_to_deserialize` (1072)

## crates/schedule/tests/corpus.rs

New file, 328 lines, 19 functions, 0 test functions.

- `item` (20), `next_after` (40), `daily` (51), `monthly` (55), `every_2_days` (62), `weekdays_mwf` (71), `every_2_weeks_tue` (86), `fortnightly_weeks_start_on_monday` (107), `monthday_31` (127), `second_tuesday` (142), `last_friday` (160), `every_3_months_15` (178), `monthly_31_skip` (195), `yearly_feb29` (210), `last_day_month` (225), `second_last_day` (240), `count_3_exhausted` (255), `until_past` (266), `dst_gap_shifts_forward_like_java` (289)

## crates/schedule/tests/imported_rule.rs

New file, 92 lines, 4 functions, 0 test functions.

- `import_id` (8), `starts` (12), `converted_cadences_have_the_same_occurrences_as_the_imported_rule` (51), `unsupported_rules_retain_only_the_import_reference` (81)

## crates/schedule/tests/ingest.rs

New file, 738 lines, 35 functions, 0 test functions.

- `parse` (68), `uuid_fixture` (73), `import_fixture` (77), `event` (81), `readable_events_are_kept_and_unreadable_ones_are_reported` (90), `a_tzid_start_stays_pinned_to_its_zone` (106), `a_z_suffix_is_utc_rather_than_floating` (128), `a_bare_local_start_stays_floating` (143), `all_day_events_are_dates_and_dtend_is_exclusive` (152), `all_day_events_are_not_filtered_out` (175), `a_cancelled_event_is_tombstoned_not_dropped` (187), `a_representable_ingested_rule_becomes_an_editable_cadence` (198), `events_without_a_rule_happen_once` (221), `ids_are_stable_across_passes_and_distinct_per_source` (228), `an_ingested_series_expands` (259), `garbage_and_incomplete_calendars_are_rejected_but_an_empty_snapshot_is_valid` (303), `parser_has_an_input_size_ceiling` (324), `a_feed_with_too_many_lines_is_rejected_before_parsing` (331), `an_unknown_tzid_skips_the_event_instead_of_becoming_floating` (353), `durations_use_instants_across_zones_and_dst` (367), `duration_properties_are_honoured_for_timed_and_all_day_events` (385), `exdate_rdate_and_recurrence_id_components_become_overrides` (416), `an_unsupported_range_override_skips_its_whole_series` (478), `exdate_takes_precedence_over_the_same_rdate` (494), `a_detached_instance_moved_across_the_window_boundary_still_overlaps` (511), `an_unsupported_rule_is_resolved_from_the_referenced_snapshot` (555), `runtime_rule_extraction_rejects_ambiguous_master_uids_and_rrules` (614), `feed_with_rrule` (634), `assert_malformed_rrule` (642), `rewritten_interval_and_count_rules_are_rejected` (656), `a_folded_rrule_with_zero_interval_is_rejected` (665), `a_valid_interval_and_count_still_becomes_a_cadence` (682), `feed_with_exdates` (698), `too_many_exdate_values_are_rejected` (709), `ten_thousand_exdate_values_parse` (731)

## crates/server/src/cleanup.rs

+374 −43 lines; 8 non-test functions touched, 7 test functions touched.

- (top level) (46 changed lines)
- `cleanup_expired_clipboard_objects` (line 65, 2 changed lines)
- `trim_user_clipboard` (line 109, 8 changed lines)
- `cleanup_orphan_object_uploads` (line 173, 8 changed lines)
- `cleanup_orphan_object_uploads_batch` (line 190, 115 changed lines)
- `revision_keys_condition` (line 324, 11 changed lines)
- `payload_paths_for_revisions` (line 337, 10 changed lines)
- `delete_objects_and_release_usage` (line 349, 6 changed lines)
- tests touched: `test_state`, `insert_user`, `insert_orphan`, `insert_complete_object`, `orphan_sweep_cleans_more_than_one_batch_in_a_single_call`, `orphan_sweep_continues_past_one_users_release_failure`, `object_cleanup_continues_past_one_users_release_failure`

## crates/server/src/config.rs

+17 −10 lines; 2 non-test functions touched, 2 test functions touched.

- (top level) (15 changed lines)
- `apply_overrides` (line 158, 4 changed lines)
- tests touched: `toml_overrides_defaults`, `validation_rejects_zero_user_quota`

## crates/server/src/entity/devices.rs

+4 −4 lines; 2 non-test functions touched, 0 test functions touched.

- (top level) (6 changed lines)
- `to` (line 42, 2 changed lines)

## crates/server/src/entity/mod.rs

+1 −0 lines; 1 non-test functions touched, 0 test functions touched.

- (top level) (1 changed lines)

## crates/server/src/entity/object_payloads.rs

+8 −6 lines; 2 non-test functions touched, 0 test functions touched.

- (top level) (12 changed lines)
- `to` (line 42, 2 changed lines)

## crates/server/src/entity/object_revisions.rs

New file, 73 lines, 1 functions, 0 test functions.

- `to` (55)

## crates/server/src/entity/objects.rs

+8 −28 lines; 2 non-test functions touched, 0 test functions touched.

- (top level) (34 changed lines)
- `to` (line 49, 2 changed lines)

## crates/server/src/main.rs

+16 −1 lines; 1 non-test functions touched, 0 test functions touched.

- `serve` (line 266, 17 changed lines)

## crates/server/src/migration/m20260908_000004_schedule_objects.rs

New file, 218 lines, 2 functions, 0 test functions.

- `up` (28), `down` (211)

## crates/server/src/migration/m20260908_000005_object_revisions.rs

New file, 275 lines, 2 functions, 0 test functions.

- `up` (41), `down` (264)

## crates/server/src/migration/mod.rs

+4 −0 lines; 2 non-test functions touched, 0 test functions touched.

- (top level) (2 changed lines)
- `migrations` (line 13, 2 changed lines)

## crates/server/src/routes/auth.rs

+15 −6 lines; 0 non-test functions touched, 4 test functions touched.

- tests touched: `(top level)`, `challenge_request`, `registration_start_request`, `challenge_rate_limits_by_username_across_client_ips`

## crates/server/src/routes/collab.rs

+7 −9 lines; 1 non-test functions touched, 0 test functions touched.

- `create_collab_doc` (line 62, 16 changed lines)

## crates/server/src/routes/objects.rs

+3426 −971 lines; 36 non-test functions touched, 53 test functions touched.

- (top level) (510 changed lines)
- `init_object` (line 83, 104 changed lines)
- `upload_payload` (line 417, 96 changed lines)
- `complete_object` (line 673, 131 changed lines)
- `revise_object` (line 947, 468 changed lines)
- `kind_supports_revisions` (line 1291, 6 changed lines)
- `head_revision_for_write` (line 1303, 74 changed lines)
- `deserialize` (line 1393, 21 changed lines)
- `head_revision_join` (line 1422, 11 changed lines)
- `select_revision_columns` (line 1435, 13 changed lines)
- `list_objects` (line 1525, 102 changed lines)
- `get_object` (line 1671, 56 changed lines)
- `get_object_revision` (line 1723, 13 changed lines)
- `load_readable_revision` (line 1737, 32 changed lines)
- `download_revision_payload` (line 1770, 12 changed lines)
- `retained_clipboard_object_ids_raw` (line 1789, 6 changed lines)
- `object_list_items` (line 1863, 30 changed lines)
- `download_payload` (line 2021, 17 changed lines)
- `read_revision_payload` (line 2071, 7 changed lines)
- `purge_object` (line 2137, 156 changed lines)
- `validate_object_envelope` (line 2399, 78 changed lines)
- `validate_envelope_payload` (line 2560, 2 changed lines)
- `object_for_upload` (line 2594, 50 changed lines)
- `init_request_storage_bytes` (line 2682, 22 changed lines)
- `reserve_user_storage_quota` (line 2707, 2 changed lines)
- `idempotent_init_response` (line 2770, 24 changed lines)
- `advance_object_head` (line 2920, 66 changed lines)
- `insert_object_event` (line 3013, 8 changed lines)
- `map_payload_batch_insert_error` (line 3099, 47 changed lines)
- `broadcast_created` (line 3108, 21 changed lines)
- `spawn_clipboard_trim` (line 3130, 7 changed lines)
- `object_payload_filename` (line 3145, 3 changed lines)
- `stream_body_to_payload_file` (line 3149, 78 changed lines)
- `reset_payload_status` (line 3228, 36 changed lines)
- `sha256_file` (line 3265, 17 changed lines)
- `remove_paths` (line 3283, 13 changed lines)
- tests touched: `validate_object_created_at_bounds_the_window`, `test_state`, `test_state_with_max_items`, `test_state_with_user_quotas`, `auth`, `user_storage_usage`, `postcard`, `init_created_seq`, `init_upload_urls`, `insert_user`, `insert_device`, `init_request`, `signed_envelope`, `signed_envelope_at`, `head_of`, `tombstone_object`, `revise_with`, `begin_streamed_revision`, `seeded`, `listed`, `a_revision_becomes_the_head_and_advances_the_sync_cursor`, `a_streamed_revision_completes_only_its_payloads_and_emits_updated`, `a_revision_signed_against_a_stale_head_is_refused`, `a_correct_revision_number_with_a_wrong_parent_hash_is_refused`, `a_tombstone_hides_the_object_but_keeps_its_history`, `a_revision_after_a_tombstone_brings_the_object_back`, `purge_refuses_an_object_that_is_still_live`, `revising_a_clipboard_object_is_rejected_before_any_write`, `a_revision_charges_bytes_but_not_an_object`, `init_request_with_meta`, `revise_meta_only`, `init_charges_payload_and_metadata_bytes`, `a_payload_free_revision_charges_its_metadata_bytes`, `purge_releases_payload_and_metadata_for_the_whole_chain`, `metadata_only_revisions_eventually_exceed_the_byte_quota`, `a_zero_byte_tombstone_succeeds_above_the_quota_and_purge_releases`, `init_rejects_wrong_payload_nonce_length_before_writing`, `init_rejects_wrong_payload_sha256_length_before_writing`, `historical_revisions_survive_edits_and_tombstones_but_not_purge`, `historical_reads_exclude_pending_revisions_and_expired_clipboard`, `reclaiming_source_device_detaches_objects_instead_of_blocking`, `inline_init_accepts_multiple_payloads_in_one_batch`, `init_rejects_duplicate_payload_id_before_insert`, `complete_object_rechecks_payload_metadata_and_uploaded_file`, `delete_file_returns_deleted_seq_and_broadcast_actor`, `streaming_upload_rejects_size_mismatch_without_final_file`, `failed_mark_uploaded_resets_payload_to_pending_for_retry`, `init_rejects_payload_exceeding_max_blob_bytes`, `init_rejects_user_storage_quota_and_rolls_back_inline_file`, `init_rejects_user_object_count_quota`, `trim_user_clipboard_keeps_newest_and_drops_files`, `deleting_a_schedule_object_logs_its_own_kind`, `clipboard_objects_are_still_not_deletable_here`

## crates/server/src/secret.rs

+73 −3 lines; 3 non-test functions touched, 1 test functions touched.

- (top level) (6 changed lines)
- `load_root_from_env` (line 86, 9 changed lines)
- `warn_if_secret_file_exposed` (line 108, 28 changed lines)
- tests touched: `secret_file_with_open_permissions_still_loads`

## crates/server/src/state.rs

+95 −16 lines; 4 non-test functions touched, 2 test functions touched.

- (top level) (15 changed lines)
- `seed_event_seq` (line 177, 28 changed lines)
- `new` (line 247, 7 changed lines)
- `ws_global_cap` (line 331, 3 changed lines)
- tests touched: `seed_event_seq_covers_objects_created_seq_after_log_pruning`, `ws_global_connection_cap_follows_config`

## crates/server/src/storage_quota.rs

+207 −12 lines; 7 non-test functions touched, 0 test functions touched.

- (top level) (35 changed lines)
- `revision_cost_bytes` (line 22, 6 changed lines)
- `meta_bytes_sum_expr` (line 35, 3 changed lines)
- `try_reserve_user_storage` (line 45, 17 changed lines)
- `revision_usage_by_user` (line 131, 62 changed lines)
- `object_usage_by_user` (line 194, 55 changed lines)
- `merge_usage` (line 285, 41 changed lines)

## crates/server/src/ws.rs

+12 −4 lines; 2 non-test functions touched, 0 test functions touched.

- `handle_socket` (line 170, 6 changed lines)
- `get_latest_seq` (line 424, 10 changed lines)

## crates/web-wasm/src/lib.rs

+258 −71 lines; 28 non-test functions touched, 0 test functions touched.

- (top level) (119 changed lines)
- `engine` (line 45, 6 changed lines)
- `get_or_build` (line 59, 24 changed lines)
- `clear_if_current` (line 92, 12 changed lines)
- `state_version` (line 112, 9 changed lines)
- `wait_for_state_change` (line 127, 27 changed lines)
- `compose_version` (line 163, 5 changed lines)
- `login` (line 193, 3 changed lines)
- `register` (line 217, 3 changed lines)
- `resume` (line 248, 3 changed lines)
- `session_resume_material` (line 279, 4 changed lines)
- `logout` (line 308, 2 changed lines)
- `get_state` (line 320, 3 changed lines)
- `create_schedule_item` (line 421, 10 changed lines)
- `update_schedule_item` (line 434, 17 changed lines)
- `delete_schedule_object` (line 453, 9 changed lines)
- `expand_schedule` (line 468, 9 changed lines)
- `start_actual` (line 481, 9 changed lines)
- `stop_actual` (line 492, 9 changed lines)
- `actuals_between` (line 503, 9 changed lines)
- `add_calendar_source` (line 514, 9 changed lines)
- `sync_calendar_source` (line 530, 9 changed lines)
- `create_collab_doc` (line 541, 3 changed lines)
- `rename_collab_doc` (line 563, 3 changed lines)
- `get_collab_doc_meta` (line 574, 3 changed lines)
- `list_devices` (line 585, 3 changed lines)
- `requested_base_url_matches` (line 605, 3 changed lines)
- `to_js` (line 660, 4 changed lines)

## web/src-tauri/src/daemon_client.rs

+1 −1 lines; 1 non-test functions touched, 0 test functions touched.

- `run` (line 253, 2 changed lines)

## web/src-tauri/src/lib.rs

+270 −17 lines; 18 non-test functions touched, 0 test functions touched.

- (top level) (48 changed lines)
- `run` (line 103, 9 changed lines)
- `upload_file_bytes` (line 372, 9 changed lines)
- `download_file_bytes` (line 422, 13 changed lines)
- `create_schedule_item` (line 465, 11 changed lines)
- `update_schedule_item` (line 478, 17 changed lines)
- `delete_schedule_object` (line 497, 12 changed lines)
- `expand_schedule` (line 512, 15 changed lines)
- `start_actual` (line 529, 11 changed lines)
- `stop_actual` (line 542, 9 changed lines)
- `actuals_between` (line 553, 13 changed lines)
- `add_calendar_source` (line 568, 13 changed lines)
- `sync_calendar_source` (line 583, 11 changed lines)
- `staging_dir` (line 707, 9 changed lines)
- `ensure_private_staging_dir` (line 715, 45 changed lines)
- `sanitize_temp_prefix` (line 761, 12 changed lines)
- `random_hex_suffix` (line 780, 5 changed lines)
- `create_private_temp_file` (line 786, 25 changed lines)
