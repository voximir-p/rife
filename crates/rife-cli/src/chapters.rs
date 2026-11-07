// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{collections::BTreeMap, fmt::Write, fs, path::Path};

#[derive(Debug, Deserialize)]
pub(crate) struct Chapter {
    time_base: String,
    start: i64,
    end: i64,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

pub(crate) fn write_retimed(
    path: &Path,
    chapters: &[Chapter],
    source_start: f64,
    speed: f64,
) -> Result<()> {
    fs::write(path, retimed_metadata(chapters, source_start, speed)?)
        .context("could not write retimed chapter metadata")
}

fn retimed_metadata(chapters: &[Chapter], source_start: f64, speed: f64) -> Result<String> {
    ensure!(source_start.is_finite(), "invalid source start time");
    ensure!(
        speed.is_finite() && speed > 0.0,
        "invalid chapter retiming speed"
    );
    let mut metadata = String::from(";FFMETADATA1\n");
    for chapter in chapters {
        let (numerator, denominator) = chapter
            .time_base
            .split_once('/')
            .and_then(|(num, den)| Some((num.parse::<u32>().ok()?, den.parse::<u32>().ok()?)))
            .filter(|(num, den)| *num > 0 && *den > 0)
            .context("invalid chapter time base")?;
        ensure!(
            chapter.start <= chapter.end,
            "chapter ends before its start"
        );
        let seconds_per_tick = f64::from(numerator) / f64::from(denominator);
        let end = chapter.end as f64 * seconds_per_tick - source_start;
        // Match FFmpeg's normal chapter copying: omit chapters before the
        // normalized input origin and clamp chapters crossing that origin.
        if end < 0.0 {
            continue;
        }
        let start = (chapter.start as f64 * seconds_per_tick - source_start).max(0.0);
        let timestamp = |seconds: f64| -> Result<i64> {
            let nanos = (seconds / speed * 1_000_000_000.0).round();
            ensure!(
                nanos.is_finite() && nanos >= 0.0 && nanos < i64::MAX as f64,
                "retimed chapter timestamp is outside FFmpeg's supported range"
            );
            Ok(nanos as i64)
        };
        writeln!(
            metadata,
            "[CHAPTER]\nTIMEBASE=1/1000000000\nSTART={}\nEND={}",
            timestamp(start)?,
            timestamp(end)?
        )?;
        for (key, value) in &chapter.tags {
            // These names otherwise override chapter timing when FFmpeg reads
            // the metadata file. Escaping the first character keeps them tags.
            if matches!(key.as_str(), "START" | "END" | "TIMEBASE") || key.starts_with('[') {
                metadata.push('\\');
            }
            escape_metadata(&mut metadata, key);
            metadata.push('=');
            escape_metadata(&mut metadata, value);
            // FFmpeg treats a newline after even an escaped backslash as a
            // continuation. Terminate the C string before that newline so a
            // trailing backslash cannot swallow the next tag or chapter.
            if value.ends_with('\\') {
                metadata.push('\0');
            }
            metadata.push('\n');
        }
    }
    Ok(metadata)
}

fn escape_metadata(output: &mut String, value: &str) {
    for character in value.chars() {
        if matches!(character, '\\' | '=' | ';' | '#' | '\n' | '\r') {
            output.push('\\');
        }
        output.push(character);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(time_base: &str, start: i64, end: i64) -> Chapter {
        Chapter {
            time_base: time_base.to_owned(),
            start,
            end,
            tags: BTreeMap::new(),
        }
    }

    #[test]
    fn retimes_both_boundaries_with_fractional_time_bases() {
        let chapters = [chapter("1001/30000", 30, 90)];
        for (speed, start, end) in [
            (0.5, 2_002_000_000_u64, 6_006_000_000_u64),
            (2.0, 500_500_000, 1_501_500_000),
            (1.25, 800_800_000, 2_402_400_000),
        ] {
            let metadata = retimed_metadata(&chapters, 0.0, speed).unwrap();
            assert!(
                metadata.contains(&format!("START={start}\nEND={end}\n")),
                "{metadata}"
            );
        }
    }

    #[test]
    fn normalizes_the_source_origin_before_retiming() {
        let chapters = [
            chapter("1/1000", 0, 1000),
            chapter("1/1000", 1000, 3000),
            chapter("1/1000", 3000, 4000),
        ];
        let metadata = retimed_metadata(&chapters, 2.0, 0.5).unwrap();
        assert_eq!(metadata.matches("[CHAPTER]").count(), 2);
        assert!(metadata.contains("START=0\nEND=2000000000\n"));
        assert!(metadata.contains("START=2000000000\nEND=4000000000\n"));
        let negative_origin = retimed_metadata(&[chapter("1/1000", 0, 1000)], -0.5, 2.0).unwrap();
        assert!(negative_origin.contains("START=250000000\nEND=750000000\n"));
    }

    #[test]
    fn escapes_metadata_without_allowing_tags_to_override_timing() {
        let mut chapter = chapter("1/1", 1, 2);
        chapter.tags = BTreeMap::from([
            (
                "title".to_owned(),
                "A = B; #1\\2\n[CHAPTER]\nSTART=0\rEND=0".to_owned(),
            ),
            ("START".to_owned(), "preserved tag".to_owned()),
            ("[custom]".to_owned(), "value".to_owned()),
            ("trailing".to_owned(), "backslash\\".to_owned()),
        ]);
        let metadata = retimed_metadata(&[chapter], 0.0, 2.0).unwrap();
        assert!(
            metadata.contains("title=A \\= B\\; \\#1\\\\2\\\n[CHAPTER]\\\nSTART\\=0\\\rEND\\=0\n")
        );
        assert!(metadata.contains("\\START=preserved tag\n"));
        assert!(metadata.contains("\\[custom]=value\n"));
        assert!(metadata.contains("trailing=backslash\\\\\0\n"));
    }

    #[test]
    fn rejects_invalid_time_bases_ranges_and_overflow() {
        for time_base in ["0/1", "1/0", "-1/1", "bad"] {
            assert!(retimed_metadata(&[chapter(time_base, 1, 2)], 0.0, 0.5).is_err());
        }
        assert!(retimed_metadata(&[chapter("1/1", 2, 1)], 0.0, 0.5).is_err());
        assert!(retimed_metadata(&[chapter("1/1", 0, i64::MAX)], 0.0, 0.5).is_err());
        for speed in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(retimed_metadata(&[chapter("1/1", 1, 2)], 0.0, speed).is_err());
        }
    }
}
