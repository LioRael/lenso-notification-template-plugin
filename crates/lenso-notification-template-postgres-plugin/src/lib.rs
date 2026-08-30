//! Versioned `PostgreSQL` notification templates with deterministic safe rendering.

#![allow(clippy::too_many_lines, clippy::wildcard_imports)]

mod operator;
#[cfg(all(test, feature = "postgres-acceptance"))]
mod postgres_tests;
mod render;
mod schema;

use std::{cell::RefCell, collections::BTreeSet, fmt, rc::Rc, time::Duration};

use lenso::prelude::*;
use lenso_auth_sdk::{
    ActorAssertion, ActorAssertionVerifier, ActorProjectionError, AssertionClock, TypedActor,
};
use lenso_capability_notification_template as template;
use lenso_capability_secrets as secrets;
use lenso_kernel::RuntimeFailure;
use lenso_postgres_kit::OwnedPostgres;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroizing;

use crate::render::{
    RENDERER_IDENTITY, RenderFailure, TemplateDefinition, render, template_digest,
    validate_definition,
};
use crate::schema::schema_plan;

pub use operator::{NotificationTemplateOperator, NotificationTemplateOperatorError};

const DEPENDENCY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CALLERS: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationTemplateConfig {
    schema: String,
    database_url_secret: String,
    auth_issuer: String,
    auth_assertion_public_key: String,
    render_callers: Vec<String>,
    admin_callers: Vec<String>,
    admin_actor_kinds: Vec<String>,
    fallback_locales: Vec<String>,
}

impl NotificationTemplateConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema: impl Into<String>,
        database_url_secret: impl Into<String>,
        auth_issuer: impl Into<String>,
        auth_assertion_public_key: impl Into<String>,
        render_callers: Vec<String>,
        admin_callers: Vec<String>,
        admin_actor_kinds: Vec<String>,
        fallback_locales: Vec<String>,
    ) -> Result<Self, NotificationTemplateConfigError> {
        let config = Self {
            schema: schema.into(),
            database_url_secret: database_url_secret.into(),
            auth_issuer: auth_issuer.into(),
            auth_assertion_public_key: auth_assertion_public_key.into(),
            render_callers,
            admin_callers,
            admin_actor_kinds,
            fallback_locales,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), NotificationTemplateConfigError> {
        schema_plan(self.schema.clone())
            .map_err(|_| NotificationTemplateConfigError::InvalidSchema)?;
        if !valid_secret_reference(&self.database_url_secret) {
            return Err(NotificationTemplateConfigError::InvalidSecretReference);
        }
        if !valid_instance(&self.auth_issuer) {
            return Err(NotificationTemplateConfigError::InvalidAuthIssuer);
        }
        self.verifier()
            .map_err(|_| NotificationTemplateConfigError::InvalidAuthPublicKey)?;
        if !valid_callers(&self.render_callers)
            || !valid_callers(&self.admin_callers)
            || self
                .render_callers
                .iter()
                .any(|caller| self.admin_callers.contains(caller))
        {
            return Err(NotificationTemplateConfigError::InvalidCallers);
        }
        let actor_kinds = self.admin_actor_kinds.iter().collect::<BTreeSet<_>>();
        if actor_kinds.len() != self.admin_actor_kinds.len()
            || actor_kinds.is_empty()
            || self
                .admin_actor_kinds
                .iter()
                .any(|kind| !matches!(kind.as_str(), "user" | "service_account"))
        {
            return Err(NotificationTemplateConfigError::InvalidActorKinds);
        }
        let locales = self.fallback_locales.iter().collect::<BTreeSet<_>>();
        if locales.len() != self.fallback_locales.len()
            || locales.is_empty()
            || locales.len() > 8
            || self
                .fallback_locales
                .iter()
                .any(|locale| normalize_locale(locale).as_deref() != Some(locale.as_str()))
        {
            return Err(NotificationTemplateConfigError::InvalidLocales);
        }
        Ok(())
    }

    fn verifier(&self) -> Result<ActorAssertionVerifier, RuntimeFailure> {
        ActorAssertionVerifier::from_public_key_base64(
            self.auth_issuer.clone(),
            &self.auth_assertion_public_key,
        )
        .map_err(|_| RuntimeFailure::InvalidResolvedPlan {
            detail: "Notification Template Auth assertion verification key is invalid".to_owned(),
        })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum NotificationTemplateConfigError {
    #[error("invalid owned PostgreSQL schema")]
    InvalidSchema,
    #[error("invalid database URL Secret reference")]
    InvalidSecretReference,
    #[error("invalid Auth issuer")]
    InvalidAuthIssuer,
    #[error("invalid Auth assertion public key")]
    InvalidAuthPublicKey,
    #[error("render and admin callers must be bounded, unique, exact, and disjoint")]
    InvalidCallers,
    #[error("admin actor kinds must be a unique subset of user and service_account")]
    InvalidActorKinds,
    #[error("fallback locales must be unique canonical locale tags")]
    InvalidLocales,
}

fn validate_config(config: &NotificationTemplateConfig) -> Result<(), RuntimeFailure> {
    config
        .validate()
        .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
            detail: error.to_string(),
        })
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "configuration.schema.json",
    validate = validate_config
)]
#[derive(Clone)]
struct NotificationTemplatePlugin {
    #[config]
    config: NotificationTemplateConfig,
    secrets: Port<secrets::SecretsClient>,
    state: Rc<RefCell<Option<PreparedTemplates>>>,
}

