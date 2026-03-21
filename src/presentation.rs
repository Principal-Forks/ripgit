use serde::Serialize;
use std::fmt;
use worker::{Request, Response, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Representation {
    Html,
    Json,
    Markdown,
}

impl Representation {
    pub fn format_value(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Json => "json",
            Self::Markdown => "md",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkdownMediaType {
    Markdown,
    Plain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegotiatedRepresentation {
    representation: Representation,
    markdown_media_type: MarkdownMediaType,
    vary_accept: bool,
}

impl NegotiatedRepresentation {
    pub fn representation(self) -> Representation {
        self.representation
    }

    pub fn query_format_value(self) -> &'static str {
        match (self.representation, self.markdown_media_type) {
            (Representation::Markdown, MarkdownMediaType::Plain) => "text",
            _ => self.representation.format_value(),
        }
    }

    pub fn vary_accept(self) -> bool {
        self.vary_accept
    }

    pub fn response_content_type(self) -> &'static str {
        match self.representation {
            Representation::Html => "text/html; charset=utf-8",
            Representation::Json => "application/json; charset=utf-8",
            Representation::Markdown => match self.markdown_media_type {
                MarkdownMediaType::Markdown => "text/markdown; charset=utf-8",
                MarkdownMediaType::Plain => "text/plain; charset=utf-8",
            },
        }
    }

    pub fn preferred_accept_value(self) -> &'static str {
        match self.representation {
            Representation::Html => "text/html",
            Representation::Json => "application/json",
            Representation::Markdown => match self.markdown_media_type {
                MarkdownMediaType::Markdown => "text/markdown",
                MarkdownMediaType::Plain => "text/plain",
            },
        }
    }

    fn new(representation: Representation) -> Self {
        Self {
            representation,
            markdown_media_type: MarkdownMediaType::Markdown,
            vary_accept: false,
        }
    }
}

#[derive(Debug)]
pub enum NegotiationError {
    UnsupportedFormat {
        requested: String,
        supported: Vec<Representation>,
    },
    NotAcceptable {
        accept: String,
        supported: Vec<Representation>,
    },
}

impl NegotiationError {
    pub fn into_response(self) -> Result<Response> {
        match self {
            Self::UnsupportedFormat {
                requested,
                supported,
            } => Response::error(
                format!(
                    "Requested format '{requested}' is not available here. Supported formats: {}",
                    supported_formats(&supported)
                ),
                406,
            ),
            Self::NotAcceptable { accept, supported } => {
                let mut resp = Response::error(
                    format!(
                        "Accept '{accept}' is not available here. Supported formats: {}",
                        supported_formats(&supported)
                    ),
                    406,
                )?;
                add_vary_accept(&mut resp)?;
                Ok(resp)
            }
        }
    }
}

pub fn preferred_representation(
    req: &Request,
    supported: &[Representation],
    default: Representation,
) -> std::result::Result<NegotiatedRepresentation, NegotiationError> {
    let default = if supported.contains(&default) {
        default
    } else {
        supported.first().copied().unwrap_or(default)
    };

    let url = req.url().map_err(|_| NegotiationError::NotAcceptable {
        accept: "invalid URL".to_string(),
        supported: supported.to_vec(),
    })?;

    if let Some(raw_format) = url
        .query_pairs()
        .find(|(k, _)| k == "format")
        .map(|(_, v)| v.to_string())
    {
        let mut selection =
            parse_query_format(&raw_format).ok_or_else(|| NegotiationError::UnsupportedFormat {
                requested: raw_format.clone(),
                supported: supported.to_vec(),
            })?;

        if !supported.contains(&selection.representation) {
            return Err(NegotiationError::UnsupportedFormat {
                requested: raw_format,
                supported: supported.to_vec(),
            });
        }

        selection.vary_accept = false;
        return Ok(selection);
    }

    let accept = req.headers().get("Accept").ok().flatten();
    let accept = match accept {
        Some(value) if !value.trim().is_empty() => value,
        _ => return Ok(NegotiatedRepresentation::new(default)),
    };

    let tokens = parse_accept(&accept);
    let selection = best_accept_match(&tokens, supported, default).ok_or_else(|| {
        NegotiationError::NotAcceptable {
            accept,
            supported: supported.to_vec(),
        }
    })?;

    Ok(selection)
}

pub fn finalize_response(
    mut resp: Response,
    selection: &NegotiatedRepresentation,
) -> Result<Response> {
    if selection.vary_accept() {
        add_vary_accept(&mut resp)?;
    }
    Ok(resp)
}

pub fn markdown_response(body: &str, selection: &NegotiatedRepresentation) -> Result<Response> {
    let mut resp = Response::ok(body.to_string())?;
    resp.headers_mut()
        .set("Content-Type", selection.response_content_type())?;
    resp.headers_mut().set("Cache-Control", "no-cache")?;
    finalize_response(resp, selection)
}

pub fn append_format(path: &str, representation: Representation) -> String {
    append_format_value(path, representation.format_value())
}

#[allow(dead_code)]
pub fn append_negotiated_format(path: &str, selection: NegotiatedRepresentation) -> String {
    append_format_value(path, selection.query_format_value())
}

fn append_format_value(path: &str, format_value: &str) -> String {
    let (path_without_fragment, fragment) = match path.split_once('#') {
        Some((before, after)) => (before, Some(after)),
        None => (path, None),
    };

    let (base, query) = match path_without_fragment.split_once('?') {
        Some((before, after)) => (before, Some(after)),
        None => (path_without_fragment, None),
    };

    let mut params: Vec<String> = query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter_map(|segment| {
            if segment.is_empty() {
                return None;
            }
            let key = segment.split('=').next().unwrap_or("");
            if key == "format" {
                return None;
            }
            Some(segment.to_string())
        })
        .collect();
    params.push(format!("format={}", format_value));

    let mut output = String::from(base);
    output.push('?');
    output.push_str(&params.join("&"));
    if let Some(fragment) = fragment {
        output.push('#');
        output.push_str(fragment);
    }
    output
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ActionMethod {
    Get,
    Post,
    Put,
    Delete,
}

impl fmt::Display for ActionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        };
        f.write_str(value)
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
pub struct ActionField {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[allow(dead_code)]
impl ActionField {
    pub fn required(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: true,
            description: Some(description.into()),
        }
    }

    pub fn optional(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: false,
            description: Some(description.into()),
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
pub struct Action {
    pub method: ActionMethod,
    pub path: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<ActionField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
}

#[allow(dead_code)]
impl Action {
    pub fn new(
        method: ActionMethod,
        path: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            description: description.into(),
            fields: Vec::new(),
            requires: None,
            effect: None,
        }
    }

    pub fn get(path: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(ActionMethod::Get, path, description)
    }

    pub fn post(path: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(ActionMethod::Post, path, description)
    }

    pub fn put(path: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(ActionMethod::Put, path, description)
    }

    pub fn delete(path: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(ActionMethod::Delete, path, description)
    }

    pub fn with_fields(mut self, fields: Vec<ActionField>) -> Self {
        self.fields = fields;
        self
    }

    pub fn with_requires(mut self, requires: impl Into<String>) -> Self {
        self.requires = Some(requires.into());
        self
    }

    pub fn with_effect(mut self, effect: impl Into<String>) -> Self {
        self.effect = Some(effect.into());
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Hint {
    pub text: String,
}

impl Hint {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

pub fn text_navigation_hint(selection: NegotiatedRepresentation) -> Hint {
    Hint::new(format!(
        "GET paths below omit `?format`. Keep `Accept: {}` to stay in this text view, or append `?format={}` when following a path without headers.",
        selection.preferred_accept_value(),
        selection.query_format_value(),
    ))
}

pub fn render_actions_section(actions: &[Action]) -> String {
    if actions.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n\n## Actions\n");
    for action in actions {
        section.push_str(&render_action_line(action));
        section.push('\n');
    }
    section
}

pub fn render_hints_section(hints: &[Hint]) -> String {
    if hints.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n## Hints\n");
    for hint in hints {
        section.push_str("- ");
        section.push_str(&hint.text);
        section.push('\n');
    }
    section
}

#[derive(Clone, Debug)]
struct AcceptToken {
    media_type: String,
    q: u16,
}

#[derive(Clone, Copy, Debug)]
struct AcceptCandidate {
    representation: Representation,
    markdown_media_type: MarkdownMediaType,
    q: u16,
    specificity: u8,
    token_index: usize,
    supported_index: usize,
    is_default: bool,
}

fn best_accept_match(
    tokens: &[AcceptToken],
    supported: &[Representation],
    default: Representation,
) -> Option<NegotiatedRepresentation> {
    let mut best: Option<AcceptCandidate> = None;

    for (supported_index, representation) in supported.iter().copied().enumerate() {
        let Some((q, specificity, token_index, markdown_media_type)) =
            best_token_for_representation(representation, tokens)
        else {
            continue;
        };

        let candidate = AcceptCandidate {
            representation,
            markdown_media_type,
            q,
            specificity,
            token_index,
            supported_index,
            is_default: representation == default,
        };

        let replace = match best {
            Some(current) => is_better_accept_candidate(candidate, current),
            None => true,
        };
        if replace {
            best = Some(candidate);
        }
    }

    best.map(|candidate| NegotiatedRepresentation {
        representation: candidate.representation,
        markdown_media_type: candidate.markdown_media_type,
        vary_accept: true,
    })
}

fn best_token_for_representation(
    representation: Representation,
    tokens: &[AcceptToken],
) -> Option<(u16, u8, usize, MarkdownMediaType)> {
    let mut best: Option<(u16, u8, usize, MarkdownMediaType)> = None;

    for (token_index, token) in tokens.iter().enumerate() {
        if token.q == 0 {
            continue;
        }

        let Some((specificity, markdown_media_type)) =
            match_representation(representation, &token.media_type)
        else {
            continue;
        };

        let candidate = (token.q, specificity, token_index, markdown_media_type);
        let replace = match best {
            Some(current) => {
                candidate.0 > current.0
                    || (candidate.0 == current.0 && candidate.1 > current.1)
                    || (candidate.0 == current.0
                        && candidate.1 == current.1
                        && candidate.2 < current.2)
            }
            None => true,
        };
        if replace {
            best = Some(candidate);
        }
    }

    best
}

fn is_better_accept_candidate(candidate: AcceptCandidate, current: AcceptCandidate) -> bool {
    candidate.q > current.q
        || (candidate.q == current.q && candidate.specificity > current.specificity)
        || (candidate.q == current.q
            && candidate.specificity == current.specificity
            && candidate.token_index < current.token_index)
        || (candidate.q == current.q
            && candidate.specificity == current.specificity
            && candidate.token_index == current.token_index
            && candidate.is_default
            && !current.is_default)
        || (candidate.q == current.q
            && candidate.specificity == current.specificity
            && candidate.token_index == current.token_index
            && candidate.is_default == current.is_default
            && candidate.supported_index < current.supported_index)
}

fn parse_query_format(value: &str) -> Option<NegotiatedRepresentation> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "html" => Some(NegotiatedRepresentation::new(Representation::Html)),
        "json" => Some(NegotiatedRepresentation::new(Representation::Json)),
        "md" | "markdown" => Some(NegotiatedRepresentation::new(Representation::Markdown)),
        "text" | "txt" | "plain" => Some(NegotiatedRepresentation {
            representation: Representation::Markdown,
            markdown_media_type: MarkdownMediaType::Plain,
            vary_accept: false,
        }),
        _ => None,
    }
}

fn parse_accept(header: &str) -> Vec<AcceptToken> {
    header
        .split(',')
        .filter_map(|raw| {
            let mut parts = raw.split(';');
            let media_type = parts.next()?.trim().to_ascii_lowercase();
            if media_type.is_empty() {
                return None;
            }

            let mut q = 1000;
            for param in parts {
                let Some((key, value)) = param.split_once('=') else {
                    continue;
                };
                if key.trim().eq_ignore_ascii_case("q") {
                    q = parse_quality(value.trim()).unwrap_or(0);
                }
            }

            Some(AcceptToken { media_type, q })
        })
        .collect()
}

fn parse_quality(value: &str) -> Option<u16> {
    let parsed = value.parse::<f32>().ok()?;
    if !(0.0..=1.0).contains(&parsed) {
        return None;
    }
    Some((parsed * 1000.0).round() as u16)
}

fn match_representation(
    representation: Representation,
    media_type: &str,
) -> Option<(u8, MarkdownMediaType)> {
    match representation {
        Representation::Html => match media_type {
            "text/html" => Some((3, MarkdownMediaType::Markdown)),
            "text/*" => Some((1, MarkdownMediaType::Markdown)),
            "*/*" => Some((0, MarkdownMediaType::Markdown)),
            _ => None,
        },
        Representation::Json => match media_type {
            "application/json" => Some((3, MarkdownMediaType::Markdown)),
            "application/*" => Some((1, MarkdownMediaType::Markdown)),
            "*/*" => Some((0, MarkdownMediaType::Markdown)),
            _ => None,
        },
        Representation::Markdown => match media_type {
            "text/markdown" => Some((3, MarkdownMediaType::Markdown)),
            "text/plain" => Some((3, MarkdownMediaType::Plain)),
            "text/*" => Some((1, MarkdownMediaType::Markdown)),
            "*/*" => Some((0, MarkdownMediaType::Markdown)),
            _ => None,
        },
    }
}

fn render_action_line(action: &Action) -> String {
    let mut line = format!(
        "- {} `{}` - {}",
        action.method, action.path, action.description
    );

    if !action.fields.is_empty() {
        line.push_str("; fields: ");
        for (idx, field) in action.fields.iter().enumerate() {
            if idx > 0 {
                line.push_str(", ");
            }
            line.push('`');
            line.push_str(&field.name);
            line.push('`');
            if !field.required {
                line.push_str(" (optional)");
            }
            if let Some(description) = &field.description {
                line.push_str(" - ");
                line.push_str(description);
            }
        }
    }

    if let Some(requires) = &action.requires {
        line.push_str("; requires ");
        line.push_str(requires);
    }
    if let Some(effect) = &action.effect {
        line.push_str("; ");
        line.push_str(effect);
    }

    line
}

fn supported_formats(formats: &[Representation]) -> String {
    formats
        .iter()
        .map(|format| format.format_value())
        .collect::<Vec<_>>()
        .join(", ")
}

fn add_vary_accept(resp: &mut Response) -> Result<()> {
    let current = resp.headers().get("Vary")?.unwrap_or_default();
    if current
        .split(',')
        .any(|value| value.trim().eq_ignore_ascii_case("Accept"))
    {
        return Ok(());
    }

    let next = if current.trim().is_empty() {
        "Accept".to_string()
    } else {
        format!("{}, Accept", current)
    };
    resp.headers_mut().set("Vary", &next)?;
    Ok(())
}

fn is_false(value: &bool) -> bool {
    !*value
}
