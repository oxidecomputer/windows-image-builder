// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! A sparse block-addressed image buffer.
//!
//! An install image is several GiB but most of it is holes: zero padding to reach a
//! whole GiB, and unallocated filesystem space. Materialising all of it costs memory
//! for nothing, so only written blocks are kept and everything else reads as zero.
//!
//! Blocks are 512 KiB, deliberately the same size as the chunks Nexus's bulk-write
//! endpoint takes. An imported Oxide disk is born zeroed, so the uploader can walk the
//! non-zero blocks and skip the rest — which is most of why our own client uploaded
//! 7 GiB in a fraction of the time the CLI took, since the CLI sends the padding too.

use anyhow::{Result, bail};
use std::collections::BTreeMap;

pub const BLOCK_SIZE: usize = 512 * 1024;

pub struct SparseImage {
    size_bytes: u64,
    block_count: u64,
    /// Ordered so iteration is ascending by offset without a sort, which is what the
    /// uploader needs anyway.
    blocks: BTreeMap<u64, Vec<u8>>,
}

/// One block the uploader has to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub offset: u64,
    pub data: &'a [u8],
}

impl SparseImage {
    pub fn new(size_bytes: u64) -> Result<Self> {
        if !size_bytes.is_multiple_of(BLOCK_SIZE as u64) {
            bail!("image size {size_bytes} is not a multiple of {BLOCK_SIZE}");
        }
        Ok(Self {
            size_bytes,
            block_count: size_bytes / BLOCK_SIZE as u64,
            blocks: BTreeMap::new(),
        })
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Write `bytes` at absolute byte `offset`, splitting across blocks as needed.
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("write offset overflows"))?;
        if end > self.size_bytes {
            bail!(
                "write of {} bytes at {offset} runs past end of {}-byte image",
                bytes.len(),
                self.size_bytes
            );
        }
        let mut written = 0usize;
        while written < bytes.len() {
            let at = offset + written as u64;
            let index = at / BLOCK_SIZE as u64;
            let into = (at % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - into).min(bytes.len() - written);
            let block = self
                .blocks
                .entry(index)
                .or_insert_with(|| vec![0u8; BLOCK_SIZE]);
            block[into..into + n].copy_from_slice(&bytes[written..written + n]);
            written += n;
        }
        Ok(())
    }

    /// Read `length` bytes at absolute byte `offset`. Holes read as zero.
    pub fn read(&self, offset: u64, length: usize) -> Vec<u8> {
        let mut out = vec![0u8; length];
        let mut done = 0usize;
        while done < length {
            let at = offset + done as u64;
            let index = at / BLOCK_SIZE as u64;
            let from = (at % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - from).min(length - done);
            if let Some(block) = self.blocks.get(&index) {
                out[done..done + n].copy_from_slice(&block[from..from + n]);
            }
            done += n;
        }
        out
    }

    /// Allocated blocks holding at least one non-zero byte, ascending by offset.
    ///
    /// A block that was written but happens to be all zeros is *excluded*: the disk is
    /// already zero on the other end, so sending it would be wasted transfer.
    pub fn non_zero_blocks(&self) -> Vec<Chunk<'_>> {
        self.blocks
            .iter()
            .filter(|(_, data)| data.iter().any(|b| *b != 0))
            .map(|(index, data)| Chunk {
                offset: index * BLOCK_SIZE as u64,
                data: data.as_slice(),
            })
            .collect()
    }

    /// Bytes the uploader will actually transfer, for progress reporting.
    pub fn occupied_bytes(&self) -> u64 {
        self.non_zero_blocks().len() as u64 * BLOCK_SIZE as u64
    }

    /// Write the full dense image to `out`, holes included.
    ///
    /// Callers that can seek should prefer writing `non_zero_blocks` into a
    /// pre-truncated file — that leaves the holes genuinely sparse on disk instead of
    /// spending gigabytes of I/O writing zeros.
    pub fn write_dense(
        &self,
        out: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        let zeros = vec![0u8; BLOCK_SIZE];
        for index in 0..self.block_count {
            match self.blocks.get(&index) {
                Some(block) => out.write_all(block)?,
                None => out.write_all(&zeros)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_must_be_a_whole_number_of_blocks() {
        assert!(SparseImage::new(BLOCK_SIZE as u64).is_ok());
        assert!(SparseImage::new(BLOCK_SIZE as u64 - 1).is_err());
        assert!(SparseImage::new(0).is_ok());
    }

    #[test]
    fn reads_back_what_was_written() {
        let mut img = SparseImage::new(4 * BLOCK_SIZE as u64).unwrap();
        img.write(10, b"hello").unwrap();
        assert_eq!(&img.read(10, 5), b"hello");
        // Everything around it is a hole.
        assert_eq!(img.read(0, 10), vec![0u8; 10]);
        assert_eq!(img.read(15, 5), vec![0u8; 5]);
    }

    #[test]
    fn a_write_spanning_a_block_boundary_lands_in_both() {
        let mut img = SparseImage::new(4 * BLOCK_SIZE as u64).unwrap();
        let payload: Vec<u8> =
            (0..1000u32).map(|i| (i % 251) as u8 + 1).collect();
        let offset = BLOCK_SIZE as u64 - 500;
        img.write(offset, &payload).unwrap();
        assert_eq!(img.read(offset, payload.len()), payload);
        assert_eq!(img.non_zero_blocks().len(), 2);
    }

    #[test]
    fn a_write_spanning_several_blocks_is_contiguous() {
        let mut img = SparseImage::new(8 * BLOCK_SIZE as u64).unwrap();
        let payload = vec![0xabu8; BLOCK_SIZE * 3 + 17];
        img.write(BLOCK_SIZE as u64 / 2, &payload).unwrap();
        assert_eq!(img.read(BLOCK_SIZE as u64 / 2, payload.len()), payload);
        assert_eq!(img.non_zero_blocks().len(), 4);
    }

    #[test]
    fn writing_past_the_end_is_refused() {
        let mut img = SparseImage::new(BLOCK_SIZE as u64).unwrap();
        assert!(img.write(BLOCK_SIZE as u64 - 2, b"abc").is_err());
        assert!(img.write(0, &vec![0u8; BLOCK_SIZE]).is_ok());
    }

    /// The property the uploader depends on: an allocated but all-zero block is not
    /// sent, because an imported disk is already zeroed.
    #[test]
    fn all_zero_blocks_are_not_transferred() {
        let mut img = SparseImage::new(4 * BLOCK_SIZE as u64).unwrap();
        img.write(0, &[0u8; 16]).unwrap(); // allocates block 0, all zeros
        assert_eq!(img.non_zero_blocks().len(), 0);
        assert_eq!(img.occupied_bytes(), 0);

        img.write(0, &[1u8]).unwrap();
        assert_eq!(img.non_zero_blocks().len(), 1);
        assert_eq!(img.occupied_bytes(), BLOCK_SIZE as u64);
    }

    #[test]
    fn blocks_come_back_ascending_by_offset() {
        let mut img = SparseImage::new(8 * BLOCK_SIZE as u64).unwrap();
        for index in [5u64, 1, 7, 0] {
            img.write(index * BLOCK_SIZE as u64, &[1u8]).unwrap();
        }
        let offsets: Vec<u64> =
            img.non_zero_blocks().iter().map(|c| c.offset).collect();
        assert_eq!(
            offsets,
            vec![
                0,
                BLOCK_SIZE as u64,
                5 * BLOCK_SIZE as u64,
                7 * BLOCK_SIZE as u64
            ]
        );
    }

    #[test]
    fn dense_output_is_the_full_size_with_holes_zeroed() {
        let mut img = SparseImage::new(3 * BLOCK_SIZE as u64).unwrap();
        img.write(2 * BLOCK_SIZE as u64, b"tail").unwrap();
        let mut buf = Vec::new();
        img.write_dense(&mut buf).unwrap();
        assert_eq!(buf.len(), 3 * BLOCK_SIZE);
        assert!(buf[..2 * BLOCK_SIZE].iter().all(|b| *b == 0));
        assert_eq!(&buf[2 * BLOCK_SIZE..2 * BLOCK_SIZE + 4], b"tail");
    }

    /// A hole is indistinguishable from a written run of zeros when reading, which is
    /// what lets the builder skip padding entirely.
    #[test]
    fn holes_and_written_zeros_read_identically() {
        let mut img = SparseImage::new(2 * BLOCK_SIZE as u64).unwrap();
        img.write(0, &[0u8; 32]).unwrap();
        assert_eq!(img.read(0, 32), img.read(BLOCK_SIZE as u64, 32));
    }
}