#[derive(Clone)]
struct PreparedTemplates {
    postgres: OwnedPostgres,
}

impl fmt::Debug for PreparedTemplates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedTemplates")
            .field("schema", &self.postgres.schema())
            .finish()
    }
}

impl fmt::Debug for NotificationTemplatePlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotificationTemplatePlugin")
            .field("schema", &self.config.schema)
            .field("prepared", &self.state.borrow().is_some())
            .field("render_caller_count", &self.config.render_callers.len())
            .field("admin_caller_count", &self.config.admin_callers.len())
            .finish_non_exhaustive()
    }
}

#[lenso::provides(template::NotificationTemplate)]
impl NotificationTemplatePlugin {
    async fn create_version(
        &self,
        context: Ctx,
        request: template::CreateVersionRequest,
    ) -> PluginResult<template::CreateVersionResponse, template::CreateVersionError> {
        let authorized = match self.authorize_admin(&context, template::CREATE_VERSION_OPERATION) {
            Ok(value) => value,
            Err(AuthorizationFailure::Unauthorized) => {
                return Err(PluginError::domain(
                    template::CreateVersionError::Unauthorized,
                ));
            }
            Err(AuthorizationFailure::Runtime(error)) => {
                return Err(PluginError::runtime(error));
            }
        };
        into_plugin(
            self.create_version_value(&authorized, request).await,
            map_create_version_error,
        )
    }

    async fn activate_version(
        &self,
        context: Ctx,
        request: template::ActivateVersionRequest,
    ) -> PluginResult<template::ActivateVersionResponse, template::ActivateVersionError> {
        let authorized = match self.authorize_admin(&context, template::ACTIVATE_VERSION_OPERATION)
        {
            Ok(value) => value,
            Err(AuthorizationFailure::Unauthorized) => {
                return Err(PluginError::domain(
                    template::ActivateVersionError::Unauthorized,
                ));
            }
            Err(AuthorizationFailure::Runtime(error)) => {
                return Err(PluginError::runtime(error));
            }
        };
        into_plugin(
            self.activate_version_value(&authorized, request).await,
            map_activate_version_error,
        )
    }

    async fn get_version(
        &self,
        context: Ctx,
        request: template::GetVersionRequest,
    ) -> PluginResult<template::GetVersionResponse, template::GetVersionError> {
        match self.authorize_admin(&context, template::GET_VERSION_OPERATION) {
            Ok(_) => {}
            Err(AuthorizationFailure::Unauthorized) => {
                return Err(PluginError::domain(template::GetVersionError::Unauthorized));
            }
            Err(AuthorizationFailure::Runtime(error)) => {
                return Err(PluginError::runtime(error));
            }
        }
        into_plugin(self.get_version_value(request).await, map_get_version_error)
    }

    async fn list_versions(
        &self,
        context: Ctx,
        request: template::ListVersionsRequest,
    ) -> PluginResult<template::ListVersionsResponse, template::ListVersionsError> {
        match self.authorize_admin(&context, template::LIST_VERSIONS_OPERATION) {
            Ok(_) => {}
            Err(AuthorizationFailure::Unauthorized) => {
                return Err(PluginError::domain(
                    template::ListVersionsError::Unauthorized,
                ));
            }
            Err(AuthorizationFailure::Runtime(error)) => {
                return Err(PluginError::runtime(error));
            }
        }
        into_plugin(
            self.list_versions_value(request).await,
            map_list_versions_error,
        )
    }

    async fn render(
        &self,
        context: Ctx,
        request: template::RenderRequest,
    ) -> PluginResult<template::RenderResponse, template::RenderError> {
        if !caller_allowed(&context, &self.config.render_callers) {
            return Err(PluginError::domain(template::RenderError::Unauthorized));
        }
        into_plugin(self.render_value(request).await, map_render_error)
    }

    async fn preview(
        &self,
        context: Ctx,
        request: template::PreviewRequest,
    ) -> PluginResult<template::PreviewResponse, template::PreviewError> {
        match self.authorize_admin(&context, template::PREVIEW_OPERATION) {
            Ok(_) => {}
            Err(AuthorizationFailure::Unauthorized) => {
                return Err(PluginError::domain(template::PreviewError::Unauthorized));
            }
            Err(AuthorizationFailure::Runtime(error)) => {
                return Err(PluginError::runtime(error));
            }
        }
        into_plugin(Ok(Self::preview_value(request)), map_preview_error)
    }
}

type TemplateResult<T> = Result<Result<T, TemplateFailure>, RuntimeFailure>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TemplateFailure {
    InvalidRequest,
    NotFound,
    Conflict,
    PreconditionFailed,
    IdempotencyConflict,
    MissingVariable,
    UnexpectedVariable,
    UnsafeVariable,
}

fn into_plugin<T, E>(
    result: TemplateResult<Value>,
    map_error: fn(TemplateFailure) -> E,
) -> PluginResult<T, E>
where
    T: DeserializeOwned,
{
    match result {
        Ok(Ok(value)) => {
            serde_json::from_value(value).map_err(|error| PluginError::runtime(protocol(error)))
        }
        Ok(Err(error)) => Err(PluginError::domain(map_error(error))),
        Err(error) => Err(PluginError::runtime(error)),
    }
}

