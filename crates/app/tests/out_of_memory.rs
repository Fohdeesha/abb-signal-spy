#[test]
fn a_refused_allocation_leaves_a_note() {
    let dir = spy_core::testdir::TestDir::new("oom");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_abb-signal-spy"))
        .env("ABB_SIGNAL_SPY_DATA", dir.path())
        .env("ABB_SIGNAL_SPY_TEST_OUT_OF_MEMORY", "1")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3), "the request was not refused, or the program went on");
    let note = std::fs::read_to_string(dir.join("crash.txt")).expect("no crash.txt: the refusal left no note");
    assert!(note.contains("Windows refused it 1125899906842624 bytes of memory."), "{note}");
    let log = std::fs::read_to_string(dir.join("signal-spy.log")).unwrap_or_default();
    assert!(log.contains(" ERROR Windows refused 1125899906842624 bytes of memory"), "{log}");
}
