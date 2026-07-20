use crate::{ContrastCheck, ContrastKind, KitTokenContract, ResolvedToken, ThemeError};
use serde::Serialize;
use std::collections::BTreeMap;

const COLOR_TOLERANCE: f64 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Srgb {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Oklch {
    pub lightness: f64,
    pub chroma: f64,
    pub hue: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorSpace {
    Srgb,
    SrgbLinear,
    DisplayP3,
    Oklab,
    Oklch,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedColor {
    pub source_space: ColorSpace,
    pub srgb: Srgb,
    pub oklch: Oklch,
    pub gamut_mapped: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContrastAlternativeReport {
    pub stack: Vec<String>,
    pub effective_background: Srgb,
    pub effective_foreground: Srgb,
    pub ratio: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContrastReport {
    pub id: String,
    pub foreground: String,
    pub background: String,
    pub kind: ContrastKind,
    pub minimum: f64,
    pub alternatives: Vec<ContrastAlternativeReport>,
    pub least_favorable_ratio: f64,
    pub passed: bool,
}

impl Srgb {
    fn opaque(self) -> bool {
        (self.alpha - 1.0).abs() <= 1e-9
    }

    fn source_over(self, backdrop: Self) -> Result<Self, ThemeError> {
        let alpha = self.alpha + backdrop.alpha * (1.0 - self.alpha);
        if alpha <= 0.0 {
            return Err(ThemeError::Resolution(
                "transparent compositing result".into(),
            ));
        }
        let mix = |source: f64, back: f64| {
            (self.alpha * source + backdrop.alpha * (1.0 - self.alpha) * back) / alpha
        };
        Ok(Self {
            red: mix(self.red, backdrop.red),
            green: mix(self.green, backdrop.green),
            blue: mix(self.blue, backdrop.blue),
            alpha,
        })
    }

    fn luminance(self) -> f64 {
        fn linear(value: f64) -> f64 {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(self.red) + 0.7152 * linear(self.green) + 0.0722 * linear(self.blue)
    }
}

pub fn parse_color(value: &serde_json::Value) -> Result<Srgb, ThemeError> {
    normalize_color(value).map(|color| color.srgb)
}

pub fn normalize_color(value: &serde_json::Value) -> Result<NormalizedColor, ThemeError> {
    if let Some(value) = value.as_str() {
        let srgb = parse_hex(value)?;
        return Ok(NormalizedColor {
            source_space: ColorSpace::Srgb,
            oklch: linear_srgb_to_oklch(
                decode_srgb(srgb.red),
                decode_srgb(srgb.green),
                decode_srgb(srgb.blue),
            ),
            srgb,
            gamut_mapped: false,
        });
    }
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("color must be a string or object".into()))?;
    if object.len() < 2
        || object.len() > 4
        || !object.contains_key("colorSpace")
        || !object.contains_key("components")
        || object
            .keys()
            .any(|name| !matches!(name.as_str(), "colorSpace" | "components" | "alpha" | "hex"))
    {
        return Err(ThemeError::Resolution(
            "color has missing or unknown members".into(),
        ));
    }
    let space = object
        .get("colorSpace")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ThemeError::Resolution("colorSpace is missing".into()))?;
    let components = object
        .get("components")
        .and_then(serde_json::Value::as_array)
        .filter(|components| components.len() == 3)
        .ok_or_else(|| ThemeError::Resolution("color needs three components".into()))?;
    let components: Vec<f64> = components
        .iter()
        .map(|component| {
            if component.as_str() == Some("none") {
                return Err(ThemeError::Resolution(
                    "color with a missing `none` component is not serializable".into(),
                ));
            }
            component
                .as_f64()
                .filter(|component| component.is_finite())
                .ok_or_else(|| ThemeError::Resolution("unresolved color component".into()))
        })
        .collect::<Result<_, _>>()?;
    let alpha = object
        .get("alpha")
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| ThemeError::Resolution("invalid color alpha".into()))
        })
        .transpose()?
        .unwrap_or(1.0);
    if !(0.0..=1.0).contains(&alpha) {
        return Err(ThemeError::Resolution("color alpha is outside 0..1".into()));
    }
    let source_space = match space {
        "srgb" => ColorSpace::Srgb,
        "srgb-linear" => ColorSpace::SrgbLinear,
        "display-p3" => ColorSpace::DisplayP3,
        "oklab" => ColorSpace::Oklab,
        "oklch" => ColorSpace::Oklch,
        _ => {
            return Err(ThemeError::Resolution(format!(
                "unsupported color space `{space}`"
            )));
        }
    };
    let (linear, oklch) = match source_space {
        ColorSpace::Srgb => {
            validate_unit_components(&components, "sRGB")?;
            let linear = (
                decode_srgb(components[0]),
                decode_srgb(components[1]),
                decode_srgb(components[2]),
            );
            (linear, linear_srgb_to_oklch(linear.0, linear.1, linear.2))
        }
        ColorSpace::SrgbLinear => {
            validate_unit_components(&components, "linear sRGB")?;
            let linear = (components[0], components[1], components[2]);
            (linear, linear_srgb_to_oklch(linear.0, linear.1, linear.2))
        }
        ColorSpace::DisplayP3 => {
            validate_unit_components(&components, "Display P3")?;
            let linear = display_p3_to_linear_srgb(components[0], components[1], components[2]);
            (linear, linear_srgb_to_oklch(linear.0, linear.1, linear.2))
        }
        ColorSpace::Oklab => {
            validate_lightness(components[0], "OKLab")?;
            let linear = oklab_to_linear_srgb(components[0], components[1], components[2]);
            (
                linear,
                oklab_to_oklch(components[0], components[1], components[2]),
            )
        }
        ColorSpace::Oklch => {
            validate_lightness(components[0], "OKLCH")?;
            if components[1] < 0.0 {
                return Err(ThemeError::Resolution(
                    "OKLCH chroma cannot be negative".into(),
                ));
            }
            let oklch = Oklch {
                lightness: components[0],
                chroma: components[1],
                hue: normalize_hue(components[2]),
            };
            (
                oklch_to_linear_srgb(oklch.lightness, oklch.chroma, oklch.hue),
                oklch,
            )
        }
    };
    let unbounded = Srgb {
        red: encode_srgb(linear.0),
        green: encode_srgb(linear.1),
        blue: encode_srgb(linear.2),
        alpha,
    };
    let gamut_mapped = !in_gamut(unbounded);
    let srgb = if !gamut_mapped {
        Srgb {
            red: unbounded.red.clamp(0.0, 1.0),
            green: unbounded.green.clamp(0.0, 1.0),
            blue: unbounded.blue.clamp(0.0, 1.0),
            alpha,
        }
    } else {
        gamut_map(oklch, alpha)
    };
    if let Some(author_hex) = object.get("hex") {
        let author_hex = author_hex
            .as_str()
            .ok_or_else(|| ThemeError::Resolution("color hex fallback must be a string".into()))?;
        let author = parse_hex(author_hex)?;
        if (author.alpha - alpha).abs() > 1.0 / 255.0
            || delta_e_between_srgb(srgb, author) > COLOR_TOLERANCE
        {
            return Err(ThemeError::Resolution(
                "author hex fallback does not agree with the normalized color".into(),
            ));
        }
    }
    Ok(NormalizedColor {
        source_space,
        srgb,
        oklch,
        gamut_mapped,
    })
}

pub fn validate_color_syntax(value: &serde_json::Value) -> Result<(), ThemeError> {
    if value.is_string() {
        parse_color(value)?;
        return Ok(());
    }
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("color must be a string or object".into()))?;
    if object.len() < 2
        || object.len() > 4
        || !object.contains_key("colorSpace")
        || !object.contains_key("components")
        || object
            .keys()
            .any(|name| !matches!(name.as_str(), "colorSpace" | "components" | "alpha" | "hex"))
    {
        return Err(ThemeError::Resolution(
            "color has missing or unknown members".into(),
        ));
    }
    let components = object
        .get("components")
        .and_then(serde_json::Value::as_array)
        .filter(|components| components.len() == 3)
        .ok_or_else(|| ThemeError::Resolution("color needs three components".into()))?;
    for component in components {
        if component.as_str() == Some("none") {
            continue;
        }
        component
            .as_f64()
            .filter(|component| component.is_finite())
            .ok_or_else(|| ThemeError::Resolution("invalid color component".into()))?;
    }
    if components
        .iter()
        .all(|component| component.as_str() != Some("none"))
    {
        normalize_color(value)?;
    } else {
        validate_color_metadata(object)?;
        validate_raw_component_ranges(
            object
                .get("colorSpace")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ThemeError::Resolution("colorSpace is missing".into()))?,
            components,
        )?;
    }
    Ok(())
}

