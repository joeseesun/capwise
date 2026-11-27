//! [INPUT]: 已验签的更新包、旧进程 PID 和应用路径。
//! [OUTPUT]: 等待旧进程退出后安装更新并启动应用。
//! [POS]: 独立安装进程复用安装器和单实例锁。
//! [PROTOCOL]: Update this header on change, then check AGENTS.md

use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use semver::Version;

use crate::installer;

const INSTALL_ARGUMENT: &str = "--capwise-install-after-exit";
const EXIT_TIMEOUT: Duration = Duration::from_secs(45);
pub(crate) const TIMED_OUT: u8 = 75;

pub(crate) fn launch(package: &Path, version: &Version) -> Result<Child, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let bundle = installer::running_app_bundle()?;
    let package = std::path::absolute(package).map_err(|error| error.to_string())?;
    let log = crate::diagnostics::logs_dir().join("capwise-update.log");
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_or_else(|_| Stdio::inherit(), Stdio::from);
    Command::new(executable)
        .arg(INSTALL_ARGUMENT)
        .arg(std::process::id().to_string())
        .arg(bundle)
        .arg(package)
        .arg(version.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|error| error.to_string())
}

pub(crate) fn run_from_args() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.as_os_str() != INSTALL_ARGUMENT {
        return None;
    }
    let result = (|| {
        let pid = args.next()?.to_str()?.parse::<libc::pid_t>().ok()?;
        let running = PathBuf::from(args.next()?);
        let package = PathBuf::from(args.next()?);
        let version = Version::parse(args.next()?.to_str()?).ok()?;
        (pid > 1 && running.is_absolute() && package.is_absolute() && args.next().is_none())
            .then(|| install_after_exit(pid, &running, &package, &version, EXIT_TIMEOUT))
    })()
    .unwrap_or_else(|| {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid update arguments",
        ))
    });
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Capwise update: {error}");
            if error.kind() == io::ErrorKind::TimedOut {
                i32::from(TIMED_OUT)
            } else {
                1
            }
        }
    })
}

fn install_after_exit(
    pid: libc::pid_t,
    running: &Path,
    package: &Path,
    version: &Version,
    timeout: Duration,
) -> io::Result<()> {
    let original = running.symlink_metadata()?;
    if !original.is_dir() {
        return Err(io::Error::other(
            "The installed app is not a bundle directory",
        ));
    }
    eprintln!("Waiting for Capwise process {pid} to exit before installing {version}");
    wait_for_exit(pid, timeout)?;

    let lock = crate::single_instance::lock_for_update()?;
    let current = running.symlink_metadata()?;
    if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
        return Err(io::Error::other(
            "The installed app changed while waiting for it to exit",
        ));
    }

    let installed = installer::verify_update_source(running, package, version).and_then(|()| {
        let outcome = installer::install_app_bundle(package, running, version);
        if outcome.is_err() {
            installer::discard_patched_update(package);
        }
        outcome
    });
    drop(lock);
    match installed {
        Ok(bundle) => {
            eprintln!("Capwise {version} installed after process {pid} exited");
            open_bundle(&bundle)
        }
        Err(error) => {
            if let Err(relaunch_error) = open_bundle(running) {
                eprintln!("Could not reopen the previous Capwise version: {relaunch_error}");
            }
            Err(io::Error::other(error))
        }
    }
}

fn open_bundle(bundle: &Path) -> io::Result<()> {
    if Command::new("/usr/bin/open")
        .arg(bundle)
        .status()?
        .success()
    {
        Ok(())
    } else {
        Err(io::Error::other("Could not reopen Capwise"))
    }
}

fn wait_for_exit(pid: libc::pid_t, timeout: Duration) -> io::Result<()> {
    let queue = unsafe { libc::kqueue() };
    if queue == -1 {
        return Err(io::Error::last_os_error());
    }
    let queue = unsafe { OwnedFd::from_raw_fd(queue) };
    let watch = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    if unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &watch,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    } == -1
    {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        };
    }

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = libc::timespec {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_nsec: remaining.subsec_nanos().into(),
        };
        let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
        match unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                &timeout,
            )
        } {
            1 => {
                let event = unsafe { event.assume_init() };
                if event.flags & libc::EV_ERROR != 0 {
                    return Err(io::Error::from_raw_os_error(event.data as i32));
                }
                return Ok(());
            }
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Capwise did not exit before the update deadline",
                ));
            }
            _ => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_keeps_the_installed_bundle_and_update_package() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("Capwise.app");
        let archive = root.path().join("Capwise.app.tar.gz");
        std::fs::create_dir(&installed).unwrap();
        std::fs::write(installed.join("version"), b"old").unwrap();
        std::fs::write(&archive, b"staged").unwrap();
        let pid = std::process::id() as libc::pid_t;
        let error = install_after_exit(
            pid,
            &installed,
            &archive,
            &Version::new(0, 3, 23),
            Duration::from_millis(25),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(std::fs::read(installed.join("version")).unwrap(), b"old");
        assert_eq!(std::fs::read(archive).unwrap(), b"staged");
    }

    #[test]
    fn observes_the_process_exit() {
        let mut app = Command::new("/bin/sleep").arg("0.2").spawn().unwrap();
        let result = wait_for_exit(app.id() as libc::pid_t, Duration::from_secs(2));
        assert!(app.wait().unwrap().success());
        result.unwrap();
    }
}
