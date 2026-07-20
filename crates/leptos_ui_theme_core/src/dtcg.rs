use crate::{
    JsonPointer, Limits, LogicalPath, ProvenanceEntry, ProvenanceOperation, ThemeError, TokenPath,
    validate_color_syntax,
};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DtcgType {
    #[serde(rename = "color")]
    Color,
    #[serde(rename = "dimension")]
    Dimension,
    #[serde(rename = "fontFamily")]
    FontFamily,
    #[serde(rename = "fontWeight")]
    FontWeight,
    #[serde(rename = "duration")]
    Duration,
    #[serde(rename = "cubicBezier")]
    CubicBezier,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "strokeStyle")]
    StrokeStyle,
    #[serde(rename = "border")]
    Border,
    #[serde(rename = "transition")]
    Transition,
    #[serde(rename = "shadow")]
    Shadow,
    #[serde(rename = "gradient")]
    Gradient,
    #[serde(rename = "typography")]
    Typography,
}

impl DtcgType {
    pub fn parse(value: &str) -> Result<Self, ThemeError> {
        serde_json::from_value(Value::String(value.to_owned()))
            .map_err(|_| ThemeError::Resolution(format!("unsupported DTCG token type `{value}`")))
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Dimension => "dimension",
            Self::FontFamily => "fontFamily",
            Self::FontWeight => "fontWeight",
            Self::Duration => "duration",
            Self::CubicBezier => "cubicBezier",
            Self::Number => "number",
            Self::StrokeStyle => "strokeStyle",
            Self::Border => "border",
            Self::Transition => "transition",
            Self::Shadow => "shadow",
            Self::Gradient => "gradient",
            Self::Typography => "typography",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum DtcgDeprecation {
    Boolean(bool),
    Message(String),
}

#[derive(Clone, Debug)]
pub struct DtcgToken {
    pub path: TokenPath,
    pub token_type: DtcgType,
    pub value: Value,
    pub description: Option<String>,
    pub deprecated: Option<DtcgDeprecation>,
    pub extensions: Map<String, Value>,
    pub source_path: LogicalPath,
    pub pointer: JsonPointer,
}

impl DtcgToken {
    #[must_use]
    pub fn provenance(&self) -> ProvenanceEntry {
        ProvenanceEntry {
            path: self.source_path.clone(),
            pointer: self.pointer.clone(),
            operation: ProvenanceOperation::Source,
            value: self.value.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DtcgGroup {
    pub path: Option<TokenPath>,
    pub declared_type: Option<DtcgType>,
    pub description: Option<String>,
    pub deprecated: Option<DtcgDeprecation>,
    pub extensions: Map<String, Value>,
    pub definitions: Option<Map<String, Value>>,
    pub children: Vec<DtcgNode>,
}

#[derive(Clone, Debug)]
pub enum DtcgNode {
    Group(DtcgGroup),
    Token(DtcgToken),
}

#[derive(Clone, Debug)]
pub struct DtcgDocument {
    pub source_path: LogicalPath,
    pub root: DtcgGroup,
    pub tokens: Vec<DtcgToken>,
}

impl DtcgDocument {
    pub fn parse(
        source_path: LogicalPath,
        bytes: &[u8],
        limits: &Limits,
    ) -> Result<Self, ThemeError> {
        let value = parse_json_strict(bytes, limits.json_depth)?;
        Self::from_value(source_path, &value, limits)
    }

    pub fn from_value(
        source_path: LogicalPath,
        value: &Value,
        limits: &Limits,
    ) -> Result<Self, ThemeError> {
        let expanded = expand_group_extends(value)?;
        let object = expanded
            .as_object()
            .ok_or_else(|| ThemeError::Resolution("a DTCG document must be an object".into()))?;
        if object.contains_key("$root") || object.contains_key("$value") {
            return Err(ThemeError::Resolution(
                "a DTCG document root cannot be a token".into(),
            ));
        }
        let root = parse_group(&source_path, object, None, None, "")?;
        let mut tokens = Vec::new();
        collect_tokens(&root, &mut tokens);
        if tokens.len() > limits.tokens as usize {
            return Err(ThemeError::Limit {
                resource: "tokens",
                limit: u64::from(limits.tokens),
                observed: tokens.len() as u64,
            });
        }
        Ok(Self {
            source_path,
            root,
            tokens,
        })
    }

    #[must_use]
    pub fn token(&self, path: &TokenPath) -> Option<&DtcgToken> {
        self.tokens.iter().find(|token| token.path == *path)
    }
}

fn parse_group(
    source_path: &LogicalPath,
    object: &Map<String, Value>,
    path: Option<TokenPath>,
    inherited_type: Option<DtcgType>,
    pointer: &str,
) -> Result<DtcgGroup, ThemeError> {
    validate_reserved_members(object, false)?;
    let declared_type = parse_optional_type(object.get("$type"))?;
    let effective_type = declared_type.or(inherited_type);
    let description = parse_description(object.get("$description"))?;
    let deprecated = parse_deprecation(object.get("$deprecated"))?;
    let extensions = parse_extensions(object.get("$extensions"))?;
    let definitions = object
        .get("$defs")
        .map(|value| {
            value
                .as_object()
                .cloned()
                .ok_or_else(|| ThemeError::Resolution("$defs must be an object".into()))
        })
        .transpose()?;
    let mut children = Vec::new();

    if let Some(root) = object.get("$root") {
        let token_path = path.clone().ok_or_else(|| {
            ThemeError::Resolution("a document root cannot declare a $root token".into())
        })?;
        let root = root.as_object().ok_or_else(|| {
            ThemeError::Resolution(format!("token `{token_path}` must be an object"))
        })?;
        children.push(DtcgNode::Token(parse_token(
            source_path,
            token_path,
            root,
            effective_type,
            &format!("{pointer}/$root"),
        )?));
    }

    for (name, child) in object.iter().filter(|(name, _)| !name.starts_with('$')) {
        validate_node_name(name)?;
        let child_object = child.as_object().ok_or_else(|| {
            ThemeError::Resolution(format!("DTCG member `{name}` must be an object"))
        })?;
        let child_path = join_token_path(path.as_ref(), name)?;
        let child_pointer = format!("{pointer}/{}", pointer_escape(name));
        if child_object.contains_key("$value") {
            children.push(DtcgNode::Token(parse_token(
                source_path,
                child_path,
                child_object,
                effective_type,
                &child_pointer,
            )?));
        } else {
            children.push(DtcgNode::Group(parse_group(
                source_path,
                child_object,
                Some(child_path),
                effective_type,
                &child_pointer,
            )?));
        }
    }

    Ok(DtcgGroup {
        path,
        declared_type,
        description,
        deprecated,
        extensions,
        definitions,
        children,
    })
}

fn parse_token(
    source_path: &LogicalPath,
    path: TokenPath,
    object: &Map<String, Value>,
    inherited_type: Option<DtcgType>,
    pointer: &str,
) -> Result<DtcgToken, ThemeError> {
    validate_reserved_members(object, true)?;
    if object.keys().any(|name| !name.starts_with('$')) {
        return Err(ThemeError::Resolution(format!(
            "token `{path}` cannot contain child members"
        )));
    }
    let value = object
        .get("$value")
        .ok_or_else(|| ThemeError::Resolution(format!("token `{path}` has no $value")))?;
    let token_type = parse_optional_type(object.get("$type"))?
        .or(inherited_type)
        .ok_or_else(|| ThemeError::Resolution(format!("token `{path}` has no effective type")))?;
    validate_token_value(token_type, value)?;
    Ok(DtcgToken {
        path,
        token_type,
        value: value.clone(),
        description: parse_description(object.get("$description"))?,
        deprecated: parse_deprecation(object.get("$deprecated"))?,
        extensions: parse_extensions(object.get("$extensions"))?,
        source_path: source_path.clone(),
        pointer: JsonPointer::new(format!("{pointer}/$value"))?,
    })
}

fn parse_optional_type(value: Option<&Value>) -> Result<Option<DtcgType>, ThemeError> {
    value
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| ThemeError::Resolution("$type must be a string".into()))
                .and_then(DtcgType::parse)
        })
        .transpose()
}

fn parse_description(value: Option<&Value>) -> Result<Option<String>, ThemeError> {
    value
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ThemeError::Resolution("$description must be a string".into()))
        })
        .transpose()
}