pub fn format_css_number(value: f64) -> Result<String, ThemeError> {
    if !value.is_finite() {
        return Err(ThemeError::Resolution("CSS number must be finite".into()));
    }
    let scaled = value * 1_000_000.0;
    if !scaled.is_finite() {
        return Err(ThemeError::Resolution(
            "CSS number is outside the serializable range".into(),
        ));
    }
    let quantized = scaled.round_ties_even() / 1_000_000.0;
    let quantized = if quantized == 0.0 { 0.0 } else { quantized };
    let mut rendered = format!("{quantized:.6}");
    while rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    Ok(rendered)
}

pub fn serialize_color_fallback(value: &serde_json::Value) -> Result<String, ThemeError> {
    let color = normalize_color(value)?.srgb;
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8;
    let red = channel(color.red);
    let green = channel(color.green);
    let blue = channel(color.blue);
    let alpha = channel(color.alpha);
    if color.alpha == 1.0 && alpha == u8::MAX {
        Ok(format!("#{red:02x}{green:02x}{blue:02x}"))
    } else {
        Ok(format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}"))
    }
}

pub fn serialize_color_modern(value: &serde_json::Value) -> Result<String, ThemeError> {
    let color = normalize_color(value)?;
    let lightness = format_css_number(color.oklch.lightness)?;
    let chroma = format_css_number(color.oklch.chroma)?;
    let (chroma, hue) = if chroma == "0" {
        ("0".to_owned(), "0".to_owned())
    } else {
        (chroma, format_css_number(normalize_hue(color.oklch.hue))?)
    };
    let alpha = format_css_number(color.srgb.alpha)?;
    if color.srgb.alpha == 1.0 {
        Ok(format!("oklch({lightness} {chroma} {hue})"))
    } else {
        Ok(format!("oklch({lightness} {chroma} {hue} / {alpha})"))
    }
}

