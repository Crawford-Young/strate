//! Running a command with a deadline, for the `claude` and `gh` runners.

use std::io::{self, Read};
use std::panic::resume_unwind;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// `program args` with no stdin, a piped stdout, `stderr` as given and no
/// console window on Windows.
pub(crate) fn command<I, S>(program: &Path, args: I, stderr: Stdio) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Drained on its own thread so a full pipe cannot stall the child.
fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        pipe.read_to_end(&mut out).map(|_| out)
    })
}

/// Spawns `command` (stdout piped) and returns its stdout, killing a run
/// that outlasts `timeout`. A non-zero exit is an error; when stderr was
/// piped, the start of it follows the status.
pub(crate) fn run(mut command: Command, timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = command.spawn()?;
    let stdout = drain(child.stdout.take().expect("stdout is piped"));
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no answer within {timeout:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let join = |reader: JoinHandle<io::Result<Vec<u8>>>| {
        reader.join().unwrap_or_else(|panic| resume_unwind(panic))
    };
    let out = join(stdout)?;
    let err = stderr.map(join).transpose()?;
    if status.success() {
        return Ok(out);
    }
    Err(io::Error::other(match err {
        Some(err) => {
            let err = String::from_utf8_lossy(&err);
            let err: String = err.trim().chars().take(300).collect();
            format!("exited with {status}: {err}")
        }
        None => format!("exited with {status}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The current test binary, run with a filter matching no test: it
    /// prints libtest's summary and exits 0.
    fn this_binary(filter: &str, stderr: Stdio) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("exe"));
        command
            .args([filter, "--exact"])
            .stdout(Stdio::piped())
            .stderr(stderr);
        command
    }

    #[test]
    fn a_failing_run_carries_its_stderr_only_when_piped() {
        let failing = |stderr| {
            let mut command = this_binary("proc::tests::complainer", stderr);
            command.env("STRATE_COMPLAINER", "1");
            run(command, Duration::from_secs(30))
                .expect_err("fails")
                .to_string()
        };
        let piped = failing(Stdio::piped());
        assert!(piped.starts_with("exited with"), "{piped}");
        assert!(piped.ends_with(": lorem went wrong"), "{piped}");
        let nulled = failing(Stdio::null());
        assert!(nulled.starts_with("exited with"), "{nulled}");
        assert!(!nulled.contains("lorem"), "{nulled}");
    }

    /// Writes to stderr and fails, only when run by
    /// [`a_failing_run_carries_its_stderr_only_when_piped`].
    #[test]
    fn complainer() {
        if std::env::var_os("STRATE_COMPLAINER").is_some() {
            // Not eprintln!: libtest would capture it.
            let _ = io::Write::write_all(
                &mut io::stderr(),
                b"lorem went wrong
",
            );
            std::process::exit(3);
        }
    }

    #[test]
    fn a_missing_program_does_not_spawn() {
        let command = command(
            Path::new("/no/such/dir/strate-proc-test"),
            ["x"],
            Stdio::null(),
        );
        assert!(run(command, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn the_command_has_the_given_args() {
        let command = command(Path::new("p"), ["a", "b"], Stdio::null());
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["a", "b"]);
    }
}
