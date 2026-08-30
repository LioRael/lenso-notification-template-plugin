CREATE TABLE notification_template_versions (
    template_id text NOT NULL,
    version text NOT NULL,
    locale text NOT NULL,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision = 1),
    subject_template text NOT NULL,
    text_template text NOT NULL,
    html_template text NOT NULL,
    renderer_identity text NOT NULL,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (template_id, version, locale)
);

CREATE TABLE notification_template_heads (
    template_id text PRIMARY KEY,
    active_version text NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);

CREATE TABLE notification_template_command_receipts (
    caller_instance text NOT NULL,
    actor_subject text NOT NULL,
    operation text NOT NULL,
    idempotency_key text NOT NULL,
    request_hash bytea NOT NULL,
    result_json jsonb,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (caller_instance, actor_subject, operation, idempotency_key)
);

CREATE INDEX notification_template_versions_list_idx
    ON notification_template_versions (template_id, version, locale);

CREATE FUNCTION reject_notification_template_version_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'published notification template versions are immutable';
END
$function$;

CREATE TRIGGER notification_template_versions_immutable
BEFORE UPDATE OR DELETE ON notification_template_versions
FOR EACH ROW EXECUTE FUNCTION reject_notification_template_version_mutation();

INSERT INTO notification_template_versions (
    template_id, version, locale, subject_template, text_template, html_template,
    renderer_identity, created_by
) VALUES
(
    'organization-invitation', 'v1', 'en',
    'Invitation to join {{organization_name}}',
    $template$Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},

{{#inviter_display_name}}{{inviter_display_name}} invited you to join {{organization_name}}.{{/inviter_display_name}}{{^inviter_display_name}}You have been invited to join {{organization_name}}.{{/inviter_display_name}}{{#role_name}}
Role: {{role_name}}{{/role_name}}

Accept invitation: {{invitation_url}}
Expires: {{expires_at}}

If you did not expect this invitation, you can ignore this email.$template$,
    $template$<!doctype html><html lang="{{locale}}"><body><p>Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},</p><p>{{#inviter_display_name}}{{inviter_display_name}} invited you to join {{organization_name}}.{{/inviter_display_name}}{{^inviter_display_name}}You have been invited to join {{organization_name}}.{{/inviter_display_name}}</p>{{#role_name}}<p>Role: {{role_name}}</p>{{/role_name}}<p><a href="{{invitation_url}}">Accept invitation</a></p><p>Expires: {{expires_at}}</p><p>If you did not expect this invitation, you can ignore this email.</p></body></html>$template$,
    'lenso.notification-template.renderer/safe-sections@1', 'builtin:migration'
),
(
    'access-request-submitted', 'v1', 'en',
    'Access request submitted',
    $template$Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},

Your access request was submitted for review.

Organization: {{organization_id}}
Role: {{role}}
Scope: {{scope_kind}}/{{scope}}
Request: {{request_id}}{{#expires_at}}
Expires: {{expires_at}}{{/expires_at}}

Contact an organization administrator if you did not expect this notification.$template$,
    $template$<!doctype html><html lang="{{locale}}"><body><p>Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},</p><p>Your access request was submitted for review.</p><p>Organization: {{organization_id}}</p><p>Role: {{role}}</p><p>Scope: {{scope_kind}}/{{scope}}</p><p>Request: {{request_id}}</p>{{#expires_at}}<p>Expires: {{expires_at}}</p>{{/expires_at}}<p>Contact an organization administrator if you did not expect this notification.</p></body></html>$template$,
    'lenso.notification-template.renderer/safe-sections@1', 'builtin:migration'
),
(
    'access-request-approved', 'v1', 'en',
    'Access request approved',
    $template$Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},

Your access request was approved.

Organization: {{organization_id}}
Role: {{role}}
Scope: {{scope_kind}}/{{scope}}
Request: {{request_id}}{{#expires_at}}
Expires: {{expires_at}}{{/expires_at}}

Contact an organization administrator if you did not expect this notification.$template$,
    $template$<!doctype html><html lang="{{locale}}"><body><p>Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},</p><p>Your access request was approved.</p><p>Organization: {{organization_id}}</p><p>Role: {{role}}</p><p>Scope: {{scope_kind}}/{{scope}}</p><p>Request: {{request_id}}</p>{{#expires_at}}<p>Expires: {{expires_at}}</p>{{/expires_at}}<p>Contact an organization administrator if you did not expect this notification.</p></body></html>$template$,
    'lenso.notification-template.renderer/safe-sections@1', 'builtin:migration'
),
(
    'access-request-denied', 'v1', 'en',
    'Access request denied',
    $template$Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},

Your access request was denied.

Organization: {{organization_id}}
Role: {{role}}
Scope: {{scope_kind}}/{{scope}}
Request: {{request_id}}

Contact an organization administrator if you did not expect this notification.$template$,
    $template$<!doctype html><html lang="{{locale}}"><body><p>Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},</p><p>Your access request was denied.</p><p>Organization: {{organization_id}}</p><p>Role: {{role}}</p><p>Scope: {{scope_kind}}/{{scope}}</p><p>Request: {{request_id}}</p><p>Contact an organization administrator if you did not expect this notification.</p></body></html>$template$,
    'lenso.notification-template.renderer/safe-sections@1', 'builtin:migration'
),
(
    'access-request-expiring', 'v1', 'en',
    'Access request expiring',
    $template$Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},

Your pending access request is expiring soon.

Organization: {{organization_id}}
Role: {{role}}
Scope: {{scope_kind}}/{{scope}}
Request: {{request_id}}
Expires: {{expires_at}}

Contact an organization administrator if you did not expect this notification.$template$,
    $template$<!doctype html><html lang="{{locale}}"><body><p>Hello{{#recipient_display_name}} {{recipient_display_name}}{{/recipient_display_name}},</p><p>Your pending access request is expiring soon.</p><p>Organization: {{organization_id}}</p><p>Role: {{role}}</p><p>Scope: {{scope_kind}}/{{scope}}</p><p>Request: {{request_id}}</p><p>Expires: {{expires_at}}</p><p>Contact an organization administrator if you did not expect this notification.</p></body></html>$template$,
    'lenso.notification-template.renderer/safe-sections@1', 'builtin:migration'
);

INSERT INTO notification_template_versions (
    template_id, version, locale, subject_template, text_template, html_template,
    renderer_identity, created_by
)
SELECT template_id, version, 'en-US', subject_template, text_template, html_template,
       renderer_identity, created_by
FROM notification_template_versions
WHERE locale = 'en';

INSERT INTO notification_template_heads (template_id, active_version, revision, updated_by)
SELECT DISTINCT template_id, 'v1', 1, 'builtin:migration'
FROM notification_template_versions;