pub fn validate_contrast(
    contract: &KitTokenContract,
    values: &[ResolvedToken],
) -> Result<(), ThemeError> {
    let reports = contrast_reports(contract, values)?;
    if let Some(report) = reports.iter().find(|report| !report.passed) {
        return Err(ThemeError::Resolution(format!(
            "contrast check `{}` failed: {:.6} < {:.6}",
            report.id, report.least_favorable_ratio, report.minimum
        )));
    }
    Ok(())
}

pub fn contrast_reports(
    contract: &KitTokenContract,
    values: &[ResolvedToken],
) -> Result<Vec<ContrastReport>, ThemeError> {
    let colors: BTreeMap<&str, Srgb> = values
        .iter()
        .filter(|token| token.token_type == "color" && token.alias_of.is_none())
        .map(|token| Ok((token.path.as_str(), parse_color(&token.value)?)))
        .collect::<Result<_, ThemeError>>()?;
    contract
        .contrast_checks
        .iter()
        .map(|check| evaluate_check(contract, check, &colors))
        .collect()
}

fn evaluate_check(
    contract: &KitTokenContract,
    check: &ContrastCheck,
    colors: &BTreeMap<&str, Srgb>,
) -> Result<ContrastReport, ThemeError> {
    let foreground_path = contract.terminal_mapping(&check.foreground)?.path.as_str();
    let background_path = contract.terminal_mapping(&check.background)?.path.as_str();
    let foreground = color(colors, foreground_path)?;
    let background = color(colors, background_path)?;
    let alternatives: Vec<Vec<String>> = if background.opaque() {
        if check.composite_on.is_some() {
            return Err(ThemeError::Contract(format!(
                "opaque contrast background `{}` cannot define compositeOn",
                check.id
            )));
        }
        vec![Vec::new()]
    } else {
        check
            .composite_on
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ThemeError::Contract(format!(
                    "translucent contrast background `{}` requires compositeOn",
                    check.id
                ))
            })?
    };
    let mut least = f64::INFINITY;
    let mut reports = Vec::with_capacity(alternatives.len());
    for stack in alternatives {
        let effective_background = if stack.is_empty() {
            background
        } else {
            let mut reversed = stack.iter().rev();
            let farthest = reversed
                .next()
                .map(|path| terminal_color(contract, colors, path))
                .transpose()?
                .ok_or_else(|| ThemeError::Contract("empty compositing stack".into()))?;
            if !farthest.opaque() {
                return Err(ThemeError::Contract(format!(
                    "contrast stack `{}` has a translucent terminal surface",
                    check.id
                )));
            }
            let mut base = farthest;
            for path in reversed {
                base = terminal_color(contract, colors, path)?.source_over(base)?;
            }
            background.source_over(base)?
        };
        let effective_foreground = foreground.source_over(effective_background)?;
        if !effective_background.opaque() || !effective_foreground.opaque() {
            return Err(ThemeError::Resolution(
                "contrast colors are not opaque".into(),
            ));
        }
        let first = effective_foreground.luminance();
        let second = effective_background.luminance();
        let ratio = (first.max(second) + 0.05) / (first.min(second) + 0.05);
        least = least.min(ratio);
        reports.push(ContrastAlternativeReport {
            stack,
            effective_background,
            effective_foreground,
            ratio,
        });
    }
    let floor: f64 = match check.kind {
        ContrastKind::Text => 4.5,
        ContrastKind::LargeText | ContrastKind::NonText | ContrastKind::FocusIndicator => 3.0,
    };
    let minimum = floor.max(check.minimum);
    Ok(ContrastReport {
        id: check.id.clone(),
        foreground: check.foreground.clone(),
        background: check.background.clone(),
        kind: check.kind,
        minimum,
        alternatives: reports,
        least_favorable_ratio: least,
        passed: least >= minimum,
    })
}

