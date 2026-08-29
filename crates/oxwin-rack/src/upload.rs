// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Getting an image onto a rack as a disk.
//!
//! The bulk-write import is a five-step handshake: create, start, write, stop,
//! finalize; and the disk is in a state that **refuses deletion** for the middle three.
//!
//! The chunk planning is separated from the network on purpose ([`plan`]), so the part
//! that decides which bytes get sent is testable without a rack.

use crate::profile::Selector;
use anyhow::{Context, Result, bail};
// The disk calls live on an extension trait, not on `Client` itself.
use oxide::ClientDisksExt;
use oxwin_core::engine::Cancel;
use oxwin_core::progress::Reporter;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes per bulk write. The API caps a single request at 512 KiB.
pub const CHUNK: usize = 512 * 1024;

/// Block size for the installer disk.
///
/// **Not a preference.** The image's MBR expresses partition offsets in sectors, so p1 at
/// LBA 2048 is 1 MiB into a 512-byte-block disk and 8 MiB into a 4096-byte one. Import
/// the same bytes at the wrong block size and the partition table points at nothing,
/// the firmware finds nothing to boot, and there is no error anywhere to explain it.
pub const INSTALLER_BLOCK_SIZE: i64 = 512;

/// One gibibyte, the granularity the control plane accepts.
const GIB: u64 = 1024 * 1024 * 1024;

/// What to create.
#[derive(Debug, Clone)]
pub struct DiskSpec {
    pub name: String,
    pub description: String,
    /// 512, 2048 or 4096. Use [`INSTALLER_BLOCK_SIZE`] for an installer image.
    pub block_size: i64,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub disk: String,
    /// Bytes actually put on the wire.
    pub sent: u64,
    /// Bytes that were all-zero and therefore skipped.
    pub skipped: u64,
}

/// Disk size for an image of `bytes`, in whole GiB.
///
/// Rounded **up**, and never zero. The control plane requires a whole number of GiB of
/// at least one, and without the rounding the very first real upload is rejected — which
/// is how this was found the first time.
pub fn disk_size_gib(bytes: u64) -> u64 {
    bytes.div_ceil(GIB).max(1)
}

/// One unit of work: where it goes, and how big it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u64,
    pub len: usize,
}

/// How many bulk writes are in flight at once.
///
/// **This is the difference between five minutes and fifty.** Sent serially, each 512 KiB
/// write costs a full round trip, measured at roughly 220 ms to a rack in on example, which
/// works out at about 2.3 MiB/s no matter how fast the link is, because nearly all of that
/// time is spent waiting rather than transferring. The upload is latency-bound, not
/// bandwidth-bound, and the only fix is to keep many requests outstanding.
///
/// 16 costs at most `16 * CHUNK` = 8 MiB of buffers. Override with
/// `OXWIN_UPLOAD_CONCURRENCY`.
pub const CONCURRENCY: usize = 16;

fn concurrency() -> usize {
    std::env::var("OXWIN_UPLOAD_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(CONCURRENCY)
}

/// The chunks of an image that actually need uploading.
///
/// **All-zero chunks are skipped**, greatly speeding up image transer of install media.
///  It is safe because a disk created with `importing_blocks` is zeroed by the control
/// plane, so a chunk we never write already holds the bytes we would have written.
pub struct Chunks<R> {
    source: R,
    offset: u64,
    cancel: Cancel,
    done: bool,
    /// Bytes skipped so far, shared with the uploader.
    ///
    /// Progress has to count these. Measured against the image, a bar fed only by bytes
    /// *sent* tops out at the non-zero fraction, the first real 7 GiB upload stopped at
    /// 68%, because 2.18 GiB of it was zeroes.
    skipped: Arc<AtomicU64>,
}

impl<R: Read> Chunks<R> {
    pub fn new(source: R, cancel: Cancel, skipped: Arc<AtomicU64>) -> Self {
        Self { source, offset: 0, cancel, done: false, skipped }
    }
}

impl<R: Read> Iterator for Chunks<R> {
    type Item = Result<(Chunk, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            if let Err(e) = self.cancel.check() {
                self.done = true;
                return Some(Err(e));
            }

            let mut buffer = vec![0u8; CHUNK];
            // A short read is not the end of the file, it is what `Read` is allowed to
            // do at any time.
            let mut filled = 0;
            while filled < buffer.len() {
                match self.source.read(&mut buffer[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        continue;
                    }
                    Err(e) => {
                        self.done = true;
                        return Some(Err(
                            anyhow::Error::new(e).context("reading the image")
                        ));
                    }
                }
            }
            if filled == 0 {
                self.done = true;
                return None;
            }

            let offset = self.offset;
            // Advance before the zero test, so a skipped chunk still leaves the next one
            // landing where it belongs in the image.
            self.offset += filled as u64;

            if buffer[..filled].iter().all(|&b| b == 0) {
                self.skipped.fetch_add(filled as u64, Ordering::Relaxed);
                continue;
            }
            buffer.truncate(filled);
            return Some(Ok((Chunk { offset, len: filled }, buffer)));
        }
    }
}

