use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityFact {
    pub score: i32,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubscribeFacts {
    slots: HashMap<(Option<u32>, Option<u32>), QualityFact>,
    // Quality is supplied by the Library ledger, never inferred from a renamed path.
    qualities: HashMap<String, domain::Release>,
    /// Owned slots that already have an upgrade download in flight (H1).
    /// Transient: built from pending rows per round, never persisted.
    in_flight: std::collections::HashSet<(Option<u32>, Option<u32>)>,
}

impl SubscribeFacts {
    pub fn upsert(&mut self, season: Option<u32>, episode: Option<u32>, fact: QualityFact) {
        let key = (season, episode);
        match self.slots.get(&key) {
            Some(existing) if fact.score <= existing.score => {}
            _ => {
                self.slots.insert(key, fact);
            }
        }
    }

    /// 无条件写入该槽位（选择阶段已批准替换时使用，见 collect.rs）。
    pub fn replace(&mut self, season: Option<u32>, episode: Option<u32>, fact: QualityFact) {
        self.slots.insert((season, episode), fact);
    }

    pub fn get(&self, season: Option<u32>, episode: Option<u32>) -> Option<&QualityFact> {
        self.slots.get(&(season, episode))
    }

    pub fn set_quality(&mut self, path: String, release: domain::Release) {
        self.qualities.insert(path, release);
    }

    pub fn quality(&self, path: &str) -> Option<&domain::Release> {
        self.qualities.get(path)
    }

    /// Remove temporary in-flight coverage before reporting or persisting owned facts.
    pub fn retain_owned(&mut self) {
        self.slots.retain(|_, fact| fact.path.is_some());
        self.in_flight.clear();
    }

    /// Mark an owned slot as having an in-flight upgrade download.
    pub fn mark_in_flight(&mut self, season: Option<u32>, episode: Option<u32>) {
        self.in_flight.insert((season, episode));
    }

    pub fn is_in_flight(&self, season: Option<u32>, episode: Option<u32>) -> bool {
        self.in_flight.contains(&(season, episode))
    }

    pub fn movie(&self) -> Option<&QualityFact> {
        self.get(None, None)
    }

    pub fn entries(&self) -> impl Iterator<Item = ((Option<u32>, Option<u32>), &QualityFact)> {
        self.slots.iter().map(|(key, fact)| (*key, fact))
    }

    pub fn entries_mut(
        &mut self,
    ) -> impl Iterator<Item = ((Option<u32>, Option<u32>), &mut QualityFact)> {
        self.slots.iter_mut().map(|(key, fact)| (*key, fact))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_keeps_higher_score_and_path() {
        let mut facts = SubscribeFacts::default();
        facts.upsert(
            None,
            None,
            QualityFact {
                score: 80,
                path: Some("/old".into()),
            },
        );
        // 同分 → 不覆盖（守卫仍在，防陈旧 pending 回退）。
        facts.upsert(
            None,
            None,
            QualityFact {
                score: 80,
                path: Some("/new".into()),
            },
        );
        assert_eq!(facts.movie().unwrap().path.as_deref(), Some("/old"));
        // 更高分 → 覆盖。
        facts.upsert(
            None,
            None,
            QualityFact {
                score: 90,
                path: Some("/best".into()),
            },
        );
        assert_eq!(facts.movie().unwrap().path.as_deref(), Some("/best"));
    }

    #[test]
    fn replace_overwrites_unconditionally() {
        let mut facts = SubscribeFacts::default();
        facts.upsert(
            None,
            None,
            QualityFact {
                score: 90,
                path: Some("/old".into()),
            },
        );
        // chooser 批准的替换（如 UpgradeLadder 同分更高分辨率）无条件覆盖。
        facts.replace(
            None,
            None,
            QualityFact {
                score: 90,
                path: Some("/new".into()),
            },
        );
        assert_eq!(facts.movie().unwrap().path.as_deref(), Some("/new"));
        facts.replace(
            None,
            None,
            QualityFact {
                score: 10,
                path: Some("/low".into()),
            },
        );
        assert_eq!(facts.movie().unwrap().path.as_deref(), Some("/low"));
    }
}
