//! Bridge started from Finder gets a 256-descriptor soft limit, too few for every FRENZ stream's LSL inlet; connecting
//! FRENZ then failed part-way with "ResourceCreation" and inlet timeouts. Startup must raise it.
//!
//! Its own test binary: it lowers this process's limit, which would starve tests running alongside it.

#[cfg(unix)]
#[test]
fn startup_raises_the_finder_default_limit() {
    use hyperstudy_bridge::open_files::{raise_open_file_limit, REQUIRED_OPEN_FILES};

    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if limit.rlim_max != libc::RLIM_INFINITY && (limit.rlim_max as u64) < REQUIRED_OPEN_FILES {
        eprintln!(
            "hard limit {} is below {REQUIRED_OPEN_FILES}; nothing to test",
            limit.rlim_max
        );
        return;
    }
    limit.rlim_cur = 256; // what a Finder launch starts with
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);

    assert_eq!(raise_open_file_limit(), Ok(Some(REQUIRED_OPEN_FILES)));

    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    assert_eq!(limit.rlim_cur as u64, REQUIRED_OPEN_FILES);
}