fn parse_deprecation(value: Option<&Value>) -> Result<Option<DtcgDeprecation>, ThemeError> {
    value
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|_| {
                ThemeError::Resolution("$deprecated must be boolean or a nonempty string".into())
            })
        })
        .transpose()
        .and_then(|deprecated| {
            if matches!(&deprecated, Some(DtcgDeprecation::Message(message)) if message.is_empty())
            {
                Err(ThemeError::Resolution(
                    "$deprecated string cannot be empty".into(),
                ))
            } else {
                Ok(deprecated)
            }
        })
}

fn parse_extensions(value: Option<&Value>) -> Result<Map<String, Value>, ThemeError> {
    validate_extensions(value)?;
    Ok(value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

fn validate_node_name(name: &str) -> Result<(), ThemeError> {
    if name.is_empty()
        || name.starts_with('$')
        || name.contains(['.', '{', '}', '/', '\\'])
        || name.chars().any(|character| character == '\0')
    {
        Err(ThemeError::Resolution(format!(
            "invalid DTCG token or group name `{name}`"
        )))
    } else {
        Ok(())
    }
}

fn join_token_path(parent: Option<&TokenPath>, name: &str) -> Result<TokenPath, ThemeError> {
    TokenPath::new(parent.map_or_else(
        || name.to_owned(),
        |parent| format!("{}.{name}", parent.as_str()),
    ))
}

fn collect_tokens(group: &DtcgGroup, output: &mut Vec<DtcgToken>) {
    for node in &group.children {
        match node {
            DtcgNode::Group(group) => collect_tokens(group, output),
            DtcgNode::Token(token) => output.push(token.clone()),
        }
    }
}

pub fn apply_shallow_reference_overrides(
    target: &Value,
    reference: &Map<String, Value>,
) -> Result<Value, ThemeError> {
    if !reference.get("$ref").is_some_and(Value::is_string) {
        return Err(ThemeError::Resolution(
            "reference object requires a string $ref".into(),
        ));
    }
    let mut merged = target
        .as_object()
        .cloned()
        .ok_or_else(|| ThemeError::Resolution("reference target must be an object".into()))?;
    for (name, value) in reference {
        if name != "$ref" {
            merged.insert(name.clone(), value.clone());
        }
    }
    Ok(Value::Object(merged))
}

pub fn parse_json_strict(bytes: &[u8], maximum_depth: u32) -> Result<Value, ThemeError> {
    if maximum_depth == 0 {
        return Err(ThemeError::Config(
            "JSON maximum depth must be positive".into(),
        ));
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValueSeed {
        parent_depth: 0,
        maximum_depth,
    }
    .deserialize(&mut deserializer)
    .map_err(|error| ThemeError::Resolution(format!("invalid strict JSON: {error}")))?;
    deserializer
        .end()
        .map_err(|error| ThemeError::Resolution(format!("invalid strict JSON: {error}")))?;
    Ok(value)
}

struct StrictValueSeed {
    parent_depth: u32,
    maximum_depth: u32,
}

impl<'de> DeserializeSeed<'de> for StrictValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor {
            parent_depth: self.parent_depth,
            maximum_depth: self.maximum_depth,
        })
    }
}

