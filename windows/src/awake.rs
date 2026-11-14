//! Keeps Windows from turning the screen off or going to sleep while a video
//! plays. Elsewhere it does nothing.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Need {
    /// Paused or stopped: let the system idle as usual.
    #[default]
    Nothing,
    /// Only sound is playing: stay awake, the screen may turn off.
    System,
    /// A picture is playing: keep the screen on too.
    Display,
}

/// Remembers what was last asked of the system so it is only told on changes.
/// The request belongs to the UI thread, so call `set` from there only.
#[derive(Default)]
pub struct KeepAwake {
    current: Need,
}

impl KeepAwake {
    pub fn set(&mut self, need: Need) {
        if need != self.current {
            self.current = need;
            apply(need);
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.set(Need::Nothing);
    }
}

#[cfg(windows)]
fn apply(need: Need) {
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
    const ES_CONTINUOUS: u32 = 0x8000_0000;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    let flags = ES_CONTINUOUS
        | match need {
            Need::Nothing => 0,
            Need::System => ES_SYSTEM_REQUIRED,
            Need::Display => ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
        };
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(not(windows))]
fn apply(_: Need) {}
