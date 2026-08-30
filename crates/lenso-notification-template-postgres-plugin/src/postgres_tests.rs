use super::*;

use std::collections::BTreeMap;

use lenso_auth_sdk::{ActorAssertionIssuer, Validity, audience};
use sqlx::AssertSqlSafe;
use time::Duration;

fn admin_context(issuer: &ActorAssertionIssuer, operation: &str) -> Ctx {
    let now = OffsetDateTime::now_utc();
    issuer
        .issue(
            "usr_template_admin",
            "user",
            "strong",
            [audience(template::CAPABILITY_ID, operation)],
            Validity::new(now - Duration::seconds(1), now + Duration::minutes(1)).unwrap(),
            BTreeMap::new(),
        )
        .attach(
            Ctx::new(1, None, CancellationToken::new()).with_caller_instance("template-admin-blue"),
        )
        .unwrap()
}

async fn prepare() -> Option<(String, String, OwnedPostgres)> {
    let database_url = std::env::var("LENSO_NOTIFICATION_TEMPLATE_TEST_DATABASE_URL").ok()?;
    let schema_name = format!("ntpl_accept_{}", uuid::Uuid::new_v4().simple());
    NotificationTemplateOperator::setup(&database_url, &schema_name)
        .await
        .unwrap();
    NotificationTemplateOperator::upgrade(&database_url, &schema_name)
        .await
        .unwrap();
    let postgres = OwnedPostgres::prepare(
        &database_url,
        schema::schema_plan(schema_name.clone()).unwrap(),
    )
    .await
    .unwrap();
    Some((database_url, schema_name, postgres))
}

