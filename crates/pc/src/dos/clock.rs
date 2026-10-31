// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Host local time, and the packed date and time words DOS keeps in
//! directory entries and FCBs.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalTime {
    pub fn now() -> Self {
        Self::of(SystemTime::now())
    }

    /// `t` in the host's time zone (UTC where that is unavailable).
    pub fn of(t: SystemTime) -> Self {
        let secs = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        };
        local(secs).unwrap_or_else(|| utc(secs))
    }

    /// Bits 15-9 year - 1980, 8-5 month, 4-0 day; clamped to 1980-2107.
    pub fn dos_date(&self) -> u16 {
        match self.year {
            ..1980 => 1 << 5 | 1,
            2108.. => 127 << 9 | 12 << 5 | 31,
            y => ((y - 1980) as u16) << 9 | (self.month as u16) << 5 | self.day as u16,
        }
    }

    /// Bits 15-11 hour, 10-5 minute, 4-0 seconds / 2.
    pub fn dos_time(&self) -> u16 {
        match self.year {
            ..1980 => 0,
            2108.. => 23 << 11 | 59 << 5 | 29,
            _ => (self.hour as u16) << 11 | (self.minute as u16) << 5 | (self.second / 2) as u16,
        }
    }
}

/// `secs` since the epoch in UTC.
pub(super) fn utc(secs: i64) -> LocalTime {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from a day count (H. Hinnant, "chrono-compatible low-level date algorithms").
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + (month <= 2) as i64) as i32;
    LocalTime { year, month, day, hour: (rem / 3600) as u8, minute: (rem / 60 % 60) as u8, second: (rem % 60) as u8 }
}

/// `secs` since the epoch in the host's time zone, from the C library.
#[cfg(all(unix, target_pointer_width = "64"))]
fn local(secs: i64) -> Option<LocalTime> {
    use std::os::raw::{c_char, c_int, c_long};

    // struct tm as glibc, musl, macOS and the BSDs define it; time_t is a C
    // long on these 64-bit targets. Unread fields are there for the layout.
    #[repr(C)]
    #[allow(dead_code)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        mon: c_int,
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        gmtoff: c_long,
        zone: *const c_char,
    }
    unsafe extern "C" {
        fn localtime_r(t: *const c_long, out: *mut Tm) -> *mut Tm;
    }

    let t: c_long = secs;
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call; localtime_r writes only `tm`.
    let r = unsafe { localtime_r(&t, tm.as_mut_ptr()) };
    if r.is_null() {
        return None;
    }
    // SAFETY: zero-initialised and then filled in by localtime_r.
    let tm = unsafe { tm.assume_init() };
    Some(LocalTime {
        year: tm.year + 1900,
        month: (tm.mon + 1) as u8,
        day: tm.mday as u8,
        hour: tm.hour as u8,
        minute: tm.min as u8,
        // A leap second shows as 60.
        second: tm.sec.min(59) as u8,
    })
}

#[cfg(not(all(unix, target_pointer_width = "64")))]
fn local(_secs: i64) -> Option<LocalTime> {
    None
}
