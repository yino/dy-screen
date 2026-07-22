use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{
    DiscoveryBinding, NewStreamer, StreamerSourceKind, StreamerTagInput,
};
use rusqlite::{Connection, OptionalExtension};
use tempfile::tempdir;

fn tag(name: &str, prompt_guidance: Option<&str>) -> StreamerTagInput {
    StreamerTagInput {
        name: name.to_owned(),
        prompt_guidance: prompt_guidance.map(str::to_owned),
    }
}

fn create_v2_database(path: &std::path::Path) {
    let connection = Connection::open(path).expect("应创建测试数据库");
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            INSERT INTO schema_migrations(version, applied_at) VALUES
                (1, '2026-07-18T00:00:00Z'),
                (2, '2026-07-18T00:00:01Z');

            CREATE TABLE streamers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                source_kind TEXT NOT NULL CHECK(source_kind IN ('profile', 'room')),
                source_url TEXT NOT NULL,
                profile_sec_uid TEXT,
                web_rid TEXT,
                room_url TEXT,
                room_id TEXT,
                monitor_enabled INTEGER NOT NULL DEFAULT 1,
                archived INTEGER NOT NULL DEFAULT 0,
                live_status TEXT NOT NULL DEFAULT 'checking',
                monitor_status TEXT NOT NULL DEFAULT 'waiting',
                last_checked_at TEXT,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX idx_streamers_profile_sec_uid
                ON streamers(profile_sec_uid) WHERE profile_sec_uid IS NOT NULL;
            CREATE UNIQUE INDEX idx_streamers_web_rid
                ON streamers(web_rid) WHERE web_rid IS NOT NULL;

            CREATE TABLE recording_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                streamer_id INTEGER NOT NULL REFERENCES streamers(id),
                started_at TEXT NOT NULL,
                ended_at TEXT,
                status TEXT NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0,
                output_root TEXT NOT NULL,
                error TEXT
            );
            CREATE TABLE videos (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES recording_sessions(id),
                path TEXT NOT NULL UNIQUE,
                started_at TEXT,
                ended_at TEXT,
                duration_seconds INTEGER,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                audio_present INTEGER,
                status TEXT NOT NULL DEFAULT 'complete',
                created_at TEXT NOT NULL
            );
            CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

            INSERT INTO streamers(
                id, name, source_kind, source_url, web_rid, room_url, room_id,
                monitor_enabled, archived, live_status, monitor_status,
                created_at, updated_at
            ) VALUES
                (1, '现有主播', 'room', 'https://live.douyin.com/100', '100',
                 'https://live.douyin.com/100', 'room-100', 1, 0, 'offline',
                 'waiting', '2026-07-18T00:00:00Z', '2026-07-18T00:00:00Z'),
                (2, '已归档主播', 'room', 'https://live.douyin.com/200', '200',
                 'https://live.douyin.com/200', 'room-200', 0, 1, 'offline',
                 'paused', '2026-07-18T00:00:00Z', '2026-07-18T00:00:00Z');
            INSERT INTO recording_sessions(
                id, streamer_id, started_at, ended_at, status, output_root
            ) VALUES(10, 1, '2026-07-18T00:00:00Z', '2026-07-18T01:00:00Z',
                     'completed', '/tmp/tag-migration');
            INSERT INTO videos(id, session_id, path, size_bytes, status, created_at)
            VALUES(20, 10, '/tmp/tag-migration/segment.mkv', 100, 'complete',
                   '2026-07-18T00:00:00Z');
            "#,
        )
        .expect("应创建 v2 数据库");
}

