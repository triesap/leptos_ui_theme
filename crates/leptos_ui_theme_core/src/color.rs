use crate::{ContrastCheck, ContrastKind, KitTokenContract, ResolvedToken, ThemeError};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Srgb {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
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
    if let Some(value) = value.as_str() {
        return parse_hex(value);
    }
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("color must be a string or object".into()))?;
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
                .ok_or_else(|| ThemeError::Resolution("unresolved color component".into()))
        })
        .collect::<Result<_, _>>()?;
    let alpha = object
        .get("alpha")
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| ThemeError::Resolution("invalid color alpha".into()))
        })
        .transpose()?
        .unwrap_or(1.0);
    if !(0.0..=1.0).contains(&alpha) {
        return Err(ThemeError::Resolution("color alpha is outside 0..1".into()));
    }
    let (red, green, blue) = match space {
        "srgb" => (components[0], components[1], components[2]),
        "display-p3" => display_p3_to_srgb(components[0], components[1], components[2]),
        "oklch" => oklch_to_srgb(components[0], components[1], components[2]),
        _ => {
            return Err(ThemeError::Resolution(format!(
                "unsupported color space `{space}`"
            )));
        }
    };
    Ok(Srgb {
        red: red.clamp(0.0, 1.0),
        green: green.clamp(0.0, 1.0),
        blue: blue.clamp(0.0, 1.0),
        alpha,
    })
}

pub fn validate_contrast(
    contract: &KitTokenContract,
    values: &[ResolvedToken],
) -> Result<(), ThemeError> {
    let colors: BTreeMap<&str, Srgb> = values
        .iter()
        .filter(|token| token.token_type == "color")
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

fn display_p3_to_srgb(red: f64, green: f64, blue: f64) -> (f64, f64, f64) {
    let linear = |value: f64| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |value: f64| {
        if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        }
    };
    let r = linear(red);
    let g = linear(green);
    let b = linear(blue);
    let x = 0.48657095 * r + 0.26566769 * g + 0.19821729 * b;
    let y = 0.22897456 * r + 0.69173852 * g + 0.07928691 * b;
    let z = 0.0 * r + 0.04511338 * g + 1.04394437 * b;
    (
        encode(3.2406 * x - 1.5372 * y - 0.4986 * z),
        encode(-0.9689 * x + 1.8758 * y + 0.0415 * z),
        encode(0.0557 * x - 0.2040 * y + 1.0570 * z),
    )
}

fn oklch_to_srgb(lightness: f64, chroma: f64, hue: f64) -> (f64, f64, f64) {
    let radians = hue.to_radians();
    let a = chroma * radians.cos();
    let b = chroma * radians.sin();
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    let encode = |value: f64| {
        if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        }
    };
    (
        encode(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
        encode(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
        encode(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s),
    )
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