/// Turns acknowledged and skipped bytes into progress events.
pub struct Progress {
    total: u64,
    /// Shared with [`Chunks`], which increments it as it skips.
    skipped: Arc<AtomicU64>,
    sent: std::cell::Cell<u64>,
    last_percent: std::cell::Cell<u8>,
    last_emit: std::cell::Cell<std::time::Instant>,
    /// Emit at least this often even when the percent has not moved.
    ///
    /// Percent alone is far too coarse at this scale. One percent of a 7 GiB image is
    /// 71 MiB, which at real upload speeds is several seconds of a byte counter sitting
    /// perfectly still, and a UI that does not move is a UI that has frozen, as far as
    /// anyone watching it can tell. That matters more than usual here, because the
    /// natural response is to kill it, and killing it mid-import strands the disk.
    min_interval: std::time::Duration,
}

impl Progress {
    /// Returns the tracker and the counter to hand to [`Chunks::new`].
    pub fn new(total: u64) -> (Self, Arc<AtomicU64>) {
        let skipped = Arc::new(AtomicU64::new(0));
        (
            Self {
                total,
                skipped: skipped.clone(),
                sent: std::cell::Cell::new(0),
                last_percent: std::cell::Cell::new(u8::MAX),
                last_emit: std::cell::Cell::new(std::time::Instant::now()),
                min_interval: std::time::Duration::from_millis(250),
            },
            skipped,
        )
    }

    /// Record `acked` newly written bytes.
    ///
    /// `Some` only when the whole percent has moved: thousands of chunks would otherwise
    /// flood the channel with events nothing can render.
    pub fn advance(&self, acked: u64) -> Option<(f32, String)> {
        self.advance_at(acked, std::time::Instant::now())
    }

    /// [`Progress::advance`] against an explicit instant, so the throttle is testable
    /// without sleeping.
    pub fn advance_at(
        &self,
        acked: u64,
        now: std::time::Instant,
    ) -> Option<(f32, String)> {
        self.sent.set(self.sent.get() + acked);
        // Skipped bytes are progress. They are bytes of the image that need no work, not
        // bytes still to come, and counting only what was sent is what made the first
        // real upload stop at 68% with nothing left to do.
        let so_far = self.sent.get() + self.skipped.load(Ordering::Relaxed);
        let total = self.total.max(1);
        let percent = (so_far * 100 / total) as u8;
        // Either the percent moved, or enough time has passed that the counter needs to
        // show it is still alive.
        let stale =
            now.duration_since(self.last_emit.get()) >= self.min_interval;
        if percent == self.last_percent.get() && !stale {
            return None;
        }
        self.last_percent.set(percent);
        self.last_emit.set(now);
        Some((
            so_far as f32 / total as f32,
            format!("{} of {} MiB", so_far / 1048576, self.total / 1048576),
        ))
    }

    pub fn sent(&self) -> u64 {
        self.sent.get()
    }
}

/// A connection to one project on one rack.
///
/// Holds the runtime, so the async never escapes this crate — see the crate docs.
pub struct Rack {
    runtime: tokio::runtime::Runtime,
    client: oxide::Client,
    project: String,
}

