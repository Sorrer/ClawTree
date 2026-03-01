mod test_helpers;

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use worktree_claude_tui::git;

/// Verify that concurrent git operations without a lock can cause failures.
/// Then verify that the same operations succeed when serialized via a mutex.
///
/// This test spawns multiple threads that all run `git status` and `git log`
/// against the same worktree simultaneously — mimicking the real race between
/// the status poller, immediate fetch, and user operations.
#[test]
fn test_concurrent_git_status_without_lock_is_unreliable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    test_helpers::setup_bare_repo_with_worktree(&dir);

    let wt = dir.join("main");
    // Create some files so git status has work to do
    for i in 0..5 {
        test_helpers::create_file(&wt, &format!("file_{}.txt", i), &format!("content {}", i));
    }

    // Hammer git with concurrent status + stage + commit operations.
    // Without locking, this can produce index.lock errors.
    let wt = Arc::new(wt);
    let errors = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for _ in 0..4 {
        let wt = Arc::clone(&wt);
        let errors = Arc::clone(&errors);
        handles.push(std::thread::spawn(move || {
            for _ in 0..10 {
                if git::status_porcelain(&wt).is_err() {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
                if git::log_oneline(&wt, 5).is_err() {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // We don't assert errors > 0 here because the race is non-deterministic.
    // The point of this test is to contrast with the locked version below.
    let unlocked_errors = errors.load(Ordering::Relaxed);
    eprintln!("Unlocked concurrent git ops: {} errors (non-deterministic)", unlocked_errors);
}

/// Verify that serializing git operations via a mutex eliminates index.lock errors.
#[test]
fn test_concurrent_git_ops_with_lock_no_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    test_helpers::setup_bare_repo_with_worktree(&dir);

    let wt = dir.join("main");
    for i in 0..5 {
        test_helpers::create_file(&wt, &format!("file_{}.txt", i), &format!("content {}", i));
    }

    let wt = Arc::new(wt);
    let git_lock = Arc::new(Mutex::new(()));
    let errors = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for _ in 0..4 {
        let wt = Arc::clone(&wt);
        let git_lock = Arc::clone(&git_lock);
        let errors = Arc::clone(&errors);
        handles.push(std::thread::spawn(move || {
            for _ in 0..10 {
                {
                    let _guard = git_lock.lock().unwrap();
                    if git::status_porcelain(&wt).is_err() {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                {
                    let _guard = git_lock.lock().unwrap();
                    if git::log_oneline(&wt, 5).is_err() {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let locked_errors = errors.load(Ordering::Relaxed);
    assert_eq!(locked_errors, 0, "Expected zero errors with lock, got {}", locked_errors);
}

/// Verify that try_lock correctly skips when the lock is held,
/// matching the status poller behavior.
#[test]
fn test_try_lock_skips_when_held() {
    let git_lock = Arc::new(Mutex::new(()));
    let skipped = Arc::new(AtomicUsize::new(0));
    let acquired = Arc::new(AtomicUsize::new(0));

    // Hold the lock on the main thread (simulating a user operation)
    let _guard = git_lock.lock().unwrap();

    let mut handles = Vec::new();
    for _ in 0..4 {
        let git_lock = Arc::clone(&git_lock);
        let skipped = Arc::clone(&skipped);
        let acquired = Arc::clone(&acquired);
        handles.push(std::thread::spawn(move || {
            // Simulate the status poller using try_lock
            match git_lock.try_lock() {
                Ok(_guard) => {
                    acquired.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(skipped.load(Ordering::Relaxed), 4, "All poller threads should skip when lock is held");
    assert_eq!(acquired.load(Ordering::Relaxed), 0, "No poller thread should acquire a held lock");
}

/// Verify that try_lock succeeds when no user operation is running.
#[test]
fn test_try_lock_succeeds_when_free() {
    let git_lock = Arc::new(Mutex::new(()));
    let acquired = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let git_lock = Arc::clone(&git_lock);
        let acquired = Arc::clone(&acquired);
        handles.push(std::thread::spawn(move || {
            // Each thread tries, does its work, releases
            if let Ok(_guard) = git_lock.try_lock() {
                acquired.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
        // Small delay so threads don't all race on an unlocked mutex
        std::thread::sleep(Duration::from_millis(5));
    }

    for h in handles {
        h.join().unwrap();
    }

    // At least some threads should have acquired the lock
    assert!(acquired.load(Ordering::Relaxed) > 0, "At least one thread should acquire the free lock");
}

/// Simulate the real pattern: a "user operation" thread uses lock() while
/// "poller" threads use try_lock(). Verify the user op always completes
/// and pollers never produce errors.
#[test]
fn test_mixed_lock_and_try_lock_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    test_helpers::setup_bare_repo_with_worktree(&dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "test.txt", "data");

    let wt = Arc::new(wt);
    let git_lock = Arc::new(Mutex::new(()));
    let user_op_completed = Arc::new(AtomicUsize::new(0));
    let poller_errors = Arc::new(AtomicUsize::new(0));
    let poller_skipped = Arc::new(AtomicUsize::new(0));
    let poller_success = Arc::new(AtomicUsize::new(0));

    // Spawn "user operation" threads that use lock() (like commit, push, stage)
    let mut handles = Vec::new();
    for _ in 0..2 {
        let wt = Arc::clone(&wt);
        let git_lock = Arc::clone(&git_lock);
        let completed = Arc::clone(&user_op_completed);
        handles.push(std::thread::spawn(move || {
            for _ in 0..5 {
                let _guard = git_lock.lock().unwrap();
                // Simulate a multi-step git operation (status check + stage)
                let _ = git::status_porcelain(&wt);
                std::thread::sleep(Duration::from_millis(5));
                completed.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    // Spawn "poller" threads that use try_lock() (like status poller)
    for _ in 0..2 {
        let wt = Arc::clone(&wt);
        let git_lock = Arc::clone(&git_lock);
        let errors = Arc::clone(&poller_errors);
        let skipped = Arc::clone(&poller_skipped);
        let success = Arc::clone(&poller_success);
        handles.push(std::thread::spawn(move || {
            for _ in 0..20 {
                match git_lock.try_lock() {
                    Ok(_guard) => {
                        if git::status_porcelain(&wt).is_err() {
                            errors.fetch_add(1, Ordering::Relaxed);
                        } else {
                            success.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(_) => {
                        skipped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(user_op_completed.load(Ordering::Relaxed), 10,
        "All user operations should complete");
    assert_eq!(poller_errors.load(Ordering::Relaxed), 0,
        "Poller should never get git errors when using lock");

    let skipped = poller_skipped.load(Ordering::Relaxed);
    let success = poller_success.load(Ordering::Relaxed);
    eprintln!("Poller: {} success, {} skipped (both are fine)", success, skipped);
    assert!(success + skipped == 40, "All poller iterations should either succeed or skip");
}

/// Verify that lock() blocks until the holder releases, rather than failing.
/// This confirms user operations (push/pull/commit) will wait for the poller
/// to finish its current fetch rather than erroring out.
#[test]
fn test_lock_waits_for_release() {
    let git_lock = Arc::new(Mutex::new(()));
    let order = Arc::new(Mutex::new(Vec::<&str>::new()));

    let git_lock2 = Arc::clone(&git_lock);
    let order2 = Arc::clone(&order);

    // Thread 1: hold the lock for 100ms (simulating a poller fetch)
    let h1 = std::thread::spawn(move || {
        let _guard = git_lock2.lock().unwrap();
        order2.lock().unwrap().push("holder_start");
        std::thread::sleep(Duration::from_millis(100));
        order2.lock().unwrap().push("holder_end");
    });

    // Give thread 1 time to acquire the lock
    std::thread::sleep(Duration::from_millis(10));

    // Thread 2 (main): try to acquire — should block, not fail
    let start = Instant::now();
    let _guard = git_lock.lock().unwrap();
    let waited = start.elapsed();
    order.lock().unwrap().push("waiter_acquired");

    h1.join().unwrap();

    let sequence = order.lock().unwrap().clone();
    assert_eq!(sequence, vec!["holder_start", "holder_end", "waiter_acquired"],
        "Waiter should acquire only after holder releases");
    assert!(waited >= Duration::from_millis(50),
        "Waiter should have blocked for a meaningful duration, waited {:?}", waited);
}
