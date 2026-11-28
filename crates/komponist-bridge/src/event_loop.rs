//! [`EventLoop`]: the bridge main loop's [`HostLoop`] (ARCHITECTURE §8.2).
//!
//! One `poll()` per tick over the caller's own fds (the control socket and the notification
//! eventfd) and every fd the plugin registered, with a timeout that ends at the next due timer.
//! No X11 connection of its own. The fds and their policies are `komponist_hosting`'s
//! [`PluginFds`], the same as in `komponist_hosting::host_loop::PollLoop` (its module docs explain
//! them; #243): the write cooldown (`DEFAULT_WRITE_INTERVAL`, 50 ms after the last `WRITE` report,
//! so a JUCE plugin that registers its always-writable X11 socket with `READ | WRITE`, Dexed,
//! cannot make the loop spin) and `POLLNVAL` parking. Unlike `PollLoop` this loop also polls the
//! caller's fds, and it does not dispatch: callbacks run outside every borrow, so
//! [`EventLoop::poll`] only collects ready fds and due timers; the caller dispatches them and
//! checks before each dispatch that the fd or timer is still registered
//! ([`EventLoop::fd_registered`], [`EventLoop::timer_registered`]).

use std::cell::RefCell;
use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::{DEFAULT_WRITE_INTERVAL, PluginFds};
use komponist_hosting::{FdEvents, HostLoop, HostingError, TimerId};
use rustix::event::PollFd;

/// Plugin fds that are ready and timers that are due after one [`EventLoop::poll`].
#[derive(Debug, Default, PartialEq)]
pub struct Ready {
    /// Ready plugin fds with what they are ready for.
    pub fds: Vec<(RawFd, FdEvents)>,
    /// Due timers.
    pub timers: Vec<TimerId>,
}

#[derive(Debug)]
struct TimerEntry {
    id: TimerId,
    period: Duration,
    next: Instant,
}

#[derive(Debug, Default)]
struct LoopState {
    next_timer: u32,
    timers: Vec<TimerEntry>,
}

/// See the module docs. Main thread only; shared with the plugin as `Rc<dyn HostLoop>`.
#[derive(Debug)]
pub struct EventLoop {
    state: RefCell<LoopState>,
    fds: RefCell<PluginFds>,
}

impl EventLoop {
    /// A loop with the default write cooldown.
    pub fn new() -> Rc<EventLoop> {
        EventLoop::with_write_interval(DEFAULT_WRITE_INTERVAL)
    }

    /// A loop with an explicit write cooldown.
    pub fn with_write_interval(write_interval: Duration) -> Rc<EventLoop> {
        Rc::new(EventLoop {
            state: RefCell::new(LoopState::default()),
            fds: RefCell::new(PluginFds::new(write_interval)),
        })
    }

    /// Number of registered timers and fds.
    #[cfg(test)]
    pub fn registrations(&self) -> (usize, usize) {
        (self.state.borrow().timers.len(), self.fds.borrow().len())
    }

    /// Whether `fd` is still registered.
    pub fn fd_registered(&self, fd: RawFd) -> bool {
        self.fds.borrow().interest(fd).is_some()
    }

    /// Whether `timer` is still registered.
    pub fn timer_registered(&self, timer: TimerId) -> bool {
        self.state.borrow().timers.iter().any(|t| t.id == timer)
    }

    /// One `poll()` over `extra` (the caller's fds; their `revents` are filled in) and the plugin's
    /// fds, waiting at most `max_wait` and less if a timer or a write cooldown is due earlier.
    /// Returns the ready plugin fds and the due timers (each timer moves on by its period).
    pub fn poll(&self, extra: &mut [PollFd<'_>], max_wait: Duration) -> Ready {
        // 1.–2. Wait until `max_wait`, the next due timer or a write cooldown; the ready fds.
        let mut deadline = Instant::now() + max_wait;
        for timer in &self.state.borrow().timers {
            deadline = deadline.min(timer.next);
        }
        let mut ready = Ready {
            fds: self.fds.borrow_mut().poll(extra, deadline),
            timers: Vec::new(),
        };

        // 3. Due timers; missed periods are skipped instead of firing a burst.
        let now = Instant::now();
        let mut state = self.state.borrow_mut();
        for timer in state.timers.iter_mut().filter(|t| t.next <= now) {
            timer.next += timer.period;
            if timer.next <= now {
                timer.next = now + timer.period;
            }
            ready.timers.push(timer.id);
        }
        ready
    }
}

impl HostLoop for EventLoop {
    fn register_timer(&self, period: Duration) -> Result<TimerId, HostingError> {
        let mut state = self.state.borrow_mut();
        state.next_timer = state.next_timer.wrapping_add(1);
        let id = TimerId(state.next_timer);
        let period = period.max(Duration::from_millis(1));
        state.timers.push(TimerEntry {
            id,
            period,
            next: Instant::now() + period,
        });
        Ok(id)
    }

