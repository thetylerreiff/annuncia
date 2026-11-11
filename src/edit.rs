//! The editor's cut list: the recording split into clips, each trimmed at both ends and
//! optionally removed. Times are seconds in the original file.

use std::ops::Range;

/// Nothing shorter than this: no slivers from a split or a trim.
const MIN_CLIP: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    /// The stretch of the recording this clip covers, between split points.
    pub span: (f64, f64),
    /// The part of `span` that's kept (the clip's trim handles).
    pub start: f64,
    pub end: f64,
    pub removed: bool,
}

impl Clip {
    fn whole(lo: f64, hi: f64) -> Self {
        Self {
            span: (lo, hi),
            start: lo,
            end: hi,
            removed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub duration: f64,
    clips: Vec<Clip>,
}

impl Edit {
    pub fn new(duration: f64) -> Self {
        Self {
            duration,
            clips: vec![Clip::whole(0., duration)],
        }
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn is_edited(&self) -> bool {
        self.kept() != [0.0..self.duration]
    }

    /// The clip whose span contains `t`.
    pub fn clip_at(&self, t: f64) -> usize {
        self.clips
            .iter()
            .position(|c| t < c.span.1)
            .unwrap_or(self.clips.len() - 1)
    }

    pub fn is_removed(&self, i: usize) -> bool {
        self.clips.get(i).is_some_and(|c| c.removed)
    }

    // `min`/`max` rather than `clamp`, which panics if a very short clip inverts the bounds.
    pub fn set_start(&mut self, i: usize, t: f64) {
        if let Some(c) = self.clips.get_mut(i) {
            c.start = t.min(c.end - MIN_CLIP).max(c.span.0);
        }
    }

    pub fn set_end(&mut self, i: usize, t: f64) {
        if let Some(c) = self.clips.get_mut(i) {
            c.end = t.max(c.start + MIN_CLIP).min(c.span.1);
        }
    }

    /// Splits the clip under `t` into two, each keeping its side of the trim. Returns false
    /// when `t` isn't inside the kept part of a clip, or is too close to its edges.
    pub fn split(&mut self, t: f64) -> bool {
        let i = self.clip_at(t);
        let clip = self.clips[i];
        if clip.removed || t <= clip.start + MIN_CLIP || t >= clip.end - MIN_CLIP {
            return false;
        }
        self.clips[i].span.1 = t;
        self.clips[i].end = t;
        self.clips.insert(
            i + 1,
            Clip {
                span: (t, clip.span.1),
                start: t,
                end: clip.end,
                removed: false,
            },
        );
        true
    }

    pub fn toggle_removed(&mut self, i: usize) {
        if let Some(c) = self.clips.get_mut(i) {
            c.removed = !c.removed;
        }
    }

    /// Removes `range` from what's kept, the way it's done by hand: a clip it covers is
    /// deleted, a clip it overlaps at one end is trimmed back to it, and a clip it falls inside
    /// is split in its middle with both sides trimmed back to its edges. What's already cut
    /// stays as it is, and the last kept clip is never deleted. Returns whether anything changed.
    pub fn cut(&mut self, range: Range<f64>) -> bool {
        let before = self.clips.clone();
        let mut i = 0;
        while i < self.clips.len() {
            let clip = self.clips[i];
            let (lo, hi) = (range.start.max(clip.start), range.end.min(clip.end));
            if !clip.removed && hi > lo {
                // Whether enough of the clip is left on either side of the cut to keep.
                match (lo - clip.start >= MIN_CLIP, clip.end - hi >= MIN_CLIP) {
                    (true, true) => {
                        if self.split((lo + hi) / 2.) {
                            self.set_end(i, lo);
                            self.set_start(i + 1, hi);
                            i += 1;
                        }
                    }
                    (true, false) => self.set_end(i, lo),
                    (false, true) => self.set_start(i, hi),
                    (false, false) => {
                        if self.clips.iter().filter(|c| !c.removed).count() > 1 {
                            self.clips[i].removed = true;
                        }
                    }
                }
            }
            i += 1;
        }
        self.clips != before
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.duration);
    }

    /// The ranges that make up the edited video, in order, merged where they touch.
    pub fn kept(&self) -> Vec<Range<f64>> {
        let mut kept: Vec<Range<f64>> = Vec::new();
        for clip in self.clips.iter().filter(|c| !c.removed) {
            match kept.last_mut() {
                Some(last) if (last.end - clip.start).abs() < 1e-9 => last.end = clip.end,
                _ => kept.push(clip.start..clip.end),
            }
        }
        kept
    }

    /// Length of the edited video.
    pub fn kept_duration(&self) -> f64 {
        self.kept().iter().map(|r| r.end - r.start).sum()
    }

    /// Position in the edited video of time `t` in the original.
    pub fn edited_time(&self, t: f64) -> f64 {
        let mut total = 0.;
        for range in self.kept() {
            if t <= range.start {
                break;
            }
            total += t.min(range.end) - range.start;
        }
        total
    }

    /// Where playback at `t` should continue: `t` itself if it's kept, the start of the next
    /// kept range if it's been cut, or `None` past the end of the edit.
    pub fn playable_from(&self, t: f64) -> Option<f64> {
        self.kept()
            .into_iter()
            .find(|r| t < r.end - 1e-3)
            .map(|r| t.max(r.start))
    }

    /// End of the last kept range, where playback stops.
    pub fn kept_end(&self) -> f64 {
        self.kept().last().map_or(0., |r| r.end)
    }
}