struct StrictValueVisitor {
    parent_depth: u32,
    maximum_depth: u32,
}

impl StrictValueVisitor {
    fn container_depth<E: serde::de::Error>(&self) -> Result<u32, E> {
        let depth = self.parent_depth.saturating_add(1);
        if depth > self.maximum_depth {
            Err(E::custom(format!(
                "JSON depth {depth} exceeds {}",
                self.maximum_depth
            )))
        } else {
            Ok(depth)
        }
    }
}

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("JSON number must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let depth = self.container_depth()?;
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(1_024));
        while let Some(value) = sequence.next_element_seed(StrictValueSeed {
            parent_depth: depth,
            maximum_depth: self.maximum_depth,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let depth = self.container_depth()?;
        let mut values = Map::new();
        while let Some(name) = entries.next_key::<String>()? {
            if values.contains_key(&name) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object member `{name}`"
                )));
            }
            let value = entries.next_value_seed(StrictValueSeed {
                parent_depth: depth,
                maximum_depth: self.maximum_depth,
            })?;
            values.insert(name, value);
        }
        Ok(Value::Object(values))
    }
}

pub fn alias_target(value: &Value) -> Result<Option<TokenPath>, ThemeError> {
    let Some(value) = value.as_str() else {
        return Ok(None);
    };
    if !value.starts_with('{') && !value.ends_with('}') {
        return Ok(None);
    }
    let target = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .ok_or_else(|| ThemeError::Resolution(format!("malformed token alias `{value}`")))?;
    TokenPath::new(target.to_owned()).map(Some)
}

