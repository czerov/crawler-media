use downloader::Downloader;
use library::{Ffprobe, MediaProbe};

use crate::add::admit_and_add;
use crate::{RunInput, RunOutcome, SubscribeError, collection_destinations};

pub fn run<D: Downloader + ?Sized>(input: RunInput<'_, D>) -> Result<RunOutcome, SubscribeError> {
    run_with_probe(input, &Ffprobe::default())
}

pub fn run_with_probe<D: Downloader + ?Sized>(
    input: RunInput<'_, D>,
    probe: &dyn MediaProbe,
) -> Result<RunOutcome, SubscribeError> {
    run_with_destinations(input, probe, &[])
}

/// Admit, collect, and reject a run whose torrents never reached the Downloader.
/// `existing` carries the persisted ledger source→destination pairs; collection
/// needs them to deduplicate already-imported videos and to recover sidecars
/// whose video was imported on an earlier tick.
pub fn run_with_destinations<D: Downloader + ?Sized>(
    input: RunInput<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[collection_destinations::DestinationMapping],
) -> Result<RunOutcome, SubscribeError> {
    run_with_destinations_controlled(input, probe, existing, &library::WorkControl::default())
}

pub fn run_with_destinations_controlled<D: Downloader + ?Sized>(
    input: RunInput<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[collection_destinations::DestinationMapping],
    control: &library::WorkControl,
) -> Result<RunOutcome, SubscribeError> {
    control.check()?;
    let added = admit_and_add(input)?;
    control.check()?;
    let outcome = crate::collect::collect_completed_controlled(added, probe, existing, control)?;
    if !outcome.submission_errors.is_empty() && outcome.torrents_added.is_empty() {
        return Err(SubscribeError::Downloader(
            downloader::DownloaderError::Message(outcome.submission_errors.join("; ")),
        ));
    }
    Ok(outcome)
}