    fn unregister_timer(&self, timer: TimerId) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        let before = state.timers.len();
        state.timers.retain(|t| t.id != timer);
        if state.timers.len() == before {
            return Err(HostingError::HostLoop(format!("unknown timer {}", timer.0)));
        }
        Ok(())
    }

    fn register_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError> {
        self.fds.borrow_mut().register(fd, events)
    }

    fn modify_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError> {
        self.fds.borrow_mut().modify(fd, events)
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostingError> {
        self.fds.borrow_mut().unregister(fd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::event::PollFlags;
    use std::io::Write;
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::net::UnixStream;

    /// Runs ticks for `duration`, counting what became ready.
    fn run_for(lp: &EventLoop, duration: Duration) -> (u64, Vec<(RawFd, FdEvents)>, Vec<TimerId>) {
        let end = Instant::now() + duration;
        let (mut ticks, mut fds, mut timers) = (0, Vec::new(), Vec::new());
        while Instant::now() < end {
            let ready = lp.poll(&mut [], end.saturating_duration_since(Instant::now()));
            ticks += 1;
            fds.extend(ready.fds);
            timers.extend(ready.timers);
        }
        (ticks, fds, timers)
    }

    #[test]
    fn timers_fire_periodically_and_stop() {
        let lp = EventLoop::new();
        let id = lp.register_timer(Duration::from_millis(10)).unwrap();
        let (_, _, fired) = run_for(&lp, Duration::from_millis(105));
        assert!(
            (7..=11).contains(&fired.len()),
            "{} timer calls",
            fired.len()
        );
        assert!(fired.iter().all(|t| *t == id));
        assert!(lp.timer_registered(id));
        lp.unregister_timer(id).unwrap();
        assert!(!lp.timer_registered(id));
        assert!(lp.unregister_timer(id).is_err());
        let (_, _, fired) = run_for(&lp, Duration::from_millis(30));
        assert!(fired.is_empty());
    }

    #[test]
    fn extra_fds_and_plugin_fds_share_one_poll() {
        let lp = EventLoop::new();
        let (control, mut app) = UnixStream::pair().unwrap();
        let (plugin, mut other) = UnixStream::pair().unwrap();
        lp.register_fd(plugin.as_raw_fd(), FdEvents::READ).unwrap();
        assert!(lp.register_fd(plugin.as_raw_fd(), FdEvents::READ).is_err());

        let mut extra = [PollFd::new(&control, PollFlags::IN)];
        let t0 = Instant::now();
        let ready = lp.poll(&mut extra, Duration::from_millis(20));
        assert!(t0.elapsed() >= Duration::from_millis(15));
        assert_eq!(ready, Ready::default());
        assert!(extra[0].revents().is_empty());

        app.write_all(b"x").unwrap();
        let ready = lp.poll(&mut extra, Duration::from_secs(5));
        assert!(ready.fds.is_empty());
        assert!(extra[0].revents().contains(PollFlags::IN));

        other.write_all(b"y").unwrap();
        let mut extra = [PollFd::from_borrowed_fd(
            control.as_fd(),
            PollFlags::empty(),
        )];
        let ready = lp.poll(&mut extra, Duration::from_secs(5));
        assert_eq!(ready.fds, [(plugin.as_raw_fd(), FdEvents::READ)]);
        assert!(extra[0].revents().is_empty());

        lp.unregister_fd(plugin.as_raw_fd()).unwrap();
        assert!(!lp.fd_registered(plugin.as_raw_fd()));
        assert!(lp.modify_fd(plugin.as_raw_fd(), FdEvents::READ).is_err());
    }

    /// Spike WP-15 §1.7: an always-writable fd with WRITE interest must not make the loop spin.
    #[test]
    fn write_interest_is_rate_limited() {
        let lp = EventLoop::new();
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let fd = ours.as_raw_fd();
        lp.register_fd(fd, FdEvents::READ.union(FdEvents::WRITE))
            .unwrap();
        let (ticks, fds, _) = run_for(&lp, Duration::from_millis(500));
        // 500 ms / 50 ms = 10 cooldowns; allow for timing slack.
        assert!((5..=12).contains(&fds.len()), "{} WRITE reports", fds.len());
        assert!(ticks < 40, "{ticks} ticks in 500 ms");
        assert!(fds.iter().all(|r| *r == (fd, FdEvents::WRITE)));
    }

    #[test]
    fn a_closed_fd_is_reported_once_and_parked() {
        let lp = EventLoop::new();
        // Never a valid fd (far above RLIMIT_NOFILE), so no parallel test can reuse the number.
        let fd: RawFd = 999_999;
        lp.register_fd(fd, FdEvents::READ).unwrap();
        let (_, fds, _) = run_for(&lp, Duration::from_millis(50));
        assert_eq!(fds, [(fd, FdEvents::ERROR)]);
        lp.modify_fd(fd, FdEvents::READ).unwrap();
        let ready = lp.poll(&mut [], Duration::from_millis(10));
        assert_eq!(ready.fds, [(fd, FdEvents::ERROR)]);
        lp.unregister_fd(fd).unwrap();
        assert_eq!(lp.registrations(), (0, 0));
    }
}