pub fn expand_group_extends(document: &Value) -> Result<Value, ThemeError> {
    let root = document
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("a DTCG document must be an object".into()))?;
    let mut stack = Vec::new();
    expand_group(root, root, "#", &mut stack).map(Value::Object)
}

fn expand_group(
    root: &Map<String, Value>,
    group: &Map<String, Value>,
    location: &str,
    stack: &mut Vec<String>,
) -> Result<Map<String, Value>, ThemeError> {
    if group.contains_key("$value") {
        return Err(ThemeError::Resolution(format!(
            "`{location}` is a token, not a group"
        )));
    }
    validate_reserved_members(group, false)?;
    if stack.iter().any(|entry| entry == location) {
        return Err(ThemeError::Resolution(format!(
            "group extension cycle at `{location}`"
        )));
    }
    stack.push(location.to_owned());

    let mut expanded = if let Some(reference) = group.get("$extends") {
        let reference = group_reference(reference)?;
        let target = group_at(root, &reference)?;
        expand_group(root, target, &reference, stack)?
    } else {
        Map::new()
    };
    for (name, value) in group {
        if name != "$extends" {
            expanded.insert(name.clone(), value.clone());
        }
    }

    let child_names = expanded
        .iter()
        .filter(|(name, value)| {
            !name.starts_with('$')
                && value
                    .as_object()
                    .is_some_and(|object| !object.contains_key("$value"))
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for name in child_names {
        let child = expanded
            .get(&name)
            .and_then(Value::as_object)
            .ok_or_else(|| ThemeError::Resolution("group expansion changed shape".into()))?;
        let child_location = if location == "#" {
            format!("#/{}", pointer_escape(&name))
        } else {
            format!("{location}/{}", pointer_escape(&name))
        };
        let child = expand_group(root, child, &child_location, stack)?;
        expanded.insert(name, Value::Object(child));
    }

    let removed = stack.pop();
    debug_assert_eq!(removed.as_deref(), Some(location));
    Ok(expanded)
}

fn group_reference(value: &Value) -> Result<String, ThemeError> {
    let value = value
        .as_str()
        .ok_or_else(|| ThemeError::Resolution("$extends must be a group reference".into()))?;
    if value.starts_with("#/") {
        return Ok(value.to_owned());
    }
    let path = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .ok_or_else(|| ThemeError::Resolution(format!("invalid group reference `{value}`")))?;
    let path = TokenPath::new(path.to_owned())?;
    Ok(format!(
        "#/{}",
        path.as_str()
            .split('.')
            .map(pointer_escape)
            .collect::<Vec<_>>()
            .join("/")
    ))
}

fn group_at<'a>(
    root: &'a Map<String, Value>,
    reference: &str,
) -> Result<&'a Map<String, Value>, ThemeError> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| ThemeError::Resolution(format!("invalid group reference `{reference}`")))?;
    let mut current = root;
    let segments = pointer
        .strip_prefix('/')
        .ok_or_else(|| ThemeError::Resolution(format!("invalid group reference `{reference}`")))?;
    let segments = segments.split('/').collect::<Vec<_>>();
    for (index, segment) in segments.iter().enumerate() {
        let segment = pointer_unescape(segment)?;
        let child = current.get(&segment).ok_or_else(|| {
            ThemeError::Resolution(format!("unknown group reference `{reference}`"))
        })?;
        let object = child.as_object().ok_or_else(|| {
            ThemeError::Resolution(format!("group reference `{reference}` is not an object"))
        })?;
        if index + 1 == segments.len() {
            if object.contains_key("$value") {
                return Err(ThemeError::Resolution(format!(
                    "group reference `{reference}` targets a token"
                )));
            }
            return Ok(object);
        }
        current = object;
    }
    Err(ThemeError::Resolution(format!(
        "invalid group reference `{reference}`"
    )))
}

fn pointer_escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn pointer_unescape(value: &str) -> Result<String, ThemeError> {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('0') => output.push('~'),
            Some('1') => output.push('/'),
            _ => {
                return Err(ThemeError::Resolution(
                    "group reference contains an invalid JSON Pointer escape".into(),
                ));
            }
        }
    }
    Ok(output)
}

