// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

use std::io::Write;
fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let iso = a.next().unwrap();
    let want = a.next().unwrap();
    let mut img = oxwin_core::UdfImage::open(std::path::Path::new(&iso))?;
    let entries = img.walk()?;
    let e = entries
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case(&want))
        .ok_or_else(|| anyhow::anyhow!("not found: {want}"))?;
    let so = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(4 << 20, so.lock());
    img.copy_file(e, &mut out, |_| {})?;
    out.flush()?;
    Ok(())
}
