//! G-H2b: map absolute episode numbers (titles without a season marker, which
//! the parser places in an inferred S01) onto the catalog's seasons.

use domain::Release;

/// The release crosses seasons or lies beyond the catalog; drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unmappable;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AbsoluteEpisodes {
    /// (season, episode count) in season order, starting at season 1.
    counts: Vec<(u32, u32)>,
}

impl AbsoluteEpisodes {
    pub fn new(mut counts: Vec<(u32, u32)>) -> Self {
        counts.retain(|(season, count)| *season >= 1 && *count > 0);
        counts.sort_unstable();
        Self { counts }
    }

    /// Episodes in season 1 (absolute numbers up to this are left alone).
    pub fn first_season_len(&self) -> Option<u32> {
        self.counts.iter().find(|(s, _)| *s == 1).map(|(_, c)| *c)
    }

    pub fn total(&self) -> u32 {
        self.counts.iter().map(|(_, c)| c).sum()
    }

    /// Absolute episode → (season, episode).
    pub fn locate(&self, absolute: u32) -> Option<(u32, u32)> {
        let mut before = 0;
        for (season, count) in &self.counts {
            if absolute > before && absolute <= before + count {
                return Some((*season, absolute - before));
            }
            before += count;
        }
        None
    }

    /// Remap an inferred-season release whose episodes lie beyond season 1.
    /// `Ok(None)` = untouched; `Err(Unmappable)` = unmappable (crosses seasons or
    /// beyond the catalog), so the caller should drop it.
    pub fn remap(&self, release: &Release) -> Result<Option<Release>, Unmappable> {
        if !release.season_inferred || release.season != Some(1) {
            return Ok(None);
        }
        let (Some((from, to)), Some(s01)) = (release.episode_span(), self.first_season_len())
        else {
            return Ok(None);
        };
        if to <= s01 {
            return Ok(None);
        }
        let (Some((sa, ea)), Some((sb, eb))) = (self.locate(from), self.locate(to)) else {
            return Err(Unmappable);
        };
        if sa != sb {
            return Err(Unmappable);
        }
        let mut mapped = release.clone();
        mapped.season = Some(sa);
        mapped.episode = Some(ea);
        mapped.episode_to = release.episode_to.map(|_| eb);
        mapped.season_inferred = false;
        Ok(Some(mapped))
    }

    /// Like [`Self::remap`] but returns the release unchanged when untouched
    /// or unmappable (callers that cannot drop).
    pub fn apply(&self, release: &Release) -> Release {
        match self.remap(release) {
            Ok(Some(mapped)) => mapped,
            _ => release.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_across_seasons() {
        let map = AbsoluteEpisodes::new(vec![(1, 12), (2, 10)]);
        assert_eq!(map.locate(13), Some((2, 1)));
        assert_eq!(map.locate(22), Some((2, 10)));
        assert_eq!(map.locate(23), None);
        let r = release::parse("Show Episode 13 1080p");
        let m = map.remap(&r).unwrap().unwrap();
        assert_eq!(
            (m.season, m.episode, m.season_inferred),
            (Some(2), Some(1), false)
        );
        assert_eq!(map.remap(&release::parse("Show Episode 5 1080p")), Ok(None));
        assert_eq!(map.remap(&release::parse("Show.S01E13.1080p")), Ok(None));
        assert_eq!(map.remap(&release::parse("Show Episode 30 1080p")), Err(Unmappable));
    }
}