#[test]
fn v2_migration_preserves_existing_data_and_adds_empty_tag_collections() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("v2.sqlite3");
    create_v2_database(&path);

    let database = Database::open(&path).expect("应打开数据库");
    database.migrate().expect("应升级数据库");
    database.migrate().expect("重复升级应保持幂等");

    let streamers = database.list_streamers(true).expect("应查询主播");
    assert_eq!(streamers.len(), 2);
    assert!(streamers.iter().all(|streamer| streamer.tags.is_empty()));
    assert!(streamers.iter().any(|streamer| streamer.archived));
    assert!(streamers.iter().any(|streamer| streamer.monitor_enabled));

    let connection = Connection::open(&path).expect("应复查数据库");
    assert_eq!(
        connection
            .query_row(
                "SELECT rs.streamer_id FROM recording_sessions rs JOIN videos v ON v.session_id = rs.id WHERE rs.id = 10 AND v.id = 20",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("历史关系应保留"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 3",
                [],
                |_| Ok(())
            )
            .optional()
            .expect("应查询 migration"),
        Some(())
    );
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(true))
            .optional()
            .expect("应检查外键"),
        None
    );
}

#[test]
fn replacing_tags_is_ordered_clearable_and_atomic() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("replace.sqlite3");
    let database = Database::open(&path).expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let streamer = database
        .add_streamer(&NewStreamer::room("标签主播", "300", "room-300", true))
        .expect("应创建主播");

    let saved = database
        .replace_streamer_tags(
            streamer.id,
            &[tag("  带货  ", Some("  商品卖点  ")), tag("搞笑", None)],
        )
        .expect("应保存标签");
    assert_eq!(
        saved
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["带货", "搞笑"]
    );
    assert_eq!(saved[0].sort_order, 0);
    assert_eq!(saved[1].sort_order, 1);

    let reordered = database
        .replace_streamer_tags(streamer.id, &[tag("搞笑", None), tag("带货", None)])
        .expect("应重排标签");
    assert_eq!(
        reordered
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["搞笑", "带货"]
    );

    let connection = Connection::open(&path).expect("应打开触发器连接");
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER fail_tag_insert BEFORE INSERT ON streamer_tags
            WHEN NEW.name = '模拟失败'
            BEGIN
                SELECT RAISE(ABORT, '模拟中途写入失败');
            END;
            "#,
        )
        .expect("应创建失败触发器");
    let error =
        database.replace_streamer_tags(streamer.id, &[tag("新标签", None), tag("模拟失败", None)]);
    assert!(error.is_err());
    assert_eq!(
        database
            .get_streamer(streamer.id)
            .expect("应读取回滚后的主播")
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["搞笑", "带货"]
    );

    connection
        .execute_batch(
            r#"
            DROP TRIGGER fail_tag_insert;
            CREATE TRIGGER fail_tag_unique AFTER INSERT ON streamer_tags
            WHEN NEW.name = '唯一冲突'
            BEGIN
                INSERT INTO streamer_tags(
                    streamer_id, name, normalized_name, prompt_guidance,
                    sort_order, created_at, updated_at
                ) VALUES(
                    NEW.streamer_id, NEW.name, NEW.normalized_name,
                    NEW.prompt_guidance, NEW.sort_order, NEW.created_at, NEW.updated_at
                );
            END;
            "#,
        )
        .expect("应创建唯一约束失败触发器");
    let error = database.replace_streamer_tags(
        streamer.id,
        &[tag("另一个新标签", None), tag("唯一冲突", None)],
    );
    assert!(error.is_err());
    assert_eq!(
        database
            .get_streamer(streamer.id)
            .expect("唯一约束失败后应保留旧标签")
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["搞笑", "带货"]
    );

    database
        .replace_streamer_tags(streamer.id, &[])
        .expect("应清空标签");
    assert!(
        database
            .get_streamer(streamer.id)
            .expect("应读取主播")
            .tags
            .is_empty()
    );
}

