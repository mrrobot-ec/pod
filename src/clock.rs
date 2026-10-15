use chrono::{DateTime, Duration as ChronoDuration, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub struct FakeClock(pub std::sync::Mutex<DateTime<Utc>>);

impl FakeClock {
    pub fn advance(&self, duration: std::time::Duration) {
        let mut now = self.0.lock().expect("fake clock mutex poisoned");
        let delta = ChronoDuration::from_std(duration).expect("duration exceeds chrono range");
        *now += delta;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("fake clock mutex poisoned")
    }
}