impl Rack {
    /// Connect using an already established login.
    ///
    /// Note what is *not* done here: a profile is named only when the user actually
    /// chose one.
    pub fn connect(selector: &Selector, project: &str) -> Result<Self> {
        let mut config = oxide::ClientConfig::default();
        if let Selector::Profile(name) = selector {
            config = config.with_profile(name);
        }
        let client = oxide::Client::new_authenticated_config(&config)
            .context(
                "no usable Oxide login. Run `oxide auth login`, or set OXIDE_HOST and \
                 OXIDE_TOKEN",
            )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("starting the HTTP runtime")?;
        Ok(Self { runtime, client, project: project.to_string() })
    }

    /// Create a disk and upload `image` into it.
    ///
    /// On any failure, including cancellation, the import is torn down so the disk
    /// does not sit in a state that refuses deletion. The disk itself is deliberately
    /// **left in place**.
    pub fn upload_image(
        &self,
        image: &std::path::Path,
        spec: &DiskSpec,
        reporter: &Reporter,
        cancel: &Cancel,
    ) -> Result<Uploaded> {
        let bytes = std::fs::metadata(image)
            .with_context(|| format!("{}", image.display()))?
            .len();
        if bytes == 0 {
            bail!("{} is empty", image.display());
        }
        let gib = disk_size_gib(bytes);

        reporter.phase(
            "upload",
            format!("creating disk {} ({gib} GiB)", spec.name),
        );
        self.create_disk(spec, gib)?;

        reporter.log(format!("starting bulk import into {}", spec.name));
        self.block_on(
            self.client
                .disk_bulk_write_import_start()
                .project(&self.project)
                .disk(&spec.name)
                .send(),
        )
        .context("starting the bulk import")?;

        // Everything from here until `stop` leaves the disk undeletable, so every exit
        // has to go through the teardown rather than returning directly.
        let outcome = self.write_all(image, bytes, spec, reporter, cancel);
        let teardown = self.finish_import(spec, outcome.is_ok());

        let sent = outcome?;
        teardown?;
        // Every byte was either sent or skipped, so the skipped total is exact without
        // being counted separately.
        let uploaded =
            Uploaded { disk: spec.name.clone(), sent, skipped: bytes - sent };
        reporter.phase(
            "upload",
            format!(
                "{} uploaded: {:.2} GiB sent, {:.2} GiB of zeroes skipped",
                spec.name,
                uploaded.sent as f64 / GIB as f64,
                uploaded.skipped as f64 / GIB as f64
            ),
        );
        Ok(uploaded)
    }

    fn create_disk(&self, spec: &DiskSpec, gib: u64) -> Result<()> {
        let body = oxide::types::DiskCreate {
            name: spec.name.parse().map_err(|e| {
                anyhow::anyhow!("disk name {:?}: {e}", spec.name)
            })?,
            description: spec.description.clone(),
            size: oxide::types::ByteCount(gib * GIB),
            disk_backend: oxide::types::DiskSource::ImportingBlocks {
                block_size: spec.block_size.try_into().map_err(|e| {
                    anyhow::anyhow!("block size {}: {e}", spec.block_size)
                })?,
            }
            .into(),
        };
        self.block_on(
            self.client.disk_create().project(&self.project).body(body).send(),
        )
        .with_context(|| format!("creating disk {}", spec.name))?;
        Ok(())
    }

