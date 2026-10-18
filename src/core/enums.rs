//! Native semantic discriminators translated from `core/enums.py`.
use serde::{Deserialize, Serialize};
use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InputStateKind {
    #[serde(rename = "mid_line")]
    MidLine,
    #[serde(rename = "skip_blanks")]
    SkipBlanks,
    #[serde(rename = "new_line")]
    NewLine,
    #[serde(rename = "token_list")]
    TokenList,
}
impl InputStateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MidLine => "mid_line",
            Self::SkipBlanks => "skip_blanks",
            Self::NewLine => "new_line",
            Self::TokenList => "token_list",
        }
    }
}
impl fmt::Display for InputStateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ListMode {
    #[serde(rename = "vertical")]
    Vertical,
    #[serde(rename = "horizontal")]
    Horizontal,
    #[serde(rename = "math")]
    Math,
    #[serde(rename = "disabled")]
    Disabled,
}
impl ListMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
            Self::Math => "math",
            Self::Disabled => "disabled",
        }
    }
}
impl fmt::Display for ListMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BoxOperation {
    #[serde(rename = "box")]
    Box,
    #[serde(rename = "copy")]
    Copy,
    #[serde(rename = "vsplit")]
    Vsplit,
    #[serde(rename = "lastbox")]
    LastBox,
    #[serde(rename = "vtop")]
    Vtop,
    #[serde(rename = "hbox")]
    Hbox,
    #[serde(rename = "vbox")]
    Vbox,
}
impl BoxOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Box => "box",
            Self::Copy => "copy",
            Self::Vsplit => "vsplit",
            Self::LastBox => "lastbox",
            Self::Vtop => "vtop",
            Self::Hbox => "hbox",
            Self::Vbox => "vbox",
        }
    }
}
impl fmt::Display for BoxOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum UnpackOperation {
    #[serde(rename = "unhbox")]
    Hbox,
    #[serde(rename = "unhcopy")]
    Hcopy,
    #[serde(rename = "unvbox")]
    Vbox,
    #[serde(rename = "unvcopy")]
    Vcopy,
    #[serde(rename = "pagediscards")]
    PageDiscards,
    #[serde(rename = "splitdiscards")]
    SplitDiscards,
}
impl UnpackOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hbox => "unhbox",
            Self::Hcopy => "unhcopy",
            Self::Vbox => "unvbox",
            Self::Vcopy => "unvcopy",
            Self::PageDiscards => "pagediscards",
            Self::SplitDiscards => "splitdiscards",
        }
    }
}
impl fmt::Display for UnpackOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DefinitionOperation {
    #[serde(rename = "def")]
    Def,
    #[serde(rename = "edef")]
    Edef,
    #[serde(rename = "gdef")]
    Gdef,
    #[serde(rename = "xdef")]
    Xdef,
}
impl DefinitionOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Def => "def",
            Self::Edef => "edef",
            Self::Gdef => "gdef",
            Self::Xdef => "xdef",
        }
    }
}
impl fmt::Display for DefinitionOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MathStyle {
    #[serde(rename = "display")]
    Display,
    #[serde(rename = "display_cramped")]
    DisplayCramped,
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "text_cramped")]
    TextCramped,
    #[serde(rename = "script")]
    Script,
    #[serde(rename = "script_cramped")]
    ScriptCramped,
    #[serde(rename = "scriptscript")]
    Scriptscript,
    #[serde(rename = "scriptscript_cramped")]
    ScriptscriptCramped,
}
impl MathStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::DisplayCramped => "display_cramped",
            Self::Text => "text",
            Self::TextCramped => "text_cramped",
            Self::Script => "script",
            Self::ScriptCramped => "script_cramped",
            Self::Scriptscript => "scriptscript",
            Self::ScriptscriptCramped => "scriptscript_cramped",
        }
    }
}
impl fmt::Display for MathStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CharacterCategory {
    Escape = 0,
    LeftBrace = 1,
    RightBrace = 2,
    MathShift = 3,
    TabMark = 4,
    CarRet = 5,
    MacParam = 6,
    SupMark = 7,
    SubMark = 8,
    Ignore = 9,
    Spacer = 10,
    Letter = 11,
    OtherChar = 12,
    ActiveChar = 13,
    Comment = 14,
    InvalidChar = 15,
}
impl CharacterCategory {
    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Escape),
            1 => Some(Self::LeftBrace),
            2 => Some(Self::RightBrace),
            3 => Some(Self::MathShift),
            4 => Some(Self::TabMark),
            5 => Some(Self::CarRet),
            6 => Some(Self::MacParam),
            7 => Some(Self::SupMark),
            8 => Some(Self::SubMark),
            9 => Some(Self::Ignore),
            10 => Some(Self::Spacer),
            11 => Some(Self::Letter),
            12 => Some(Self::OtherChar),
            13 => Some(Self::ActiveChar),
            14 => Some(Self::Comment),
            15 => Some(Self::InvalidChar),
            _ => None,
        }
    }
}
impl fmt::Display for CharacterCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", *self as u8)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Command {
    #[serde(rename = "relax")]
    Relax,
    #[serde(rename = "macro")]
    Macro,
    #[serde(rename = "undefined_cs")]
    UndefinedCs,
    #[serde(rename = "char_given")]
    CharGiven,
    #[serde(rename = "char_num")]
    CharNum,
    #[serde(rename = "math_char_num")]
    MathCharNum,
    #[serde(rename = "def")]
    Def,
    #[serde(rename = "make_box")]
    MakeBox,
    #[serde(rename = "let")]
    Let,
    #[serde(rename = "hskip")]
    Hskip,
    #[serde(rename = "vskip")]
    Vskip,
    #[serde(rename = "set_box")]
    SetBox,
    #[serde(rename = "un_hbox")]
    UnHbox,
    #[serde(rename = "box_dimen")]
    BoxDimen,
    #[serde(rename = "the")]
    The,
    #[serde(rename = "input")]
    Input,
    #[serde(rename = "convert")]
    Convert,
    #[serde(rename = "begin_group")]
    BeginGroup,
    #[serde(rename = "end_group")]
    EndGroup,
    #[serde(rename = "global")]
    Global,
    #[serde(rename = "noexpand")]
    Noexpand,
    #[serde(rename = "expandafter")]
    Expandafter,
    #[serde(rename = "register")]
    Register,
    #[serde(rename = "case_shift")]
    CaseShift,
    #[serde(rename = "last_item")]
    LastItem,
    #[serde(rename = "leaders")]
    Leaders,
    #[serde(rename = "ignore_spaces")]
    IgnoreSpaces,
    #[serde(rename = "protected_prefix")]
    ProtectedPrefix,
    #[serde(rename = "unless")]
    Unless,
    #[serde(rename = "expression")]
    Expression,
    #[serde(rename = "etex_item")]
    EtexItem,
    #[serde(rename = "pdf_convert")]
    PdfConvert,
    #[serde(rename = "pdf_extension")]
    PdfExtension,
    #[serde(rename = "pdf_item")]
    PdfItem,
    #[serde(rename = "ex_space")]
    ExSpace,
    #[serde(rename = "reset_timer")]
    ResetTimer,
    #[serde(rename = "glyph_to_unicode")]
    GlyphToUnicode,
    #[serde(rename = "dump")]
    Dump,
    #[serde(rename = "end_job")]
    EndJob,
    #[serde(rename = "set_interaction")]
    SetInteraction,
    #[serde(rename = "penalties")]
    Penalties,
    #[serde(rename = "special")]
    Special,
    #[serde(rename = "pdf_font_map")]
    PdfFontMap,
    #[serde(rename = "partoken_name")]
    PartokenName,
    #[serde(rename = "set_random_seed")]
    SetRandomSeed,
    #[serde(rename = "pdf_font_command")]
    PdfFontCommand,
    #[serde(rename = "xray")]
    Xray,
    #[serde(rename = "reg_alias")]
    RegAlias,
    #[serde(rename = "shorthand")]
    Shorthand,
    #[serde(rename = "arithmetic")]
    Arithmetic,
    #[serde(rename = "code_assign")]
    CodeAssign,
    #[serde(rename = "font_dimen")]
    FontDimen,
    #[serde(rename = "space_factor")]
    SpaceFactor,
    #[serde(rename = "after_assignment")]
    AfterAssignment,
    #[serde(rename = "after_group")]
    AfterGroup,
    #[serde(rename = "long_prefix")]
    LongPrefix,
    #[serde(rename = "cs_name")]
    CsName,
    #[serde(rename = "end_cs_name")]
    EndCsName,
    #[serde(rename = "outer_prefix")]
    OuterPrefix,
    #[serde(rename = "def_font")]
    DefFont,
    #[serde(rename = "set_font")]
    SetFont,
    #[serde(rename = "ital_corr")]
    ItalCorr,
    #[serde(rename = "font_int")]
    FontInt,
    #[serde(rename = "font_code")]
    FontCode,
    #[serde(rename = "message")]
    Message,
    #[serde(rename = "shift_box")]
    ShiftBox,
    #[serde(rename = "rule")]
    Rule,
    #[serde(rename = "accent")]
    Accent,
    #[serde(rename = "no_boundary")]
    NoBoundary,
    #[serde(rename = "ship_out")]
    ShipOut,
    #[serde(rename = "start_par")]
    StartPar,
    #[serde(rename = "prev_depth")]
    PrevDepth,
    #[serde(rename = "penalty")]
    Penalty,
    #[serde(rename = "discretionary")]
    Discretionary,
    #[serde(rename = "par_shape")]
    ParShape,
    #[serde(rename = "prev_graf")]
    PrevGraf,
    #[serde(rename = "hyph_data")]
    HyphData,
    #[serde(rename = "set_language")]
    SetLanguage,
    #[serde(rename = "in_stream")]
    InStream,
    #[serde(rename = "read_stream")]
    ReadStream,
    #[serde(rename = "out_stream")]
    OutStream,
    #[serde(rename = "immediate")]
    Immediate,
    #[serde(rename = "mark")]
    Mark,
    #[serde(rename = "mark_text")]
    MarkText,
    #[serde(rename = "page_dimen")]
    PageDimen,
    #[serde(rename = "page_int")]
    PageInt,
    #[serde(rename = "insert")]
    Insert,
    #[serde(rename = "math_family")]
    MathFamily,
    #[serde(rename = "math_char")]
    MathChar,
    #[serde(rename = "math_style")]
    MathStyle,
    #[serde(rename = "math_atom")]
    MathAtom,
    #[serde(rename = "math_fraction")]
    MathFraction,
    #[serde(rename = "math_left_right")]
    MathLeftRight,
    #[serde(rename = "math_construct")]
    MathConstruct,
    #[serde(rename = "math_limits")]
    MathLimits,
    #[serde(rename = "math_choice")]
    MathChoice,
    #[serde(rename = "math_glue")]
    MathGlue,
    #[serde(rename = "math_kern")]
    MathKern,
    #[serde(rename = "math_eqno")]
    MathEqno,
    #[serde(rename = "delimiter")]
    Delimiter,
    #[serde(rename = "align")]
    Align,
    #[serde(rename = "align_end")]
    AlignEnd,
    #[serde(rename = "align_span")]
    AlignSpan,
    #[serde(rename = "align_omit")]
    AlignOmit,
    #[serde(rename = "align_noalign")]
    AlignNoalign,
    #[serde(rename = "align_sentinel")]
    AlignSentinel,
    #[serde(rename = "vadjust")]
    Vadjust,
    #[serde(rename = "math_nonscript")]
    MathNonscript,
    #[serde(rename = "if_test")]
    IfTest,
    #[serde(rename = "fi_else")]
    FiElse,
    #[serde(rename = "parameter")]
    Parameter,
    #[serde(rename = "kern")]
    Kern,
    #[serde(rename = "remove_item")]
    RemoveItem,
    #[serde(rename = "par_end")]
    ParEnd,
}
impl Command {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relax => "relax",
            Self::Macro => "macro",
            Self::UndefinedCs => "undefined_cs",
            Self::CharGiven => "char_given",
            Self::CharNum => "char_num",
            Self::MathCharNum => "math_char_num",
            Self::Def => "def",
            Self::MakeBox => "make_box",
            Self::Let => "let",
            Self::Hskip => "hskip",
            Self::Vskip => "vskip",
            Self::SetBox => "set_box",
            Self::UnHbox => "un_hbox",
            Self::BoxDimen => "box_dimen",
            Self::The => "the",
            Self::Input => "input",
            Self::Convert => "convert",
            Self::BeginGroup => "begin_group",
            Self::EndGroup => "end_group",
            Self::Global => "global",
            Self::Noexpand => "noexpand",
            Self::Expandafter => "expandafter",
            Self::Register => "register",
            Self::CaseShift => "case_shift",
            Self::LastItem => "last_item",
            Self::Leaders => "leaders",
            Self::IgnoreSpaces => "ignore_spaces",
            Self::ProtectedPrefix => "protected_prefix",
            Self::Unless => "unless",
            Self::Expression => "expression",
            Self::EtexItem => "etex_item",
            Self::PdfConvert => "pdf_convert",
            Self::PdfExtension => "pdf_extension",
            Self::PdfItem => "pdf_item",
            Self::ExSpace => "ex_space",
            Self::ResetTimer => "reset_timer",
            Self::GlyphToUnicode => "glyph_to_unicode",
            Self::Dump => "dump",
            Self::EndJob => "end_job",
            Self::SetInteraction => "set_interaction",
            Self::Penalties => "penalties",
            Self::Special => "special",
            Self::PdfFontMap => "pdf_font_map",
            Self::PartokenName => "partoken_name",
            Self::SetRandomSeed => "set_random_seed",
            Self::PdfFontCommand => "pdf_font_command",
            Self::Xray => "xray",
            Self::RegAlias => "reg_alias",
            Self::Shorthand => "shorthand",
            Self::Arithmetic => "arithmetic",
            Self::CodeAssign => "code_assign",
            Self::FontDimen => "font_dimen",
            Self::SpaceFactor => "space_factor",
            Self::AfterAssignment => "after_assignment",
            Self::AfterGroup => "after_group",
            Self::LongPrefix => "long_prefix",
            Self::CsName => "cs_name",
            Self::EndCsName => "end_cs_name",
            Self::OuterPrefix => "outer_prefix",
            Self::DefFont => "def_font",
            Self::SetFont => "set_font",
            Self::ItalCorr => "ital_corr",
            Self::FontInt => "font_int",
            Self::FontCode => "font_code",
            Self::Message => "message",
            Self::ShiftBox => "shift_box",
            Self::Rule => "rule",
            Self::Accent => "accent",
            Self::NoBoundary => "no_boundary",
            Self::ShipOut => "ship_out",
            Self::StartPar => "start_par",
            Self::PrevDepth => "prev_depth",
            Self::Penalty => "penalty",
            Self::Discretionary => "discretionary",
            Self::ParShape => "par_shape",
            Self::PrevGraf => "prev_graf",
            Self::HyphData => "hyph_data",
            Self::SetLanguage => "set_language",
            Self::InStream => "in_stream",
            Self::ReadStream => "read_stream",
            Self::OutStream => "out_stream",
            Self::Immediate => "immediate",
            Self::Mark => "mark",
            Self::MarkText => "mark_text",
            Self::PageDimen => "page_dimen",
            Self::PageInt => "page_int",
            Self::Insert => "insert",
            Self::MathFamily => "math_family",
            Self::MathChar => "math_char",
            Self::MathStyle => "math_style",
            Self::MathAtom => "math_atom",
            Self::MathFraction => "math_fraction",
            Self::MathLeftRight => "math_left_right",
            Self::MathConstruct => "math_construct",
            Self::MathLimits => "math_limits",
            Self::MathChoice => "math_choice",
            Self::MathGlue => "math_glue",
            Self::MathKern => "math_kern",
            Self::MathEqno => "math_eqno",
            Self::Delimiter => "delimiter",
            Self::Align => "align",
            Self::AlignEnd => "align_end",
            Self::AlignSpan => "align_span",
            Self::AlignOmit => "align_omit",
            Self::AlignNoalign => "align_noalign",
            Self::AlignSentinel => "align_sentinel",
            Self::Vadjust => "vadjust",
            Self::MathNonscript => "math_nonscript",
            Self::IfTest => "if_test",
            Self::FiElse => "fi_else",
            Self::Parameter => "parameter",
            Self::Kern => "kern",
            Self::RemoveItem => "remove_item",
            Self::ParEnd => "par_end",
        }
    }
}
impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TokenProtection {
    #[serde(rename = "noexpand")]
    Noexpand,
}
impl TokenProtection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Noexpand => "noexpand",
        }
    }
}
impl fmt::Display for TokenProtection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    #[serde(rename = "hlist")]
    Hlist,
    #[serde(rename = "vlist")]
    Vlist,
    #[serde(rename = "rule")]
    Rule,
    #[serde(rename = "insert")]
    Insert,
    #[serde(rename = "mark")]
    Mark,
    #[serde(rename = "adjust")]
    Adjust,
    #[serde(rename = "ligature")]
    Ligature,
    #[serde(rename = "disc")]
    Disc,
    #[serde(rename = "whatsit")]
    Whatsit,
    #[serde(rename = "math")]
    Math,
    #[serde(rename = "glue")]
    Glue,
    #[serde(rename = "kern")]
    Kern,
    #[serde(rename = "margin_kern")]
    MarginKern,
    #[serde(rename = "penalty")]
    Penalty,
    #[serde(rename = "char")]
    Char,
}
impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hlist => "hlist",
            Self::Vlist => "vlist",
            Self::Rule => "rule",
            Self::Insert => "insert",
            Self::Mark => "mark",
            Self::Adjust => "adjust",
            Self::Ligature => "ligature",
            Self::Disc => "disc",
            Self::Whatsit => "whatsit",
            Self::Math => "math",
            Self::Glue => "glue",
            Self::Kern => "kern",
            Self::MarginKern => "margin_kern",
            Self::Penalty => "penalty",
            Self::Char => "char",
        }
    }
}
impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GroupKind {
    #[serde(rename = "bottom")]
    Bottom,
    #[serde(rename = "brace")]
    Brace,
    #[serde(rename = "hbox")]
    Hbox,
    #[serde(rename = "adjusted_hbox")]
    AdjustedHbox,
    #[serde(rename = "vbox")]
    Vbox,
    #[serde(rename = "vtop")]
    Vtop,
    #[serde(rename = "alignment")]
    Alignment,
    #[serde(rename = "no_align")]
    NoAlign,
    #[serde(rename = "output")]
    Output,
    #[serde(rename = "math_field")]
    MathField,
    #[serde(rename = "disc")]
    Disc,
    #[serde(rename = "insert")]
    Insert,
    #[serde(rename = "vcenter")]
    Vcenter,
    #[serde(rename = "math_choice")]
    MathChoice,
    #[serde(rename = "explicit")]
    Explicit,
    #[serde(rename = "math_shift")]
    MathShift,
    #[serde(rename = "math_left")]
    MathLeft,
}
impl GroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Brace => "brace",
            Self::Hbox => "hbox",
            Self::AdjustedHbox => "adjusted_hbox",
            Self::Vbox => "vbox",
            Self::Vtop => "vtop",
            Self::Alignment => "alignment",
            Self::NoAlign => "no_align",
            Self::Output => "output",
            Self::MathField => "math_field",
            Self::Disc => "disc",
            Self::Insert => "insert",
            Self::Vcenter => "vcenter",
            Self::MathChoice => "math_choice",
            Self::Explicit => "explicit",
            Self::MathShift => "math_shift",
            Self::MathLeft => "math_left",
        }
    }
}
impl fmt::Display for GroupKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WriteDestination {
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "log")]
    Log,
    #[serde(rename = "shell")]
    Shell,
}
impl WriteDestination {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Log => "log",
            Self::Shell => "shell",
        }
    }
}
impl fmt::Display for WriteDestination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AlignmentBoundary {
    #[serde(rename = "prefix_end")]
    PrefixEnd,
    #[serde(rename = "entry_end")]
    EntryEnd,
}
impl AlignmentBoundary {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrefixEnd => "prefix_end",
            Self::EntryEnd => "entry_end",
        }
    }
}
impl fmt::Display for AlignmentBoundary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl UnpackOperation {
    pub fn is_horizontal(self) -> bool {
        matches!(self, Self::Hbox | Self::Hcopy)
    }
    pub fn copies_contents(self) -> bool {
        matches!(self, Self::Hcopy | Self::Vcopy)
    }
    pub fn discard_source(self) -> Option<&'static str> {
        match self {
            Self::PageDiscards => Some("page"),
            Self::SplitDiscards => Some("split"),
            _ => None,
        }
    }
}
impl DefinitionOperation {
    pub fn is_global(self) -> bool {
        matches!(self, Self::Gdef | Self::Xdef)
    }
    pub fn expands_body(self) -> bool {
        matches!(self, Self::Edef | Self::Xdef)
    }
}
impl MathStyle {
    pub fn font_size(self) -> usize {
        match self {
            Self::Display | Self::DisplayCramped | Self::Text | Self::TextCramped => 0,
            Self::Script | Self::ScriptCramped => 1,
            Self::Scriptscript | Self::ScriptscriptCramped => 2,
        }
    }
    pub fn is_cramped(self) -> bool {
        matches!(
            self,
            Self::DisplayCramped
                | Self::TextCramped
                | Self::ScriptCramped
                | Self::ScriptscriptCramped
        )
    }
    pub fn choice_index(self) -> usize {
        match self {
            Self::Display | Self::DisplayCramped => 0,
            Self::Text | Self::TextCramped => 1,
            Self::Script | Self::ScriptCramped => 2,
            Self::Scriptscript | Self::ScriptscriptCramped => 3,
        }
    }
    pub fn is_script(self) -> bool {
        self.font_size() != 0
    }
    pub fn is_display(self) -> bool {
        self.choice_index() == 0
    }
    pub fn cramped(self) -> Self {
        match self {
            Self::Display => Self::DisplayCramped,
            Self::Text => Self::TextCramped,
            Self::Script => Self::ScriptCramped,
            Self::Scriptscript => Self::ScriptscriptCramped,
            other => other,
        }
    }
    pub fn superscript(self) -> Self {
        let smaller = if self.is_script() {
            Self::Scriptscript
        } else {
            Self::Script
        };
        if self.is_cramped() {
            smaller.cramped()
        } else {
            smaller
        }
    }
    pub fn subscript(self) -> Self {
        self.superscript().cramped()
    }
    pub fn numerator(self) -> Self {
        match self {
            Self::Display => Self::Text,
            Self::DisplayCramped => Self::TextCramped,
            Self::Text => Self::Script,
            Self::TextCramped => Self::ScriptCramped,
            Self::Script | Self::Scriptscript => Self::Scriptscript,
            Self::ScriptCramped | Self::ScriptscriptCramped => Self::ScriptscriptCramped,
        }
    }
    pub fn denominator(self) -> Self {
        self.numerator().cramped()
    }
}
