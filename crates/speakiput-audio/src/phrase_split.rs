// Copied from whisrs src/transcription/phrase_split.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

#![allow(clippy::cast_possible_truncation)]

//! Silence-delimited phrase segmentation for local streaming ASR.

use crate::rms_energy;

const FRAME_MS: usize = 100;
const PAD_MS: usize = 100;
const MIN_PHRASE_SPEECH_MS: usize = 250;
const MAX_PHRASE_SECS: usize = 20;
const QUIET_SEARCH_MS: usize = 2000;

/// Incremental silence-delimited phrase splitter over i16 PCM samples.
pub struct PhraseSplitter {
    frame_len: usize,
    threshold: f64,
    split_silence_frames: usize,
    min_speech_frames: usize,
    max_phrase_frames: usize,
    pad_frames: usize,
    quiet_search_frames: usize,
    buffer: Vec<i16>,
    energies: Vec<f64>,
    in_phrase: bool,
    emit_start_frame: usize,
    phrase_start_frame: usize,
    speech_frames: usize,
    trailing_silence: usize,
}

impl PhraseSplitter {
    #[must_use]
    pub fn new(sample_rate: usize, threshold: f64, phrase_silence_ms: u64) -> Self {
        let frame_len = (sample_rate * FRAME_MS / 1000).max(1);
        Self::with_params(
            frame_len,
            threshold,
            (phrase_silence_ms as usize).div_ceil(FRAME_MS).max(1),
            MIN_PHRASE_SPEECH_MS.div_ceil(FRAME_MS).max(1),
            (MAX_PHRASE_SECS * 1000 / FRAME_MS).max(2),
            PAD_MS / FRAME_MS,
            (QUIET_SEARCH_MS / FRAME_MS).max(1),
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_params(
        frame_len: usize,
        threshold: f64,
        split_silence_frames: usize,
        min_speech_frames: usize,
        max_phrase_frames: usize,
        pad_frames: usize,
        quiet_search_frames: usize,
    ) -> Self {
        Self {
            frame_len: frame_len.max(1),
            threshold,
            split_silence_frames: split_silence_frames.max(1),
            min_speech_frames: min_speech_frames.max(1),
            max_phrase_frames: max_phrase_frames.max(2),
            pad_frames,
            quiet_search_frames: quiet_search_frames.max(1),
            buffer: Vec::new(),
            energies: Vec::new(),
            in_phrase: false,
            emit_start_frame: 0,
            phrase_start_frame: 0,
            speech_frames: 0,
            trailing_silence: 0,
        }
    }

    pub fn feed(&mut self, samples: &[i16]) -> Vec<Vec<i16>> {
        self.buffer.extend_from_slice(samples);
        let mut out = Vec::new();
        while (self.energies.len() + 1) * self.frame_len <= self.buffer.len() {
            let i = self.energies.len();
            let energy = rms_energy(&self.buffer[i * self.frame_len..(i + 1) * self.frame_len]);
            self.energies.push(energy);
            self.process_frame(i, energy, &mut out);
        }
        out
    }

    pub fn flush(&mut self) -> Option<Vec<i16>> {
        if !self.in_phrase || self.speech_frames < self.min_speech_frames {
            self.clear();
            return None;
        }
        let end_sample = if self.trailing_silence > self.pad_frames {
            let speech_end = self.energies.len() - self.trailing_silence;
            (speech_end + self.pad_frames) * self.frame_len
        } else {
            self.buffer.len()
        };
        let phrase = self.buffer[self.emit_start_frame * self.frame_len..end_sample].to_vec();
        self.clear();
        Some(phrase)
    }

    fn process_frame(&mut self, i: usize, energy: f64, out: &mut Vec<Vec<i16>>) {
        let silent = energy < self.threshold;
        if !self.in_phrase {
            if silent {
                if self.energies.len() > self.pad_frames {
                    let drop = self.energies.len() - self.pad_frames;
                    self.drop_frames(drop);
                }
            } else {
                self.in_phrase = true;
                self.phrase_start_frame = i;
                self.emit_start_frame = i.saturating_sub(self.pad_frames);
                self.speech_frames = 1;
                self.trailing_silence = 0;
            }
            return;
        }
        if silent {
            self.trailing_silence += 1;
            if self.trailing_silence >= self.split_silence_frames {
                self.end_phrase(i, out);
                return;
            }
        } else {
            self.speech_frames += 1;
            self.trailing_silence = 0;
        }
        if i + 1 - self.phrase_start_frame >= self.max_phrase_frames {
            self.force_split(i, out);
        }
    }

    fn end_phrase(&mut self, i: usize, out: &mut Vec<Vec<i16>>) {
        let speech_end = i + 1 - self.trailing_silence;
        let end_frame = (speech_end + self.pad_frames).min(self.energies.len());
        if self.speech_frames >= self.min_speech_frames {
            out.push(
                self.buffer[self.emit_start_frame * self.frame_len..end_frame * self.frame_len]
                    .to_vec(),
            );
        }
        self.in_phrase = false;
        self.speech_frames = 0;
        self.trailing_silence = 0;
        let keep_from = self
            .energies
            .len()
            .saturating_sub(self.pad_frames)
            .max(speech_end);
        self.drop_frames(keep_from);
    }

    fn force_split(&mut self, i: usize, out: &mut Vec<Vec<i16>>) {
        let search_start = (i + 1)
            .saturating_sub(self.quiet_search_frames)
            .max(self.phrase_start_frame + 1);
        let mut split_frame = search_start;
        let mut min_energy = f64::INFINITY;
        for j in search_start..=i {
            if self.energies[j] < min_energy {
                min_energy = self.energies[j];
                split_frame = j;
            }
        }
        out.push(
            self.buffer[self.emit_start_frame * self.frame_len..split_frame * self.frame_len]
                .to_vec(),
        );
        self.drop_frames(split_frame);
        self.phrase_start_frame = 0;
        self.emit_start_frame = 0;
        self.speech_frames = self
            .energies
            .iter()
            .filter(|&&energy| energy >= self.threshold)
            .count();
        self.trailing_silence = self
            .energies
            .iter()
            .rev()
            .take_while(|&&energy| energy < self.threshold)
            .count();
    }

    fn drop_frames(&mut self, frame_count: usize) {
        if frame_count == 0 {
            return;
        }
        self.buffer.drain(..frame_count * self.frame_len);
        self.energies.drain(..frame_count);
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.energies.clear();
        self.in_phrase = false;
        self.emit_start_frame = 0;
        self.phrase_start_frame = 0;
        self.speech_frames = 0;
        self.trailing_silence = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 10;
    const SPEECH: i16 = 1000;
    const QUIET_SPEECH: i16 = 500;

    fn splitter() -> PhraseSplitter {
        PhraseSplitter::with_params(FRAME, 0.01, 3, 2, 100, 1, 5)
    }

    fn frames(pattern: &[(i16, usize)]) -> Vec<i16> {
        let mut out = Vec::new();
        for &(value, count) in pattern {
            out.extend(std::iter::repeat_n(value, count * FRAME));
        }
        out
    }

    #[test]
    fn basic_split_two_phrases() {
        let mut splitter = splitter();
        let audio = frames(&[(0, 2), (SPEECH, 5), (0, 4), (SPEECH, 3), (0, 4)]);
        let mut phrases = splitter.feed(&audio);
        phrases.extend(splitter.flush());
        assert_eq!(phrases.len(), 2);
        assert_eq!(phrases[0].len(), 7 * FRAME);
        assert_eq!(phrases[1].len(), 5 * FRAME);
    }

    #[test]
    fn padding_included_around_speech() {
        let mut splitter = splitter();
        let phrases = splitter.feed(&frames(&[(0, 3), (SPEECH, 4), (0, 4)]));
        assert_eq!(phrases.len(), 1);
        assert_eq!(phrases[0].len(), 6 * FRAME);
        assert!(phrases[0][..FRAME].iter().all(|&sample| sample == 0));
        assert!(
            phrases[0][FRAME..5 * FRAME]
                .iter()
                .all(|&sample| sample == SPEECH)
        );
        assert!(phrases[0][5 * FRAME..].iter().all(|&sample| sample == 0));
    }

    #[test]
    fn no_leading_pad_when_speech_starts_at_zero() {
        let mut splitter = splitter();
        let phrases = splitter.feed(&frames(&[(SPEECH, 4), (0, 4)]));
        assert_eq!(phrases[0].len(), 5 * FRAME);
        assert_eq!(phrases[0][0], SPEECH);
    }

    #[test]
    fn short_blip_discarded_as_noise() {
        let mut splitter = splitter();
        assert!(
            splitter
                .feed(&frames(&[(0, 2), (SPEECH, 1), (0, 4)]))
                .is_empty()
        );
        assert!(splitter.flush().is_none());
    }

    #[test]
    fn incremental_feeding_matches_single_feed() {
        let audio = frames(&[(0, 2), (SPEECH, 5), (0, 4), (SPEECH, 3), (0, 4)]);
        let mut whole = splitter();
        let mut expected = whole.feed(&audio);
        expected.extend(whole.flush());
        let mut incremental = splitter();
        let mut got = Vec::new();
        for chunk in audio.chunks(7) {
            got.extend(incremental.feed(chunk));
        }
        got.extend(incremental.flush());
        assert_eq!(expected, got);
    }

    #[test]
    fn cap_force_splits_at_quietest_frame() {
        let mut splitter = PhraseSplitter::with_params(FRAME, 0.01, 3, 2, 6, 1, 3);
        let audio = frames(&[(SPEECH, 4), (QUIET_SPEECH, 1), (SPEECH, 3)]);
        let mut phrases = splitter.feed(&audio);
        phrases.extend(splitter.flush());
        assert_eq!(phrases.len(), 2);
        assert_eq!(phrases[0].len(), 4 * FRAME);
        assert!(phrases[0].iter().all(|&sample| sample == SPEECH));
        assert_eq!(phrases[1].len(), 4 * FRAME);
        assert_eq!(phrases[1][0], QUIET_SPEECH);
    }

    #[test]
    fn continuous_speech_is_emitted_without_loss_or_duplication() {
        let mut splitter = PhraseSplitter::with_params(FRAME, 0.01, 3, 2, 6, 1, 3);
        let audio = frames(&[(SPEECH, 25)]);
        let mut phrases = splitter.feed(&audio);
        phrases.extend(splitter.flush());
        assert!(phrases.len() >= 4);
        assert_eq!(phrases.into_iter().flatten().collect::<Vec<_>>(), audio);
    }

    #[test]
    fn flush_emits_trailing_phrase_once() {
        let mut splitter = splitter();
        assert!(
            splitter
                .feed(&frames(&[(0, 1), (SPEECH, 4), (0, 1)]))
                .is_empty()
        );
        assert_eq!(splitter.flush().unwrap().len(), 6 * FRAME);
        assert!(splitter.flush().is_none());
    }

    #[test]
    fn flush_trims_excess_trailing_silence() {
        let mut splitter = splitter();
        assert!(
            splitter
                .feed(&frames(&[(0, 1), (SPEECH, 4), (0, 2)]))
                .is_empty()
        );
        assert_eq!(splitter.flush().unwrap().len(), 6 * FRAME);
    }

    #[test]
    fn pure_silence_yields_nothing() {
        let mut splitter = splitter();
        assert!(splitter.feed(&frames(&[(0, 50)])).is_empty());
        assert!(splitter.flush().is_none());
    }

    #[test]
    fn long_leading_silence_does_not_grow_buffer() {
        let mut splitter = splitter();
        splitter.feed(&frames(&[(0, 1000)]));
        assert!(splitter.buffer.len() <= 2 * FRAME);
    }
}
