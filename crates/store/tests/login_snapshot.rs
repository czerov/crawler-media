//! `login_credential` 快照与 `finish_verified_login` CAS 的回归测试。
//!
//! 校验密码必须移到 Store 锁之外：先取不可变快照，offlock 校验后再用 CAS
//! finalizer 签发。快照之后发生的改密 / 禁用 / 删除必须让 finalizer 返回
//! `None` 且不签发任何会话 token。已哈希（`new_hash = None`）与存量明文透明
//! 升级（`new_hash = Some`）两条路径都要覆盖。

use domain::{User, UserId, UserRole};
use rusqlite::{Connection, params};
use store::Store;

const LOGIN: &str = "member";
const SECRET: &str = "original-secret";

#[derive(Clone, Copy, Debug)]
enum Mutation {
    ChangePassword,
    Disable,
    Delete,
}

#[derive(Clone, Copy, Debug)]
enum CredentialKind {
    /// 已存 Argon2id 哈希：finalizer 走重查路径（`new_hash = None`）。
    Hashed,
    /// 存量明文口令：finalizer 走透明升级路径（`new_hash = Some`）。
    Legacy,
}

fn insert_admin(store: &Store) {
    store
        .insert_user_with_password(
            &User {
                id: UserId::new(),
                login: "admin".into(),
                enabled: true,
                role: UserRole::Admin,
            },
            "admin-secret",
        )
        .unwrap();
}

fn insert_member(store: &Store, kind: CredentialKind) -> UserId {
    let id = UserId::new();
    let user = User {
        id,
        login: LOGIN.into(),
        enabled: true,
        role: UserRole::Member,
    };
    match kind {
        CredentialKind::Hashed => store.insert_user_with_password(&user, SECRET).unwrap(),
        CredentialKind::Legacy => {
            // 存量数据是明文，只能直写 app.db 模拟迁移前的行。
            let conn = Connection::open(store.data_dir().join("app.db")).unwrap();
            conn.execute(
                "INSERT INTO users (id, login, password, enabled, role)
                 VALUES (?1, ?2, ?3, 1, 'member')",
                params![id.to_string(), LOGIN, SECRET],
            )
            .unwrap();
        }
    }
    id
}

fn apply_mutation(store: &Store, user_id: UserId, mutation: Mutation) {
    match mutation {
        Mutation::ChangePassword => store.set_password(user_id, "changed-secret").unwrap(),
        Mutation::Disable => {
            let mut user = store.get_user(user_id).unwrap().unwrap();
            user.enabled = false;
            store.save_user(&user).unwrap();
        }
        Mutation::Delete => assert!(store.delete_user(user_id).unwrap()),
    }
}

/// 快照之后发生 `mutation`，finalizer 必须拒绝并且不留下任何会话 token。
fn assert_stale_snapshot_rejected(kind: CredentialKind, mutation: Mutation) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    insert_admin(&store);
    insert_member(&store, kind);

    let snapshot = store.login_credential(LOGIN).unwrap().unwrap();
    match kind {
        CredentialKind::Hashed => assert!(
            snapshot.observed_password.starts_with("$argon2"),
            "hashed user snapshot must carry the argon2 hash"
        ),
        CredentialKind::Legacy => assert_eq!(snapshot.observed_password, SECRET),
    }

    apply_mutation(&store, snapshot.user_id, mutation);

    let new_hash = match kind {
        CredentialKind::Hashed => None,
        CredentialKind::Legacy => Some(store::password::hash_password(SECRET).unwrap()),
    };
    let token = store
        .finish_verified_login(
            &snapshot.user_id.to_string(),
            &snapshot.observed_password,
            new_hash.as_deref(),
        )
        .unwrap();

    assert!(
        token.is_none(),
        "stale {kind:?} snapshot must not be finalized after {mutation:?}"
    );
    assert!(
        store
            .latest_token_for_user(snapshot.user_id)
            .unwrap()
            .is_none(),
        "no session token may be issued for a stale snapshot"
    );
}

#[test]
fn changed_hashed_password_rejects_observed_login_snapshot() {
    assert_stale_snapshot_rejected(CredentialKind::Hashed, Mutation::ChangePassword);
}

#[test]
fn disabled_user_rejects_observed_login_snapshot() {
    assert_stale_snapshot_rejected(CredentialKind::Hashed, Mutation::Disable);
}

#[test]
fn deleted_user_rejects_observed_login_snapshot() {
    assert_stale_snapshot_rejected(CredentialKind::Hashed, Mutation::Delete);
}

#[test]
fn changed_legacy_password_rejects_observed_login_snapshot_upgrade() {
    assert_stale_snapshot_rejected(CredentialKind::Legacy, Mutation::ChangePassword);
}

#[test]
fn disabled_legacy_user_rejects_observed_login_snapshot_upgrade() {
    assert_stale_snapshot_rejected(CredentialKind::Legacy, Mutation::Disable);
}

#[test]
fn deleted_legacy_user_rejects_observed_login_snapshot_upgrade() {
    assert_stale_snapshot_rejected(CredentialKind::Legacy, Mutation::Delete);
}

#[test]
fn unchanged_hashed_snapshot_still_issues_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    insert_admin(&store);
    insert_member(&store, CredentialKind::Hashed);

    let snapshot = store.login_credential(LOGIN).unwrap().unwrap();
    let token = store
        .finish_verified_login(
            &snapshot.user_id.to_string(),
            &snapshot.observed_password,
            None,
        )
        .unwrap()
        .expect("unchanged snapshot must issue a token");
    assert!(store.user_by_token(&token).unwrap().is_some());
}

#[test]
fn unchanged_legacy_snapshot_upgrades_password_and_issues_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    insert_admin(&store);
    let user_id = insert_member(&store, CredentialKind::Legacy);

    let snapshot = store.login_credential(LOGIN).unwrap().unwrap();
    let new_hash = store::password::hash_password(SECRET).unwrap();
    let token = store
        .finish_verified_login(
            &snapshot.user_id.to_string(),
            &snapshot.observed_password,
            Some(&new_hash),
        )
        .unwrap()
        .expect("legacy snapshot must upgrade and issue a token");
    assert!(store.user_by_token(&token).unwrap().is_some());

    let conn = Connection::open(store.data_dir().join("app.db")).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT password FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        stored.starts_with("$argon2"),
        "legacy row must be upgraded: {stored}"
    );
}

#[test]
fn login_credential_hides_disabled_deleted_and_passwordless_users() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    insert_admin(&store);

    let passwordless = User {
        id: UserId::new(),
        login: "passwordless".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&passwordless).unwrap();
    assert!(store.login_credential("passwordless").unwrap().is_none());

    let disabled = UserId::new();
    store
        .insert_user_with_password(
            &User {
                id: disabled,
                login: "disabled".into(),
                enabled: true,
                role: UserRole::Member,
            },
            SECRET,
        )
        .unwrap();
    let mut row = store.get_user(disabled).unwrap().unwrap();
    row.enabled = false;
    store.save_user(&row).unwrap();
    assert!(store.login_credential("disabled").unwrap().is_none());

    assert!(store.login_credential("nobody").unwrap().is_none());
}
