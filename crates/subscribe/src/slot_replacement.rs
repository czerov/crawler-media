use crate::facts::SubscribeFacts;
use domain::{Filter, Subscribe};
use filter::ScoredTorrent;

/// 判定文件转存时是否批准执行覆盖替换：
/// 1. 若覆盖单槽位，按常规升级逻辑判定；
/// 2. 若为跨多集/多槽位的合并文件，只要其覆盖的已有槽位中存在比当前候选更高分的版本，
///    绝对不允许批准降级覆盖，坚决保护本地已有的高分单集不被误删。
pub fn is_replacement_approved(
    subscribe: &Subscribe,
    wash_filter: Option<&Filter>,
    facts: &SubscribeFacts,
    candidate: &ScoredTorrent,
    slots: &[(Option<u32>, Option<u32>)],
) -> bool {
    if slots.is_empty() {
        return false;
    }
    if slots.len() > 1 && subscribe.wash_cut {
        let owned = slots
            .iter()
            .filter(|(s, e)| facts.get(*s, *e).is_some())
            .cloned()
            .collect::<Vec<_>>();
        if owned.iter().any(|slot| {
            !crate::choose::should_replace_slots(
                subscribe,
                wash_filter,
                facts,
                candidate,
                std::slice::from_ref(slot),
            )
        }) {
            return false;
        }
    }
    crate::choose::should_replace_slots(subscribe, wash_filter, facts, candidate, slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{
        AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, MediaId, SubscribeId, UserId,
    };
    use filter::ScoredTorrent;

    fn subscribe() -> Subscribe {
        Subscribe {
            id: SubscribeId::new(),
            user_id: UserId::new(),
            media_id: MediaId::new(),
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(2),
            },
            fetch_mode: FetchMode::Search,
            filter_id: FilterId::new(),
            wash_cut: true,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        }
    }

    #[test]
    fn multi_slot_pack_cannot_replace_a_better_owned_episode() {
        let tmp = tempfile::tempdir().unwrap();
        let owned = tmp.path().join("Test Show - S01E01.mkv");
        std::fs::write(
            &owned,
            r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
        )
        .unwrap();
        let mut facts = SubscribeFacts::default();
        facts.replace(
            Some(1),
            Some(1),
            crate::QualityFact {
                score: 100,
                path: Some(owned.display().to_string()),
            },
        );
        facts.set_quality(
            owned.display().to_string(),
            release::parse("Test.Show.S01E01.2160p.HEVC.HDR10"),
        );
        let candidate = ScoredTorrent {
            torrent: domain::Torrent {
                site_id: domain::SiteId::new(),
                title: "Test.Show.S01E01-E02.1080p".into(),
                enclosure: "https://pt.example/pack".into(),
                size_bytes: Some(1),
                seeders: Some(1),
                free: true,
                hr: false,
                imdb_id: None,
                id: None,
                leechers: None,
                snatched: None,
                upload_time: None,
                detail_url: None,
                category: None,
                poster_url: None,
            },
            release: domain::Release {
                title: "Test Show".into(),
                year: None,
                season: Some(1),
                episode: Some(1),
                episode_to: Some(2),
                resolution: Some("1080p".into()),
                source: None,
                codec: None,
                hdr: None,
                subtitle_language: None,
                audio_language: None,
                group: None,
                confidence: domain::Confidence::High,
                season_inferred: false,
            },
            score: 100,
        };
        let wash = Filter {
            id: FilterId::new(),
            name: "ladder".into(),
            atoms: vec![FilterAtom {
                priority: 1,
                rule: AtomRule::UpgradeLadder("resolution".into()),
                exclude: false,
            }],
            keep_old_versions: false,
        };
        assert!(!is_replacement_approved(
            &subscribe(),
            Some(&wash),
            &facts,
            &candidate,
            &[(Some(1), Some(1)), (Some(1), Some(2))]
        ));
    }
}
