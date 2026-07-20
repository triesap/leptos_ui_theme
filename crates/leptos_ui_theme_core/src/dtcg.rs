use crate::{ThemeError, TokenPath, parse_color};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

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

pub fn validate_token_value(token_type: DtcgType, value: &Value) -> Result<(), ThemeError> {
    if alias_target(value)?.is_some() {
        return Ok(());
    }
    match token_type {
        DtcgType::Color => {
            parse_color(value)?;
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
        if name.is_empty()
            || name.starts_with('$')
            || !name.contains('.')
            || name.chars().any(char::is_whitespace)
        {
            return Err(ThemeError::Resolution(format!(
                "invalid extension namespace `{name}`"
            )));
        }
    }
    Ok(())
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
