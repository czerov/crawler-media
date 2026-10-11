//! Cooperative cancellation for Library work and its directly owned children.
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use crate::LibraryError;

#[derive(Clone, Default, Debug)]
pub struct WorkControl {
    signal: marker::WorkControl,
}

impl WorkControl {
    pub fn signal(&self) -> &marker::WorkControl { &self.signal }
    pub fn publish<T>(&self, publish: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.signal.publish(publish)
    }
    pub fn cancel(&self) { self.signal.cancel(); }
    pub fn is_cancelled(&self) -> bool { self.signal.is_cancelled() }
    pub fn check(&self) -> Result<(), LibraryError> {
        if self.is_cancelled() { Err(LibraryError::Cancelled) } else { Ok(()) }
    }

    pub fn output(&self, command: &mut Command) -> Result<Output, LibraryError> {
        self.check()?;
        let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out = thread::spawn(move || read_pipe(stdout));
        let err = thread::spawn(move || read_pipe(stderr));
        let mut cancelled = false;
        let status = loop {
            if self.is_cancelled() {
                cancelled = true;
                // Even if the child already exited, wait reaps it before returning.
                let _ = child.kill();
                break child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(error);
                }
            }
        };
        let stdout = join_reader(out)?;
        let stderr = join_reader(err)?;
        if cancelled { return Err(LibraryError::Cancelled); }
        self.check()?;
        Ok(Output { status: status?, stdout, stderr })
    }
}

fn read_pipe(pipe: Option<impl Read>) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe { pipe.read_to_end(&mut bytes)?; }
    Ok(bytes)
}

fn join_reader(reader: thread::JoinHandle<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>, LibraryError> {
    reader.join().map_err(|_| LibraryError::Probe("child output reader panicked".into()))?
        .map_err(LibraryError::Io)
}