#[test]
fn streamer_queries_and_dashboard_batch_load_ordered_tags() {
    let database = Database::open_in_memory().expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let first = database
        .add_streamer(&NewStreamer::room("主播 A", "401", "room-401", true))
        .expect("应创建主播 A");
    let second = database
        .add_streamer(&NewStreamer::room("主播 B", "402", "room-402", true))
        .expect("应创建主播 B");
    database
        .replace_streamer_tags(first.id, &[tag("带货", None), tag("搞笑", None)])
        .expect("应保存主播 A 标签");
    database
        .replace_streamer_tags(second.id, &[tag("知识", Some("关注解释"))])
        .expect("应保存主播 B 标签");

    assert_eq!(
        database
            .get_streamer(first.id)
            .expect("应查询单主播")
            .tags
            .len(),
        2
    );
    assert_eq!(
        database
            .find_streamer_by_web_rid("402")
            .expect("应查询入口")
            .expect("应找到主播")
            .tags[0]
            .name,
        "知识"
    );
    let listed = database.list_streamers(false).expect("应查询列表");
    assert_eq!(
        listed
            .iter()
            .map(|streamer| streamer.tags.len())
            .sum::<usize>(),
        3
    );
    let dashboard = database.dashboard().expect("应查询 dashboard");
    assert_eq!(
        dashboard
            .streamers
            .iter()
            .map(|streamer| streamer.tags.len())
            .sum::<usize>(),
        3
    );
}

#[test]
fn tag_name_suggestions_are_case_insensitive_deduplicated_and_limited() {
    let database = Database::open_in_memory().expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    for index in 0..4 {
        let streamer = database
            .add_streamer(&NewStreamer::room(
                format!("主播 {index}"),
                format!("50{index}"),
                format!("room-50{index}"),
                true,
            ))
            .expect("应创建主播");
        let name = match index {
            0 => "Funny",
            1 => "funny",
            2 => "带货",
            _ => "知识",
        };
        database
            .replace_streamer_tags(streamer.id, &[tag(name, Some("不能通过建议泄露"))])
            .expect("应保存标签");
    }

    let suggestions = database
        .list_streamer_tag_name_suggestions(10)
        .expect("应查询建议");
    assert_eq!(suggestions.len(), 3);
    assert_eq!(
        suggestions
            .iter()
            .filter(|name| name.eq_ignore_ascii_case("funny"))
            .count(),
        1
    );
    assert_eq!(
        database
            .list_streamer_tag_name_suggestions(2)
            .expect("应限制建议数量")
            .len(),
        2
    );
    assert!(
        database
            .list_streamer_tag_name_suggestions(0)
            .expect("零限制应返回空列表")
            .is_empty()
    );
}

#[test]
fn prompt_context_uses_tag_order_and_empty_streamer_returns_empty_tags() {
    let database = Database::open_in_memory().expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let tagged = database
        .add_streamer(&NewStreamer::room("内容主播", "600", "room-600", true))
        .expect("应创建主播");
    let empty = database
        .add_streamer(&NewStreamer::room("空标签主播", "601", "room-601", true))
        .expect("应创建主播");
    database
        .replace_streamer_tags(
            tagged.id,
            &[tag("带货", Some("关注商品表达")), tag("搞笑", None)],
        )
        .expect("应保存标签");

    let context = database
        .streamer_prompt_context(tagged.id)
        .expect("应读取上下文");
    assert_eq!(context.streamer_id, tagged.id);
    assert_eq!(context.streamer_name, "内容主播");
    assert_eq!(context.tags[0].name, "带货");
    assert_eq!(context.tags[0].priority, 0);
    assert_eq!(context.tags[1].name, "搞笑");
    assert_eq!(context.tags[1].priority, 1);
    assert!(
        database
            .streamer_prompt_context(empty.id)
            .expect("应读取空上下文")
            .tags
            .is_empty()
    );
}

