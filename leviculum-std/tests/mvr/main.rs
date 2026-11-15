//! Minimum-viable-reproduction (mvr) test tier.
//!
//! See CLAUDE.md §Protocol debugging discipline for the policy these tests
//! implement. Each file here isolates one named protocol-layer failure from
//! the full-scenario tests so that it runs deterministically in seconds
//! rather than minutes, with full structured event logs on failure.
//!
//! mvrs must not depend on LoRa hardware, Docker, or Python. When a full-
//! scenario bug reproduces over a non-LoRa transport, the mvr builds that
//! transport from process primitives and holds the rest of the protocol
//! stack (daemon, client tools, resource machinery) unchanged.

// Shared harness — loaded once here so all mvr test files can reach
// it via `use crate::harness::...`.  Loading it once avoids the
// `clippy::duplicate_mod` warning that fires when each mvr file
// pulls in `#[path] mod harness;` independently.
#[path = "../rnsd_interop/harness.rs"]
#[allow(dead_code)]
pub mod harness;

/// Who picks a port and when: the consumer binds port 0 wherever it can.
#[path = "../support/ports.rs"]
pub mod ports;

/// The host-wide allocator, where `ports::free_port` draws.
pub use harness::port_alloc;

/// The counting allocator and the release-binary check (support.rs).
mod support;
pub use support::{alloc_probe, assert_release_bin_fresh};

// The mvr modules, one `mod` line each and nothing after them.
//
// `.gitattributes` gives this file git's `union` merge driver: two branches
// that each add an mvr here merge without a conflict, where a text merge
// stops on two neighbouring additions (order 1848; before it, every landing
// of one lane conflicted with the other on this list). That is safe because
// everything above changes rarely and every line below stands alone: a
// module gated on a feature says so in its own file (`#![cfg(feature =
// "serial")]` at its top, #463, `just lnsd-minimal`), not in a line here. Add
// the new line where it sorts; `scripts/registry-merge-selftest.py` shows
// the merge. A module listed twice does not compile.
mod a_connection_flood_starves_the_tcp_server;
mod a_dead_sync_link_ends_the_round_beside_a_second_node;
mod a_frame_the_modem_consumed_without_transmitting;
mod a_frame_torn_by_a_gap_leaves_the_serial_decoder_in_phase;
mod a_link_proof_later_than_six_rtts_fails_the_python_receipt;
mod a_lost_link_rtt_strands_the_resource_transfer;
mod a_new_interface_hears_an_announce_made_before_it;
mod a_raw_xoff_mutes_a_single_uart_board;
mod a_repeated_path_request_queues_one_response_on_the_board;
mod a_returning_phone_is_asked_for_its_stored_mail;
mod a_stranger_inside_the_port_handoff_window;
mod a_third_announce_on_a_fresh_interface_waits_at_python;
mod an_application_path_request_inside_the_interval_leaves;
mod an_older_emissions_echo_keeps_the_renamed_announce;
mod announce_emission_unix_time;
mod burst_continuation_contends_inside_its_own_answer;
mod cad_tears_down_the_frame_it_then_detects;
mod client_wait_for_path_request_fallback;
mod core_lock_reentrancy;
mod core_processor_seam;
mod duty_hold_relay_advertises_no_route_until_the_hold_lifts;
mod forwarding_daemon_forgets_the_request_before_the_proof_returns;
mod iface_online_after_task_death;
mod link_failure_recovery_silent_resume;
mod lncp_fetch_rust_responder;
mod lnsd_learns_the_serial_mux_from_the_board;
mod local_client_announce_burst_half_duplex;
mod local_clients_announces_on_a_fresh_interface_reach_python;
mod lxmf_opportunistic_fallback_threshold;
mod media_silence_restore_signal;
mod one_frame_in_the_modem_at_a_time;
mod pn_offer_outgrows_the_link_mdu;
mod pn_serve_cap_bounds_one_fetch;
mod pn_serve_cap_survives_a_shrinking_heap;
mod pn_serve_peak_outgrows_the_board_heap;
mod radio_config_sleeps_through_the_peer_yield_window;
mod radio_config_wedges_without_lora_task;
mod ratchet_rotation_single_packet;
mod rate_window_withholds_no_reference_relay;
mod recall_survives_a_restart;
mod reliable_channel_delivery_backpressure;
mod resource_consecutive_push_window_policy;
mod responder_close_delivery;
mod retained_mark_survives_a_daemon_swap;
mod rust_client_path_install_from_python;
mod rust_client_path_install_loop_race;
mod rust_client_path_install_via_relay;
mod rust_client_path_install_with_own_echo;
mod sender_modem_counts_a_frame_the_far_modem_never_hears;
mod shared_instance_client_survives_daemon_restart;
mod storage_flush_holds_the_core_lock;
mod tcp_client_unresolvable_target;
mod two_emissions_in_one_second_both_leave_the_relay;
mod two_responders_overlap_inside_one_airtime;
mod udp_hostname_forward;
mod unparsable_config_must_not_start_a_daemon;
