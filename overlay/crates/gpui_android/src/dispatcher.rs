//! Dispatcher: worker pool + one timer thread + main-thread queue that wakes
//! the Android looper (`AndroidAppWaker`) so `poll_events` returns promptly.

use android_activity::AndroidAppWaker;
use gpui::{PlatformDispatcher, Priority, PriorityQueueReceiver, PriorityQueueSender, RunnableVariant};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::BinaryHeap,
    cmp::Reverse,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const MIN_THREADS: usize = 2;

struct Timer {
    at: Instant,
    runnable: RunnableVariant,
}
impl PartialEq for Timer { fn eq(&self, o: &Self) -> bool { self.at == o.at } }
impl Eq for Timer {}
impl PartialOrd for Timer { fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(o)) } }
impl Ord for Timer { fn cmp(&self, o: &Self) -> std::cmp::Ordering { self.at.cmp(&o.at) } }

type TimerHeap = Arc<(Mutex<BinaryHeap<Reverse<Timer>>>, Condvar)>;

pub(crate) struct AndroidDispatcher {
    main_sender: Arc<PriorityQueueSender<RunnableVariant>>,
    background_sender: PriorityQueueSender<RunnableVariant>,
    timers: TimerHeap,
    waker: AndroidAppWaker,
    main_thread_id: thread::ThreadId,
}

impl AndroidDispatcher {
    /// Returns the dispatcher and the receiver the platform run loop drains.
    pub fn new(waker: AndroidAppWaker) -> (Self, PriorityQueueReceiver<RunnableVariant>) {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let main_sender = Arc::new(main_sender);
        let (background_sender, background_receiver) = PriorityQueueReceiver::new();

        let n = thread::available_parallelism().map_or(MIN_THREADS, |n| n.get().max(MIN_THREADS));
        for i in 0..n {
            let rx: PriorityQueueReceiver<RunnableVariant> = background_receiver.clone();
            thread::Builder::new()
                .name(format!("gpui-worker-{i}"))
                .spawn(move || {
                    for runnable in rx.iter() {
                        runnable.run();
                    }
                })
                .expect("spawn worker");
        }

        let timers: TimerHeap = Arc::new((Mutex::new(BinaryHeap::new()), Condvar::new()));
        {
            let timers = timers.clone();
            let main_sender = main_sender.clone();
            let waker = waker.clone();
            thread::Builder::new()
                .name("gpui-timer".into())
                .spawn(move || loop {
                    let (heap, cv) = &*timers;
                    let mut heap = heap.lock();
                    match heap.peek().map(|t| t.0.at) {
                        None => cv.wait(&mut heap),
                        Some(at) if at <= Instant::now() => {
                            if let Some(Reverse(t)) = heap.pop() {
                                let _ = main_sender.send(Priority::Medium, t.runnable);
                                waker.wake();
                            }
                        }
                        Some(at) => {
                            let _ = cv.wait_until(&mut heap, at);
                        }
                    }
                })
                .expect("spawn timer");
        }

        (
            Self { main_sender, background_sender, timers, waker, main_thread_id: thread::current().id() },
            main_receiver,
        )
    }
}

impl PlatformDispatcher for AndroidDispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }
    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        let _ = self.background_sender.send(priority, runnable);
    }
    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
        let _ = self.main_sender.send(priority, runnable);
        self.waker.wake();
    }
    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        let (heap, cv) = &*self.timers;
        heap.lock().push(Reverse(Timer { at: Instant::now() + duration, runnable }));
        cv.notify_one();
    }
    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        thread::Builder::new().name("gpui-realtime".into()).spawn(f).expect("spawn realtime");
    }
}
