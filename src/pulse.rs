//! Experiment pulse schedule types and conflict checks (used by calibration / crosstalk templates).

#[derive(Clone, Debug)]
pub struct Pulse {
    pub channel: u8,
    pub start_ns: u64,
    pub frequency_ghz: f64,
}

#[derive(Debug)]
pub enum PulseError {
    InvalidChannel(u8),
    InvalidFrequency(f64),
}

pub fn validate_pulse(channel: u8, frequency_ghz: f64) -> Result<Pulse, PulseError> {
    if channel > 7 {
        return Err(PulseError::InvalidChannel(channel));
    }
    if frequency_ghz <= 0.0 || frequency_ghz > 10.0 {
        return Err(PulseError::InvalidFrequency(frequency_ghz));
    }
    Ok(Pulse {
        channel,
        start_ns: 0,
        frequency_ghz,
    })
}

pub fn sort_pulses_by_start_ns(mut pulses: Vec<Pulse>) -> Vec<Pulse> {
    pulses.sort_by_key(|p| p.start_ns);
    pulses
}

/// Two pulses conflict if they use the same channel and their start times are within 50 ns.
pub fn pulses_conflict(a: &Pulse, b: &Pulse) -> bool {
    a.channel == b.channel && a.start_ns.abs_diff(b.start_ns) <= 50
}

/// O(n²) — acceptable for small experiment schedules.
/// For large schedules, sort by channel first and use a sliding window.
pub fn schedule_has_conflicts(pulses: &[Pulse]) -> bool {
    pulses.iter().enumerate().any(|(i, a)| {
        pulses
            .iter()
            .enumerate()
            .any(|(j, b)| i < j && pulses_conflict(a, b))
    })
}

/// True if this pulse conflicts with any other pulse in the same schedule.
pub fn pulse_has_conflict(schedule: &[Pulse], index: usize) -> bool {
    let p = &schedule[index];
    schedule
        .iter()
        .enumerate()
        .any(|(i, other)| i != index && pulses_conflict(p, other))
}