macro_rules! template_error_mapper {
    ($name:ident, $ty:path) => {
        fn $name(error: TemplateFailure) -> $ty {
            match error {
                TemplateFailure::InvalidRequest => <$ty>::InvalidRequest,
                TemplateFailure::NotFound => <$ty>::NotFound,
                TemplateFailure::Conflict => <$ty>::Conflict,
                TemplateFailure::PreconditionFailed => <$ty>::PreconditionFailed,
                TemplateFailure::IdempotencyConflict => <$ty>::IdempotencyConflict,
                TemplateFailure::MissingVariable => <$ty>::MissingVariable,
                TemplateFailure::UnexpectedVariable => <$ty>::UnexpectedVariable,
                TemplateFailure::UnsafeVariable => <$ty>::UnsafeVariable,
            }
        }
    };
}

template_error_mapper!(map_create_version_error, template::CreateVersionError);
template_error_mapper!(map_activate_version_error, template::ActivateVersionError);
template_error_mapper!(map_get_version_error, template::GetVersionError);
template_error_mapper!(map_list_versions_error, template::ListVersionsError);
template_error_mapper!(map_render_error, template::RenderError);
template_error_mapper!(map_preview_error, template::PreviewError);

#[derive(Clone, Debug)]
struct AuthorizedAdmin {
    caller: String,
    subject: String,
}

#[derive(Debug)]
enum AuthorizationFailure {
    Unauthorized,
    Runtime(RuntimeFailure),
}

#[derive(Clone, Debug)]
struct TemplateActor {
    subject: String,
    kind: String,
}

