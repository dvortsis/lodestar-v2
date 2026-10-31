// Generated from the event list in the logs catalogue; one plain name and one line of help per
// event id (en and zh in messages/). Loaded after the event list shows, as its own chunk.
import { m } from '$lib/paraglide/messages.js';

export const NAMES: Record<string, [name: () => string, help: () => string]> = {
	'app.start': [m.settings_logging_ev_app_start_n, m.settings_logging_ev_app_start_h],
	'app.stop': [m.settings_logging_ev_app_stop_n, m.settings_logging_ev_app_stop_h],
	'app.crash': [m.settings_logging_ev_app_crash_n, m.settings_logging_ev_app_crash_h],
	'app.restart': [m.settings_logging_ev_app_restart_n, m.settings_logging_ev_app_restart_h],
	'auth.setup_token': [
		m.settings_logging_ev_auth_setup_token_n,
		m.settings_logging_ev_auth_setup_token_h
	],
	'settings.saved': [
		m.settings_logging_ev_settings_saved_n,
		m.settings_logging_ev_settings_saved_h
	],
	'logging.kind_switched': [
		m.settings_logging_ev_logging_kind_switched_n,
		m.settings_logging_ev_logging_kind_switched_h
	],
	'logging.disk_full': [
		m.settings_logging_ev_logging_disk_full_n,
		m.settings_logging_ev_logging_disk_full_h
	],
	'logging.rotation_failed': [
		m.settings_logging_ev_logging_rotation_failed_n,
		m.settings_logging_ev_logging_rotation_failed_h
	],
	'logging.applied': [
		m.settings_logging_ev_logging_applied_n,
		m.settings_logging_ev_logging_applied_h
	],
	'logging.file_error': [
		m.settings_logging_ev_logging_file_error_n,
		m.settings_logging_ev_logging_file_error_h
	],
	'logging.file_unavailable': [
		m.settings_logging_ev_logging_file_unavailable_n,
		m.settings_logging_ev_logging_file_unavailable_h
	],
	'logging.startup_dropped': [
		m.settings_logging_ev_logging_startup_dropped_n,
		m.settings_logging_ev_logging_startup_dropped_h
	],
	'logging.over_cap': [
		m.settings_logging_ev_logging_over_cap_n,
		m.settings_logging_ev_logging_over_cap_h
	],
	'logging.folder_shared': [
		m.settings_logging_ev_logging_folder_shared_n,
		m.settings_logging_ev_logging_folder_shared_h
	],
	'logging.cleanup_failed': [
		m.settings_logging_ev_logging_cleanup_failed_n,
		m.settings_logging_ev_logging_cleanup_failed_h
	],
	'logging.files_deleted': [
		m.settings_logging_ev_logging_files_deleted_n,
		m.settings_logging_ev_logging_files_deleted_h
	],
	'logging.filter_not_applied': [
		m.settings_logging_ev_logging_filter_not_applied_n,
		m.settings_logging_ev_logging_filter_not_applied_h
	],
	'settings.saved_ignored': [
		m.settings_logging_ev_settings_saved_ignored_n,
		m.settings_logging_ev_settings_saved_ignored_h
	],
	'settings.overrides_file': [
		m.settings_logging_ev_settings_overrides_file_n,
		m.settings_logging_ev_settings_overrides_file_h
	],
	'settings.retired': [
		m.settings_logging_ev_settings_retired_n,
		m.settings_logging_ev_settings_retired_h
	],
	'settings.seal_failed': [
		m.settings_logging_ev_settings_seal_failed_n,
		m.settings_logging_ev_settings_seal_failed_h
	],
	'settings.revision_reset': [
		m.settings_logging_ev_settings_revision_reset_n,
		m.settings_logging_ev_settings_revision_reset_h
	],
	'settings.log_level_moved': [
		m.settings_logging_ev_settings_log_level_moved_n,
		m.settings_logging_ev_settings_log_level_moved_h
	],
	'settings.intensity_applied': [
		m.settings_logging_ev_settings_intensity_applied_n,
		m.settings_logging_ev_settings_intensity_applied_h
	],
	'system.ids_ignored': [
		m.settings_logging_ev_system_ids_ignored_n,
		m.settings_logging_ev_system_ids_ignored_h
	],
	'system.data_dir_owned': [
		m.settings_logging_ev_system_data_dir_owned_n,
		m.settings_logging_ev_system_data_dir_owned_h
	],
	'system.data_dir_chowned': [
		m.settings_logging_ev_system_data_dir_chowned_n,
		m.settings_logging_ev_system_data_dir_chowned_h
	],
	'system.witness_dir': [
		m.settings_logging_ev_system_witness_dir_n,
		m.settings_logging_ev_system_witness_dir_h
	],
	'system.witness_dir_left': [
		m.settings_logging_ev_system_witness_dir_left_n,
		m.settings_logging_ev_system_witness_dir_left_h
	],
	'system.running_as_root': [
		m.settings_logging_ev_system_running_as_root_n,
		m.settings_logging_ev_system_running_as_root_h
	],
	'system.dropped_root': [
		m.settings_logging_ev_system_dropped_root_n,
		m.settings_logging_ev_system_dropped_root_h
	],
	'system.data_dir_mismatch': [
		m.settings_logging_ev_system_data_dir_mismatch_n,
		m.settings_logging_ev_system_data_dir_mismatch_h
	],
	'system.data_dir_user_share': [
		m.settings_logging_ev_system_data_dir_user_share_n,
		m.settings_logging_ev_system_data_dir_user_share_h
	],
	'system.data_dir_fuse': [
		m.settings_logging_ev_system_data_dir_fuse_n,
		m.settings_logging_ev_system_data_dir_fuse_h
	],
	'system.data_dir_network': [
		m.settings_logging_ev_system_data_dir_network_n,
		m.settings_logging_ev_system_data_dir_network_h
	],
	'filters.floor_removals_pending': [
		m.settings_logging_ev_filters_floor_removals_pending_n,
		m.settings_logging_ev_filters_floor_removals_pending_h
	],
	'filters.subscription_floor_ignored': [
		m.settings_logging_ev_filters_subscription_floor_ignored_n,
		m.settings_logging_ev_filters_subscription_floor_ignored_h
	],
	'filters.subscription_refresh_failed': [
		m.settings_logging_ev_filters_subscription_refresh_failed_n,
		m.settings_logging_ev_filters_subscription_refresh_failed_h
	],
	'filters.subscription_held': [
		m.settings_logging_ev_filters_subscription_held_n,
		m.settings_logging_ev_filters_subscription_held_h
	],
	'filters.subscription_record_failed': [
		m.settings_logging_ev_filters_subscription_record_failed_n,
		m.settings_logging_ev_filters_subscription_record_failed_h
	],
	'filters.subscriptions_read_failed': [
		m.settings_logging_ev_filters_subscriptions_read_failed_n,
		m.settings_logging_ev_filters_subscriptions_read_failed_h
	],
	'filters.subscriptions_client_failed': [
		m.settings_logging_ev_filters_subscriptions_client_failed_n,
		m.settings_logging_ev_filters_subscriptions_client_failed_h
	],
	'filters.subscriptions_apply_failed': [
		m.settings_logging_ev_filters_subscriptions_apply_failed_n,
		m.settings_logging_ev_filters_subscriptions_apply_failed_h
	],
	'alerts.search_unreadable': [
		m.settings_logging_ev_alerts_search_unreadable_n,
		m.settings_logging_ev_alerts_search_unreadable_h
	],
	'alerts.announce_failed': [
		m.settings_logging_ev_alerts_announce_failed_n,
		m.settings_logging_ev_alerts_announce_failed_h
	],
	'alerts.run': [m.settings_logging_ev_alerts_run_n, m.settings_logging_ev_alerts_run_h],
	'alerts.found': [m.settings_logging_ev_alerts_found_n, m.settings_logging_ev_alerts_found_h],
	'alerts.pruned': [m.settings_logging_ev_alerts_pruned_n, m.settings_logging_ev_alerts_pruned_h],
	'alerts.prune_failed': [
		m.settings_logging_ev_alerts_prune_failed_n,
		m.settings_logging_ev_alerts_prune_failed_h
	],
	'alerts.watermark_failed': [
		m.settings_logging_ev_alerts_watermark_failed_n,
		m.settings_logging_ev_alerts_watermark_failed_h
	],
	'alerts.run_failed': [
		m.settings_logging_ev_alerts_run_failed_n,
		m.settings_logging_ev_alerts_run_failed_h
	],
	'address.pending_unreadable': [
		m.settings_logging_ev_address_pending_unreadable_n,
		m.settings_logging_ev_address_pending_unreadable_h
	],
	'address.save_failed': [
		m.settings_logging_ev_address_save_failed_n,
		m.settings_logging_ev_address_save_failed_h
	],
	'address.env_holds': [
		m.settings_logging_ev_address_env_holds_n,
		m.settings_logging_ev_address_env_holds_h
	],
	'address.change_dropped': [
		m.settings_logging_ev_address_change_dropped_n,
		m.settings_logging_ev_address_change_dropped_h
	],
	'address.reverting': [
		m.settings_logging_ev_address_reverting_n,
		m.settings_logging_ev_address_reverting_h
	],
	'address.revert_asked': [
		m.settings_logging_ev_address_revert_asked_n,
		m.settings_logging_ev_address_revert_asked_h
	],
	'address.bind_failed': [
		m.settings_logging_ev_address_bind_failed_n,
		m.settings_logging_ev_address_bind_failed_h
	],
	'address.revert_save_failed': [
		m.settings_logging_ev_address_revert_save_failed_n,
		m.settings_logging_ev_address_revert_save_failed_h
	],
	'address.confirm_needed': [
		m.settings_logging_ev_address_confirm_needed_n,
		m.settings_logging_ev_address_confirm_needed_h
	],
	'address.serving_fallback': [
		m.settings_logging_ev_address_serving_fallback_n,
		m.settings_logging_ev_address_serving_fallback_h
	],
	'address.kept': [m.settings_logging_ev_address_kept_n, m.settings_logging_ev_address_kept_h],
	'address.change_undone': [
		m.settings_logging_ev_address_change_undone_n,
		m.settings_logging_ev_address_change_undone_h
	],
	'address.applies_at_restart': [
		m.settings_logging_ev_address_applies_at_restart_n,
		m.settings_logging_ev_address_applies_at_restart_h
	],
	'address.revert_failed': [
		m.settings_logging_ev_address_revert_failed_n,
		m.settings_logging_ev_address_revert_failed_h
	],
	'app.reexec': [m.settings_logging_ev_app_reexec_n, m.settings_logging_ev_app_reexec_h],
	'windows.ports_low': [
		m.settings_logging_ev_windows_ports_low_n,
		m.settings_logging_ev_windows_ports_low_h
	],
	'windows.ports_ok': [
		m.settings_logging_ev_windows_ports_ok_n,
		m.settings_logging_ev_windows_ports_ok_h
	],
	'windows.ports_unreadable': [
		m.settings_logging_ev_windows_ports_unreadable_n,
		m.settings_logging_ev_windows_ports_unreadable_h
	],
	'windows.priority_unknown': [
		m.settings_logging_ev_windows_priority_unknown_n,
		m.settings_logging_ev_windows_priority_unknown_h
	],
	'windows.priority_lowered': [
		m.settings_logging_ev_windows_priority_lowered_n,
		m.settings_logging_ev_windows_priority_lowered_h
	],
	'windows.priority_failed': [
		m.settings_logging_ev_windows_priority_failed_n,
		m.settings_logging_ev_windows_priority_failed_h
	],
	'service.starting': [
		m.settings_logging_ev_service_starting_n,
		m.settings_logging_ev_service_starting_h
	],
	'service.stopping': [
		m.settings_logging_ev_service_stopping_n,
		m.settings_logging_ev_service_stopping_h
	],
	'service.stopped': [
		m.settings_logging_ev_service_stopped_n,
		m.settings_logging_ev_service_stopped_h
	],
	'service.witness_refused': [
		m.settings_logging_ev_service_witness_refused_n,
		m.settings_logging_ev_service_witness_refused_h
	],
	'service.failed': [
		m.settings_logging_ev_service_failed_n,
		m.settings_logging_ev_service_failed_h
	],
	'fixture.mode': [m.settings_logging_ev_fixture_mode_n, m.settings_logging_ev_fixture_mode_h],
	'fixture.stale': [m.settings_logging_ev_fixture_stale_n, m.settings_logging_ev_fixture_stale_h],
	'fixture.building': [
		m.settings_logging_ev_fixture_building_n,
		m.settings_logging_ev_fixture_building_h
	],
	'fixture.built': [m.settings_logging_ev_fixture_built_n, m.settings_logging_ev_fixture_built_h],
	'fixture.serving': [
		m.settings_logging_ev_fixture_serving_n,
		m.settings_logging_ev_fixture_serving_h
	],
	'fixture.claimed': [
		m.settings_logging_ev_fixture_claimed_n,
		m.settings_logging_ev_fixture_claimed_h
	],
	'fixture.password_ignored': [
		m.settings_logging_ev_fixture_password_ignored_n,
		m.settings_logging_ev_fixture_password_ignored_h
	],
	'fixture.public': [
		m.settings_logging_ev_fixture_public_n,
		m.settings_logging_ev_fixture_public_h
	],
	'fixture.cleanup_failed': [
		m.settings_logging_ev_fixture_cleanup_failed_n,
		m.settings_logging_ev_fixture_cleanup_failed_h
	],
	'fixture.failed': [
		m.settings_logging_ev_fixture_failed_n,
		m.settings_logging_ev_fixture_failed_h
	],
	'insights.recorder_stopped': [
		m.settings_logging_ev_insights_recorder_stopped_n,
		m.settings_logging_ev_insights_recorder_stopped_h
	],
	'insights.step_failed': [
		m.settings_logging_ev_insights_step_failed_n,
		m.settings_logging_ev_insights_step_failed_h
	],
	'insights.unknown_metrics': [
		m.settings_logging_ev_insights_unknown_metrics_n,
		m.settings_logging_ev_insights_unknown_metrics_h
	],
	'insights.compacted': [
		m.settings_logging_ev_insights_compacted_n,
		m.settings_logging_ev_insights_compacted_h
	],
	'insights.repaired': [
		m.settings_logging_ev_insights_repaired_n,
		m.settings_logging_ev_insights_repaired_h
	],
	'app.hot_columns': [
		m.settings_logging_ev_app_hot_columns_n,
		m.settings_logging_ev_app_hot_columns_h
	],
	'app.serving': [m.settings_logging_ev_app_serving_n, m.settings_logging_ev_app_serving_h],
	'app.stopping': [m.settings_logging_ev_app_stopping_n, m.settings_logging_ev_app_stopping_h],
	'app.hot_save_failed': [
		m.settings_logging_ev_app_hot_save_failed_n,
		m.settings_logging_ev_app_hot_save_failed_h
	],
	'app.shutdown_forced': [
		m.settings_logging_ev_app_shutdown_forced_n,
		m.settings_logging_ev_app_shutdown_forced_h
	],
	'app.command_failed': [
		m.settings_logging_ev_app_command_failed_n,
		m.settings_logging_ev_app_command_failed_h
	],
	'app.import_progress': [
		m.settings_logging_ev_app_import_progress_n,
		m.settings_logging_ev_app_import_progress_h
	],
	'app.stopped_cleanly': [
		m.settings_logging_ev_app_stopped_cleanly_n,
		m.settings_logging_ev_app_stopped_cleanly_h
	],
	'api.internal_error': [
		m.settings_logging_ev_api_internal_error_n,
		m.settings_logging_ev_api_internal_error_h
	],
	'api.request': [m.settings_logging_ev_api_request_n, m.settings_logging_ev_api_request_h],
	'filters.rule_set_updated': [
		m.settings_logging_ev_filters_rule_set_updated_n,
		m.settings_logging_ev_filters_rule_set_updated_h
	],
	'filters.change_detail': [
		m.settings_logging_ev_filters_change_detail_n,
		m.settings_logging_ev_filters_change_detail_h
	],
	'filters.rule_skipped': [
		m.settings_logging_ev_filters_rule_skipped_n,
		m.settings_logging_ev_filters_rule_skipped_h
	],
	'filters.fixture_unreadable': [
		m.settings_logging_ev_filters_fixture_unreadable_n,
		m.settings_logging_ev_filters_fixture_unreadable_h
	],
	'auth.setup_complete': [
		m.settings_logging_ev_auth_setup_complete_n,
		m.settings_logging_ev_auth_setup_complete_h
	],
	'auth.sign_in': [m.settings_logging_ev_auth_sign_in_n, m.settings_logging_ev_auth_sign_in_h],
	'auth.sign_in_failed': [
		m.settings_logging_ev_auth_sign_in_failed_n,
		m.settings_logging_ev_auth_sign_in_failed_h
	],
	'auth.rate_limited': [
		m.settings_logging_ev_auth_rate_limited_n,
		m.settings_logging_ev_auth_rate_limited_h
	],
	'auth.password_changed': [
		m.settings_logging_ev_auth_password_changed_n,
		m.settings_logging_ev_auth_password_changed_h
	],
	'auth.sessions_revoked': [
		m.settings_logging_ev_auth_sessions_revoked_n,
		m.settings_logging_ev_auth_sessions_revoked_h
	],
	'auth.proxy_changed': [
		m.settings_logging_ev_auth_proxy_changed_n,
		m.settings_logging_ev_auth_proxy_changed_h
	],
	'auth.proxy_forced_off': [
		m.settings_logging_ev_auth_proxy_forced_off_n,
		m.settings_logging_ev_auth_proxy_forced_off_h
	],
	'auth.proxy_override_ignored': [
		m.settings_logging_ev_auth_proxy_override_ignored_n,
		m.settings_logging_ev_auth_proxy_override_ignored_h
	],
	'auth.proxy_settings_invalid': [
		m.settings_logging_ev_auth_proxy_settings_invalid_n,
		m.settings_logging_ev_auth_proxy_settings_invalid_h
	],
	'auth.proxy_wide_range': [
		m.settings_logging_ev_auth_proxy_wide_range_n,
		m.settings_logging_ev_auth_proxy_wide_range_h
	],
	'auth.key_refused': [
		m.settings_logging_ev_auth_key_refused_n,
		m.settings_logging_ev_auth_key_refused_h
	],
	'auth.key_use_unrecorded': [
		m.settings_logging_ev_auth_key_use_unrecorded_n,
		m.settings_logging_ev_auth_key_use_unrecorded_h
	],
	'auth.dir_sync_failed': [
		m.settings_logging_ev_auth_dir_sync_failed_n,
		m.settings_logging_ev_auth_dir_sync_failed_h
	],
	'filters.ip_lists_broad': [
		m.settings_logging_ev_filters_ip_lists_broad_n,
		m.settings_logging_ev_filters_ip_lists_broad_h
	],
	'integrations.client_request': [
		m.settings_logging_ev_integrations_client_request_n,
		m.settings_logging_ev_integrations_client_request_h
	],
	'integrations.client_left_paused': [
		m.settings_logging_ev_integrations_client_left_paused_n,
		m.settings_logging_ev_integrations_client_left_paused_h
	],
	'integrations.client_no_labels': [
		m.settings_logging_ev_integrations_client_no_labels_n,
		m.settings_logging_ev_integrations_client_no_labels_h
	],
	'integrations.client_label_failed': [
		m.settings_logging_ev_integrations_client_label_failed_n,
		m.settings_logging_ev_integrations_client_label_failed_h
	],
	'integrations.notify_attempt': [
		m.settings_logging_ev_integrations_notify_attempt_n,
		m.settings_logging_ev_integrations_notify_attempt_h
	],
	'integrations.notify_retry': [
		m.settings_logging_ev_integrations_notify_retry_n,
		m.settings_logging_ev_integrations_notify_retry_h
	],
	'integrations.notify_failed': [
		m.settings_logging_ev_integrations_notify_failed_n,
		m.settings_logging_ev_integrations_notify_failed_h
	],
	'integrations.secret_key_tightened': [
		m.settings_logging_ev_integrations_secret_key_tightened_n,
		m.settings_logging_ev_integrations_secret_key_tightened_h
	],
	'integrations.secret_key_acl_failed': [
		m.settings_logging_ev_integrations_secret_key_acl_failed_n,
		m.settings_logging_ev_integrations_secret_key_acl_failed_h
	],
	'pipeline.scrape_failed': [
		m.settings_logging_ev_pipeline_scrape_failed_n,
		m.settings_logging_ev_pipeline_scrape_failed_h
	],
	'pipeline.http_trackers_disabled': [
		m.settings_logging_ev_pipeline_http_trackers_disabled_n,
		m.settings_logging_ev_pipeline_http_trackers_disabled_h
	],
	'pipeline.tracker_skipped': [
		m.settings_logging_ev_pipeline_tracker_skipped_n,
		m.settings_logging_ev_pipeline_tracker_skipped_h
	],
	'pipeline.hot_snapshot_unreadable': [
		m.settings_logging_ev_pipeline_hot_snapshot_unreadable_n,
		m.settings_logging_ev_pipeline_hot_snapshot_unreadable_h
	],
	'pipeline.scheduler_store_failed': [
		m.settings_logging_ev_pipeline_scheduler_store_failed_n,
		m.settings_logging_ev_pipeline_scheduler_store_failed_h
	],
	'pipeline.priority_unreadable': [
		m.settings_logging_ev_pipeline_priority_unreadable_n,
		m.settings_logging_ev_pipeline_priority_unreadable_h
	],
	'pipeline.crawler_toggled': [
		m.settings_logging_ev_pipeline_crawler_toggled_n,
		m.settings_logging_ev_pipeline_crawler_toggled_h
	],
	'pipeline.event_unrecorded': [
		m.settings_logging_ev_pipeline_event_unrecorded_n,
		m.settings_logging_ev_pipeline_event_unrecorded_h
	],
	'pipeline.source_state_changed': [
		m.settings_logging_ev_pipeline_source_state_changed_n,
		m.settings_logging_ev_pipeline_source_state_changed_h
	],
	'pipeline.dht_started': [
		m.settings_logging_ev_pipeline_dht_started_n,
		m.settings_logging_ev_pipeline_dht_started_h
	],
	'pipeline.cs_origin_unknown': [
		m.settings_logging_ev_pipeline_cs_origin_unknown_n,
		m.settings_logging_ev_pipeline_cs_origin_unknown_h
	],
	'pipeline.starting': [
		m.settings_logging_ev_pipeline_starting_n,
		m.settings_logging_ev_pipeline_starting_h
	],
	'pipeline.filter_rule_skipped': [
		m.settings_logging_ev_pipeline_filter_rule_skipped_n,
		m.settings_logging_ev_pipeline_filter_rule_skipped_h
	],
	'pipeline.source_not_started': [
		m.settings_logging_ev_pipeline_source_not_started_n,
		m.settings_logging_ev_pipeline_source_not_started_h
	],
	'pipeline.source_failed': [
		m.settings_logging_ev_pipeline_source_failed_n,
		m.settings_logging_ev_pipeline_source_failed_h
	],
	'pipeline.source_stop_failed': [
		m.settings_logging_ev_pipeline_source_stop_failed_n,
		m.settings_logging_ev_pipeline_source_stop_failed_h
	],
	'pipeline.source_stop_timeout': [
		m.settings_logging_ev_pipeline_source_stop_timeout_n,
		m.settings_logging_ev_pipeline_source_stop_timeout_h
	],
	'pipeline.lease_missing': [
		m.settings_logging_ev_pipeline_lease_missing_n,
		m.settings_logging_ev_pipeline_lease_missing_h
	],
	'pipeline.prepare_failed': [
		m.settings_logging_ev_pipeline_prepare_failed_n,
		m.settings_logging_ev_pipeline_prepare_failed_h
	],
	'pipeline.checkpoint_failed': [
		m.settings_logging_ev_pipeline_checkpoint_failed_n,
		m.settings_logging_ev_pipeline_checkpoint_failed_h
	],
	'pipeline.snapshot_expired': [
		m.settings_logging_ev_pipeline_snapshot_expired_n,
		m.settings_logging_ev_pipeline_snapshot_expired_h
	],
	'pipeline.snapshot_purge_problems': [
		m.settings_logging_ev_pipeline_snapshot_purge_problems_n,
		m.settings_logging_ev_pipeline_snapshot_purge_problems_h
	],
	'pipeline.filter_purge_failed': [
		m.settings_logging_ev_pipeline_filter_purge_failed_n,
		m.settings_logging_ev_pipeline_filter_purge_failed_h
	],
	'pipeline.store_write_failed': [
		m.settings_logging_ev_pipeline_store_write_failed_n,
		m.settings_logging_ev_pipeline_store_write_failed_h
	],
	'pipeline.enrichment_failed': [
		m.settings_logging_ev_pipeline_enrichment_failed_n,
		m.settings_logging_ev_pipeline_enrichment_failed_h
	],
	'pipeline.have_rebuild_failed': [
		m.settings_logging_ev_pipeline_have_rebuild_failed_n,
		m.settings_logging_ev_pipeline_have_rebuild_failed_h
	],
	'pipeline.queue_save_failed': [
		m.settings_logging_ev_pipeline_queue_save_failed_n,
		m.settings_logging_ev_pipeline_queue_save_failed_h
	],
	'search.suggest_titles_rebuilt': [
		m.settings_logging_ev_search_suggest_titles_rebuilt_n,
		m.settings_logging_ev_search_suggest_titles_rebuilt_h
	],
	'search.suggest_titles_failed': [
		m.settings_logging_ev_search_suggest_titles_failed_n,
		m.settings_logging_ev_search_suggest_titles_failed_h
	],
	'search.percolate_search_failed': [
		m.settings_logging_ev_search_percolate_search_failed_n,
		m.settings_logging_ev_search_percolate_search_failed_h
	],
	'search.rename_retry': [
		m.settings_logging_ev_search_rename_retry_n,
		m.settings_logging_ev_search_rename_retry_h
	],
	'search.rebuild_started': [
		m.settings_logging_ev_search_rebuild_started_n,
		m.settings_logging_ev_search_rebuild_started_h
	],
	'search.rebuild_failed': [
		m.settings_logging_ev_search_rebuild_failed_n,
		m.settings_logging_ev_search_rebuild_failed_h
	],
	'search.rebuild_retry': [
		m.settings_logging_ev_search_rebuild_retry_n,
		m.settings_logging_ev_search_rebuild_retry_h
	],
	'search.swapped': [
		m.settings_logging_ev_search_swapped_n,
		m.settings_logging_ev_search_swapped_h
	],
	'search.stop_failed': [
		m.settings_logging_ev_search_stop_failed_n,
		m.settings_logging_ev_search_stop_failed_h
	],
	'search.old_indexer_restart_failed': [
		m.settings_logging_ev_search_old_indexer_restart_failed_n,
		m.settings_logging_ev_search_old_indexer_restart_failed_h
	],
	'search.version_skipped': [
		m.settings_logging_ev_search_version_skipped_n,
		m.settings_logging_ev_search_version_skipped_h
	],
	'search.open_retry': [
		m.settings_logging_ev_search_open_retry_n,
		m.settings_logging_ev_search_open_retry_h
	],
	'search.retired_still_open': [
		m.settings_logging_ev_search_retired_still_open_n,
		m.settings_logging_ev_search_retired_still_open_h
	],
	'search.cleanup_retry': [
		m.settings_logging_ev_search_cleanup_retry_n,
		m.settings_logging_ev_search_cleanup_retry_h
	],
	'search.cleanup_left': [
		m.settings_logging_ev_search_cleanup_left_n,
		m.settings_logging_ev_search_cleanup_left_h
	],
	'search.indexer_retry': [
		m.settings_logging_ev_search_indexer_retry_n,
		m.settings_logging_ev_search_indexer_retry_h
	],
	'search.indexer_count_failed': [
		m.settings_logging_ev_search_indexer_count_failed_n,
		m.settings_logging_ev_search_indexer_count_failed_h
	],
	'search.file_search_failed': [
		m.settings_logging_ev_search_file_search_failed_n,
		m.settings_logging_ev_search_file_search_failed_h
	],
	'search.enrichment_unreadable': [
		m.settings_logging_ev_search_enrichment_unreadable_n,
		m.settings_logging_ev_search_enrichment_unreadable_h
	],
	'dht.socket_buffers': [
		m.settings_logging_ev_dht_socket_buffers_n,
		m.settings_logging_ev_dht_socket_buffers_h
	],
	'dht.bootstrap_unreachable': [
		m.settings_logging_ev_dht_bootstrap_unreachable_n,
		m.settings_logging_ev_dht_bootstrap_unreachable_h
	],
	'dht.restart_check': [
		m.settings_logging_ev_dht_restart_check_n,
		m.settings_logging_ev_dht_restart_check_h
	],
	'dht.restart_commit': [
		m.settings_logging_ev_dht_restart_commit_n,
		m.settings_logging_ev_dht_restart_commit_h
	],
	'dht.restart_persist_failed': [
		m.settings_logging_ev_dht_restart_persist_failed_n,
		m.settings_logging_ev_dht_restart_persist_failed_h
	],
	'dht.key_harvested': [
		m.settings_logging_ev_dht_key_harvested_n,
		m.settings_logging_ev_dht_key_harvested_h
	],
	'dht.harvest_summary': [
		m.settings_logging_ev_dht_harvest_summary_n,
		m.settings_logging_ev_dht_harvest_summary_h
	],
	'fetch.failed': [m.settings_logging_ev_fetch_failed_n, m.settings_logging_ev_fetch_failed_h],
	'fetch.failed_summary': [
		m.settings_logging_ev_fetch_failed_summary_n,
		m.settings_logging_ev_fetch_failed_summary_h
	],
	'update.migrating': [
		m.settings_logging_ev_update_migrating_n,
		m.settings_logging_ev_update_migrating_h
	],
	'update.migration_unrecorded': [
		m.settings_logging_ev_update_migration_unrecorded_n,
		m.settings_logging_ev_update_migration_unrecorded_h
	],
	'update.available': [
		m.settings_logging_ev_update_available_n,
		m.settings_logging_ev_update_available_h
	],
	'update.check_failed': [
		m.settings_logging_ev_update_check_failed_n,
		m.settings_logging_ev_update_check_failed_h
	],
	'update.state_unsaved': [
		m.settings_logging_ev_update_state_unsaved_n,
		m.settings_logging_ev_update_state_unsaved_h
	],
	'update.state_unreadable': [
		m.settings_logging_ev_update_state_unreadable_n,
		m.settings_logging_ev_update_state_unreadable_h
	],
	'update.dir_sync_unsupported': [
		m.settings_logging_ev_update_dir_sync_unsupported_n,
		m.settings_logging_ev_update_dir_sync_unsupported_h
	],
	'update.overlay_unreadable': [
		m.settings_logging_ev_update_overlay_unreadable_n,
		m.settings_logging_ev_update_overlay_unreadable_h
	],
	'update.probation_passed': [
		m.settings_logging_ev_update_probation_passed_n,
		m.settings_logging_ev_update_probation_passed_h
	],
	'update.probation_failed': [
		m.settings_logging_ev_update_probation_failed_n,
		m.settings_logging_ev_update_probation_failed_h
	]
};
