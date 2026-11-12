//! Bridge's own open-file limit.
//!
//! liblsl opens several sockets per inlet, and Bridge opens one inlet per FRENZ stream (24 of them) plus its marker
//! outlet. macOS starts apps from Finder or `open` with a soft limit of 256 descriptors, so connecting FRENZ ran out
//! part-way: inlets failed with "ResourceCreation", "No stream found" and "Inlet thread response timeout". The FRENZ
//! sidecar raises its own limit (`prepare_lsl_runtime`); this does the same for Bridge, soft limit only, at startup.

/// The soft limit Bridge needs: every FRENZ and Neon stream open at once, with room to spare.
pub const REQUIRED_OPEN_FILES: u64 = 4096;

/// The soft limit to set for a current `(soft, hard)` pair, or `None` when it is already enough. Never above `hard`.
pub fn raised_soft_limit(soft: u64, hard: u64, infinity: u64) -> Option<u64> {
    if soft == infinity || soft >= REQUIRED_OPEN_FILES {
        return None;
    }
    let target = if hard == infinity { REQUIRED_OPEN_FILES } else { REQUIRED_OPEN_FILES.min(hard) };
    (target > soft).then_some(target)
}

/// Raises this process's soft open-file limit to `REQUIRED_OPEN_FILES` (or the hard limit, if lower).
/// Returns the new soft limit, or `None` when no change was needed.
#[cfg(unix)]
pub fn raise_open_file_limit() -> Result<Option<u64>, String> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit/setrlimit only read and write the struct passed to them.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(format!("getrlimit failed: {}", std::io::Error::last_os_error()));
    }
    let Some(target) = raised_soft_limit(limit.rlim_cur as u64, limit.rlim_max as u64, libc::RLIM_INFINITY as u64)
    else {
        return Ok(None);
    };
    limit.rlim_cur = target as libc::rlim_t;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(format!("setrlimit to {target} failed: {}", std::io::Error::last_os_error()));
    }
    Ok(Some(target))
}

#[cfg(not(unix))]
pub fn raise_open_file_limit() -> Result<Option<u64>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    const INF: u64 = u64::MAX;

    #[test]
    fn the_finder_default_is_raised() {
        assert_eq!(raised_soft_limit(256, INF, INF), Some(REQUIRED_OPEN_FILES));
    }

    #[test]
    fn a_lower_hard_limit_caps_the_raise() {
        assert_eq!(raised_soft_limit(256, 1024, INF), Some(1024));
    }

    #[test]
    fn enough_already_is_left_alone() {
        assert_eq!(raised_soft_limit(10240, INF, INF), None);
        assert_eq!(raised_soft_limit(INF, INF, INF), None);
        assert_eq!(raised_soft_limit(256, 256, INF), None);
    }
}
