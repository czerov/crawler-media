//! Shared cancellation signal for media child processes; no API dependencies.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Clone, Default, Debug)]
pub struct WorkControl {
    cancelled: Arc<AtomicBool>,
    publication: Arc<std::sync::Mutex<()>>,
}

impl WorkControl {
    pub fn cancel(&self) {
        let _guard = self.publication.lock().unwrap_or_else(|poison| poison.into_inner());
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn publish<T>(&self, publish: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _guard = self.publication.lock().map_err(|_| "publication gate poisoned".to_string())?;
        if self.is_cancelled() { return Err("Library work cancelled".into()); }
        publish()
    }
    pub fn is_cancelled(&self) -> bool { self.cancelled.load(Ordering::Acquire) }
    pub fn output(&self, command: &mut std::process::Command) -> Result<std::process::Output, String> {
        use std::{io::Read, process::Stdio, thread, time::Duration};
        if self.is_cancelled() { return Err("Library work cancelled".into()); }
        let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out = thread::spawn(move || { let mut bytes = Vec::new(); if let Some(mut pipe) = stdout { pipe.read_to_end(&mut bytes)?; } Ok::<_, std::io::Error>(bytes) });
        let err = thread::spawn(move || { let mut bytes = Vec::new(); if let Some(mut pipe) = stderr { pipe.read_to_end(&mut bytes)?; } Ok::<_, std::io::Error>(bytes) });
        let status = loop {
            if self.is_cancelled() { let _ = child.kill(); break child.wait(); }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(error) => { let _ = child.kill(); let _ = child.wait(); break Err(error); }
            }
        };
        let stdout = out.join().map_err(|_| "stdout reader panicked")?.map_err(|e| e.to_string())?;
        let stderr = err.join().map_err(|_| "stderr reader panicked")?.map_err(|e| e.to_string())?;
        if self.is_cancelled() { return Err("Library work cancelled".into()); }
        Ok(std::process::Output { status: status.map_err(|e| e.to_string())?, stdout, stderr })
    }
}
