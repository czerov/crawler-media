pub(super) const APP_TABLES: &str = r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            login TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS media (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            year INTEGER,
            original_title TEXT,
            tmdb_id TEXT,
            douban_id TEXT,
            tvdb_id TEXT,
            bangumi_id TEXT,
            anilist_id TEXT
        );
        CREATE TABLE IF NOT EXISTS sites (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            profile_id TEXT NOT NULL,
            cookie TEXT,
            api_key TEXT,
            rss_url TEXT,
            proxy TEXT,
            rate_limit_per_minute INTEGER,
            cdp_url TEXT,
            downloader_id TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            maintenance_via TEXT NOT NULL DEFAULT 'http' CHECK (maintenance_via IN ('http','browser'))
        );
        CREATE TABLE IF NOT EXISTS filters (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            atoms_json TEXT NOT NULL,
            keep_old_versions INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS user_tokens (
            token TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS playback_device_credentials (
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            token TEXT NOT NULL,
            PRIMARY KEY (user_id, device_id, token)
        );
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS search_snapshots (
            id TEXT PRIMARY KEY,
            vertical TEXT NOT NULL,
            query TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS downloaders (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            url TEXT NOT NULL,
            username TEXT,
            password TEXT,
            category TEXT,
            is_default INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS library_roots (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS libraries (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            access_mode TEXT NOT NULL DEFAULT 'everyone',
            admin_visible INTEGER NOT NULL DEFAULT 1,
            member_ids_json TEXT NOT NULL DEFAULT '[]',
            detect_intros INTEGER NOT NULL DEFAULT 0,
            enable_fingerprint INTEGER NOT NULL DEFAULT 0,
            cover_path TEXT,
            match_rules_json TEXT NOT NULL DEFAULT '[]',
            default_filter_id TEXT,
            realtime_watch INTEGER NOT NULL DEFAULT 1,
            generate_thumbnails INTEGER NOT NULL DEFAULT 1,
            extract_chapter_images INTEGER NOT NULL DEFAULT 1,
            exclude_from_home INTEGER NOT NULL DEFAULT 0,
            auto_series_collections INTEGER NOT NULL DEFAULT 1,
            cover_revision INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS naming_templates (
            kind TEXT PRIMARY KEY,
            pattern TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS sites_enabled_idx ON sites(enabled);
        CREATE INDEX IF NOT EXISTS user_tokens_user_idx ON user_tokens(user_id);
        -- Media 身份查询索引。实测（bundled SQLite 3.50.2，5 万行）：
        --   alias 查找 4.06ms→0.011ms；title/kind/year 1.52ms→0.014ms；
        --   resolve_scanned_media 的 OR 形状只有在这 6 个分支**全部**可索引时才
        --   从 SCAN 变 MULTI-INDEX OR（6.72ms→0.035ms）。
        -- 证据：docs/audits/2026-10-12-remediation-review-repro/media-lookup-plans.md
        -- 故意**不加 UNIQUE**：Movie 与 TV 的 TMDB 数字 id 可以相同，身份是内部 id。
        CREATE INDEX IF NOT EXISTS media_tmdb_idx ON media(tmdb_id);
        CREATE INDEX IF NOT EXISTS media_douban_idx ON media(douban_id);
        CREATE INDEX IF NOT EXISTS media_tvdb_idx ON media(tvdb_id);
        CREATE INDEX IF NOT EXISTS media_bangumi_idx ON media(bangumi_id);
        CREATE INDEX IF NOT EXISTS media_anilist_idx ON media(anilist_id);
        CREATE INDEX IF NOT EXISTS media_title_kind_year_idx ON media(title, kind, year);
        "#;

pub(super) const LIBRARY_TABLES: &str = r#"
        CREATE TABLE IF NOT EXISTS ledger (
            id TEXT PRIMARY KEY,
            media_id TEXT NOT NULL,
            root_id TEXT,
            path TEXT NOT NULL UNIQUE,
            season INTEGER,
            episode INTEGER,
            resolution TEXT,
            codec TEXT,
            hdr TEXT,
            quality_source TEXT NOT NULL,
            confidence TEXT NOT NULL,
            filter_score INTEGER,
            transfer_mode TEXT
        );
        CREATE INDEX IF NOT EXISTS ledger_path_idx ON ledger(path);
        CREATE INDEX IF NOT EXISTS ledger_media_id_idx ON ledger(media_id);
        CREATE TABLE IF NOT EXISTS scrape_sidecars (
            ledger_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            PRIMARY KEY (ledger_id, kind)
        );
        CREATE TABLE IF NOT EXISTS unidentified (
            path TEXT PRIMARY KEY,
            confidence TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS file_meta (
            ledger_id TEXT PRIMARY KEY,
            audio_json TEXT NOT NULL,
            subtitle_json TEXT NOT NULL,
            tracks_version INTEGER NOT NULL DEFAULT 0,
            source_version TEXT,
            format_duration_ms INTEGER
        );
        CREATE TABLE IF NOT EXISTS fingerprint_cache (
            ledger_id TEXT PRIMARY KEY,
            cache_key TEXT NOT NULL,
            algorithm_version INTEGER NOT NULL,
            sample_duration_secs INTEGER NOT NULL,
            media_duration_ms INTEGER,
            intro_json TEXT NOT NULL,
            outro_json TEXT,
            captured_at_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS media_markers (
            media_id TEXT NOT NULL,
            season INTEGER NOT NULL,
            episode INTEGER NOT NULL,
            intro_start_ms INTEGER,
            intro_end_ms INTEGER,
            outro_start_ms INTEGER,
            outro_end_ms INTEGER,
            source TEXT NOT NULL,
            locked INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (media_id, season, episode)
        );
        CREATE TABLE IF NOT EXISTS probe_jobs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            scope_key TEXT NOT NULL,
            status TEXT NOT NULL,
            total INTEGER NOT NULL,
            completed INTEGER NOT NULL DEFAULT 0,
            succeeded INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0,
            partial INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            created_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            finished_at_ms INTEGER
        );
        CREATE TABLE IF NOT EXISTS probe_job_units (
            job_id TEXT NOT NULL,
            ledger_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            force_fingerprint INTEGER NOT NULL DEFAULT 0,
            reuse_fingerprint_cache INTEGER NOT NULL DEFAULT 0,
            overwrite_markers INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL,
            error TEXT,
            reuse_media_info_cache INTEGER NOT NULL DEFAULT 0,
            stage TEXT NOT NULL DEFAULT 'metadata',
            PRIMARY KEY (job_id, ledger_id),
            FOREIGN KEY (job_id) REFERENCES probe_jobs(id) ON DELETE CASCADE
        );
        CREATE UNIQUE INDEX IF NOT EXISTS probe_jobs_active_scope_idx
            ON probe_jobs(scope_key) WHERE status IN ('queued', 'running');
        CREATE UNIQUE INDEX IF NOT EXISTS probe_job_units_active_ledger_idx
            ON probe_job_units(ledger_id) WHERE status IN ('queued', 'running');
        CREATE INDEX IF NOT EXISTS probe_jobs_media_created_idx
            ON probe_jobs(media_id, created_at_ms DESC);
        CREATE INDEX IF NOT EXISTS probe_job_units_job_status_idx
            ON probe_job_units(job_id, status);
        CREATE TABLE IF NOT EXISTS ledger_probe_obligations (
            ledger_id TEXT PRIMARY KEY,
            created_at_ms INTEGER NOT NULL,
            reason TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ledger_probe_obligations_created_ledger_idx
            ON ledger_probe_obligations(created_at_ms, ledger_id);
        "#;

pub(super) const SUBSCRIBE_TABLES: &str = r#"
        CREATE TABLE IF NOT EXISTS subscribes (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            coverage_kind TEXT NOT NULL,
            season INTEGER,
            episode_from INTEGER,
            episode_to INTEGER,
            fetch_mode TEXT NOT NULL,
            filter_id TEXT NOT NULL,
            wash_cut INTEGER NOT NULL DEFAULT 0,
            wash_cut_filter_id TEXT,
            full_season_pack INTEGER NOT NULL DEFAULT 0,
            downloader_id TEXT,
            tracking_state TEXT NOT NULL DEFAULT 'active',
            follow_future INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS subscribe_facts (
            subscribe_id TEXT NOT NULL,
            season INTEGER NOT NULL DEFAULT -1,
            episode INTEGER NOT NULL DEFAULT -1,
            score INTEGER NOT NULL,
            path TEXT,
            PRIMARY KEY (subscribe_id, season, episode)
        );
        CREATE TABLE IF NOT EXISTS subscribe_wanted (
            subscribe_id TEXT NOT NULL,
            season INTEGER NOT NULL DEFAULT -1,
            episode INTEGER NOT NULL DEFAULT -1,
            search_attempts INTEGER NOT NULL DEFAULT 0,
            last_search_at INTEGER,
            grabbed_at INTEGER,
            imported_at INTEGER,
            grab_title TEXT,
            last_reject_reason TEXT,
            PRIMARY KEY (subscribe_id, season, episode)
        );
        CREATE TABLE IF NOT EXISTS pending_downloads (
            subscribe_id TEXT NOT NULL,
            enclosure TEXT NOT NULL,
            title TEXT NOT NULL,
            score INTEGER NOT NULL,
            torrent_json TEXT NOT NULL,
            PRIMARY KEY (subscribe_id, enclosure)
        );
        CREATE TABLE IF NOT EXISTS playback_sessions (
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            client_version TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            position_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            paused INTEGER NOT NULL DEFAULT 0,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            rate_bps INTEGER NOT NULL DEFAULT 0,
            bytes_sent INTEGER NOT NULL DEFAULT 0,
            connections INTEGER NOT NULL DEFAULT 0,
            admin_ended INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL,
            last_report_at INTEGER NOT NULL,
            ledger_id TEXT,
            play_session_id TEXT,
            last_report_at_ms INTEGER NOT NULL DEFAULT 0,
            last_event_sequence INTEGER,
            PRIMARY KEY (user_id, device_id)
        );
        CREATE TABLE IF NOT EXISTS playback_session_ids (
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            play_session_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            issued_at INTEGER NOT NULL,
            generation INTEGER NOT NULL,
            state TEXT NOT NULL,
            PRIMARY KEY (user_id, device_id, play_session_id)
        );
        CREATE INDEX IF NOT EXISTS playback_session_ids_expiry_idx ON playback_session_ids(issued_at);
        CREATE TABLE IF NOT EXISTS playback_logs (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            started_at INTEGER NOT NULL,
            ended_at INTEGER NOT NULL,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            end_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            completed INTEGER NOT NULL DEFAULT 0,
            admin_ended INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS playback_metrics (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            tier INTEGER NOT NULL DEFAULT 0,
            engine TEXT,
            ttff_ms INTEGER,
            rebuffer_ms INTEGER NOT NULL DEFAULT 0,
            rebuffer_count INTEGER NOT NULL DEFAULT 0,
            seek_count INTEGER NOT NULL DEFAULT 0,
            dropped_frames INTEGER,
            total_frames INTEGER,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            recorded_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS playback_units (
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER NOT NULL DEFAULT -1,
            episode INTEGER NOT NULL DEFAULT -1,
            position_ms INTEGER NOT NULL DEFAULT 0,
            played INTEGER NOT NULL DEFAULT 0,
            favorite INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            audio_track TEXT,
            subtitle_track TEXT,
            play_count INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            favorite_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (user_id, media_id, season, episode)
        );
        CREATE INDEX IF NOT EXISTS subscribes_user_idx ON subscribes(user_id);
        CREATE INDEX IF NOT EXISTS subscribes_media_idx ON subscribes(media_id);
        CREATE INDEX IF NOT EXISTS subscribe_facts_sub_idx ON subscribe_facts(subscribe_id);
        CREATE INDEX IF NOT EXISTS subscribe_wanted_sub_idx ON subscribe_wanted(subscribe_id);
        CREATE INDEX IF NOT EXISTS playback_units_user_media_idx ON playback_units(user_id, media_id);
        CREATE INDEX IF NOT EXISTS playback_sessions_user_idx ON playback_sessions(user_id);
        CREATE INDEX IF NOT EXISTS playback_logs_user_started_idx ON playback_logs(user_id, started_at);
        CREATE INDEX IF NOT EXISTS playback_logs_started_idx ON playback_logs(started_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS playback_logs_ended_idx ON playback_logs(ended_at ASC, started_at ASC, id ASC);
        "#;
