use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use sha2::{Digest, Sha256};

pub(crate) const RENDERER_IDENTITY: &str = "lenso.notification-template.renderer/safe-sections@1";

#[derive(Clone, Debug)]
pub(crate) struct TemplateDefinition {
    pub subject: String,
    pub text: String,
    pub html: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RenderedMessage {
    pub subject: String,
    pub text: String,
    pub html: String,
    pub required_variables: Vec<String>,
    pub template_digest: String,
    pub content_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderFailure {
    InvalidTemplate,
    MissingVariable,
    UnexpectedVariable,
    UnsafeVariable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Node {
    Text(String),
    Variable(String),
    Section {
        name: String,
        inverted: bool,
        children: Vec<Self>,
    },
}

pub(crate) fn validate_definition(
    definition: &TemplateDefinition,
) -> Result<Vec<String>, RenderFailure> {
    if definition.subject.trim().is_empty()
        || definition.subject.len() > 998
        || definition.subject.contains(['\r', '\n', '\0'])
        || definition.text.trim().is_empty()
        || definition.text.len() > 131_072
        || definition.text.contains('\0')
        || definition.html.trim().is_empty()
        || definition.html.len() > 262_144
        || definition.html.contains('\0')
        || unsafe_html_literal(&definition.html)
    {
        return Err(RenderFailure::InvalidTemplate);
    }
    let mut required = BTreeSet::new();
    parse(&definition.subject, &mut required)?;
    parse(&definition.text, &mut required)?;
    parse(&definition.html, &mut required)?;
    if required.is_empty() || required.len() > 64 {
        return Err(RenderFailure::InvalidTemplate);
    }
    Ok(required.into_iter().collect())
}

pub(crate) fn template_digest(definition: &TemplateDefinition) -> String {
    digest(&[&definition.subject, &definition.text, &definition.html])
}

pub(crate) fn render(
    definition: &TemplateDefinition,
    variables: impl IntoIterator<Item = (String, String)>,
) -> Result<RenderedMessage, RenderFailure> {
    let required_variables = validate_definition(definition)?;
    let mut values = BTreeMap::new();
    for (name, value) in variables {
        if !valid_variable_name(&name)
            || value.len() > 4_096
            || value.contains('\0')
            || value
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
            || values.insert(name.clone(), value).is_some()
        {
            return Err(RenderFailure::UnsafeVariable);
        }
        if name.ends_with("_url") {
            let value = values.get(&name).expect("inserted variable");
            if !(value.starts_with("https://") || value.starts_with("http://localhost")) {
                return Err(RenderFailure::UnsafeVariable);
            }
        }
    }
    let supplied = values.keys().cloned().collect::<BTreeSet<_>>();
    let required = required_variables.iter().cloned().collect::<BTreeSet<_>>();
    if !required.is_subset(&supplied) {
        return Err(RenderFailure::MissingVariable);
    }
    if supplied != required {
        return Err(RenderFailure::UnexpectedVariable);
    }

    let subject = render_nodes(
        &parse(&definition.subject, &mut BTreeSet::new())?,
        &values,
        false,
    );
    let text = render_nodes(
        &parse(&definition.text, &mut BTreeSet::new())?,
        &values,
        false,
    );
    let html = render_nodes(
        &parse(&definition.html, &mut BTreeSet::new())?,
        &values,
        true,
    );
    if subject.trim().is_empty()
        || subject.len() > 998
        || subject.contains(['\r', '\n'])
        || text.len() > 262_144
        || html.len() > 524_288
    {
        return Err(RenderFailure::UnsafeVariable);
    }
    let template_digest = template_digest(definition);
    let content_digest = digest(&[&subject, &text, &html]);
    Ok(RenderedMessage {
        subject,
        text,
        html,
        required_variables,
        template_digest,
        content_digest,
    })
}

fn parse(source: &str, required: &mut BTreeSet<String>) -> Result<Vec<Node>, RenderFailure> {
    let mut offset = 0;
    let nodes = parse_until(source, &mut offset, None, required, 0)?;
    if offset != source.len() {
        return Err(RenderFailure::InvalidTemplate);
    }
    Ok(nodes)
}

fn parse_until(
    source: &str,
    offset: &mut usize,
    closing: Option<&str>,
    required: &mut BTreeSet<String>,
    depth: usize,
) -> Result<Vec<Node>, RenderFailure> {
    if depth > 16 {
        return Err(RenderFailure::InvalidTemplate);
    }
    let mut nodes = Vec::new();
    while *offset < source.len() {
        let Some(open_relative) = source[*offset..].find("{{") else {
            if closing.is_some() {
                return Err(RenderFailure::InvalidTemplate);
            }
            nodes.push(Node::Text(source[*offset..].to_owned()));
            *offset = source.len();
            break;
        };
        let open = *offset + open_relative;
        if open > *offset {
            nodes.push(Node::Text(source[*offset..open].to_owned()));
        }
        let token_start = open + 2;
        let Some(close_relative) = source[token_start..].find("}}") else {
            return Err(RenderFailure::InvalidTemplate);
        };
        let close = token_start + close_relative;
        let token = source[token_start..close].trim();
        *offset = close + 2;
        if let Some(name) = token.strip_prefix('/') {
            if closing == Some(name) && valid_variable_name(name) {
                return Ok(nodes);
            }
            return Err(RenderFailure::InvalidTemplate);
        }
        if let Some((inverted, name)) = token
            .strip_prefix('#')
            .map(|name| (false, name))
            .or_else(|| token.strip_prefix('^').map(|name| (true, name)))
        {
            if !valid_variable_name(name) {
                return Err(RenderFailure::InvalidTemplate);
            }
            required.insert(name.to_owned());
            let children = parse_until(source, offset, Some(name), required, depth + 1)?;
            nodes.push(Node::Section {
                name: name.to_owned(),
                inverted,
                children,
            });
            continue;
        }
        if !valid_variable_name(token) {
            return Err(RenderFailure::InvalidTemplate);
        }
        required.insert(token.to_owned());
        nodes.push(Node::Variable(token.to_owned()));
    }
    if closing.is_some() {
        Err(RenderFailure::InvalidTemplate)
    } else {
        Ok(nodes)
    }
}

fn render_nodes(nodes: &[Node], values: &BTreeMap<String, String>, html: bool) -> String {
    let mut output = String::new();
    for node in nodes {
        match node {
            Node::Text(value) => output.push_str(value),
            Node::Variable(name) => {
                let value = values.get(name).expect("validated complete variables");
                if html {
                    output.push_str(&escape_html(value));
                } else {
                    output.push_str(value);
                }
            }
            Node::Section {
                name,
                inverted,
                children,
            } => {
                let truthy = values
                    .get(name)
                    .is_some_and(|value| !value.trim().is_empty());
                if truthy != *inverted {
                    output.push_str(&render_nodes(children, values, html));
                }
            }
        }
    }
    output
}

fn valid_variable_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && value.len() <= 64
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn unsafe_html_literal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "<script",
        "</script",
        "javascript:",
        "data:text/html",
        "<iframe",
        "<object",
        "<embed",
        "<form",
        " onload=",
        " onclick=",
        " onerror=",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let bytes = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    format!("sha256:{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> TemplateDefinition {
        TemplateDefinition {
            subject: "Hello {{name}}".to_owned(),
            text: "{{#note}}{{note}}{{/note}}{{^note}}none{{/note}}".to_owned(),
            html: "<p>{{name}}</p>{{#url}}<a href=\"{{url}}\">Open</a>{{/url}}".to_owned(),
        }
    }

    #[test]
    fn sections_and_html_values_are_deterministic_and_escaped() {
        let rendered = render(
            &definition(),
            [
                ("name".to_owned(), "Ada <admin>".to_owned()),
                ("note".to_owned(), String::new()),
                ("url".to_owned(), String::new()),
            ],
        )
        .unwrap();
        assert_eq!(rendered.subject, "Hello Ada <admin>");
        assert_eq!(rendered.text, "none");
        assert!(rendered.html.contains("Ada &lt;admin&gt;"));
        assert!(!rendered.html.contains("<a"));
    }

    #[test]
    fn variables_are_exact_and_url_suffixes_are_scheme_checked() {
        let url_definition = TemplateDefinition {
            subject: "Open".to_owned(),
            text: "{{invitation_url}}".to_owned(),
            html: "<a href=\"{{invitation_url}}\">Open</a>".to_owned(),
        };
        assert_eq!(
            render(
                &url_definition,
                [(
                    "invitation_url".to_owned(),
                    "javascript:alert(1)".to_owned()
                )]
            ),
            Err(RenderFailure::UnsafeVariable)
        );
        assert_eq!(
            render(
                &url_definition,
                [
                    (
                        "invitation_url".to_owned(),
                        "https://example.test".to_owned()
                    ),
                    ("extra".to_owned(), "x".to_owned())
                ]
            ),
            Err(RenderFailure::UnexpectedVariable)
        );
    }

    #[test]
    fn nested_or_unclosed_sections_fail_closed() {
        let invalid = TemplateDefinition {
            subject: "Hello".to_owned(),
            text: "{{#name}}missing close".to_owned(),
            html: "<p>{{name}}</p>".to_owned(),
        };
        assert_eq!(
            validate_definition(&invalid),
            Err(RenderFailure::InvalidTemplate)
        );
    }
}