pub fn validate_token_value(token_type: DtcgType, value: &Value) -> Result<(), ThemeError> {
    if alias_target(value)?.is_some() {
        return Ok(());
    }
    match token_type {
        DtcgType::Color => {
            validate_color_syntax(value)?;
        }
        DtcgType::Dimension => validate_unit_value(value, &["px", "rem"], false)?,
        DtcgType::Duration => validate_unit_value(value, &["ms", "s"], true)?,
        DtcgType::Number => {
            finite(value, "number")?;
        }
        DtcgType::CubicBezier => {
            let values = array(value, 4, 4, "cubicBezier")?;
            for (index, value) in values.iter().enumerate() {
                let value = finite(value, "cubicBezier component")?;
                if matches!(index, 0 | 2) && !(0.0..=1.0).contains(&value) {
                    return Err(ThemeError::Resolution(
                        "cubicBezier x components must be within 0..1".into(),
                    ));
                }
            }
        }
        DtcgType::FontFamily => validate_font_family(value)?,
        DtcgType::FontWeight => {
            let weight = finite(value, "fontWeight")?;
            if !(1.0..=1_000.0).contains(&weight) {
                return Err(ThemeError::Resolution(
                    "fontWeight must be within 1..1000".into(),
                ));
            }
        }
        DtcgType::StrokeStyle => validate_stroke_style(value)?,
        DtcgType::Border => validate_border(value)?,
        DtcgType::Transition => validate_transition(value)?,
        DtcgType::Shadow => validate_shadow(value)?,
        DtcgType::Gradient => validate_gradient(value)?,
        DtcgType::Typography => validate_typography(value)?,
    }
    Ok(())
}

pub fn validate_extensions(value: Option<&Value>) -> Result<(), ThemeError> {
    let Some(value) = value else {
        return Ok(());
    };
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("$extensions must be an object".into()))?;
    for name in object.keys() {
        if !valid_extension_namespace(name) {
            return Err(ThemeError::Resolution(format!(
                "invalid extension namespace `{name}`"
            )));
        }
    }
    Ok(())
}

fn valid_extension_namespace(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').count() >= 2
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

pub fn validate_reserved_members(
    object: &Map<String, Value>,
    token: bool,
) -> Result<(), ThemeError> {
    let token_members = [
        "$value",
        "$type",
        "$description",
        "$deprecated",
        "$extensions",
    ];
    let group_members = [
        "$type",
        "$description",
        "$deprecated",
        "$extensions",
        "$defs",
        "$root",
        "$extends",
    ];
    let allowed = if token {
        token_members.as_slice()
    } else {
        group_members.as_slice()
    };
    for name in object.keys().filter(|name| name.starts_with('$')) {
        if !allowed.contains(&name.as_str()) {
            return Err(ThemeError::Resolution(format!(
                "reserved member `{name}` is not valid at this location"
            )));
        }
    }
    if object
        .get("$description")
        .is_some_and(|value| !value.is_string())
    {
        return Err(ThemeError::Resolution(
            "$description must be a string".into(),
        ));
    }
    if object.get("$deprecated").is_some_and(|value| {
        !value.is_boolean() && !value.as_str().is_some_and(|value| !value.is_empty())
    }) {
        return Err(ThemeError::Resolution(
            "$deprecated must be true or a nonempty string".into(),
        ));
    }
    validate_extensions(object.get("$extensions"))?;
    if !token && object.get("$defs").is_some_and(|value| !value.is_object()) {
        return Err(ThemeError::Resolution("$defs must be an object".into()));
    }
    Ok(())
}

fn validate_unit_value(value: &Value, units: &[&str], nonnegative: bool) -> Result<(), ThemeError> {
    let object = exact_object(value, &["value", "unit"], "unit value")?;
    let number = finite(&object["value"], "unit value")?;
    if nonnegative && number < 0.0 {
        return Err(ThemeError::Resolution("duration cannot be negative".into()));
    }
    let unit = object["unit"]
        .as_str()
        .filter(|unit| units.contains(unit))
        .ok_or_else(|| ThemeError::Resolution("unsupported unit".into()))?;
    let _ = unit;
    Ok(())
}

fn validate_font_family(value: &Value) -> Result<(), ThemeError> {
    let families: Vec<&str> = if let Some(value) = value.as_str() {
        vec![value]
    } else {
        array(value, 1, 32, "fontFamily")?
            .iter()
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    ThemeError::Resolution("fontFamily entries must be strings".into())
                })
            })
            .collect::<Result<_, _>>()?
    };
    let mut unique = BTreeSet::new();
    for family in families {
        if family.is_empty()
            || family.len() > 255
            || !unique.insert(family)
            || family.chars().any(forbidden_text_scalar)
        {
            return Err(ThemeError::Resolution(
                "fontFamily contains an invalid or duplicate family".into(),
            ));
        }
    }
    Ok(())
}