impl TypedActor for TemplateActor {
    fn from_assertion(assertion: &ActorAssertion) -> Result<Self, ActorProjectionError> {
        Ok(Self {
            subject: assertion.subject().to_owned(),
            kind: assertion.actor_kind().to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct UtcClock;

impl AssertionClock for UtcClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[derive(Clone, Debug)]
struct TemplateRecord {
    template_id: String,
    version: String,
    locale: String,
    revision: i64,
    subject_template: String,
    text_template: String,
    html_template: String,
    renderer_identity: String,
    created_by: String,
    created_at: OffsetDateTime,
    active_version: Option<String>,
    head_revision: Option<i64>,
}

impl TemplateRecord {
    fn from_row(row: &PgRow) -> Result<Self, RuntimeFailure> {
        Ok(Self {
            template_id: row.try_get("template_id").map_err(database)?,
            version: row.try_get("version").map_err(database)?,
            locale: row.try_get("locale").map_err(database)?,
            revision: row.try_get("revision").map_err(database)?,
            subject_template: row.try_get("subject_template").map_err(database)?,
            text_template: row.try_get("text_template").map_err(database)?,
            html_template: row.try_get("html_template").map_err(database)?,
            renderer_identity: row.try_get("renderer_identity").map_err(database)?,
            created_by: row.try_get("created_by").map_err(database)?,
            created_at: row.try_get("created_at").map_err(database)?,
            active_version: row.try_get("active_version").map_err(database)?,
            head_revision: row.try_get("head_revision").map_err(database)?,
        })
    }

    fn definition(&self) -> TemplateDefinition {
        TemplateDefinition {
            subject: self.subject_template.clone(),
            text: self.text_template.clone(),
            html: self.html_template.clone(),
        }
    }

    fn value(&self) -> Result<Value, RuntimeFailure> {
        let definition = self.definition();
        let required_variables =
            validate_definition(&definition).map_err(|_| RuntimeFailure::Internal {
                detail: "stored Notification Template definition is invalid".to_owned(),
            })?;
        if self.renderer_identity != RENDERER_IDENTITY || self.revision != 1 {
            return Err(RuntimeFailure::Internal {
                detail: "stored Notification Template metadata is invalid".to_owned(),
            });
        }
        Ok(json!({
            "template_id": self.template_id,
            "version": self.version,
            "locale": self.locale,
            "revision": self.revision.to_string(),
            "subject_template": self.subject_template,
            "text_template": self.text_template,
            "html_template": self.html_template,
            "required_variables": required_variables,
            "renderer_identity": self.renderer_identity,
            "template_digest": template_digest(&definition),
            "created_by": self.created_by,
            "created_at": timestamp(self.created_at)?,
            "active": self.active_version.as_deref() == Some(self.version.as_str()),
            "head_revision": self.head_revision.map(|revision| revision.to_string()),
        }))
    }
}

#[derive(Clone, Debug)]
enum ReceiptClaim {
    New,
    Replay(Value),
    Conflict,
    InProgress,
}

impl NotificationTemplatePlugin {
    fn prepared(&self) -> Result<PreparedTemplates, RuntimeFailure> {
        self.state
            .borrow()
            .clone()
            .ok_or_else(|| RuntimeFailure::PluginFailure {
                detail: "Notification Template Plugin is not prepared".to_owned(),
            })
    }

    fn authorize_admin(
        &self,
        context: &Ctx,
        operation: &str,
    ) -> Result<AuthorizedAdmin, AuthorizationFailure> {
        let caller = context
            .caller_instance()
            .filter(|caller| {
                self.config
                    .admin_callers
                    .iter()
                    .any(|allowed| allowed == *caller)
            })
            .map(ToOwned::to_owned)
            .ok_or(AuthorizationFailure::Unauthorized)?;
        let actor = self
            .config
            .verifier()
            .map_err(AuthorizationFailure::Runtime)?
            .project_context::<TemplateActor>(
                context,
                template::CAPABILITY_ID,
                operation,
                &UtcClock,
            )
            .map_err(|_| AuthorizationFailure::Unauthorized)?;
        if !valid_opaque(&actor.subject, 256)
            || !self
                .config
                .admin_actor_kinds
                .iter()
                .any(|kind| kind == &actor.kind)
        {
            return Err(AuthorizationFailure::Unauthorized);
        }
        Ok(AuthorizedAdmin {
            caller,
            subject: actor.subject,
        })
    }

    async fn create_version_value(
        &self,
        authorized: &AuthorizedAdmin,
        request: template::CreateVersionRequest,
    ) -> TemplateResult<Value> {
        let Some(locale) = normalize_locale(&request.locale) else {
            return Ok(Err(TemplateFailure::InvalidRequest));
        };
        if !valid_identifier(&request.template_id, 160)
            || !valid_identifier(&request.version, 80)
            || !valid_idempotency_key(&request.idempotency_key)
        {
            return Ok(Err(TemplateFailure::InvalidRequest));
        }
        let definition = TemplateDefinition {
            subject: request.subject_template.clone(),
            text: request.text_template.clone(),
            html: request.html_template.clone(),
        };
        let Ok(required_variables) = validate_definition(&definition) else {
            return Ok(Err(TemplateFailure::InvalidRequest));
        };
        let prepared = self.prepared()?;
        let request_hash = request_hash(&request)?;
        let mut transaction = prepared.postgres.pool().begin().await.map_err(database)?;
        match claim_receipt(
            &mut transaction,
            authorized,
            template::CREATE_VERSION_OPERATION,
            &request.idempotency_key,
            &request_hash,
        )
        .await?
        {
            ReceiptClaim::Replay(mut value) => {
                value["idempotent_replay"] = Value::Bool(true);
                transaction.commit().await.map_err(database)?;
                return Ok(Ok(value));
            }
            ReceiptClaim::Conflict => return Ok(Err(TemplateFailure::IdempotencyConflict)),
            ReceiptClaim::InProgress => return Ok(Err(TemplateFailure::Conflict)),
            ReceiptClaim::New => {}
        }
        let inserted = sqlx::query(
            "INSERT INTO notification_template_versions(template_id,version,locale,subject_template,text_template,html_template,renderer_identity,created_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING created_at",
        )
        .bind(&request.template_id)
        .bind(&request.version)
        .bind(&locale)
        .bind(&request.subject_template)
        .bind(&request.text_template)
        .bind(&request.html_template)
        .bind(RENDERER_IDENTITY)
        .bind(&authorized.subject)
        .fetch_one(transaction.as_mut())
        .await;
        let created_at: OffsetDateTime = match inserted {
            Ok(row) => row.try_get("created_at").map_err(database)?,
            Err(error) if unique_violation(&error) => return Ok(Err(TemplateFailure::Conflict)),
            Err(error) => return Err(database(error)),
        };
        let response = json!({
            "template_id": request.template_id,
            "version": request.version,
            "locale": locale,
            "revision": "1",
            "required_variables": required_variables,
            "renderer_identity": RENDERER_IDENTITY,
            "template_digest": template_digest(&definition),
            "created_at": timestamp(created_at)?,
            "idempotent_replay": false,
        });
        complete_receipt(
            &mut transaction,
            authorized,
            template::CREATE_VERSION_OPERATION,
            &request.idempotency_key,
            &response,
        )
        .await?;
        transaction.commit().await.map_err(database)?;
        Ok(Ok(response))
    }

    async fn activate_version_value(
        &self,
        authorized: &AuthorizedAdmin,
        request: template::ActivateVersionRequest,
    ) -> TemplateResult<Value> {
        let Some(expected_revision) = request
            .expected_revision
            .parse::<i64>()
            .ok()
            .filter(|revision| *revision >= 0)
        else {
            return Ok(Err(TemplateFailure::InvalidRequest));
        };
        if !valid_identifier(&request.template_id, 160)
            || !valid_identifier(&request.version, 80)
            || !valid_idempotency_key(&request.idempotency_key)
        {
            return Ok(Err(TemplateFailure::InvalidRequest));
        }
        let prepared = self.prepared()?;
        let request_hash = request_hash(&request)?;
        let mut transaction = prepared.postgres.pool().begin().await.map_err(database)?;
        match claim_receipt(
            &mut transaction,
            authorized,
            template::ACTIVATE_VERSION_OPERATION,
            &request.idempotency_key,
            &request_hash,
        )
        .await?
        {
            ReceiptClaim::Replay(mut value) => {
                value["idempotent_replay"] = Value::Bool(true);
                transaction.commit().await.map_err(database)?;
                return Ok(Ok(value));
            }
            ReceiptClaim::Conflict => return Ok(Err(TemplateFailure::IdempotencyConflict)),
            ReceiptClaim::InProgress => return Ok(Err(TemplateFailure::Conflict)),
            ReceiptClaim::New => {}
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM notification_template_versions WHERE template_id=$1 AND version=$2)",
        )
        .bind(&request.template_id)
        .bind(&request.version)
        .fetch_one(transaction.as_mut())
        .await
        .map_err(database)?;
        if !exists {
            return Ok(Err(TemplateFailure::NotFound));
        }
        let head = sqlx::query(
            "SELECT revision FROM notification_template_heads WHERE template_id=$1 FOR UPDATE",
        )
        .bind(&request.template_id)
        .fetch_optional(transaction.as_mut())
        .await
        .map_err(database)?;
        let revision = if let Some(head) = head {
            let current: i64 = head.try_get("revision").map_err(database)?;
            if current != expected_revision {
                return Ok(Err(TemplateFailure::PreconditionFailed));
            }
            let next = current
                .checked_add(1)
                .ok_or_else(|| RuntimeFailure::Internal {
                    detail: "Notification Template head revision exhausted".to_owned(),
                })?;
            sqlx::query("UPDATE notification_template_heads SET active_version=$2,revision=$3,updated_by=$4,updated_at=transaction_timestamp() WHERE template_id=$1")
                .bind(&request.template_id)
                .bind(&request.version)
                .bind(next)
                .bind(&authorized.subject)
                .execute(transaction.as_mut())
                .await
                .map_err(database)?;
            next
        } else {
            if expected_revision != 0 {
                return Ok(Err(TemplateFailure::PreconditionFailed));
            }
            sqlx::query("INSERT INTO notification_template_heads(template_id,active_version,revision,updated_by) VALUES($1,$2,1,$3)")
                .bind(&request.template_id)
                .bind(&request.version)
                .bind(&authorized.subject)
                .execute(transaction.as_mut())
                .await
                .map_err(database)?;
            1
        };
        let updated_at: OffsetDateTime = sqlx::query_scalar(
            "SELECT updated_at FROM notification_template_heads WHERE template_id=$1",
        )
        .bind(&request.template_id)
        .fetch_one(transaction.as_mut())
        .await
        .map_err(database)?;
        let response = json!({
            "template_id": request.template_id,
            "active_version": request.version,
            "revision": revision.to_string(),
            "updated_at": timestamp(updated_at)?,
            "idempotent_replay": false,
        });
        complete_receipt(
            &mut transaction,
            authorized,
            template::ACTIVATE_VERSION_OPERATION,
            &request.idempotency_key,
            &response,
        )
        .await?;
        transaction.commit().await.map_err(database)?;
        Ok(Ok(response))
    }

    async fn get_version_value(
        &self,
        request: template::GetVersionRequest,
    ) -> TemplateResult<Value> {
        let Some(locale) = normalize_locale(&request.locale) else {
            return Ok(Err(TemplateFailure::InvalidRequest));
        };
        if !valid_identifier(&request.template_id, 160) || !valid_identifier(&request.version, 80) {
            return Ok(Err(TemplateFailure::InvalidRequest));
        }
        let prepared = self.prepared()?;
        let row = select_template_sql()
            .bind(&request.template_id)
            .bind(&request.version)
            .bind(&locale)
            .fetch_optional(prepared.postgres.pool())
            .await
            .map_err(database)?;
        let Some(row) = row else {
            return Ok(Err(TemplateFailure::NotFound));
        };
        Ok(Ok(TemplateRecord::from_row(&row)?.value()?))
    }

    async fn list_versions_value(
        &self,
        request: template::ListVersionsRequest,
    ) -> TemplateResult<Value> {
        if !(1..=i64::MAX).contains(&request.start_index)
            || !(0..=200).contains(&request.count)
            || request
                .template_id
                .as_deref()
                .is_some_and(|value| !valid_identifier(value, 160))
        {
            return Ok(Err(TemplateFailure::InvalidRequest));
        }
        let prepared = self.prepared()?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notification_template_versions WHERE ($1::text IS NULL OR template_id=$1)",
        )
        .bind(request.template_id.as_deref())
        .fetch_one(prepared.postgres.pool())
        .await
        .map_err(database)?;
        let rows = sqlx::query(
            "SELECT v.template_id,v.version,v.locale,v.revision,v.subject_template,v.text_template,v.html_template,v.renderer_identity,v.created_by,v.created_at,h.active_version,h.revision AS head_revision FROM notification_template_versions v LEFT JOIN notification_template_heads h ON h.template_id=v.template_id WHERE ($1::text IS NULL OR v.template_id=$1) ORDER BY v.template_id,v.version,v.locale LIMIT $2 OFFSET $3",
        )
        .bind(request.template_id.as_deref())
        .bind(request.count)
        .bind(request.start_index - 1)
        .fetch_all(prepared.postgres.pool())
        .await
        .map_err(database)?;
        let resources = rows
            .iter()
            .map(TemplateRecord::from_row)
            .map(|record| record.and_then(|record| record.value()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Ok(json!({
            "resources": resources,
            "total_results": total,
            "start_index": request.start_index,
            "items_per_page": rows.len(),
        })))
    }

    async fn render_value(&self, request: template::RenderRequest) -> TemplateResult<Value> {
        let Some(requested_locale) = normalize_locale(&request.locale) else {
            return Ok(Err(TemplateFailure::InvalidRequest));
        };
        if !valid_identifier(&request.template_id, 160)
            || request
                .version
                .as_deref()
                .is_some_and(|version| !valid_identifier(version, 80))
        {
            return Ok(Err(TemplateFailure::InvalidRequest));
        }
        let prepared = self.prepared()?;
        let version = if let Some(version) = request.version {
            version
        } else {
            let version = sqlx::query_scalar::<_, String>(
                "SELECT active_version FROM notification_template_heads WHERE template_id=$1",
            )
            .bind(&request.template_id)
            .fetch_optional(prepared.postgres.pool())
            .await
            .map_err(database)?;
            let Some(version) = version else {
                return Ok(Err(TemplateFailure::NotFound));
            };
            version
        };
        let mut selected = None;
        for locale in locale_candidates(&requested_locale, &self.config.fallback_locales) {
            let row = select_template_sql()
                .bind(&request.template_id)
                .bind(&version)
                .bind(&locale)
                .fetch_optional(prepared.postgres.pool())
                .await
                .map_err(database)?;
            if let Some(row) = row {
                selected = Some(TemplateRecord::from_row(&row)?);
                break;
            }
        }
        let Some(record) = selected else {
            return Ok(Err(TemplateFailure::NotFound));
        };
        let definition = record.definition();
        validate_definition(&definition).map_err(|_| RuntimeFailure::Internal {
            detail: "stored Notification Template definition is invalid".to_owned(),
        })?;
        let rendered = match render(
            &definition,
            request
                .variables
                .into_iter()
                .map(|variable| (variable.name, variable.value)),
        ) {
            Ok(value) => value,
            Err(RenderFailure::InvalidTemplate) => {
                return Err(RuntimeFailure::Internal {
                    detail: "stored Notification Template definition is invalid".to_owned(),
                });
            }
            Err(error) => return Ok(Err(map_render_failure(error))),
        };
        Ok(Ok(rendered_value(
            &record.template_id,
            &record.version,
            &requested_locale,
            &record.locale,
            &rendered,
        )))
    }

    fn preview_value(request: template::PreviewRequest) -> Result<Value, TemplateFailure> {
        let Some(locale) = normalize_locale(&request.locale) else {
            return Err(TemplateFailure::InvalidRequest);
        };
        if !valid_identifier(&request.template_id, 160) || !valid_identifier(&request.version, 80) {
            return Err(TemplateFailure::InvalidRequest);
        }
        let definition = TemplateDefinition {
            subject: request.subject_template,
            text: request.text_template,
            html: request.html_template,
        };
        let rendered = match render(
            &definition,
            request
                .variables
                .into_iter()
                .map(|variable| (variable.name, variable.value)),
        ) {
            Ok(value) => value,
            Err(RenderFailure::InvalidTemplate) => {
                return Err(TemplateFailure::InvalidRequest);
            }
            Err(error) => return Err(map_render_failure(error)),
        };
        Ok(rendered_value(
            &request.template_id,
            &request.version,
            &locale,
            &locale,
            &rendered,
        ))
    }
}

fn select_template_sql<'a>() -> sqlx::query::Query<'a, Postgres, sqlx::postgres::PgArguments> {
    sqlx::query(
        "SELECT v.template_id,v.version,v.locale,v.revision,v.subject_template,v.text_template,v.html_template,v.renderer_identity,v.created_by,v.created_at,h.active_version,h.revision AS head_revision FROM notification_template_versions v LEFT JOIN notification_template_heads h ON h.template_id=v.template_id WHERE v.template_id=$1 AND v.version=$2 AND v.locale=$3",
    )
}

async fn claim_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedAdmin,
    operation: &str,
    idempotency_key: &str,
    request_hash: &[u8],
) -> Result<ReceiptClaim, RuntimeFailure> {
    let inserted = sqlx::query("INSERT INTO notification_template_command_receipts(caller_instance,actor_subject,operation,idempotency_key,request_hash,result_json) VALUES($1,$2,$3,$4,$5,NULL) ON CONFLICT DO NOTHING")
        .bind(&authorized.caller)
        .bind(&authorized.subject)
        .bind(operation)
        .bind(idempotency_key)
        .bind(request_hash)
        .execute(transaction.as_mut())
        .await
        .map_err(database)?;
    if inserted.rows_affected() == 1 {
        return Ok(ReceiptClaim::New);
    }
    let row = sqlx::query("SELECT request_hash,result_json FROM notification_template_command_receipts WHERE caller_instance=$1 AND actor_subject=$2 AND operation=$3 AND idempotency_key=$4 FOR UPDATE")
        .bind(&authorized.caller)
        .bind(&authorized.subject)
        .bind(operation)
        .bind(idempotency_key)
        .fetch_one(transaction.as_mut())
        .await
        .map_err(database)?;
    let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(database)?;
    if stored_hash != request_hash {
        return Ok(ReceiptClaim::Conflict);
    }
    let result: Option<Value> = row.try_get("result_json").map_err(database)?;
    Ok(result.map_or(ReceiptClaim::InProgress, ReceiptClaim::Replay))
}

async fn complete_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    authorized: &AuthorizedAdmin,
    operation: &str,
    idempotency_key: &str,
    result: &Value,
) -> Result<(), RuntimeFailure> {
    let updated = sqlx::query("UPDATE notification_template_command_receipts SET result_json=$5 WHERE caller_instance=$1 AND actor_subject=$2 AND operation=$3 AND idempotency_key=$4 AND result_json IS NULL")
        .bind(&authorized.caller)
        .bind(&authorized.subject)
        .bind(operation)
        .bind(idempotency_key)
        .bind(result)
        .execute(transaction.as_mut())
        .await
        .map_err(database)?;
    if updated.rows_affected() != 1 {
        return Err(RuntimeFailure::Internal {
            detail: "Notification Template command receipt completion lost ownership".to_owned(),
        });
    }
    Ok(())
}

fn rendered_value(
    template_id: &str,
    version: &str,
    requested_locale: &str,
    resolved_locale: &str,
    rendered: &render::RenderedMessage,
) -> Value {
    json!({
        "template_id": template_id,
        "version": version,
        "requested_locale": requested_locale,
        "resolved_locale": resolved_locale,
        "fallback_used": requested_locale != resolved_locale,
        "renderer_identity": RENDERER_IDENTITY,
        "template_digest": rendered.template_digest,
        "content_digest": rendered.content_digest,
        "subject": rendered.subject,
        "text": rendered.text,
        "html": rendered.html,
    })
}

fn map_render_failure(error: RenderFailure) -> TemplateFailure {
    match error {
        RenderFailure::InvalidTemplate => TemplateFailure::InvalidRequest,
        RenderFailure::MissingVariable => TemplateFailure::MissingVariable,
        RenderFailure::UnexpectedVariable => TemplateFailure::UnexpectedVariable,
        RenderFailure::UnsafeVariable => TemplateFailure::UnsafeVariable,
    }
}

fn request_hash(request: &impl Serialize) -> Result<Vec<u8>, RuntimeFailure> {
    serde_json::to_vec(request)
        .map(|bytes| Sha256::digest(bytes).to_vec())
        .map_err(protocol)
}

fn timestamp(value: OffsetDateTime) -> Result<String, RuntimeFailure> {
    value
        .format(&Rfc3339)
        .map_err(|_| RuntimeFailure::Internal {
            detail: "Notification Template PostgreSQL timestamp is not RFC 3339".to_owned(),
        })
}

fn caller_allowed(context: &Ctx, allowed: &[String]) -> bool {
    context
        .caller_instance()
        .is_some_and(|caller| allowed.iter().any(|value| value == caller))
}

fn valid_callers(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= MAX_CALLERS
        && values.iter().all(|value| valid_instance(value))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn valid_instance(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_opaque(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= maximum
        && !value.contains('\0')
        && !value.chars().any(char::is_control)
}

fn valid_idempotency_key(value: &str) -> bool {
    valid_identifier(value, 240)
}

fn valid_secret_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("//")
        && value.split('/').all(|part| part != "." && part != "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

fn normalize_locale(value: &str) -> Option<String> {
    if value.len() < 2 || value.len() > 32 || value.contains('_') {
        return None;
    }
    let mut parts = value.split('-');
    let language = parts.next()?;
    if !(2..=8).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    let mut normalized = language.to_ascii_lowercase();
    for part in parts {
        if !(2..=8).contains(&part.len()) || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return None;
        }
        normalized.push('-');
        if part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            normalized.push_str(&part.to_ascii_uppercase());
        } else {
            normalized.push_str(&part.to_ascii_lowercase());
        }
    }
    Some(normalized)
}

fn locale_candidates(requested: &str, configured: &[String]) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut add = |locale: &str| {
        if !candidates.iter().any(|candidate| candidate == locale) {
            candidates.push(locale.to_owned());
        }
    };
    add(requested);
    if let Some((language, _)) = requested.split_once('-') {
        add(language);
    }
    for locale in configured {
        add(locale);
    }
    candidates
}

fn unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(error) if error.code().as_deref() == Some("23505"))
}