fn color(colors: &BTreeMap<&str, Srgb>, path: &str) -> Result<Srgb, ThemeError> {
    colors
        .get(path)
        .copied()
        .ok_or_else(|| ThemeError::Resolution(format!("unknown contrast color `{path}`")))
}

fn terminal_color(
    contract: &KitTokenContract,
    colors: &BTreeMap<&str, Srgb>,
    path: &str,
) -> Result<Srgb, ThemeError> {
    color(colors, &contract.terminal_mapping(path)?.path)
}

fn validate_unit_components(components: &[f64], space: &str) -> Result<(), ThemeError> {
    if components
        .iter()
        .any(|component| !(0.0..=1.0).contains(component))
    {
        return Err(ThemeError::Resolution(format!(
            "{space} components must be within 0..1"
        )));
    }
    Ok(())
}

fn validate_lightness(lightness: f64, space: &str) -> Result<(), ThemeError> {
    if !(0.0..=1.0).contains(&lightness) {
        return Err(ThemeError::Resolution(format!(
            "{space} lightness must be within 0..1"
        )));
    }
    Ok(())
}

fn validate_color_metadata(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ThemeError> {
    let space = object
        .get("colorSpace")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ThemeError::Resolution("colorSpace is missing".into()))?;
    if !matches!(
        space,
        "srgb" | "srgb-linear" | "display-p3" | "oklab" | "oklch"
    ) {
        return Err(ThemeError::Resolution(format!(
            "unsupported color space `{space}`"
        )));
    }
    let alpha = object
        .get("alpha")
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| ThemeError::Resolution("invalid color alpha".into()))
        })
        .transpose()?
        .unwrap_or(1.0);
    if !(0.0..=1.0).contains(&alpha) {
        return Err(ThemeError::Resolution("color alpha is outside 0..1".into()));
    }
    if let Some(hex) = object.get("hex") {
        parse_hex(
            hex.as_str()
                .ok_or_else(|| ThemeError::Resolution("color hex must be a string".into()))?,
        )?;
    }
    Ok(())
}