async fn cleanup(database_url: &str, schema_name: &str, postgres: OwnedPostgres) {
    assert!(schema_name.starts_with("ntpl_accept_"));
    postgres.pool().close().await;
    let pool = sqlx::PgPool::connect(database_url).await.unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "DROP SCHEMA \"{schema_name}\" CASCADE"
    )))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn postgres_seed_restart_immutability_cas_and_receipt_acceptance() {
    let Some((database_url, schema_name, postgres)) = prepare().await else {
        eprintln!(
            "skipping PostgreSQL acceptance; LENSO_NOTIFICATION_TEMPLATE_TEST_DATABASE_URL is unset"
        );
        return;
    };
    let seeded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notification_template_versions WHERE version='v1'",
    )
    .fetch_one(postgres.pool())
    .await
    .unwrap();
    assert_eq!(seeded, 10);
    let seeded_definitions = sqlx::query(
        "SELECT subject_template,text_template,html_template FROM notification_template_versions",
    )
    .fetch_all(postgres.pool())
    .await
    .unwrap();
    for row in seeded_definitions {
        validate_definition(&TemplateDefinition {
            subject: row.try_get("subject_template").unwrap(),
            text: row.try_get("text_template").unwrap(),
            html: row.try_get("html_template").unwrap(),
        })
        .expect("seeded templates must satisfy the production renderer");
    }

    let issuer = ActorAssertionIssuer::new("auth.users", b"notification-template-test-key");
    let plugin = NotificationTemplatePlugin {
        config: NotificationTemplateConfig::new(
            &schema_name,
            "notification-templates/database-url",
            "auth.users",
            issuer.public_key_base64(),
            vec!["notification-blue".to_owned()],
            vec!["template-admin-blue".to_owned()],
            vec!["user".to_owned()],
            vec!["en".to_owned()],
        )
        .unwrap(),
        secrets: Port::default(),
        state: Rc::new(RefCell::new(Some(PreparedTemplates {
            postgres: postgres.clone(),
        }))),
    };
    let rendered = plugin
        .render(
            Ctx::new(1, None, CancellationToken::new()).with_caller_instance("notification-blue"),
            template::RenderRequest {
                locale: "en-GB".to_owned(),
                template_id: "organization-invitation".to_owned(),
                variables: vec![
                    template::RenderRequestVariablesItem {
                        name: "expires_at".to_owned(),
                        value: "2026-09-01T00:00:00Z".to_owned(),
                    },
                    template::RenderRequestVariablesItem {
                        name: "invitation_url".to_owned(),
                        value: "https://example.test/invitations/secret".to_owned(),
                    },
                    template::RenderRequestVariablesItem {
                        name: "inviter_display_name".to_owned(),
                        value: "Alice".to_owned(),
                    },
                    template::RenderRequestVariablesItem {
                        name: "locale".to_owned(),
                        value: "en-GB".to_owned(),
                    },
                    template::RenderRequestVariablesItem {
                        name: "organization_name".to_owned(),
                        value: "Acme <Ops>".to_owned(),
                    },
                    template::RenderRequestVariablesItem {
                        name: "recipient_display_name".to_owned(),
                        value: String::new(),
                    },
                    template::RenderRequestVariablesItem {
                        name: "role_name".to_owned(),
                        value: "Member".to_owned(),
                    },
                ],
                version: Some("v1".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(rendered.fallback_used);
    assert_eq!(rendered.resolved_locale, "en");
    assert!(rendered.html.contains("Acme &lt;Ops&gt;"));

    let create = template::CreateVersionRequest {
        html_template: "<p>Hello {{name}}</p>".to_owned(),
        idempotency_key: "custom-v1".to_owned(),
        locale: "fr".to_owned(),
        subject_template: "Hello {{name}}".to_owned(),
        template_id: "custom".to_owned(),
        text_template: "Hello {{name}}".to_owned(),
        version: "v1".to_owned(),
    };
    let created = plugin
        .create_version(
            admin_context(&issuer, template::CREATE_VERSION_OPERATION),
            create.clone(),
        )
        .await
        .unwrap();
    assert!(!created.idempotent_replay);
    let replay = plugin
        .create_version(
            admin_context(&issuer, template::CREATE_VERSION_OPERATION),
            create.clone(),
        )
        .await
        .unwrap();
    assert!(replay.idempotent_replay);
    let mut changed = create.clone();
    changed.subject_template = "Changed {{name}}".to_owned();
    assert!(matches!(
        plugin
            .create_version(
                admin_context(&issuer, template::CREATE_VERSION_OPERATION),
                changed,
            )
            .await,
        Err(PluginError::Domain(
            template::CreateVersionError::IdempotencyConflict
        ))
    ));
    let mut create_v2 = create;
    create_v2.version = "v2".to_owned();
    create_v2.idempotency_key = "custom-v2".to_owned();
    plugin
        .create_version(
            admin_context(&issuer, template::CREATE_VERSION_OPERATION),
            create_v2,
        )
        .await
        .unwrap();
    assert!(
        sqlx::query("INSERT INTO notification_template_versions(template_id,version,locale,subject_template,text_template,html_template,renderer_identity,created_by) VALUES('custom','v1','fr','changed','changed','changed',$1,'acceptance')")
            .bind(RENDERER_IDENTITY)
            .execute(postgres.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE notification_template_versions SET subject_template='changed' WHERE template_id='custom' AND version='v1' AND locale='fr'")
            .execute(postgres.pool())
            .await
            .is_err()
    );
    let activated = plugin
        .activate_version(
            admin_context(&issuer, template::ACTIVATE_VERSION_OPERATION),
            template::ActivateVersionRequest {
                expected_revision: "0".to_owned(),
                idempotency_key: "activate-custom-v1".to_owned(),
                template_id: "custom".to_owned(),
                version: "v1".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(activated.revision, "1");
    let (left, right) = tokio::join!(
        sqlx::query("UPDATE notification_template_heads SET active_version='v2',revision=2 WHERE template_id='custom' AND revision=1").execute(postgres.pool()),
        sqlx::query("UPDATE notification_template_heads SET active_version='v1',revision=2 WHERE template_id='custom' AND revision=1").execute(postgres.pool()),
    );
    assert_eq!(
        left.unwrap().rows_affected() + right.unwrap().rows_affected(),
        1
    );
    postgres.pool().close().await;
    let restarted = OwnedPostgres::prepare(
        &database_url,
        schema::schema_plan(schema_name.clone()).unwrap(),
    )
    .await
    .unwrap();
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM notification_template_heads WHERE template_id='custom'",
    )
    .fetch_one(restarted.pool())
    .await
    .unwrap();
    assert_eq!(revision, 2);
    let receipt_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notification_template_command_receipts")
            .fetch_one(restarted.pool())
            .await
            .unwrap();
    assert_eq!(receipt_count, 3);
    cleanup(&database_url, &schema_name, restarted).await;
}
