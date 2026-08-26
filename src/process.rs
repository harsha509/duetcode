//! Helpers for child processes: bounded waits, deadline kills, pipe draining.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);

/// Exit-wait budget once a child has closed its output. The run's real budget
/// was already spent streaming; this only bounds a child that will not exit.
pub(crate) const REAP_GRACE: Duration = Duration::from_secs(10);

/// Live child process groups, killed by the signal handler when dt dies so a
/// half-done CLI run is not orphaned. Fixed-size and lock-free: the handler
/// may only touch async-signal-safe state. A zero slot is free.
static GROUPS: [AtomicI32; 16] = [const { AtomicI32::new(0) }; 16];

/// Active turn depth. Non-zero while a task or review is in flight; gates
/// what the first Ctrl+C means: stop the turn, or tear everything down.
static TURN_ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// Set when the user asked to stop the current turn (first Ctrl+C mid-turn).
static CANCELLED: AtomicBool = AtomicBool::new(false);

extern "C" fn kill_groups_and_die(signal: libc::c_int) {
    // First Ctrl+C during a turn only asks it to stop; every other signal,
    // and another press, tears dt and every child down.
    if signal == libc::SIGINT
        && TURN_ACTIVE.load(Ordering::Acquire) > 0
        && !CANCELLED.swap(true, Ordering::AcqRel)
    {
        const NOTICE: &[u8] =
            b"\n(stopping the current turn \xE2\x80\x94 press Ctrl+C again to quit)\n";
        unsafe {
            libc::write(2, NOTICE.as_ptr().cast(), NOTICE.len());
        }
        return;
    }
    for slot in &GROUPS {
        let pgid = slot.load(Ordering::Relaxed);
        if pgid > 0 {
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Children run in their own process groups, so a terminal signal no longer
/// reaches them through the foreground group — dt forwards the kill instead.
fn install_signal_forwarding() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let handler: extern "C" fn(libc::c_int) = kill_groups_and_die;
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            unsafe {
                // A disposition set to ignore (nohup) stays ignored.
                if libc::signal(signal, handler as libc::sighandler_t) == libc::SIG_IGN {
                    libc::signal(signal, libc::SIG_IGN);
                }
            }
        }
    });
}

/// Whether the user asked to stop the current turn.
pub(crate) fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::Acquire)
}

/// Whether an in-flight turn should unwind now. Gated on a turn being active
/// so a stray flag left over between turns can never kill an unrelated child.
fn stop_requested() -> bool {
    TURN_ACTIVE.load(Ordering::Acquire) > 0 && CANCELLED.load(Ordering::Acquire)
}

/// Requests that the current turn stop at its next check.
pub(crate) fn request_cancel() {
    CANCELLED.store(true, Ordering::Release);
}

/// Marks the span of one task/review turn: while active, the first Ctrl+C
/// stops the turn instead of killing dt. Supports nesting; only the outermost
/// guard resets the cancel flag on entry and deactivates on exit.
pub(crate) struct TurnGuard;

