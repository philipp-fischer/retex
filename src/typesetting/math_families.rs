//! Scoped family selection and selected-size parameters (`math_families.py`).
use super::math_atoms::MathField;
use super::math_codes::MathCharacterCode;
use super::nodes::{integer, parameter};
use crate::core::enums::MathStyle;
use crate::core::errors::{Error, Result};
use crate::fonts::catalog::FontCatalog;
use crate::language::code_validation::MATH_FAMILY_COUNT;
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::language::state::Environment;
pub struct MathFamilies;
pub trait FamilyInput {
    fn scan_register(&mut self, maximum: i64) -> Result<i64>;
    fn scan_optional_equals(&mut self) -> Result<()>;
    fn scan_font_ident(&mut self) -> Result<usize>;
}
impl MathFamilies {
    pub fn family(env: &Environment, level: usize, number: i64) -> usize {
        integer(
            env,
            &ValueKey {
                table: "mathfont".into(),
                index: ValueIndex::Pair(level as i64, number),
            },
            0,
        ) as usize
    }
    pub fn assign_family(
        env: &mut Environment,
        input: &mut impl FamilyInput,
        level: usize,
        global: bool,
    ) -> Result<()> {
        let number = input.scan_register(MATH_FAMILY_COUNT as i64 - 1)?;
        input.scan_optional_equals()?;
        let font = input.scan_font_ident()?;
        env.assign_value(
            ValueKey {
                table: "mathfont".into(),
                index: ValueIndex::Pair(level as i64, number),
            },
            Quantity::Integer(font as i64),
            global,
        );
        Ok(())
    }
    pub fn param(
        env: &Environment,
        fonts: &FontCatalog,
        index: usize,
        style: MathStyle,
        family: i64,
    ) -> Result<i64> {
        let font = Self::family(env, style.font_size(), family);
        if font == 0 {
            return Err(Error::value(format!(
                "Undefined math font family {family}, size {}",
                style.font_size()
            )));
        }
        Ok(fonts.items[font].get_parameter(index))
    }
    pub fn char_field(env: &Environment, code: MathCharacterCode) -> MathField {
        let mut family = code.family;
        let selected = parameter(env, "fam", -1);
        if code.variable_family && (0..MATH_FAMILY_COUNT as i64).contains(&selected) {
            family = selected;
        }
        MathField::Character {
            family,
            code: code.character,
            origins: Vec::new(),
        }
    }
}
