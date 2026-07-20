use crate::{ContrastCheck, ContrastKind, KitTokenContract, ResolvedToken, ThemeError};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Srgb {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub lightness: f64,
    pub chroma: f64,
    pub hue: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedColor {
    pub srgb: Srgb,
    pub oklch: Oklch,
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
            oklch: linear_srgb_to_oklch(
                decode_srgb(srgb.red),
                decode_srgb(srgb.green),
                decode_srgb(srgb.blue),
            ),
            srgb,
        });
    }
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("color must be a string or object".into()))?;
    if object.len() < 2
        || object.len() > 3
        || !object.contains_key("colorSpace")
        || !object.contains_key("components")
        || object
            .keys()
            .any(|name| !matches!(name.as_str(), "colorSpace" | "components" | "alpha"))
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
    let (linear, oklch) = match space {
        "srgb" => {
            let linear = (
                decode_srgb(components[0]),
                decode_srgb(components[1]),
                decode_srgb(components[2]),
            );
            (linear, linear_srgb_to_oklch(linear.0, linear.1, linear.2))
        }
        "srgb-linear" => {
            let linear = (components[0], components[1], components[2]);
            (linear, linear_srgb_to_oklch(linear.0, linear.1, linear.2))
        }
        "display-p3" => {
            let linear = display_p3_to_linear_srgb(components[0], components[1], components[2]);
            (linear, linear_srgb_to_oklch(linear.0, linear.1, linear.2))
        }
        "oklab" => {
            let linear = oklab_to_linear_srgb(components[0], components[1], components[2]);
            (
                linear,
                oklab_to_oklch(components[0], components[1], components[2]),
            )
        }
        "oklch" => {
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
        _ => {
            return Err(ThemeError::Resolution(format!(
                "unsupported color space `{space}`"
            )));
        }
    };
    let unbounded = Srgb {
        red: encode_srgb(linear.0),
        green: encode_srgb(linear.1),
        blue: encode_srgb(linear.2),
        alpha,
    };
    let srgb = if in_gamut(unbounded) {
        Srgb {
            red: unbounded.red.clamp(0.0, 1.0),
            green: unbounded.green.clamp(0.0, 1.0),
            blue: unbounded.blue.clamp(0.0, 1.0),
            alpha,
        }
    } else {
        gamut_map(oklch, alpha)
    };
    Ok(NormalizedColor { srgb, oklch })
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
    let colors: BTreeMap<&str, Srgb> = values
        .iter()
        .filter(|token| token.token_type == "color" && token.alias_of.is_none())
        .map(|token| Ok((token.path.as_str(), parse_color(&token.value)?)))
        .collect::<Result<_, ThemeError>>()?;
    for check in &contract.contrast_checks {
        validate_check(check, &colors)?;
    }
    Ok(())
}

fn validate_check(check: &ContrastCheck, colors: &BTreeMap<&str, Srgb>) -> Result<(), ThemeError> {
    let foreground = color(colors, &check.foreground)?;
    let background = color(colors, &check.background)?;
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
    for stack in alternatives {
        let effective_background = if stack.is_empty() {
            background
        } else {
            let mut reversed = stack.iter().rev();
            let farthest = reversed
                .next()
                .map(|path| color(colors, path))
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
                base = color(colors, path)?.source_over(base)?;
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
    }
    let floor: f64 = match check.kind {
        ContrastKind::Text => 4.5,
        ContrastKind::LargeText | ContrastKind::NonText | ContrastKind::FocusIndicator => 3.0,
    };
    let minimum = floor.max(check.minimum);
    if least + 1e-9 < minimum {
        Err(ThemeError::Resolution(format!(
            "contrast check `{}` failed: {least:.3} < {minimum:.3}",
            check.id
        )))
    } else {
        Ok(())
    }
}

fn color(colors: &BTreeMap<&str, Srgb>, path: &str) -> Result<Srgb, ThemeError> {
    colors
        .get(path)
        .copied()
        .ok_or_else(|| ThemeError::Resolution(format!("unknown contrast color `{path}`")))
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
    let x = 0.48657095 * r + 0.26566769 * g + 0.19821729 * b;
    let y = 0.22897456 * r + 0.69173852 * g + 0.07928691 * b;
    let z = 0.0 * r + 0.04511338 * g + 1.04394437 * b;
    (
        3.2406 * x - 1.5372 * y - 0.4986 * z,
        -0.9689 * x + 1.8758 * y + 0.0415 * z,
        0.0557 * x - 0.2040 * y + 1.0570 * z,
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
    const JND: f64 = 0.02;
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
        if minimum_in_gamut && maximum - minimum < EPSILON {
            return clipped;
        }
        if in_gamut(unbounded) {
            minimum = chroma;
            continue;
        }
        clipped = clip_oklch(candidate, alpha);
        let difference = delta_e_ok(candidate, clipped);
        if difference < JND {
            if JND - difference < EPSILON {
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

#[cfg(test)]
mod tests {
    use super::parse_color;

    #[test]
    fn parses_hex_and_oklch() {
        assert_eq!(parse_color(&"#fff".into()).unwrap().alpha, 1.0);
        let color = serde_json::json!({"colorSpace":"oklch","components":[1.0,0.0,0.0]});
        let parsed = parse_color(&color).unwrap();
        assert!(parsed.red > 0.99 && parsed.green > 0.99 && parsed.blue > 0.99);
    }
}
