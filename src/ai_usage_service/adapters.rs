use super::{Error, Report, Resets, parse_report, parse_resets};
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const STATUS_URL: &str = "https://codex-resets.com/api/v1/status";

static CLIENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into()
});

/// Never formats CLI errors or stderr: both can echo authentication material.
pub(super) fn usage() -> Result<Report, Error> {
    let mut command = Command::new("ai-usagebar");
    command.args(["usage", "--json"]);
    let bytes = command_output(&mut command, TIMEOUT, RESPONSE_LIMIT)?;
    parse_report(&bytes)
}

struct Process(Child);

impl Process {
    fn status(&self) -> Result<Option<rustix::process::WaitIdStatus>, Error> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = Pid::from_raw(self.0.id() as i32).ok_or(Error::Cli)?;
        loop {
            match waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Err(rustix::io::Errno::INTR) => continue,
                result => return result.map_err(|_| Error::Cli),
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Keep the leader waitable until after signaling: reaping it first
        // would allow its PID/PGID to be reused by an unrelated process group.
        // A provider helper may still hold stdout after its leader has exited.
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command_output(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    let started = Instant::now();
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Error::MissingCli
            } else {
                Error::Start
            }
        })?;
    let mut child = Process(child);
    let mut stdout = child.0.stdout.take().ok_or(Error::Start)?;
    let flags = rustix::fs::fcntl_getfl(&stdout).map_err(|_| Error::Start)?;
    rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| Error::Start)?;
    let mut output = Vec::new();
    let mut status = None;
    let mut eof = false;
    let mut buffer = [0u8; 8192];
    loop {
        if started.elapsed() >= timeout {
            return Err(Error::Timeout);
        }
        let read = if eof {
            0
        } else {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    0
                }
                Ok(count) => {
                    if count > limit.saturating_sub(output.len()) {
                        return Err(Error::Format);
                    }
                    output.extend_from_slice(&buffer[..count]);
                    count
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => 0,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Cli),
            }
        };
        if status.is_none() {
            status = child.status()?;
        }
        if let Some(status) = status {
            if status.exit_status() != Some(0) {
                return Err(Error::Cli);
            }
            if eof {
                return Ok(output);
            }
        }
        // Never wait for exit while the pipe can fill, or wait for pipe EOF
        // after exit without checking the same overall deadline.
        if read == 0 {
            std::thread::sleep(
                Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
            );
        }
    }
}

pub(super) fn resets() -> Result<Resets, Error> {
    let mut response = CLIENT
        .get(STATUS_URL)
        .header("Accept", "application/json")
        .call()
        .map_err(http_error)?;
    match response.status().as_u16() {
        200 => {}
        429 => return Err(Error::RateLimit),
        _ => return Err(Error::Network),
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(RESPONSE_LIMIT as u64)
        .read_to_vec()
        .map_err(http_error)?;
    parse_resets(&bytes)
}

fn http_error(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => Error::Timeout,
        ureq::Error::BodyExceedsLimit(_) => Error::Format,
        _ => Error::Network,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observing_exit_keeps_the_process_group_identity_reserved_until_cleanup() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]).process_group(0);
        let child = Process(command.spawn().unwrap());
        let pid = Pid::from_raw(child.0.id() as i32).unwrap();
        waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        )
        .unwrap();
        assert_eq!(child.status().unwrap().unwrap().exit_status(), Some(0));
        assert_eq!(child.status().unwrap().unwrap().exit_status(), Some(0));
        drop(child);
        assert!(matches!(
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG
            ),
            Err(rustix::io::Errno::CHILD)
        ));
    }

    #[test]
    fn capture_drains_large_pipes_and_discards_diagnostics() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf 'diagnostic-not-a-report' >&2; printf '%080000d' 0",
        ]);
        let output = command_output(&mut command, Duration::from_secs(5), 100_000).unwrap();
        assert_eq!(output.len(), 80_000);
        assert!(output.iter().all(u8::is_ascii_digit));
    }

    #[test]
    fn oversized_output_is_bounded_without_waiting_for_the_writer() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "while :; do printf '0123456789'; done"]);
        assert_eq!(
            command_output(&mut command, Duration::from_secs(5), 100),
            Err(Error::Format)
        );
    }

    #[test]
    fn inherited_pipe_cannot_escape_the_command_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "/bin/sleep 10 & exit 0"]);
        assert_eq!(
            command_output(&mut command, Duration::from_millis(30), 100),
            Err(Error::Timeout)
        );
    }

    #[test]
    fn cli_failure_never_publishes_stdout_or_stderr() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf secret; printf secret >&2; exit 1"]);
        assert_eq!(
            command_output(&mut command, Duration::from_secs(5), 100),
            Err(Error::Cli)
        );
    }
}