fn database(_error: sqlx::Error) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: "Notification Template PostgreSQL operation failed".to_owned(),
    }
}

fn protocol(_error: serde_json::Error) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: "Notification Template portable representation failed".to_owned(),
    }
}

impl Lifecycle for NotificationTemplatePlugin {
    async fn prepare(&self, context: PrepareContext) -> Result<(), RuntimeFailure> {
        let dependencies = context.dependencies().clone();
        let cancellation = context.cancellation();
        let secrets = secrets::SecretsClient::from_dependencies(&dependencies)?;
        let invocation = dependencies.invocation_context_after(DEPENDENCY_TIMEOUT, cancellation)?;
        let database_url = secrets
            .resolve_with_context(
                invocation,
                secrets::ResolveRequest {
                    reference: self.config.database_url_secret.clone(),
                },
            )
            .await
            .map(|response| Zeroizing::new(response.value))
            .map_err(|error| match error {
                secrets::SecretsInvocationError::Domain(_) => RuntimeFailure::PluginFailure {
                    detail: "Notification Template database Secret was rejected".to_owned(),
                },
                secrets::SecretsInvocationError::Runtime(error) => error,
            })?;
        let postgres = OwnedPostgres::prepare(
            &database_url,
            schema_plan(self.config.schema.clone()).map_err(|error| {
                RuntimeFailure::InvalidResolvedPlan {
                    detail: error.to_string(),
                }
            })?,
        )
        .await
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "Notification Template PostgreSQL preparation failed".to_owned(),
        })?;
        self.state.replace(Some(PreparedTemplates { postgres }));
        Ok(())
    }

    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        let prepared = self.state.borrow_mut().take();
        if let Some(prepared) = prepared {
            prepared.postgres.pool().close().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_auth_sdk::{ActorAssertionIssuer, Validity, audience};
    use lenso_native_adapter::NativePluginRegistry;
    use std::collections::BTreeMap;
    use time::Duration as TimeDuration;

    fn config() -> NotificationTemplateConfig {
        let issuer = ActorAssertionIssuer::new("auth.users", b"notification-template-test-key");
        NotificationTemplateConfig::new(
            "notification_templates",
            "notification-templates/database-url",
            "auth.users",
            issuer.public_key_base64(),
            vec!["notification-blue".to_owned()],
            vec!["template-admin-blue".to_owned()],
            vec!["user".to_owned(), "service_account".to_owned()],
            vec!["en".to_owned()],
        )
        .unwrap()
    }

    fn plugin() -> NotificationTemplatePlugin {
        NotificationTemplatePlugin {
            config: config(),
            secrets: Port::default(),
            state: Rc::new(RefCell::new(None)),
        }
    }

    fn context(caller: &str) -> Ctx {
        Ctx::new(1, None, CancellationToken::new()).with_caller_instance(caller)
    }

    #[test]
    fn generated_descriptor_declares_template_role_and_secrets_dependency() {
        let descriptor: Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(PACKAGE_ID, "lenso.notification-template.postgres");
        assert_eq!(descriptor["root_slot"], "notification_templates");
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            template::CAPABILITY_ID
        );
        assert_eq!(
            descriptor["required_capabilities"][0]["capability_id"],
            secrets::CAPABILITY_ID
        );
        assert_eq!(
            NativePluginRegistry::new()
                .with_linked_factories()
                .factories()
                .filter(|factory| factory.package_id() == PACKAGE_ID)
                .count(),
            1
        );
    }

    #[test]
    fn configuration_is_strict_and_authority_roles_are_disjoint() {
        assert_eq!(config().validate(), Ok(()));
        let mut invalid = config();
        invalid.admin_callers = invalid.render_callers.clone();
        assert_eq!(
            invalid.validate(),
            Err(NotificationTemplateConfigError::InvalidCallers)
        );
        let mut invalid = config();
        invalid.fallback_locales = vec!["EN_us".to_owned()];
        assert_eq!(
            invalid.validate(),
            Err(NotificationTemplateConfigError::InvalidLocales)
        );
    }

    #[test]
    fn render_rejects_unknown_caller_before_storage() {
        let result = futures::executor::block_on(plugin().render(
            context("unknown"),
            template::RenderRequest {
                locale: "en".to_owned(),
                template_id: "organization-invitation".to_owned(),
                variables: Vec::new(),
                version: Some("v1".to_owned()),
            },
        ));
        assert_eq!(
            result,
            Err(PluginError::Domain(template::RenderError::Unauthorized))
        );
    }

    #[test]
    fn admin_assertion_is_issuer_signature_time_and_operation_bound() {
        let issuer = ActorAssertionIssuer::new("auth.users", b"notification-template-test-key");
        let now = OffsetDateTime::now_utc();
        let assertion = issuer.issue(
            "usr_admin",
            "user",
            "strong",
            [audience(
                template::CAPABILITY_ID,
                template::CREATE_VERSION_OPERATION,
            )],
            Validity::new(
                now - TimeDuration::seconds(1),
                now + TimeDuration::minutes(1),
            )
            .unwrap(),
            BTreeMap::new(),
        );
        let context = assertion.attach(context("template-admin-blue")).unwrap();
        assert!(
            plugin()
                .authorize_admin(&context, template::CREATE_VERSION_OPERATION)
                .is_ok()
        );
        assert!(matches!(
            plugin().authorize_admin(&context, template::ACTIVATE_VERSION_OPERATION),
            Err(AuthorizationFailure::Unauthorized)
        ));
    }

    #[test]
    fn locale_fallback_is_exact_then_language_then_configured() {
        assert_eq!(
            locale_candidates("fr-CA", &["en-US".to_owned(), "en".to_owned()]),
            vec!["fr-CA", "fr", "en-US", "en"]
        );
        assert_eq!(normalize_locale("en-us").as_deref(), Some("en-US"));
        assert_eq!(normalize_locale("en_US"), None);
    }

    #[test]
    fn migration_seeds_only_template_owned_state() {
        let migration = include_str!("../migrations/001_create_notification_template_catalog.sql");
        for table in [
            "notification_template_versions",
            "notification_template_heads",
            "notification_template_command_receipts",
        ] {
            assert!(migration.contains(&format!("CREATE TABLE {table}")));
        }
        for forbidden in ["notifications", "deliveries", "identities", "memberships"] {
            assert!(!migration.contains(&format!("CREATE TABLE {forbidden}")));
        }
        assert!(migration.contains("organization-invitation"));
        assert!(migration.contains("access-request-expiring"));
    }
}
