//! Startup registration and original frozen meanings (`primitives.py`).
use super::character_tokens::Token;
use super::expansion::Expander;
use super::input::InputProcessor;
use super::records::{
    CommandOperand, Equivalence, MeaningCommand, Quantity, QuantityKind, QuantityReference,
    ValueKey,
};
use super::state::Environment;
use crate::core::constants::Constants as C;
use crate::core::enums::Command;
use crate::core::errors::{Error, Result};
use crate::typesetting::layout::Layout;
#[derive(Clone, Copy, Debug)]
pub struct StartupDate {
    pub hour: i64,
    pub minute: i64,
    pub day: i64,
    pub month: i64,
    pub year: i64,
}
pub fn register(
    env: &mut Environment,
    expansion: &mut Expander,
    name: &str,
    command: Command,
    value: CommandOperand,
) -> Result<()> {
    let cs = if name.chars().count() == 1 {
        C::SINGLE_BASE + name.chars().next().unwrap() as i64
    } else {
        env.intern(name, true)
    };
    env.define(cs, MeaningCommand::Operation(command), value.clone(), true);
    expansion.register_name(command, value.clone(), name.into());
    if !env.frozen_primitives.contains_key(&cs) {
        let mut frozen = C::FROZEN_PRIMITIVE - 1 - env.frozen_primitives.len() as i64;
        if frozen < C::HASH_BASE + C::HASH_SIZE {
            frozen = env.reserve_symbol()
        }
        env.frozen_primitives.insert(cs, frozen);
        env.font_identifiers.insert(frozen, name.into());
        env.define(frozen, MeaningCommand::Operation(command), value, true);
    }
    Ok(())
}
pub fn parameter(
    env: &mut Environment,
    expansion: &mut Expander,
    name: &str,
    kind: QuantityKind,
    default: Quantity,
) {
    let key = ValueKey::parameter(name);
    env.values.insert(key.clone(), default);
    register(
        env,
        expansion,
        name,
        Command::Parameter,
        CommandOperand::Quantity(QuantityReference { kind, key }),
    )
    .expect("Primitive registration is infallible");
}
pub fn alias(
    env: &mut Environment,
    expansion: &mut Expander,
    name: &str,
    original: &str,
) -> Result<()> {
    let cs = env.intern(original, true);
    let meaning = env
        .meaning(cs)
        .cloned()
        .ok_or_else(|| Error::value("Missing parameter alias target"))?;
    let MeaningCommand::Operation(command) = meaning.command else {
        return Err(Error::value("Invalid parameter alias"));
    };
    register(env, expansion, name, command, meaning.value)
}
pub fn initialize(
    env: &mut Environment,
    expansion: &mut Expander,
    input: &mut InputProcessor,
    layout: &Layout,
    date: StartupDate,
    ini: bool,
) -> Result<()> {
    super::primitive_data::initialize(env, expansion, layout, date, ini)?;
    let par = env.intern("par", true);
    input.par_loc = par;
    input.par_token = Token::control_sequence(par);
    env.font_identifiers
        .insert(C::FROZEN_PRIMITIVE, "pdfprimitive".into());
    let key = env.symbol_key(C::FROZEN_PRIMITIVE, true);
    env.meanings.insert(
        key.into(),
        Equivalence {
            command: MeaningCommand::Operation(Command::IgnoreSpaces),
            level: C::LEVEL_ZERO,
            value: CommandOperand::Number(1),
        }
        .into(),
    );
    expansion.register_name(
        Command::IgnoreSpaces,
        CommandOperand::Number(1),
        "pdfprimitive".into(),
    );
    if ini {
        env.values
            .insert(ValueKey::parameter("tolerance"), Quantity::Integer(10000));
        env.values
            .insert(ValueKey::parameter("maxdeadcycles"), Quantity::Integer(25));
    }
    // Python's final None output routine has the same meaning as an empty token list.
    env.values.insert(
        ValueKey::parameter("output"),
        Quantity::Tokens(std::sync::Arc::from([])),
    );
    Ok(())
}
