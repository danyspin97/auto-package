/// Tests that touch the process environment must not run concurrently.
pub(super) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