fn validate_raw_component_ranges(
    space: &str,
    components: &[serde_json::Value],
) -> Result<(), ThemeError> {
    let number = |index: usize| components[index].as_f64();
    match space {
        "srgb" | "srgb-linear" | "display-p3" => {
            for component in components {
                if component
                    .as_f64()
                    .is_some_and(|value| !(0.0..=1.0).contains(&value))
                {
                    return Err(ThemeError::Resolution(format!(
                        "{space} components must be within 0..1"
                    )));
                }
            }
        }
        "oklab" => {
            if number(0).is_some_and(|value| !(0.0..=1.0).contains(&value)) {
                return Err(ThemeError::Resolution(
                    "OKLab lightness must be within 0..1".into(),
                ));
            }
        }
        "oklch" => {
            if number(0).is_some_and(|value| !(0.0..=1.0).contains(&value)) {
                return Err(ThemeError::Resolution(
                    "OKLCH lightness must be within 0..1".into(),
                ));
            }
            if number(1).is_some_and(|value| value < 0.0) {
                return Err(ThemeError::Resolution(
                    "OKLCH chroma cannot be negative".into(),
                ));
            }
        }
        _ => unreachable!("metadata validation closes the color-space vocabulary"),
    }
    Ok(())
}

fn parse_hex(value: &str) -> Result<Srgb, ThemeError> {
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| ThemeError::Resolution("color string must use hexadecimal syntax".into()))?;
    let expanded = match hex.len() {
        3 | 4 => hex
            .chars()
            .flat_map(|value| [value, value])
            .collect::<String>(),
        6 | 8 => hex.to_string(),
        _ => {
            return Err(ThemeError::Resolution(
                "invalid hexadecimal color length".into(),
            ));
        }
    };
    let channel = |start| {
        u8::from_str_radix(&expanded[start..start + 2], 16)
            .map(|value| f64::from(value) / 255.0)
            .map_err(|_| ThemeError::Resolution("invalid hexadecimal color".into()))
    };
    Ok(Srgb {
        red: channel(0)?,
        green: channel(2)?,
        blue: channel(4)?,
        alpha: if expanded.len() == 8 {
            channel(6)?
        } else {
            1.0
        },
    })
}

