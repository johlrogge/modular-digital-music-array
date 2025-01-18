use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

pub mod protocol;

#[derive(Debug, Error)]
pub enum ClockError {
    #[error("Invalid tick update")]
    InvalidTick,
}

pub trait TimeSource {
    fn now(&self) -> Instant;
}

#[derive(Clone)]
pub struct SystemTimeSource;

impl TimeSource for SystemTimeSource {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub struct ClockState {
    ticks: u64,
    tempo: f64,
    last_tick_time: Instant,
}

impl ClockState {
    fn new(time_source: &dyn TimeSource) -> Self {
        Self {
            ticks: 0,
            tempo: 120.0,
            last_tick_time: time_source.now(),
        }
    }
}

pub struct MusicalClock<T: TimeSource> {
    state: Arc<RwLock<ClockState>>,
    time_source: T,
    ppqn: u32,
}

impl<T: TimeSource> MusicalClock<T> {
    pub fn new(time_source: T) -> Self {
        Self {
            state: Arc::new(RwLock::new(ClockState::new(&time_source))),
            time_source,
            ppqn: 960,
        }
    }

    pub fn tick(&self) -> Result<(), ClockError> {
        let mut state = self.state.write();
        state.ticks += 1;
        state.last_tick_time = self.time_source.now();
        Ok(())
    }

    pub fn set_tempo(&self, bpm: f64) -> Result<(), ClockError> {
        let mut state = self.state.write();
        state.tempo = bpm.clamp(20.0, 400.0);
        Ok(())
    }

    pub fn get_position(&self) -> (u64, f64) {
        let state = self.state.read();
        (state.ticks, state.tempo)
    }

    pub fn time_since_last_tick(&self) -> Duration {
        let state = self.state.read();
        self.time_source.now().duration_since(state.last_tick_time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Clone)]
    pub struct TimeSourceStub {
        current_time: Arc<AtomicU64>,
        start: Instant,
    }

    impl TimeSourceStub {
        pub fn new() -> Self {
            Self {
                current_time: Arc::new(AtomicU64::new(0)),
                start: Instant::now(),
            }
        }

        pub fn advance(&self, duration: Duration) {
            self.current_time
                .fetch_add(duration.as_nanos() as u64, Ordering::SeqCst);
        }
    }

    impl TimeSource for TimeSourceStub {
        fn now(&self) -> Instant {
            let nanos = self.current_time.load(Ordering::SeqCst);
            self.start + Duration::from_nanos(nanos)
        }
    }

    #[test]
    fn test_clock_creation() {
        let time_source = TimeSourceStub::new();
        let clock = MusicalClock::new(time_source);
        let (ticks, tempo) = clock.get_position();

        assert_eq!(ticks, 0);
        assert_eq!(tempo, 120.0);
    }

    #[test]
    fn test_tick_updates_time() {
        let time_source = TimeSourceStub::new();
        let clock = MusicalClock::new(time_source.clone());

        time_source.advance(Duration::from_millis(10));
        clock.tick().unwrap();

        assert_eq!(clock.time_since_last_tick().as_millis(), 0);

        time_source.advance(Duration::from_millis(5));
        assert_eq!(clock.time_since_last_tick().as_millis(), 5);
    }
}