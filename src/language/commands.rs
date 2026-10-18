//! Command traits translated from `language/commands.py`.
use crate::core::enums::Command;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandTraits {
    pub assignment: bool,
    pub expandable: bool,
    pub prefix: bool,
}
pub fn traits(command: Command) -> CommandTraits {
    CommandTraits {
        assignment: matches!(
            command,
            Command::Def
                | Command::Let
                | Command::SetBox
                | Command::Register
                | Command::RegAlias
                | Command::Parameter
                | Command::Shorthand
                | Command::CodeAssign
                | Command::FontDimen
                | Command::SpaceFactor
                | Command::PrevDepth
                | Command::Arithmetic
                | Command::BoxDimen
                | Command::DefFont
                | Command::SetFont
                | Command::FontInt
                | Command::FontCode
                | Command::ParShape
                | Command::PrevGraf
                | Command::HyphData
                | Command::ReadStream
                | Command::MathFamily
                | Command::PageInt
                | Command::PageDimen
                | Command::Penalties
                | Command::SetInteraction
        ),
        expandable: matches!(
            command,
            Command::Macro
                | Command::CsName
                | Command::Input
                | Command::Convert
                | Command::PdfConvert
                | Command::The
                | Command::Noexpand
                | Command::Expandafter
                | Command::Unless
                | Command::UndefinedCs
                | Command::IfTest
                | Command::FiElse
                | Command::MarkText
        ),
        prefix: matches!(
            command,
            Command::Global | Command::LongPrefix | Command::OuterPrefix | Command::ProtectedPrefix
        ),
    }
}
