#![cfg(unix)]

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::fd::{FromRawFd, RawFd},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use orc::{preferences::WorkspacePreferences, state};
use wait_timeout::ChildExt;

fn duplicate(fd: RawFd) -> File {
    let duplicated = unsafe { libc::dup(fd) };
    assert!(duplicated >= 0, "duplicate PTY file descriptor");
    unsafe { File::from_raw_fd(duplicated) }
}

#[test]
fn tui_layout_commands_persist_through_a_pty() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let scope = temporary.path().join("project");
    let state_home = temporary.path().join("state");
    let config_home = temporary.path().join("config");
    fs::create_dir_all(&scope).expect("scope directory");
    fs::create_dir_all(&config_home).expect("config directory");

    let mut master = -1;
    let mut slave = -1;
    #[cfg(target_os = "linux")]
    let dimensions = libc::winsize {
        ws_row: 36,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    #[cfg(not(target_os = "linux"))]
    let mut dimensions = libc::winsize {
        ws_row: 36,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    #[cfg(target_os = "linux")]
    let dimensions = std::ptr::from_ref(&dimensions);
    #[cfg(not(target_os = "linux"))]
    let dimensions = std::ptr::from_mut(&mut dimensions);
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dimensions,
        )
    };
    assert_eq!(opened, 0, "open PTY");

    let mut child = Command::new(env!("CARGO_BIN_EXE_orc"))
        .args(["tui", "--scope"])
        .arg(&scope)
        .env("ORC_DAEMON_AUTOSTART", "false")
        .env(
            "ORC_PROVIDERS_DIRECTORY",
            temporary.path().join("providers"),
        )
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdin(Stdio::from(duplicate(slave)))
        .stdout(Stdio::from(duplicate(slave)))
        .stderr(Stdio::from(duplicate(slave)))
        .spawn()
        .expect("spawn Orc TUI");
    unsafe { libc::close(slave) };

    let mut terminal = unsafe { File::from_raw_fd(master) };
    let mut reader = terminal.try_clone().expect("clone PTY master");
    let output = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&output);
    let reader_thread = thread::spawn(move || {
        let mut chunk = [0; 4096];
        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 {
                break;
            }
            captured
                .lock()
                .expect("capture TUI output")
                .extend_from_slice(&chunk[..read]);
        }
    });

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !output
        .lock()
        .expect("read captured TUI output")
        .windows(b"\x1b[?1049h".len())
        .any(|window| window == b"\x1b[?1049h")
    {
        assert!(
            Instant::now() < ready_deadline,
            "TUI did not enter raw mode"
        );
        thread::sleep(Duration::from_millis(10));
    }
    for input in [
        b"\x0e".as_slice(),
        b":resize 55\r".as_slice(),
        b":dock left\r".as_slice(),
        b"q".as_slice(),
    ] {
        terminal.write_all(input).expect("send layout command");
        terminal.flush().expect("flush layout command");
        thread::sleep(Duration::from_millis(75));
    }

    let status = child
        .wait_timeout(Duration::from_secs(5))
        .expect("wait for TUI")
        .unwrap_or_else(|| {
            child.kill().expect("stop unresponsive TUI");
            child.wait().expect("reap TUI")
        });
    drop(terminal);
    reader_thread.join().expect("read TUI output");
    let output = output.lock().expect("captured TUI output");
    assert!(status.success(), "TUI exited unsuccessfully: {output:?}");

    let preferences_path = state_home
        .join("orc/workspaces")
        .join(state::scope_key(
            &fs::canonicalize(&scope).expect("canonical scope"),
        ))
        .join("preferences.json");
    let preferences_source = fs::read(&preferences_path).unwrap_or_else(|error| {
        panic!(
            "saved workspace preferences at {}: {error}; terminal output: {}",
            preferences_path.display(),
            String::from_utf8_lossy(&output)
        )
    });
    let preferences: WorkspacePreferences =
        serde_json::from_slice(&preferences_source).expect("parse workspace preferences");
    assert_eq!(preferences.inspector_dock, "left");
    assert_eq!(preferences.inspector_last_dock, "left");
    assert_eq!(preferences.inspector_percent, 55);
    assert!(preferences.inspector_visible);
}
