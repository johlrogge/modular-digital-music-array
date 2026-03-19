use crossterm::event::KeyEvent;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("terminal setup failed: {source}")]
    Setup {
        #[source]
        source: std::io::Error,
    },
    #[error("terminal draw failed: {source}")]
    Draw {
        #[source]
        source: std::io::Error,
    },
    #[error("event read failed: {source}")]
    Event {
        #[source]
        source: std::io::Error,
    },
}

pub struct TuiConfig {
    pub poll_interval_ms: u64,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 100,
        }
    }
}

/// Trait implemented by the application driving the TUI loop.
///
/// `Error` must implement `From<TuiError>` so infrastructure errors propagate
/// through the caller's error type without boxing.
pub trait TuiApp {
    type Error: From<TuiError>;

    /// Handle a key event.
    fn on_key(&mut self, key: KeyEvent);

    /// Called once per poll cycle. Use for background work: draining event
    /// channels, polling NNG, updating state on each tick, etc.
    fn on_tick(&mut self);

    /// Render the current frame.
    fn render(&self, frame: &mut ratatui::Frame);

    /// Return true to exit the event loop.
    fn should_quit(&self) -> bool;
}

/// Set up the crossterm terminal, run the event loop, and restore the terminal
/// on exit — including on panic via scopeguard.
///
/// Does NOT call `color_eyre::install()` or any global tracing init.
pub fn run<A: TuiApp>(app: &mut A, config: &TuiConfig) -> Result<(), A::Error> {
    use crossterm::{
        event::{poll, Event},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::{backend::CrosstermBackend, Terminal};

    enable_raw_mode().map_err(|e| TuiError::Setup { source: e })?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| TuiError::Setup { source: e })?;

    scopeguard::defer! {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| TuiError::Setup { source: e })?;

    let interval = Duration::from_millis(config.poll_interval_ms);

    loop {
        if poll(interval).map_err(|e| TuiError::Event { source: e })? {
            match crossterm::event::read().map_err(|e| TuiError::Event { source: e })? {
                Event::Key(key) => app.on_key(key),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        app.on_tick();

        terminal
            .draw(|f| app.render(f))
            .map_err(|e| TuiError::Draw { source: e })?;

        if app.should_quit() {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Minimal concrete error type that wraps TuiError for use in tests.
    #[derive(Debug)]
    enum FakeError {
        #[allow(dead_code)]
        Tui(TuiError),
    }

    impl From<TuiError> for FakeError {
        fn from(e: TuiError) -> Self {
            FakeError::Tui(e)
        }
    }

    struct FakeApp {
        ticks: u32,
        quit_after: u32,
    }

    impl TuiApp for FakeApp {
        type Error = FakeError;

        fn on_key(&mut self, _key: crossterm::event::KeyEvent) {}

        fn on_tick(&mut self) {
            self.ticks += 1;
        }

        fn render(&self, _frame: &mut ratatui::Frame) {}

        fn should_quit(&self) -> bool {
            self.ticks >= self.quit_after
        }
    }

    #[test]
    fn mock_app_starts_not_quitting() {
        let app = FakeApp {
            ticks: 0,
            quit_after: 3,
        };
        assert!(!app.should_quit());
    }

    #[test]
    fn mock_app_quits_after_tick_threshold() {
        let mut app = FakeApp {
            ticks: 0,
            quit_after: 3,
        };
        app.on_tick();
        app.on_tick();
        app.on_tick();
        assert!(app.should_quit());
    }

    #[test]
    fn tui_config_default_poll_interval() {
        let cfg = TuiConfig::default();
        assert_eq!(cfg.poll_interval_ms, 100);
    }

    #[test]
    fn tui_error_setup_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = TuiError::Setup { source: io_err };
        assert_eq!(err.to_string(), "terminal setup failed: denied");
    }

    #[test]
    fn tui_error_draw_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken");
        let err = TuiError::Draw { source: io_err };
        assert_eq!(err.to_string(), "terminal draw failed: broken");
    }

    #[test]
    fn tui_error_event_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout");
        let err = TuiError::Event { source: io_err };
        assert_eq!(err.to_string(), "event read failed: timeout");
    }
}