    /// Upload every non-zero chunk, `concurrency()` requests in flight.
    ///
    /// Progress is reported from *completed* bytes rather than from the read position.
    /// Completions arrive out of order, so the read position would run ahead of what the
    /// rack has actually accepted and the bar would reach 100% with writes outstanding.
    fn write_all(
        &self,
        image: &std::path::Path,
        bytes: u64,
        spec: &DiskSpec,
        reporter: &Reporter,
        cancel: &Cancel,
    ) -> Result<u64> {
        use futures::stream::{StreamExt, TryStreamExt};

        let file = std::fs::File::open(image)
            .with_context(|| format!("{}", image.display()))?;
        let source = std::io::BufReader::with_capacity(CHUNK, file);
        let (progress, skipped) = Progress::new(bytes);
        let chunks = Chunks::new(source, cancel.clone(), skipped);

        self.runtime.block_on(async {
            futures::stream::iter(chunks)
                .map(|item| {
                    let progress = &progress;
                    async move {
                        let (chunk, data) = item?;
                        let body = oxide::types::ImportBlocksBulkWrite {
                            offset: chunk.offset,
                            base64_encoded_data: base64(&data),
                        };
                        self.client
                            .disk_bulk_write_import()
                            .project(&self.project)
                            .disk(&spec.name)
                            .body(body)
                            .send()
                            .await
                            .with_context(|| {
                                format!(
                                    "writing {} bytes at offset {}",
                                    chunk.len, chunk.offset
                                )
                            })?;

                        if let Some((fraction, detail)) =
                            progress.advance(chunk.len as u64)
                        {
                            reporter.fraction(fraction, detail);
                        }
                        Ok::<(), anyhow::Error>(())
                    }
                })
                .buffer_unordered(concurrency())
                .try_collect::<Vec<()>>()
                .await
        })?;

        Ok(progress.sent())
    }

    /// Leave the import state, whether or not the upload worked.
    ///
    /// `stop` always runs: a disk left mid-import is stuck in `import_ready`, and the
    /// only way out is stop → finalize → detached → delete. Skipping it because the
    /// upload failed is what turns a failed upload into a resource the user cannot
    /// remove.
    ///
    /// `finalize` runs only on success, because finalizing a half-written disk would
    /// present it as a complete one.
    fn finish_import(&self, spec: &DiskSpec, succeeded: bool) -> Result<()> {
        let stop = self
            .block_on(
                self.client
                    .disk_bulk_write_import_stop()
                    .project(&self.project)
                    .disk(&spec.name)
                    .send(),
            )
            .with_context(|| {
                format!(
                    "stopping the bulk import of {}. It may be stuck in \
                     import_ready; `oxide disk finalize-import --disk {}` then delete \
                     it",
                    spec.name, spec.name
                )
            });

        if !succeeded {
            // The upload already failed; report that, not this. But a failing stop is
            // worth surfacing, because it is the one that stranded the disk.
            return stop.map(|_| ());
        }
        stop?;

        self.block_on(
            self.client
                .disk_finalize_import()
                .project(&self.project)
                .disk(&spec.name)
                .body(oxide::types::FinalizeDisk::default())
                .send(),
        )
        .with_context(|| format!("finalizing {}", spec.name))?;
        Ok(())
    }

    pub(crate) fn block_on<F: std::future::Future>(
        &self,
        future: F,
    ) -> F::Output {
        self.runtime.block_on(future)
    }

    pub(crate) fn client(&self) -> &oxide::Client {
        &self.client
    }

    /// The project every call in this crate is scoped to.
    pub fn project(&self) -> &str {
        &self.project
    }
}

