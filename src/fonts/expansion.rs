//! Quantized font expansion recipes (`fonts/expansion.py`).
use super::formats::vf::{VirtualFont, VirtualOperation};
use super::microtypography::{FontCode, code_value};
use super::model::FontInfo;
use crate::core::errors::{Error, Result};
use crate::core::values::round_ratio;
use crate::language::expressions::quotient;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionPolicy {
    pub stretch: i64,
    pub shrink: i64,
    pub step: i64,
    pub automatic: bool,
}
pub trait ExpansionCatalog {
    fn items(&self) -> &[FontInfo];
    fn items_mut(&mut self) -> &mut Vec<FontInfo>;
    fn load(&mut self, name: &str, directory: &Path, size: i64) -> Result<usize>;
}
pub fn configure(
    catalog: &mut impl ExpansionCatalog,
    number: usize,
    stretch: i64,
    shrink: i64,
    step: i64,
    automatic: bool,
) -> Result<()> {
    let font = &catalog.items()[number];
    if font.null_font || font.expansion_base.is_some() {
        return Err(Error::value(
            "Font expansion requires an unexpanded base font",
        ));
    }
    let (stretch, shrink, step) = (
        stretch.clamp(0, 1000),
        shrink.clamp(0, 500),
        step.clamp(0, 100),
    );
    if step == 0 {
        return Err(Error::value("Font expansion step must be positive"));
    }
    let (stretch, shrink) = (stretch - stretch % step, shrink - shrink % step);
    if stretch == 0 && shrink == 0 {
        return Err(Error::value("Font expansion requires a positive limit"));
    }
    let policy = ExpansionPolicy {
        stretch,
        shrink,
        step,
        automatic,
    };
    if font
        .expansion_policy
        .is_some_and(|previous| previous != policy)
    {
        return Err(Error::value(
            "Font already has different expansion parameters",
        ));
    }
    catalog.items_mut()[number].expansion_policy = Some(policy);
    if stretch != 0 {
        expanded(catalog, number, stretch)?;
    }
    if shrink != 0 {
        expanded(catalog, number, -shrink)?;
    }
    Ok(())
}
pub fn expanded(catalog: &mut impl ExpansionCatalog, number: usize, ratio: i64) -> Result<usize> {
    let source = &catalog.items()[number];
    let Some(policy) = source.expansion_policy.filter(|_| ratio != 0) else {
        return Ok(number);
    };
    let limit = if ratio > 0 {
        policy.stretch
    } else {
        policy.shrink
    };
    let ratio = limit
        .min(policy.step * round_ratio((ratio as i128).abs(), policy.step as i128)? as i64)
        * ratio.signum();
    if ratio == 0 {
        return Ok(number);
    }
    if let Some(&selected) = source.expansion_variants.get(&ratio) {
        return Ok(selected);
    }
    let name = format!("{}{}{ratio}", source.name, if ratio > 0 { "+" } else { "" });
    let selected = if policy.automatic {
        let mut font = source.clone();
        font.name = name;
        font.width_overrides.clear();
        font.italic_overrides.clear();
        font.expansion_variants.clear();
        font.expansion_base = Some(number);
        font.expansion_ratio = ratio;
        font.virtual_font = None;
        font.virtual_checked = true;
        font.materialization.map_bound = false;
        font.materialization.resource_ready = false;
        font.materialization.included_codes.clear();
        let codes: Vec<u32> = source.tfm_data.as_ref().map_or_else(
            || (0..256).collect(),
            |tfm| tfm.width_fix.keys().copied().collect(),
        );
        for code in codes {
            font.width_overrides.insert(
                code,
                quotient(
                    source.get_char_width(code) as i128 * (1000 + ratio) as i128,
                    1000,
                )? as i64,
            );
            font.italic_overrides.insert(
                code,
                quotient(
                    source.get_char_italic(code) as i128 * (1000 + ratio) as i128,
                    1000,
                )? as i64,
            );
        }
        font.tfm_kerns = Some(
            source
                .tfm_kerns
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|&kern| {
                    quotient(kern as i128 * (1000 + ratio) as i128, 1000).map(|value| value as i64)
                })
                .collect::<Result<_>>()?,
        );
        let selected = catalog.items().len();
        catalog.items_mut().push(font);
        selected
    } else {
        let directory = source
            .tfm_file
            .as_deref()
            .and_then(|file| Path::new(file).parent())
            .ok_or_else(|| Error::value("Expanded font requires a metric path"))?
            .to_path_buf();
        let size = source.size;
        let codes = source.micro_codes.clone();
        let selected = catalog.load(&name, &directory, size)?;
        let font = &mut catalog.items_mut()[selected];
        font.expansion_base = Some(number);
        font.expansion_ratio = ratio;
        font.expansion_policy = Some(policy);
        font.micro_codes = codes;
        selected
    };
    catalog.items_mut()[number]
        .expansion_variants
        .insert(ratio, selected);
    Ok(selected)
}
pub fn capacity(
    catalog: &mut impl ExpansionCatalog,
    number: usize,
    code: u32,
) -> Result<(i64, i64)> {
    let font = &catalog.items()[number];
    let Some(policy) = font
        .expansion_policy
        .filter(|_| font.expansion_base.is_none())
    else {
        return Ok((0, 0));
    };
    let factor = code_value(font, FontCode::Expansion, code);
    let width = font.get_char_width(code);
    let stretch_font = expanded(catalog, number, policy.stretch)?;
    let stretch = catalog.items()[stretch_font].get_char_width(code) - width;
    let shrink_font = expanded(catalog, number, -policy.shrink)?;
    let shrink = width - catalog.items()[shrink_font].get_char_width(code);
    Ok((
        round_ratio(stretch.max(0) as i128 * factor as i128, 1000)? as i64,
        round_ratio(shrink.max(0) as i128 * factor as i128, 1000)? as i64,
    ))
}
pub fn substitute(
    catalog: &mut impl ExpansionCatalog,
    number: usize,
    code: u32,
    normalized: i64,
) -> Result<usize> {
    let font = &catalog.items()[number];
    let Some(policy) = font
        .expansion_policy
        .filter(|_| font.expansion_base.is_none())
    else {
        return Ok(number);
    };
    let limit = if normalized > 0 {
        policy.stretch
    } else {
        policy.shrink
    };
    let ratio = quotient(
        normalized as i128 * code_value(font, FontCode::Expansion, code) as i128 * limit as i128,
        1_000_000,
    )?;
    expanded(catalog, number, ratio as i64)
}
pub fn virtual_variant(
    catalog: &mut impl ExpansionCatalog,
    font: &FontInfo,
    base: &FontInfo,
) -> Result<VirtualFont> {
    let policy = font
        .expansion_policy
        .ok_or_else(|| Error::value("Missing expansion policy"))?;
    let base = base
        .virtual_font
        .as_ref()
        .ok_or_else(|| Error::value("Missing virtual font"))?;
    let mut replacements = BTreeMap::new();
    for &number in base.fonts.values() {
        if catalog.items()[number].expansion_policy.is_none() {
            configure(
                catalog,
                number,
                policy.stretch,
                policy.shrink,
                policy.step,
                policy.automatic,
            )?;
        }
        replacements.insert(number, expanded(catalog, number, font.expansion_ratio)?);
    }
    Ok(VirtualFont {
        path: format!("{}:{}", base.path, font.name),
        fonts: base
            .fonts
            .iter()
            .map(|(&external, number)| (external, replacements[number]))
            .collect(),
        font_order: base.font_order.clone(),
        packets: base
            .packets
            .iter()
            .map(|(&code, packet)| {
                (
                    code,
                    Arc::from(
                        packet
                            .iter()
                            .map(|operation| match operation {
                                VirtualOperation::Font(number) => {
                                    VirtualOperation::Font(replacements[number])
                                }
                                other => other.clone(),
                            })
                            .collect::<Vec<_>>(),
                    ),
                )
            })
            .collect(),
    })
}