#[test]
fn deleting_streamer_cascades_only_its_tags() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("cascade.sqlite3");
    let database = Database::open(&path).expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let first = database
        .add_streamer(&NewStreamer::room("主播 A", "701", "room-701", true))
        .expect("应创建主播 A");
    let second = database
        .add_streamer(&NewStreamer::room("主播 B", "702", "room-702", true))
        .expect("应创建主播 B");
    database
        .replace_streamer_tags(first.id, &[tag("带货", None)])
        .expect("应保存主播 A 标签");
    database
        .replace_streamer_tags(second.id, &[tag("搞笑", None)])
        .expect("应保存主播 B 标签");

    let connection = Connection::open(&path).expect("应打开复查连接");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("应启用外键");
    connection
        .execute("DELETE FROM streamers WHERE id = ?1", [first.id])
        .expect("应删除主播");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM streamer_tags WHERE streamer_id = ?1",
                [first.id],
                |row| row.get::<_, i64>(0)
            )
            .expect("应查询已删除主播标签"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM streamer_tags WHERE streamer_id = ?1",
                [second.id],
                |row| row.get::<_, i64>(0)
            )
            .expect("应查询其他主播标签"),
        1
    );
}

#[test]
fn delayed_identity_merge_unions_tags_with_target_priority_and_guidance_rules() {
    let database = Database::open_in_memory().expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let mut target_input = NewStreamer::room("目标主播", "801", "room-801", true);
    target_input.tags = vec![
        tag("带货", None),
        tag("搞笑", Some("保留目标指导")),
        tag("目标标签", None),
    ];
    let target = database
        .add_streamer(&target_input)
        .expect("应创建目标主播");
    let temporary = database
        .add_streamer(&NewStreamer {
            name: "临时主页".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-801".to_owned(),
            profile_sec_uid: Some("profile-801".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: true,
            tags: vec![
                tag(" 带货 ", Some("补充来源指导")),
                tag("搞笑", Some("不能覆盖目标指导")),
                tag("知识", Some("追加来源标签")),
            ],
        })
        .expect("应创建临时主播");

    let outcome = database
        .bind_discovered_room(
            temporary.id,
            "801",
            "https://live.douyin.com/801",
            Some("room-801-new"),
        )
        .expect("应合并身份");
    assert_eq!(
        outcome,
        DiscoveryBinding::Merged {
            target_streamer_id: target.id,
            removed_streamer_id: temporary.id,
        }
    );
    let merged = database.get_streamer(target.id).expect("应读取目标主播");
    assert_eq!(
        merged
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["带货", "搞笑", "目标标签", "知识"]
    );
    assert_eq!(
        merged.tags[0].prompt_guidance.as_deref(),
        Some("补充来源指导")
    );
    assert_eq!(
        merged.tags[1].prompt_guidance.as_deref(),
        Some("保留目标指导")
    );
    assert_eq!(
        merged.tags[3].prompt_guidance.as_deref(),
        Some("追加来源标签")
    );
}

#[test]
fn delayed_identity_merge_keeps_target_tags_then_appends_source_to_limit() {
    let database = Database::open_in_memory().expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let mut target_input = NewStreamer::room("目标主播", "802", "room-802", true);
    target_input.tags = (0..8)
        .map(|index| tag(&format!("目标{index}"), None))
        .collect();
    let target = database
        .add_streamer(&target_input)
        .expect("应创建目标主播");
    let temporary = database
        .add_streamer(&NewStreamer {
            name: "临时主页".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-802".to_owned(),
            profile_sec_uid: Some("profile-802".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: true,
            tags: (0..5)
                .map(|index| tag(&format!("来源{index}"), None))
                .collect(),
        })
        .expect("应创建临时主播");

    database
        .bind_discovered_room(
            temporary.id,
            "802",
            "https://live.douyin.com/802",
            Some("room-802-new"),
        )
        .expect("超过标签上限仍应成功合并");
    let names = database
        .get_streamer(target.id)
        .expect("应读取目标主播")
        .tags
        .into_iter()
        .map(|tag| tag.name)
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 10);
    assert_eq!(
        names[..8],
        [
            "目标0", "目标1", "目标2", "目标3", "目标4", "目标5", "目标6", "目标7"
        ]
    );
    assert_eq!(names[8..], ["来源0", "来源1"]);
}

#[test]
fn delayed_identity_merge_rolls_back_both_streamers_when_tag_write_fails() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("merge-rollback.sqlite3");
    let database = Database::open(&path).expect("应打开数据库");
    database.migrate().expect("应迁移数据库");
    let mut target_input = NewStreamer::room("目标主播", "803", "room-803", true);
    target_input.tags = vec![tag("目标标签", None)];
    let target = database
        .add_streamer(&target_input)
        .expect("应创建目标主播");
    let temporary = database
        .add_streamer(&NewStreamer {
            name: "临时主页".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-803".to_owned(),
            profile_sec_uid: Some("profile-803".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: true,
            tags: vec![tag("触发失败", None)],
        })
        .expect("应创建临时主播");
    let connection = Connection::open(&path).expect("应打开触发器连接");
    connection
        .execute_batch(&format!(
            r#"
            CREATE TRIGGER fail_merged_tag BEFORE INSERT ON streamer_tags
            WHEN NEW.streamer_id = {} AND NEW.name = '触发失败'
            BEGIN
                SELECT RAISE(ABORT, '模拟合并标签失败');
            END;
            "#,
            target.id
        ))
        .expect("应创建失败触发器");

    let outcome = database.bind_discovered_room(
        temporary.id,
        "803",
        "https://live.douyin.com/803",
        Some("room-803-new"),
    );
    assert!(outcome.is_err());
    assert_eq!(
        database
            .get_streamer(target.id)
            .expect("目标主播应保留")
            .tags[0]
            .name,
        "目标标签"
    );
    assert_eq!(
        database
            .get_streamer(temporary.id)
            .expect("临时主播应保留")
            .tags[0]
            .name,
        "触发失败"
    );
}

#[test]
fn tags_survive_restart_reorder_archive_restore_and_prompt_context_read() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("tag-lifecycle.sqlite3");
    let streamer_id = {
        let database = Database::open(&path).expect("应打开数据库");
        database.migrate().expect("应迁移数据库");
        let mut input = NewStreamer::room("生命周期主播", "901", "room-901", true);
        input.tags = vec![tag("带货", Some("重点提取商品表达")), tag("搞笑", None)];
        database.add_streamer(&input).expect("应创建带标签主播").id
    };

    let database = Database::open(&path).expect("重启后应打开数据库");
    database.migrate().expect("重启后 migration 应幂等");
    let restarted = database.get_streamer(streamer_id).expect("应恢复主播");
    assert_eq!(
        restarted
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["带货", "搞笑"]
    );

    let updated = database
        .update_streamer(
            streamer_id,
            &NewStreamer {
                name: restarted.name.clone(),
                source_kind: restarted.source_kind,
                source_url: restarted.source_url.clone(),
                profile_sec_uid: restarted.profile_sec_uid.clone(),
                web_rid: restarted.web_rid.clone(),
                room_url: restarted.room_url.clone(),
                room_id: restarted.room_id.clone(),
                monitor_enabled: restarted.monitor_enabled,
                tags: vec![tag("搞笑", None), tag("带货", Some("商品表达"))],
            },
        )
        .expect("应编辑标签顺序");
    assert_eq!(
        updated
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["搞笑", "带货"]
    );

    database.archive_streamer(streamer_id).expect("应归档主播");
    let restored = database
        .restore_streamer(
            streamer_id,
            &NewStreamer::room("恢复后的主播", "901", "room-901-new", true),
        )
        .expect("应恢复主播");
    assert_eq!(
        restored
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["搞笑", "带货"]
    );

    let context = database
        .streamer_prompt_context(streamer_id)
        .expect("应读取未来上下文");
    assert_eq!(context.streamer_name, "恢复后的主播");
    assert_eq!(context.tags[0].name, "搞笑");
    assert_eq!(context.tags[0].priority, 0);
    assert_eq!(context.tags[1].name, "带货");
    assert_eq!(context.tags[1].prompt_guidance.as_deref(), Some("商品表达"));
}