/// Standard base64, which is what the API expects for a bulk write body.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let b = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let n =
            (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if group.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if group.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_size_rounds_up_to_whole_gibibytes() {
        assert_eq!(disk_size_gib(1), 1, "never zero, and never fractional");
        assert_eq!(disk_size_gib(GIB), 1);
        assert_eq!(
            disk_size_gib(GIB + 1),
            2,
            "a single byte over needs another"
        );
        assert_eq!(disk_size_gib(7 * GIB), 7);
        // The real case: an image that is not a round size must not be truncated, which
        // is what rounding *down* would do, losing the tail of the exFAT volume.
        assert_eq!(disk_size_gib(7 * GIB + 1234), 8);
    }

    /// Collect what would go on the wire.
    fn run(data: &[u8]) -> (Vec<(u64, usize)>, u64) {
        let seen: Vec<(u64, usize)> = Chunks::new(
            std::io::Cursor::new(data.to_vec()),
            Cancel::new(),
            Arc::new(AtomicU64::new(0)),
        )
        .map(|item| {
            let (chunk, bytes) = item.unwrap();
            assert_eq!(
                chunk.len,
                bytes.len(),
                "the chunk must describe the buffer it came with"
            );
            (chunk.offset, chunk.len)
        })
        .collect();
        let sent = seen.iter().map(|(_, len)| *len as u64).sum();
        (seen, sent)
    }

    #[test]
    fn zero_chunks_are_skipped_and_the_rest_keep_their_offsets() {
        let mut data = vec![0u8; CHUNK * 3];
        // Only the middle chunk has content.
        data[CHUNK] = 1;
        let (seen, sent) = run(&data);

        assert_eq!(seen, vec![(CHUNK as u64, CHUNK)], "only the middle chunk");
        assert_eq!(sent, CHUNK as u64);
        // Skipped is derived as total minus sent, which is what the uploader does.
        assert_eq!(data.len() as u64 - sent, (CHUNK * 2) as u64);
    }

    /// The offset is the position in the *image*, not a running count of what was sent.
    /// Deriving it from bytes sent would place every chunk after the first skipped one
    /// at the wrong offset, producing a disk that is subtly wrong and boots as garbage.
    #[test]
    fn skipping_does_not_shift_later_offsets() {
        let mut data = vec![0u8; CHUNK * 4];
        data[CHUNK * 3] = 1;
        let (seen, _) = run(&data);
        assert_eq!(seen, vec![((CHUNK * 3) as u64, CHUNK)]);
    }

    #[test]
    fn a_final_short_chunk_is_sent_whole() {
        let data = vec![7u8; CHUNK + 10];
        let (seen, sent) = run(&data);
        assert_eq!(seen, vec![(0, CHUNK), (CHUNK as u64, 10)]);
        assert_eq!(sent, (CHUNK + 10) as u64);
    }

    /// An entirely zero image sends nothing at all, and must not error, because an
    /// image can legitimately have long zero runs and the degenerate case is the same
    /// code path.
    #[test]
    fn an_all_zero_image_sends_nothing() {
        let (seen, sent) = run(&vec![0u8; CHUNK * 2]);
        assert!(seen.is_empty());
        assert_eq!(sent, 0);
    }

    /// The buffer handed out must be exactly the chunk, not a full-size buffer with a
    /// zero tail. Uploading the padding would write beyond the end of the image.
    #[test]
    fn the_last_buffer_is_truncated_to_what_was_read() {
        let data = vec![7u8; CHUNK + 10];
        let last = Chunks::new(
            std::io::Cursor::new(data),
            Cancel::new(),
            Arc::new(AtomicU64::new(0)),
        )
        .last()
        .unwrap()
        .unwrap();
        assert_eq!(last.1.len(), 10);
        assert!(last.1.iter().all(|&b| b == 7), "no padding may be uploaded");
    }

    /// A reader that hands back one byte at a time, which `Read` is entirely allowed to
    /// do. Without the fill loop this chunks the image into 1-byte writes at every
    /// offset, a working-looking upload that takes a week and produces a wrong disk.
    struct Dribble(std::io::Cursor<Vec<u8>>);

    impl Read for Dribble {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            self.0.read(&mut buf[..1])
        }
    }

    #[test]
    fn short_reads_still_produce_full_chunks() {
        let data = vec![3u8; CHUNK + 5];
        let seen: Vec<(u64, usize)> = Chunks::new(
            Dribble(std::io::Cursor::new(data)),
            Cancel::new(),
            Arc::new(AtomicU64::new(0)),
        )
        .map(|i| {
            let (c, _) = i.unwrap();
            (c.offset, c.len)
        })
        .collect();
        assert_eq!(seen, vec![(0, CHUNK), (CHUNK as u64, 5)]);
    }

    #[test]
    fn cancellation_stops_the_upload() {
        let cancel = Cancel::new();
        cancel.cancel();
        let mut chunks = Chunks::new(
            std::io::Cursor::new(vec![1u8; CHUNK * 2]),
            cancel,
            Arc::new(AtomicU64::new(0)),
        );
        assert!(
            chunks.next().expect("an error, not the end").is_err(),
            "a cancelled upload must not yield chunks to send"
        );
        assert!(chunks.next().is_none(), "and must not resume afterwards");
    }

    /// Every byte of the image is either sent or skipped.
    ///
    /// The first real upload proved it matters: 2.18 GiB of a 7 GiB image was zeroes, so
    /// a bar fed only by bytes sent stopped dead at 68% with the upload finished. This
    /// pins the arithmetic that fixes it.
    #[test]
    fn sent_plus_skipped_accounts_for_the_whole_image() {
        // Two zero chunks, one with content, one short tail with content.
        let mut data = vec![0u8; CHUNK * 3 + 100];
        data[CHUNK] = 1;
        data[CHUNK * 3 + 5] = 1;

        let skipped = Arc::new(AtomicU64::new(0));
        let sent: u64 = Chunks::new(
            std::io::Cursor::new(data.clone()),
            Cancel::new(),
            skipped.clone(),
        )
        .map(|i| i.unwrap().0.len as u64)
        .sum();

        assert_eq!(sent, (CHUNK + 100) as u64);
        assert_eq!(skipped.load(Ordering::Relaxed), (CHUNK * 2) as u64);
        assert_eq!(
            sent + skipped.load(Ordering::Relaxed),
            data.len() as u64,
            "progress would never reach 100%"
        );
    }

    /// The bar must reach 100% on an image with zeroes in it.
    ///
    /// This is the defect the first real 7 GiB upload exposed: 2.18 GiB of it was
    /// zeroes, so a bar fed only by bytes *sent* stopped dead at 68% while the upload
    /// was in fact finished. We dont want the progress bar looking frozen.
    #[test]
    fn progress_counts_skipped_bytes_and_reaches_the_end() {
        let total = 1000u64;
        let (progress, skipped) = Progress::new(total);

        // 310 bytes were all-zero and never sent, as the iterator would record.
        skipped.store(310, Ordering::Relaxed);
        // The remaining 690 are acknowledged in pieces.
        let mut last = None;
        for _ in 0..69 {
            if let Some(update) = progress.advance(10) {
                last = Some(update);
            }
        }

        assert_eq!(progress.sent(), 690);
        let (fraction, _) = last.expect("some update must have been emitted");
        assert_eq!(
            fraction, 1.0,
            "the bar must reach the end; counting only sent bytes stops it at 69%"
        );
    }

    /// A byte counter that stands still reads as a frozen app. Break progress down.
    #[test]
    fn progress_speaks_periodically_even_within_one_percent() {
        let start = std::time::Instant::now();
        // A big image, so a single chunk is far below one percent.
        let (progress, _) = Progress::new(100 * 1024 * 1024 * 1024);

        assert!(progress.advance_at(CHUNK as u64, start).is_some(), "first");
        // Same percent, no time passed: silence is correct.
        assert!(progress.advance_at(CHUNK as u64, start).is_none());
        // Same percent, but a quarter second later, say something.
        let later = start + std::time::Duration::from_millis(300);
        assert!(
            progress.advance_at(CHUNK as u64, later).is_some(),
            "the counter must move even inside a single percent"
        );
    }

    /// One event per whole percent.
    #[test]
    fn progress_only_speaks_when_the_percent_moves() {
        let (progress, _) = Progress::new(10_000);
        // One instant throughout, so this tests the percent rule alone rather than
        // racing the time-based one.
        let now = std::time::Instant::now();
        // Each of these is a hundredth of a percent.
        assert!(
            progress.advance_at(1, now).is_some(),
            "the first is always news"
        );
        assert!(progress.advance_at(1, now).is_none());
        assert!(progress.advance_at(1, now).is_none());
        // Crossing into 1% is.
        assert!(progress.advance_at(97, now).is_some());
    }

    /// The default is a constant, but the environment has to be able to raise it on a
    /// link where 16 in flight is not enough.
    #[test]
    fn concurrency_is_at_least_one() {
        assert!(
            concurrency() >= 1,
            "zero would stall buffer_unordered forever"
        );
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // Bytes above 0x7f and the two characters that distinguish standard base64 from
        // the URL-safe variant. Sending URL-safe would be accepted as a string and
        // decode to different bytes.
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
    }
}