fn decode_srgb(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_srgb(value: f64) -> f64 {
    if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn display_p3_to_linear_srgb(red: f64, green: f64, blue: f64) -> (f64, f64, f64) {
    let r = decode_srgb(red);
    let g = decode_srgb(green);
    let b = decode_srgb(blue);
    let x =
        0.486_570_948_648_216_2 * r + 0.265_667_693_169_093_06 * g + 0.198_217_285_234_362_5 * b;
    let y = 0.228_974_564_069_748_8 * r + 0.691_738_521_836_506_4 * g + 0.079_286_914_093_745 * b;
    let z = 0.045_113_381_858_902_64 * g + 1.043_944_368_900_976 * b;
    (
        3.240_969_941_904_522_6 * x - 1.537_383_177_570_094 * y - 0.498_610_760_293_003_4 * z,
        -0.969_243_636_280_879_6 * x + 1.875_967_501_507_720_2 * y + 0.041_555_057_407_175_59 * z,
        0.055_630_079_696_993_66 * x - 0.203_976_958_888_976_52 * y + 1.056_971_514_242_878_6 * z,
    )
}

fn oklch_to_linear_srgb(lightness: f64, chroma: f64, hue: f64) -> (f64, f64, f64) {
    let radians = hue.to_radians();
    let a = chroma * radians.cos();
    let b = chroma * radians.sin();
    oklab_to_linear_srgb(lightness, a, b)
}

fn oklab_to_linear_srgb(lightness: f64, a: f64, b: f64) -> (f64, f64, f64) {
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    (
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    )
}

fn linear_srgb_to_oklab(red: f64, green: f64, blue: f64) -> (f64, f64, f64) {
    let l = (0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue).cbrt();
    let m = (0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue).cbrt();
    let s = (0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue).cbrt();
    (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )
}

fn linear_srgb_to_oklch(red: f64, green: f64, blue: f64) -> Oklch {
    let (lightness, a, b) = linear_srgb_to_oklab(red, green, blue);
    oklab_to_oklch(lightness, a, b)
}

fn oklab_to_oklch(lightness: f64, a: f64, b: f64) -> Oklch {
    let chroma = a.hypot(b);
    let hue = if chroma <= f64::EPSILON {
        0.0
    } else {
        normalize_hue(b.atan2(a).to_degrees())
    };
    Oklch {
        lightness,
        chroma,
        hue,
    }
}

fn normalize_hue(hue: f64) -> f64 {
    let normalized = hue.rem_euclid(360.0);
    if normalized == 0.0 { 0.0 } else { normalized }
}

fn in_gamut(color: Srgb) -> bool {
    [color.red, color.green, color.blue]
        .into_iter()
        .all(|channel| (-1e-7..=1.000_000_1).contains(&channel))
}

fn gamut_map(color: Oklch, alpha: f64) -> Srgb {
    const EPSILON: f64 = 0.0001;
    if color.lightness >= 1.0 {
        return Srgb {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha,
        };
    }
    if color.lightness <= 0.0 {
        return Srgb {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha,
        };
    }

    let mut minimum = 0.0;
    let mut maximum = color.chroma.max(0.0);
    let mut minimum_in_gamut = true;
    let mut clipped = clip_oklch(color, alpha);
    while maximum - minimum > EPSILON {
        let chroma = (minimum + maximum) / 2.0;
        let candidate = Oklch { chroma, ..color };
        let unbounded = unbounded_srgb(candidate, alpha);
        if minimum_in_gamut && in_gamut(unbounded) {
            return Srgb {
                red: unbounded.red.clamp(0.0, 1.0),
                green: unbounded.green.clamp(0.0, 1.0),
                blue: unbounded.blue.clamp(0.0, 1.0),
                alpha,
            };
        }
        clipped = clip_oklch(candidate, alpha);
        let difference = delta_e_ok(candidate, clipped);
        if difference < COLOR_TOLERANCE {
            if COLOR_TOLERANCE - difference < EPSILON {
                return clipped;
            }
            minimum_in_gamut = false;
            minimum = chroma;
        } else {
            maximum = chroma;
        }
    }
    clipped
}

fn unbounded_srgb(color: Oklch, alpha: f64) -> Srgb {
    let linear = oklch_to_linear_srgb(color.lightness, color.chroma, color.hue);
    Srgb {
        red: encode_srgb(linear.0),
        green: encode_srgb(linear.1),
        blue: encode_srgb(linear.2),
        alpha,
    }
}

fn clip_oklch(color: Oklch, alpha: f64) -> Srgb {
    let color = unbounded_srgb(color, alpha);
    Srgb {
        red: color.red.clamp(0.0, 1.0),
        green: color.green.clamp(0.0, 1.0),
        blue: color.blue.clamp(0.0, 1.0),
        alpha,
    }
}

fn delta_e_ok(source: Oklch, mapped: Srgb) -> f64 {
    let radians = source.hue.to_radians();
    let source_a = source.chroma * radians.cos();
    let source_b = source.chroma * radians.sin();
    let (mapped_l, mapped_a, mapped_b) = linear_srgb_to_oklab(
        decode_srgb(mapped.red),
        decode_srgb(mapped.green),
        decode_srgb(mapped.blue),
    );
    ((source.lightness - mapped_l).powi(2)
        + (source_a - mapped_a).powi(2)
        + (source_b - mapped_b).powi(2))
    .sqrt()
}

fn delta_e_between_srgb(first: Srgb, second: Srgb) -> f64 {
    let first = linear_srgb_to_oklch(
        decode_srgb(first.red),
        decode_srgb(first.green),
        decode_srgb(first.blue),
    );
    delta_e_ok(first, second)
}

#[cfg(test)]
mod tests {
    use super::{
        ColorSpace, contrast_reports, format_css_number, normalize_color, parse_color,
        serialize_color_fallback, serialize_color_modern, validate_color_syntax,
    };
    use crate::{
        ContrastCheck, ContrastKind, KitTokenContract, ResolvedToken, TokenDomain, TokenMapping,
    };
    use std::collections::BTreeMap;

    #[test]
    fn parses_hex_and_oklch() {
        assert_eq!(parse_color(&"#fff".into()).unwrap().alpha, 1.0);
        let color = serde_json::json!({"colorSpace":"oklch","components":[1.0,0.0,0.0]});
        let parsed = parse_color(&color).unwrap();
        assert!(parsed.red > 0.99 && parsed.green > 0.99 && parsed.blue > 0.99);
    }

    #[test]
    fn normalizes_every_supported_space_and_rejects_ranges() {
        let cases = [
            ("srgb", serde_json::json!([0.25, 0.5, 0.75])),
            ("srgb-linear", serde_json::json!([0.25, 0.5, 0.75])),
            ("display-p3", serde_json::json!([0.25, 0.5, 0.75])),
            ("oklab", serde_json::json!([0.5, 0.1, -0.1])),
            ("oklch", serde_json::json!([0.5, 0.2, 420.0])),
        ];
        for (space, components) in cases {
            let value = serde_json::json!({
                "colorSpace": space,
                "components": components,
                "alpha": 0.5
            });
            let normalized = normalize_color(&value).unwrap();
            assert_eq!(normalized.srgb.alpha, 0.5);
        }
        let invalid = serde_json::json!({
            "colorSpace": "srgb",
            "components": [1.01, 0, 0]
        });
        assert!(normalize_color(&invalid).is_err());
        let invalid = serde_json::json!({
            "colorSpace": "oklch",
            "components": [1.01, 0, 0]
        });
        assert!(normalize_color(&invalid).is_err());
    }

    #[test]
    fn preserves_missing_components_at_the_raw_boundary_only() {
        let value = serde_json::json!({
            "colorSpace": "oklch",
            "components": [0.5, "none", 30]
        });
        validate_color_syntax(&value).unwrap();
        assert!(normalize_color(&value).is_err());
    }

    #[test]
    fn author_fallback_must_agree_with_normalized_color() {
        let matching = serde_json::json!({
            "colorSpace": "srgb",
            "components": [1, 0, 0],
            "hex": "#ff0000"
        });
        assert_eq!(
            normalize_color(&matching).unwrap().source_space,
            ColorSpace::Srgb
        );
        let mismatch = serde_json::json!({
            "colorSpace": "srgb",
            "components": [1, 0, 0],
            "hex": "#0000ff"
        });
        assert!(normalize_color(&mismatch).is_err());
    }

    #[test]
    fn serialization_is_canonical_and_shared() {
        let value = serde_json::json!({
            "colorSpace": "display-p3",
            "components": [1, 0, 0],
            "alpha": 0.5
        });
        let normalized = normalize_color(&value).unwrap();
        assert!(normalized.gamut_mapped);
        assert_eq!(serialize_color_fallback(&value).unwrap().len(), 9);
        assert!(
            serialize_color_modern(&value)
                .unwrap()
                .starts_with("oklch(")
        );
        assert_eq!(format_css_number(-0.0).unwrap(), "0");
        assert_eq!(format_css_number(0.500_000_4).unwrap(), "0.5");
    }

    #[test]
    fn contrast_reports_preserve_alternatives_and_gate_on_the_least_ratio() {
        let paths = [
            "color.foreground",
            "color.background",
            "color.surface-light",
            "color.surface-dark",
        ];
        let tokens = paths
            .iter()
            .enumerate()
            .map(|(index, path)| TokenMapping {
                path: (*path).into(),
                token_type: "color".into(),
                css_custom_property: format!("--kit-color-test-{index}"),
                domain: TokenDomain::Theme,
                required: true,
                order: index as u32,
                theme_override: true,
                default: Some(serde_json::Value::Null),
                description: None,
                deprecation: None,
            })
            .collect();
        let contract = KitTokenContract {
            schema: String::new(),
            schema_version: String::new(),
            contract_id: String::new(),
            abi_version: 1,
            revision: 2,
            dtcg_version: String::new(),
            dtcg_profile: String::new(),
            canonical_digest: String::new(),
            tokens,
            contrast_checks: vec![ContrastCheck {
                id: "body-text".into(),
                foreground: paths[0].into(),
                background: paths[1].into(),
                kind: ContrastKind::Text,
                minimum: 1.0,
                composite_on: Some(vec![vec![paths[2].into()], vec![paths[3].into()]]),
                description: None,
            }],
            extensions: BTreeMap::new(),
        };
        let colors = ["#00000080", "#ffffff80", "#ffffff", "#000000"];
        let values = paths
            .iter()
            .zip(colors)
            .map(|(path, value)| ResolvedToken {
                path: (*path).into(),
                token_type: "color".into(),
                css_custom_property: String::new(),
                domain: TokenDomain::Theme,
                value: value.into(),
                provenance: Vec::new(),
                alias_of: None,
            })
            .collect::<Vec<_>>();
        let reports = contrast_reports(&contract, &values).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].alternatives.len(), 2);
        assert_eq!(reports[0].minimum, 4.5);
        assert_eq!(
            reports[0].least_favorable_ratio,
            reports[0]
                .alternatives
                .iter()
                .map(|alternative| alternative.ratio)
                .reduce(f64::min)
                .unwrap()
        );
    }
}
