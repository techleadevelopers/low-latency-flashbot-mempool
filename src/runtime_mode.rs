use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RuntimeMode {
    Normal = 0,
    Alert = 1,
    Lockdown = 2,
}

impl RuntimeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeMode::Normal => "NORMAL",
            RuntimeMode::Alert => "ALERT",
            RuntimeMode::Lockdown => "LOCKDOWN",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            2 => RuntimeMode::Lockdown,
            1 => RuntimeMode::Alert,
            _ => RuntimeMode::Normal,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeModeController {
    inner: Arc<AtomicU8>,
}

impl RuntimeModeController {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(RuntimeMode::Normal as u8)),
        }
    }

    pub fn mode(&self) -> RuntimeMode {
        RuntimeMode::from_u8(self.inner.load(Ordering::Relaxed))
    }

    pub fn set(&self, mode: RuntimeMode) {
        self.inner.store(mode as u8, Ordering::Relaxed);
    }

    pub fn escalate(&self, mode: RuntimeMode) -> RuntimeMode {
        let mut current = self.inner.load(Ordering::Relaxed);
        loop {
            let current_mode = RuntimeMode::from_u8(current);
            if current_mode as u8 >= mode as u8 {
                return current_mode;
            }

            match self.inner.compare_exchange(
                current,
                mode as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return mode,
                Err(observed) => current = observed,
            }
        }
    }

    pub fn permits_non_critical(&self) -> bool {
        matches!(self.mode(), RuntimeMode::Normal)
    }
}
