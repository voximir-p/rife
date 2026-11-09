// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Parse output-scoped FFmpeg options without executing shell text.
use std::str::FromStr;

#[derive(Debug, Clone)]
pub(crate) struct EncoderOptions(Vec<(String, Option<String>)>);

impl EncoderOptions {
    pub(crate) fn arguments(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .flat_map(|(key, value)| std::iter::once(key.as_str()).chain(value.as_deref()))
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_deref()))
    }
}

fn is_option(value: &str) -> bool {
    value.starts_with('-') && value.parse::<f64>().is_err()
}

impl FromStr for EncoderOptions {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let words = shlex::split(value)
            .ok_or("extra encoder options contain an unclosed quote or trailing escape")?;
        if words.is_empty() || value.contains('\0') {
            return Err("extra encoder options must contain FFmpeg output flags".into());
        }
        let mut words = words.into_iter().peekable();
        let mut arguments = Vec::new();
        while let Some(word) = words.next() {
            if !is_option(&word) || word == "-" || word == "--" {
                return Err("extra encoder options must be flags with optional values, not output filenames".into());
            }
            let (option, inline_value) = word
                .split_once('=')
                .map_or((word.as_str(), None), |(key, value)| (key, Some(value)));
            if option == "-" || option == "--" {
                return Err("extra encoder options must contain named FFmpeg flags".into());
            }
            if matches!(option, "-i" | "-progress" | "-stats_period") {
                return Err(format!(
                    "{option} is managed by rife and cannot be an extra encoder option"
                ));
            }
            let value = if let Some(value) = inline_value {
                Some(value.to_owned())
            } else if words.peek().is_some_and(|word| !is_option(word)) {
                words.next()
            } else {
                None
            };
            arguments.push((option.to_owned(), value));
        }
        Ok(Self(arguments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_grouped_quoted_and_negative_values_without_shell_expansion() {
        for (text, expected) in [
            ("-crf=23", vec!["-crf", "23"]),
            ("-crf 12 -a:b 10k", vec!["-crf", "12", "-a:b", "10k"]),
            (
                "-metadata title='hello world' -x264-params=aq-mode=3:deblock=1,1",
                vec![
                    "-metadata",
                    "title=hello world",
                    "-x264-params",
                    "aq-mode=3:deblock=1,1",
                ],
            ),
            (
                "-an -sn -strict -2 -crf -1",
                vec!["-an", "-sn", "-strict", "-2", "-crf", "-1"],
            ),
            (
                "-metadata 'title=$(echo hello); $HOME & *'",
                vec!["-metadata", "title=$(echo hello); $HOME & *"],
            ),
            (
                r#"-vf "select=not(mod(n\,2))""#,
                vec!["-vf", r"select=not(mod(n\,2))"],
            ),
        ] {
            assert_eq!(
                text.parse::<EncoderOptions>()
                    .unwrap()
                    .arguments()
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn rejects_malformed_values_and_changes_to_inputs_outputs_or_progress() {
        for text in [
            "",
            " ",
            "-metadata 'unclosed",
            "-crf 23\\",
            "-crf 23 extra.mp4",
            "file.mp4",
            "-i other.mp4",
            "-i=other.mp4",
            "-progress pipe:2",
            "-stats_period 2",
            "-metadata title=\0",
            "--",
        ] {
            assert!(text.parse::<EncoderOptions>().is_err(), "{text:?}");
        }
    }
}
