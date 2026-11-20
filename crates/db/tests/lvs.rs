//! Läuft nur mit `GALA_TEST_DATABASE_URL` (leere Wegwerf-Datenbank, wird migriert).

use db::lvs::{LvStats, NewLv};

#[tokio::test]
async fn lv_roundtrip() {
    let Ok(url) = std::env::var("GALA_TEST_DATABASE_URL") else {
        return;
    };
    let pool = db::connect(&url).await.unwrap();
    db::migrate(&pool).await.unwrap();
    let user: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO app_user (username, pw_hash, role) VALUES ('lv-test-' || gen_random_uuid(), 'x', 'kalkulator') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let stats = |n| LvStats {
        positions: 3,
        priced: n,
        total: "12.50".into(),
    };
    let id = db::lvs::insert(
        &pool,
        NewLv {
            name: "Kita",
            format: "D83",
            file_name: "kita.d83",
            original: b"00 original",
            lv: serde_json::json!({"name": "Kita"}),
            warnings: serde_json::json!(["w"]),
            stats: stats(0),
            created_by: user,
        },
    )
    .await
    .unwrap();
    let full = db::lvs::get(&pool, id).await.unwrap().unwrap();
    assert_eq!(full.original, b"00 original");
    assert_eq!(full.row.revision, 1);
    assert!(full.row.created_by.unwrap().starts_with("lv-test-"));
    assert!(
        db::lvs::list(&pool)
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == id)
    );

    let rev = db::lvs::update(
        &pool,
        id,
        1,
        serde_json::json!({"name": "Kita 2"}),
        stats(2),
    )
    .await
    .unwrap();
    assert_eq!(rev, Some(2));
    // Veraltete Revision: kein Speichern.
    let stale = db::lvs::update(&pool, id, 1, serde_json::json!({}), stats(3))
        .await
        .unwrap();
    assert_eq!(stale, None);
    let full = db::lvs::get(&pool, id).await.unwrap().unwrap();
    assert_eq!(
        (full.row.priced, full.lv["name"].as_str()),
        (2, Some("Kita 2"))
    );

    assert!(db::lvs::delete(&pool, id).await.unwrap());
    assert!(!db::lvs::delete(&pool, id).await.unwrap());
    sqlx::query("DELETE FROM app_user WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}