impl TurnGuard {
    pub(crate) fn new() -> Self {
        let prev = TURN_ACTIVE.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            CANCELLED.store(false, Ordering::Release);
        }
        TurnGuard
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        TURN_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

fn register_group(pgid: u32) {
    for slot in &GROUPS {
        if slot.compare_exchange(0, pgid as i32, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return;
        }
    }
    // Registry full: the group still dies by its own watchdog or wait.
}

fn unregister_group(pgid: u32) {
    for slot in &GROUPS {
        let _ = slot.compare_exchange(pgid as i32, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// Spawns `cmd` as the leader of its own process group, so a deadline kill
/// reaches its grandchildren too, and registers the group so dt dying by
/// signal takes it along instead of orphaning it.
pub(crate) fn spawn_grouped(cmd: &mut Command) -> std::io::Result<Child> {
    install_signal_forwarding();
    cmd.process_group(0);
    let child = cmd.spawn()?;
    register_group(child.id());
    Ok(child)
}

/// SIGKILLs the group led by `pid`. A no-op for a child not spawned via
/// [`spawn_grouped`]; never falls back to the pid alone, which may be reused
/// once the leader is reaped.
fn kill_group(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// SIGKILLs the group, or just the process for a child not spawned via
/// [`spawn_grouped`]. Only for a child that has not been reaped yet.
fn kill_group_or_process(pid: u32) {
    let pid = pid as libc::pid_t;
    unsafe {
        if libc::killpg(pid, libc::SIGKILL) != 0 {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Whether the child exited within `timeout` (zero waits forever). Observes
/// the exit without reaping — the leader stays a zombie, holding the group id
/// so a follow-up group kill cannot hit a recycled id. A user stop ends the
/// wait early, reporting "not exited" so the caller kills and reaps.
fn exited_within(pid: u32, timeout: Duration) -> std::io::Result<bool> {
    let start = Instant::now();
    loop {
        if stop_requested() {
            return Ok(false);
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOWAIT | libc::WNOHANG;
        let rc = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
        if rc == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if exited_pid(&info) != 0 {
            return Ok(true);
        }
        if !timeout.is_zero() && start.elapsed() >= timeout {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(target_os = "linux")]
fn exited_pid(info: &libc::siginfo_t) -> libc::pid_t {
    unsafe { info.si_pid() }
}

#[cfg(not(target_os = "linux"))]
fn exited_pid(info: &libc::siginfo_t) -> libc::pid_t {
    info.si_pid
}

/// Waits up to `timeout` (zero waits forever); on expiry kills the child's
/// group and reaps it — `Ok(None)` means exactly that. On a normal exit any
/// group survivors are killed pre-reap, so they cannot hang the drain joins.
pub(crate) fn wait_or_kill(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let pid = child.id();
    let result = if exited_within(pid, timeout)? {
        kill_group(pid);
        child.wait().map(Some)
    } else {
        kill_group_or_process(pid);
        let _ = child.kill();
        let _ = child.wait();
        Ok(None)
    };
    unregister_group(pid);
    result
}

/// Kills the child's group when `timeout` passes or the user stops the turn,
/// unless disarmed first. Armed before stdout is read: a wedged child never
/// closes the pipe, the reader blocks until EOF, and only a kill from outside
/// ends the wait.
pub(crate) struct Watchdog {
    disarmed: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    pub(crate) fn arm(child: &Child, timeout: Duration) -> Self {
        let disarmed = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        // Spawned even for an untimed run: a stop request must still reach a
        // wedged child whose output pipe nobody could otherwise close.
        let thread = {
            let pid = child.id();
            let (disarmed, fired) = (disarmed.clone(), fired.clone());
            std::thread::spawn(move || {
                let deadline = (!timeout.is_zero()).then(|| Instant::now() + timeout);
                while !disarmed.load(Ordering::Acquire) {
                    let expired = deadline.is_some_and(|d| Instant::now() >= d);
                    if expired || stop_requested() {
                        if !disarmed.load(Ordering::Acquire) {
                            if expired {
                                fired.store(true, Ordering::Release);
                            }
                            kill_group_or_process(pid);
                        }
                        return;
                    }
                    std::thread::sleep(POLL);
                }
            })
        };
        Self { disarmed, fired, thread: Some(thread) }
    }

    /// Stops the countdown, waiting out any in-flight kill so the child is
    /// safe to reap. True when the deadline fired; callers treat that as a
    /// timeout only if the run failed, since a clean finish can tie the race.
    pub(crate) fn disarm(mut self) -> bool {
        self.disarmed.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.fired.load(Ordering::Acquire)
    }
}

/// Reads a pipe to EOF on its own thread, returning the text it carried.
/// A pipe nobody drains fills up and stalls the process writing into it, so
/// callers start this before doing anything else with the child.
pub(crate) fn drain_read<R: Read + Send + 'static>(mut reader: R) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Runs `cmd` to completion capturing stdout and stderr, killing its whole
/// group if it outlives `timeout`. `None` means the deadline — or a user stop
/// — fired first. For short probes and helpers (`which`, `git status`) that
/// must never hang startup or a round.
pub(crate) fn capture_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> std::io::Result<Option<std::process::Output>> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_grouped(cmd)?;
    let stderr_drain = child.stderr.take().map(drain_read);
    let watchdog = Watchdog::arm(&child, timeout);
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let timed_out = watchdog.disarm();
    let Some(status) = wait_or_kill(&mut child, REAP_GRACE)? else {
        return Ok(None);
    };
    if timed_out {
        // The kill beat the reap, so the wait above saw a normal-looking
        // corpse; report it as the timeout it was.
        return Ok(None);
    }
    let stderr =
        stderr_drain.map(|handle| handle.join().unwrap_or_default()).unwrap_or_default();
    Ok(Some(std::process::Output { status, stdout, stderr: stderr.into_bytes() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::sync::Mutex;

    /// The tests share process-global state — signal handler, cancel flags,
    /// group registry — so they run one at a time.
    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn a_fast_process_finishes_before_its_deadline() {
        let _serial = serial();
        let mut child = spawn_grouped(&mut Command::new("true")).expect("spawn true");
        let status =
            wait_or_kill(&mut child, Duration::from_secs(5)).expect("wait").expect("exit status");
        assert!(status.success());
    }

    #[test]
    fn drained_text_arrives_whole() {
        let _serial = serial();
        let mut child = Command::new("echo")
            .arg("hello")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn echo");
        let handle = drain_read(child.stdout.take().expect("stdout"));
        child.wait().expect("wait");
        assert_eq!(handle.join().expect("join"), "hello\n");
    }

    /// The reported hang: the check exits in milliseconds, but a backgrounded
    /// grandchild inherits the stdout pipe and holds it open — the drain join
    /// must not wait for it.
    #[test]
    fn a_finished_child_is_not_held_hostage_by_its_grandchildren() {
        let _serial = serial();
        let start = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; sleep 5 & exit 0"]).stdout(Stdio::piped());
        let mut child = spawn_grouped(&mut cmd).expect("spawn sh");
        let drain = drain_read(child.stdout.take().expect("stdout"));

        let status =
            wait_or_kill(&mut child, Duration::from_secs(10)).expect("wait").expect("status");
        assert!(status.success());
        assert_eq!(drain.join().expect("join"), "out\n");
        assert!(start.elapsed() < Duration::from_secs(4), "hung on the grandchild");
    }

    #[test]
    fn a_timed_out_child_and_its_group_are_killed_and_reaped() {
        let _serial = serial();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let mut child = spawn_grouped(&mut cmd).expect("spawn sh");
        let start = Instant::now();
        assert!(wait_or_kill(&mut child, Duration::from_millis(150)).expect("wait").is_none());
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    /// The dominant hang mode: a child that stays silent with stdout open. The
    /// watchdog is what unblocks the reader, because nothing else can.
    #[test]
    fn a_wedged_child_is_killed_at_the_watchdog_deadline() {
        let _serial = serial();
        let mut cmd = Command::new("sleep");
        cmd.arg("5").stdout(Stdio::piped());
        let mut child = spawn_grouped(&mut cmd).expect("spawn sleep");
        let watchdog = Watchdog::arm(&child, Duration::from_millis(150));

        let start = Instant::now();
        let drain = drain_read(child.stdout.take().expect("stdout"));
        drain.join().expect("join");
        assert!(start.elapsed() < Duration::from_secs(4), "reader was never unblocked");
        let _ = child.wait();
        assert!(watchdog.disarm(), "the deadline kill went unreported");
    }

    #[test]
    fn a_disarmed_watchdog_never_fires() {
        let _serial = serial();
        let mut child = Command::new("sleep").arg("0.2").spawn().expect("spawn sleep");
        let watchdog = Watchdog::arm(&child, Duration::from_secs(60));
        let status = child.wait().expect("wait");
        assert!(!watchdog.disarm());
        assert!(status.success(), "killed a child that beat its deadline");
    }

    #[test]
    fn capture_collects_output_from_a_fast_command() {
        let _serial = serial();
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let output = capture_with_timeout(&mut cmd, Duration::from_secs(5))
            .expect("run")
            .expect("not killed");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
    }

    #[test]
    fn capture_kills_a_command_that_outlives_its_deadline() {
        let _serial = serial();
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let start = Instant::now();
        assert!(capture_with_timeout(&mut cmd, Duration::from_millis(150))
            .expect("run")
            .is_none());
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    /// One test on purpose: it owns the process-global cancel flag and turn
    /// state while it runs, so no sibling test can observe either.
    #[test]
    fn a_stop_request_interrupts_waits_and_kills_the_turns_children() {
        let _serial = serial();
        let _turn = TurnGuard::new();
        request_cancel();

        // An untimed exit wait ends promptly instead of waiting forever.
        let mut child = spawn_grouped(&mut Command::new("sh").args(["-c", "sleep 30"]))
            .expect("spawn sh");
        let start = Instant::now();
        assert!(wait_or_kill(&mut child, Duration::from_secs(60)).expect("wait").is_none());
        assert!(start.elapsed() < Duration::from_secs(4), "cancel did not end the wait");

        // So does a wedged child's output read, via the watchdog. The kill is
        // not reported as a deadline timeout — a stop is not a timeout.
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdout(Stdio::piped());
        let mut child = spawn_grouped(&mut cmd).expect("spawn sleep");
        request_cancel();
        let watchdog = Watchdog::arm(&child, Duration::ZERO);
        let drain = drain_read(child.stdout.take().expect("stdout"));

        let start = Instant::now();
        drain.join().expect("join");
        assert!(start.elapsed() < Duration::from_secs(4), "stop did not unblock the reader");
        let _ = child.wait();
        assert!(!watchdog.disarm(), "a stop must not read as a deadline timeout");
    }

    #[test]
    fn turn_guards_support_nesting_and_clear_stale_cancel_on_entry() {
        let _serial = serial();

        // A stop while idle sets the flag.
        request_cancel();
        assert!(is_cancelled());
        assert!(!stop_requested());

        // An outermost turn clears the stale flag.
        {
            let _outer = TurnGuard::new();
            assert!(!is_cancelled());
            assert!(!stop_requested());

            // An inner turn keeps the active status without clearing mid-turn cancel.
            request_cancel();
            assert!(is_cancelled());
            assert!(stop_requested());
            {
                let _inner = TurnGuard::new();
                assert!(is_cancelled());
                assert!(stop_requested());
            }
            // Inner drop keeps the outer turn active.
            assert!(is_cancelled());
            assert!(stop_requested());
        }

        // Outer drop deactivates turn state.
        assert!(!stop_requested());
    }
}