fn forbidden_text_scalar(value: char) -> bool {
    value <= '\u{001f}'
        || ('\u{007f}'..='\u{009f}').contains(&value)
        || matches!(
            value,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn validate_stroke_style(value: &Value) -> Result<(), ThemeError> {
    if value.as_str().is_some_and(|value| {
        matches!(
            value,
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "outset" | "inset"
        )
    }) {
        return Ok(());
    }
    let object = exact_object(value, &["dashArray", "lineCap"], "strokeStyle")?;
    let dash = array(&object["dashArray"], 1, 32, "strokeStyle dashArray")?;
    for value in dash {
        validate_unit_value(value, &["px", "rem"], true)?;
    }
    if !object["lineCap"]
        .as_str()
        .is_some_and(|value| matches!(value, "round" | "butt" | "square"))
    {
        return Err(ThemeError::Resolution(
            "strokeStyle lineCap is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_border(value: &Value) -> Result<(), ThemeError> {
    let object = exact_object(value, &["color", "width", "style"], "border")?;
    validate_nested(DtcgType::Color, &object["color"])?;
    validate_nested(DtcgType::Dimension, &object["width"])?;
    validate_nested(DtcgType::StrokeStyle, &object["style"])
}

fn validate_transition(value: &Value) -> Result<(), ThemeError> {
    let object = exact_object(
        value,
        &["duration", "delay", "timingFunction"],
        "transition",
    )?;
    validate_nested(DtcgType::Duration, &object["duration"])?;
    validate_nested(DtcgType::Duration, &object["delay"])?;
    validate_nested(DtcgType::CubicBezier, &object["timingFunction"])
}

fn validate_shadow(value: &Value) -> Result<(), ThemeError> {
    if let Some(values) = value.as_array() {
        if values.is_empty() || values.len() > 32 {
            return Err(ThemeError::Resolution(
                "shadow array cardinality is invalid".into(),
            ));
        }
        for value in values {
            validate_shadow_entry(value)?;
        }
        Ok(())
    } else {
        validate_shadow_entry(value)
    }
}

fn validate_shadow_entry(value: &Value) -> Result<(), ThemeError> {
    if alias_target(value)?.is_some() {
        return Ok(());
    }
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("shadow must be an object".into()))?;
    exact_keys(
        object,
        &["color", "offsetX", "offsetY", "blur", "spread"],
        &["inset"],
        "shadow",
    )?;
    validate_nested(DtcgType::Color, &object["color"])?;
    for name in ["offsetX", "offsetY", "blur", "spread"] {
        validate_nested(DtcgType::Dimension, &object[name])?;
    }
    if alias_target(&object["blur"])?.is_none() {
        let blur = object["blur"]
            .get("value")
            .and_then(Value::as_f64)
            .ok_or_else(|| ThemeError::Resolution("shadow blur must be concrete".into()))?;
        if blur < 0.0 {
            return Err(ThemeError::Resolution(
                "shadow blur cannot be negative".into(),
            ));
        }
    }
    if object.get("inset").is_some_and(|value| !value.is_boolean()) {
        return Err(ThemeError::Resolution(
            "shadow inset must be boolean".into(),
        ));
    }
    Ok(())
}

fn validate_gradient(value: &Value) -> Result<(), ThemeError> {
    for stop in array(value, 2, 64, "gradient")? {
        let object = exact_object(stop, &["color", "position"], "gradient stop")?;
        validate_nested(DtcgType::Color, &object["color"])?;
        let position = finite(&object["position"], "gradient position")?;
        if !(0.0..=1.0).contains(&position) {
            return Err(ThemeError::Resolution(
                "gradient position must be within 0..1".into(),
            ));
        }
    }
    Ok(())
}

fn validate_typography(value: &Value) -> Result<(), ThemeError> {
    let object = exact_object(
        value,
        &[
            "fontFamily",
            "fontSize",
            "fontWeight",
            "letterSpacing",
            "lineHeight",
        ],
        "typography",
    )?;
    validate_nested(DtcgType::FontFamily, &object["fontFamily"])?;
    validate_nested(DtcgType::Dimension, &object["fontSize"])?;
    validate_nested(DtcgType::FontWeight, &object["fontWeight"])?;
    validate_nested(DtcgType::Dimension, &object["letterSpacing"])?;
    validate_nested(DtcgType::Number, &object["lineHeight"])
}

fn validate_nested(token_type: DtcgType, value: &Value) -> Result<(), ThemeError> {
    if alias_target(value)?.is_some() {
        Ok(())
    } else {
        validate_token_value(token_type, value)
    }
}

fn exact_object<'a>(
    value: &'a Value,
    required: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>, ThemeError> {
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution(format!("{label} must be an object")))?;
    exact_keys(object, required, &[], label)?;
    Ok(object)
}

fn exact_keys(
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    label: &str,
) -> Result<(), ThemeError> {
    if required.iter().any(|name| !object.contains_key(*name))
        || object
            .keys()
            .any(|name| !required.contains(&name.as_str()) && !optional.contains(&name.as_str()))
    {
        return Err(ThemeError::Resolution(format!(
            "{label} has missing or unknown members"
        )));
    }
    Ok(())
}

fn array<'a>(
    value: &'a Value,
    minimum: usize,
    maximum: usize,
    label: &str,
) -> Result<&'a [Value], ThemeError> {
    value
        .as_array()
        .filter(|values| (minimum..=maximum).contains(&values.len()))
        .map(Vec::as_slice)
        .ok_or_else(|| ThemeError::Resolution(format!("{label} array cardinality is invalid")))
}

fn finite(value: &Value, label: &str) -> Result<f64, ThemeError> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| ThemeError::Resolution(format!("{label} must be a finite number")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_extends_inherits_and_overrides_members() {
        let document = serde_json::json!({
            "base": {
                "$type": "color",
                "background": {"$value": "#000000"},
                "foreground": {"$value": "#ffffff"}
            },
            "brand": {
                "$extends": "{base}",
                "background": {"$value": "#112233"}
            }
        });
        let expanded = expand_group_extends(&document).expect("expand group");
        assert_eq!(
            expanded["brand"]["background"]["$value"],
            serde_json::json!("#112233")
        );
        assert_eq!(
            expanded["brand"]["foreground"]["$value"],
            serde_json::json!("#ffffff")
        );
        assert_eq!(expanded["brand"]["$type"], serde_json::json!("color"));
        assert!(expanded["brand"].get("$extends").is_none());
    }

    #[test]
    fn group_extension_cycles_fail() {
        let document = serde_json::json!({
            "a": {"$extends": "{b}"},
            "b": {"$extends": "{a}"}
        });
        assert!(expand_group_extends(&document).is_err());
    }

    #[test]
    fn strict_json_rejects_duplicates_and_depth_overflow() {
        assert!(parse_json_strict(br#"{"a":1,"a":2}"#, 8).is_err());
        assert!(parse_json_strict(br#"[[[]]]"#, 2).is_err());
        assert_eq!(
            parse_json_strict(br#"{"a":[1,true,null]}"#, 2).unwrap()["a"][0],
            1
        );
    }

    #[test]
    fn typed_document_preserves_format_metadata_and_provenance() {
        let source = LogicalPath::new("tokens/theme.tokens.json").unwrap();
        let document = DtcgDocument::parse(
            source,
            br##"{
                "$description":"root",
                "$defs":{"retained":{"anything":true}},
                "surface":{
                    "$type":"color",
                    "$description":"surfaces",
                    "$extensions":{"org.example.tokens":{"stable":true}},
                    "$root":{"$value":"#112233","$deprecated":"Use surface.base"},
                    "base":{"$value":"#ffffff"}
                }
            }"##,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            document
                .tokens
                .iter()
                .map(|token| token.path.as_str())
                .collect::<Vec<_>>(),
            ["surface", "surface.base"]
        );
        assert_eq!(document.tokens[0].token_type, DtcgType::Color);
        assert_eq!(document.tokens[0].pointer.as_str(), "/surface/$root/$value");
        assert_eq!(
            document.tokens[0].provenance().operation,
            ProvenanceOperation::Source
        );
        assert!(document.root.definitions.is_some());
        let DtcgNode::Group(surface) = &document.root.children[0] else {
            panic!("surface is a group");
        };
        assert!(surface.extensions.contains_key("org.example.tokens"));
    }

    #[test]
    fn ambiguous_or_untyped_format_nodes_fail() {
        let source = LogicalPath::new("tokens/invalid.json").unwrap();
        for value in [
            serde_json::json!({"token":{"$value":1,"child":{"$value":2},"$type":"number"}}),
            serde_json::json!({"token":{"$value":1}}),
            serde_json::json!({"$root":{"$value":1,"$type":"number"}}),
            serde_json::json!({"token":{"$value":1,"$type":"number","$extensions":{"Not.Valid":{}}}}),
        ] {
            assert!(DtcgDocument::from_value(source.clone(), &value, &Limits::default()).is_err());
        }
    }

    #[test]
    fn every_2025_10_type_is_recognized_and_shape_validated() {
        let fixtures = [
            (DtcgType::Color, serde_json::json!("#112233")),
            (
                DtcgType::Dimension,
                serde_json::json!({"value": 1, "unit": "rem"}),
            ),
            (
                DtcgType::FontFamily,
                serde_json::json!(["Inter", "sans-serif"]),
            ),
            (DtcgType::FontWeight, serde_json::json!(400)),
            (
                DtcgType::Duration,
                serde_json::json!({"value": 150, "unit": "ms"}),
            ),
            (
                DtcgType::CubicBezier,
                serde_json::json!([0.2, 0.0, 0.8, 1.0]),
            ),
            (DtcgType::Number, serde_json::json!(1.25)),
            (DtcgType::StrokeStyle, serde_json::json!("solid")),
            (
                DtcgType::Border,
                serde_json::json!({
                    "color":"#112233",
                    "width":{"value":1,"unit":"px"},
                    "style":"solid"
                }),
            ),
            (
                DtcgType::Transition,
                serde_json::json!({
                    "duration":{"value":150,"unit":"ms"},
                    "delay":{"value":0,"unit":"ms"},
                    "timingFunction":[0.2,0.0,0.8,1.0]
                }),
            ),
            (
                DtcgType::Shadow,
                serde_json::json!({
                    "color":"#00000080",
                    "offsetX":{"value":0,"unit":"px"},
                    "offsetY":{"value":1,"unit":"px"},
                    "blur":{"value":4,"unit":"px"},
                    "spread":{"value":0,"unit":"px"}
                }),
            ),
            (
                DtcgType::Gradient,
                serde_json::json!([
                    {"color":"#000000","position":0},
                    {"color":"#ffffff","position":1}
                ]),
            ),
            (
                DtcgType::Typography,
                serde_json::json!({
                    "fontFamily":"Inter",
                    "fontSize":{"value":1,"unit":"rem"},
                    "fontWeight":400,
                    "letterSpacing":{"value":0,"unit":"px"},
                    "lineHeight":1.5
                }),
            ),
        ];
        for (token_type, value) in fixtures {
            assert_eq!(DtcgType::parse(token_type.as_str()).unwrap(), token_type);
            validate_token_value(token_type, &value).unwrap();
        }
        assert!(DtcgType::parse("unknown").is_err());
        assert!(validate_token_value(DtcgType::Duration, &serde_json::json!(-1)).is_err());
    }

    #[test]
    fn reference_siblings_apply_one_shallow_override() {
        let target = serde_json::json!({
            "nested":{"kept":true,"replaced":false},
            "untouched":1
        });
        let reference = serde_json::json!({
            "$ref":"theme.tokens.json#/base",
            "nested":{"replacement":true}
        });
        let merged =
            apply_shallow_reference_overrides(&target, reference.as_object().unwrap()).unwrap();
        assert_eq!(merged["nested"], serde_json::json!({"replacement":true}));
        assert_eq!(merged["untouched"], 1);
        assert!(merged.get("$ref").is_none());
    }
}
