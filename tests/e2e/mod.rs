mod session;

macro_rules! e2e_test {
    ($name:ident) => {
        #[test]
        #[ignore = "requires POD_E2E=1 and herdr 0.9.3 or newer"]
        fn $name() {
            session::run(stringify!($name));
        }
    };
}

e2e_test!(e2e_run_create_worker_start_ask_reply_heartbeat_done_release);
e2e_test!(e2e_nudge_delivered_to_idle_fake_agent_and_consumed_by_check);
e2e_test!(e2e_pane_closed_by_user_becomes_exited_and_release_leaves_it);
e2e_test!(e2e_herdr_restart_reconciles_and_ladder_count_persists);
e2e_test!(e2e_two_workers_specialist_ask_first_reply_wins);
e2e_test!(e2e_user_gate_wakes_check_wait_decision_gate);
e2e_test!(e2e_runs_on_linux);
