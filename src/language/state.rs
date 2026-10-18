//! Control-sequence identities and ordered scoped assignments (`language/state.py`).
use super::character_tokens::Token;
use super::groups::{GroupFrame, Restoration, RestorationKey, SavedValue};
use super::records::{CommandOperand, Equivalence, MeaningCommand, Quantity, ValueKey};
use crate::core::capacities::Capacities;
use crate::core::constants::Constants as C;
use crate::core::enums::{Command, GroupKind};
use crate::core::errors::{Error, Result};
use crate::core::persistence::{PersistentComponent, SharedTable};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(test)]
mod meaning_storage_tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn immutable_meaning_cells_preserve_legacy_map_bytes() {
        let legacy = BTreeMap::from([
            (
                "control-word:α".to_string(),
                Equivalence {
                    command: MeaningCommand::Operation(Command::IgnoreSpaces),
                    level: 1,
                    value: CommandOperand::Number(-17),
                },
            ),
            (
                "control-word:text".to_string(),
                Equivalence {
                    command: MeaningCommand::Operation(Command::IgnoreSpaces),
                    level: 3,
                    value: CommandOperand::Text("café".into()),
                },
            ),
        ]);
        let shared: SharedTable<Arc<str>, Arc<Equivalence>> = legacy
            .iter()
            .map(|(key, value)| (Arc::from(key.as_str()), Arc::new(value.clone())))
            .collect();
        let bytes = bincode::serialize(&legacy).unwrap();
        assert_eq!(bincode::serialize(&shared).unwrap(), bytes);
        let restored: SharedTable<Arc<str>, Arc<Equivalence>> =
            bincode::deserialize(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) enum ReuseStateEffect {
    Value(ValueKey, Quantity, bool),
    Meaning(String, MeaningCommand, CommandOperand, bool),
    FontSymbol { number: usize, id: i64 },
    FontName { id: i64, name: String },
}
#[derive(Default)]
pub(crate) struct ReuseEffectJournal {
    pub effects: Vec<(u64, ReuseStateEffect)>,
    pub values: std::collections::BTreeSet<ValueKey>,
    pub meanings: std::collections::BTreeSet<Arc<str>>,
    pub bytes: usize,
}
/// Arena actions occur at the same assignment/restoration boundary as in Python.
pub trait StateServices {
    fn copy_box(&mut self, pointer: i64) -> Result<i64>;
    fn reclaim_unreachable(&mut self, environment_roots: &[i64]);
    fn restored(&mut self, key: &RestorationKey);
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    pub values: super::observed_table::ObservedTable<ValueKey, Quantity>,
    pub font_identifiers: SharedTable<i64, String>,
    pub frozen_primitives: SharedTable<i64, i64>,
    pub symbols: SharedTable<i64, String>,
    pub symbol_keys: SharedTable<i64, String>,
    pub symbol_next: i64,
    pub font_symbols: SharedTable<usize, i64>,
    pub meanings: super::observed_table::ObservedTable<Arc<str>, Arc<Equivalence>>,
    pub boxes: super::observed_table::ObservedTable<i64, i64>,
}
#[derive(Clone)]
pub(crate) struct ReuseInputLookup {
    pub filename: std::path::PathBuf,
    pub directory: std::path::PathBuf,
    pub resolved: std::path::PathBuf,
}
#[derive(Clone)]
pub struct Environment {
    pub(crate) local_prefix_armed: std::cell::Cell<bool>,
    pub(crate) local_expansion_epoch: std::cell::Cell<u64>,
    pub(crate) reuse_effects:
        std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<ReuseEffectJournal>>>>,
    pub(crate) reuse_sequence: std::cell::RefCell<Option<std::rc::Rc<std::cell::Cell<u64>>>>,
    pub(crate) reuse_font_names:
        std::cell::RefCell<std::collections::BTreeMap<i64, Option<String>>>,
    pub(crate) reuse_font_construction_allowed: std::cell::Cell<bool>,
    // None forbids input; Some tracks whether this segment actually used it.
    pub(crate) reuse_physical_input: std::cell::Cell<Option<bool>>,
    pub(crate) reuse_input_lookups: std::cell::RefCell<Vec<ReuseInputLookup>>,
    pub(crate) reuse_font_writes: std::cell::RefCell<std::collections::BTreeSet<i64>>,
    pub(crate) reuse_font_symbol_outputs: std::cell::RefCell<std::collections::BTreeSet<i64>>,
    pub(crate) reuse_font_symbol_inputs:
        std::cell::RefCell<std::collections::BTreeMap<i64, String>>,
    pub(crate) reuse_ignore_meanings: std::cell::Cell<bool>,
    pub(crate) reuse_outer_prev: std::cell::RefCell<std::collections::BTreeMap<usize, i64>>,
    pub(crate) reuse_base_context_depth: std::cell::Cell<usize>,
    pub(crate) reuse_line_sensitive: std::cell::Cell<bool>,
    pub(crate) reuse_stateful: std::cell::Cell<bool>,
    pub(crate) reuse_paragraph_level: std::cell::Cell<Option<usize>>,
    pub(crate) reuse_rejected_by: std::cell::Cell<Option<&'static str>>,
    #[cfg(feature = "latency-diagnostics")]
    pub(crate) reuse_rejection_site: std::cell::Cell<Option<(&'static str, u32)>>,
    #[cfg(feature = "latency-diagnostics")]
    pub(crate) expansion_packet_probe: std::cell::Cell<Option<(u64, u8)>>,
    pub(crate) reuse_boundary: std::cell::Cell<bool>,
    pub(crate) reuse_expansions: std::cell::Cell<Option<(u64, bool)>>,
    pub(crate) reuse_symbol_keys: std::cell::RefCell<std::collections::BTreeMap<i64, String>>,
    pub(crate) reuse_symbols: std::cell::RefCell<std::collections::BTreeMap<i64, String>>,
    pub capacities: Capacities,
    pub symbols: SharedTable<i64, String>,
    pub symbol_ids: SharedTable<String, i64>,
    pub next_symbol: i64,
    pub meanings: super::observed_table::ObservedTable<Arc<str>, Arc<Equivalence>>,
    pub symbol_keys: SharedTable<i64, String>,
    pub cur_level: usize,
    pub cur_group: GroupKind,
    pub groups: Vec<GroupFrame>,
    pub value_levels: SharedTable<RestorationKey, usize>,
    pub values: super::observed_table::ObservedTable<ValueKey, Quantity>,
    pub register_max: i64,
    pub box_registers: super::observed_table::ObservedTable<i64, i64>,
    pub font_identifiers: SharedTable<i64, String>,
    pub font_symbols: SharedTable<usize, i64>,
    pub frozen_primitives: SharedTable<i64, i64>,
    character_ids: SharedTable<(bool, u32), i64>,
}
impl Default for Environment {
    fn default() -> Self {
        Self::new(Capacities::default())
    }
}
impl Environment {
    pub fn new(capacities: Capacities) -> Self {
        Self {
            local_prefix_armed: Default::default(),
            local_expansion_epoch: Default::default(),
            reuse_effects: Default::default(),
            reuse_sequence: Default::default(),
            reuse_font_names: Default::default(),
            reuse_font_construction_allowed: std::cell::Cell::new(false),
            reuse_physical_input: std::cell::Cell::new(None),
            reuse_input_lookups: Default::default(),
            reuse_font_writes: Default::default(),
            reuse_font_symbol_outputs: Default::default(),
            reuse_font_symbol_inputs: Default::default(),
            reuse_ignore_meanings: std::cell::Cell::new(false),
            reuse_outer_prev: Default::default(),
            reuse_base_context_depth: std::cell::Cell::new(0),
            reuse_line_sensitive: std::cell::Cell::new(false),
            reuse_stateful: std::cell::Cell::new(false),
            reuse_paragraph_level: std::cell::Cell::new(None),
            reuse_rejected_by: std::cell::Cell::new(None),
            #[cfg(feature = "latency-diagnostics")]
            reuse_rejection_site: std::cell::Cell::new(None),
            #[cfg(feature = "latency-diagnostics")]
            expansion_packet_probe: std::cell::Cell::new(None),
            reuse_boundary: std::cell::Cell::new(false),
            reuse_expansions: std::cell::Cell::new(None),
            reuse_symbol_keys: Default::default(),
            reuse_symbols: Default::default(),
            capacities,
            symbols: SharedTable::new(),
            symbol_ids: SharedTable::new(),
            next_symbol: C::FONT_ID_BASE + 9000 + 1024,
            meanings: Default::default(),
            symbol_keys: SharedTable::new(),
            cur_level: C::LEVEL_ONE,
            cur_group: GroupKind::Bottom,
            groups: Vec::new(),
            value_levels: SharedTable::new(),
            values: Default::default(),
            register_max: 32767,
            box_registers: Default::default(),
            font_identifiers: SharedTable::new(),
            font_symbols: SharedTable::new(),
            frozen_primitives: SharedTable::new(),
            character_ids: SharedTable::new(),
        }
    }
    fn record_reuse_effect(&self, bytes: usize, effect: ReuseStateEffect) {
        let Some(journal) = self.reuse_effects.borrow().clone() else {
            self.reject_reuse_observation();
            return;
        };
        let mut journal = journal.borrow_mut();
        if bytes > 64 * 1024
            || journal.bytes.saturating_add(bytes) > 4 * 1024 * 1024
            || journal.effects.len() >= 1024
        {
            self.reject_reuse_observation();
            journal.effects.clear();
            return;
        }
        let Some(sequence) = self.reuse_sequence.borrow().clone() else {
            self.reject_reuse_observation();
            return;
        };
        let order = sequence.get();
        sequence.set(order + 1);
        journal.bytes += bytes;
        journal.effects.push((order, effect));
    }
    #[cfg_attr(feature = "latency-diagnostics", track_caller)]
    pub(crate) fn reject_reuse_observation(&self) {
        #[cfg(feature = "latency-diagnostics")]
        if self.reuse_rejection_site.get().is_none() {
            let site = std::panic::Location::caller();
            self.reuse_rejection_site
                .set(Some((site.file(), site.line())));
        }
        if self.reuse_rejected_by.get().is_none() {
            self.reuse_rejected_by.set(Some("state-read"));
        }
        self.reuse_expansions.set(None);
        self.reuse_symbols.borrow_mut().clear();
        self.values.cancel_reads();
        self.meanings.cancel_reads();
        self.box_registers.cancel_reads();
    }
    pub(crate) fn observe_expansion(&self, command: Command) {
        if let Some((count, valid)) = self.reuse_expansions.get() {
            let allowed = command == Command::Macro
                || (self.reuse_stateful.get()
                    && command == Command::Input
                    && self.reuse_physical_input.get().is_some())
                || (self.reuse_stateful.get()
                    && matches!(
                        command,
                        Command::IfTest
                            | Command::FiElse
                            | Command::Unless
                            | Command::Noexpand
                            | Command::Expandafter
                            | Command::CsName
                            | Command::The
                            | Command::Convert
                            | Command::PdfConvert
                    ));
            if !valid || !allowed || (self.reuse_stateful.get() && count >= 8192) {
                self.reuse_rejected_by.set(Some(command.as_str()));
                self.reject_reuse_observation();
            } else {
                self.reuse_expansions
                    .set(Some((count + u64::from(command == Command::Macro), true)));
            }
        }
    }
    pub fn reserve_symbol(&mut self) -> i64 {
        let address = self.next_symbol;
        self.next_symbol += 1;
        address
    }
    pub fn intern(&mut self, spelling: &str, create: bool) -> i64 {
        if let Some(&address) = self.symbol_ids.get(spelling) {
            return address;
        }
        if !create {
            return C::NULL_CS;
        }
        let address = self.reserve_symbol();
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::active() {
            crate::output_diagnostics::trigger(format!("intern:{spelling}"));
        }
        self.symbols.insert(address, spelling.into());
        self.symbol_ids.insert(spelling.into(), address);
        address
    }
    pub fn symbol_key(&mut self, cs: i64, remember: bool) -> String {
        if let Some(key) = self.symbol_keys.get(&cs) {
            return key.clone();
        }
        let key = if (C::ACTIVE_BASE..C::SINGLE_BASE).contains(&cs) {
            format!("active:{}", char::from((cs - C::ACTIVE_BASE) as u8))
        } else if (C::SINGLE_BASE..C::NULL_CS).contains(&cs) {
            format!("control-symbol:{}", char::from((cs - C::SINGLE_BASE) as u8))
        } else if cs == C::NULL_CS {
            "control-word:".into()
        } else if let Some(name) = self.symbols.get(&cs).filter(|name| !name.is_empty()) {
            format!("control-word:{name}")
        } else if let Some((&number, _)) = self
            .font_symbols
            .iter()
            .find(|(_, address)| **address == cs)
        {
            format!("font:{number}")
        } else if let Some((&public, _)) = self
            .frozen_primitives
            .iter()
            .find(|(_, frozen)| **frozen == cs)
        {
            format!("frozen:{}", self.symbol_key(public, false))
        } else {
            let key = format!("private:{cs}");
            if !remember {
                return key;
            }
            key
        };
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::active() {
            crate::output_diagnostics::trigger(format!("symbol_key:{key}"));
        }
        if self.reuse_stateful.get() {
            if self.reuse_symbol_keys.borrow().len() >= 1024 {
                self.reject_reuse_observation();
            } else {
                self.reuse_symbol_keys.borrow_mut().insert(cs, key.clone());
            }
        }
        self.symbol_keys.insert(cs, key.clone());
        key
    }
    pub fn character_symbol(&mut self, code: u32, active: bool, create: bool) -> i64 {
        if code <= 255 {
            return (if active {
                C::ACTIVE_BASE
            } else {
                C::SINGLE_BASE
            }) + code as i64;
        }
        if let Some(&address) = self.character_ids.get(&(active, code)) {
            return address;
        }
        let key = format!(
            "{}{}",
            if active { "active:" } else { "control-symbol:" },
            char::from_u32(code).expect("validated scalar")
        );
        let address = if let Some((&address, _)) =
            self.symbol_keys.iter().find(|(_, known)| **known == key)
        {
            address
        } else {
            if !create {
                return C::NULL_CS;
            }
            let address = self.reserve_symbol();
            self.symbol_keys.insert(address, key);
            address
        };
        self.character_ids.insert((active, code), address);
        address
    }
    pub fn active_character(&self, symbol: i64) -> Option<u32> {
        if (C::ACTIVE_BASE..C::SINGLE_BASE).contains(&symbol) {
            return Some((symbol - C::ACTIVE_BASE) as u32);
        }
        self.symbol_keys
            .get(&symbol)?
            .strip_prefix("active:")?
            .chars()
            .next()
            .map(|c| c as u32)
    }
    pub(crate) fn font_identifier(&self, cs: i64) -> Option<&String> {
        let value = self.font_identifiers.get(&cs);
        if self.reuse_stateful.get() && !self.reuse_font_writes.borrow().contains(&cs) {
            let mut reads = self.reuse_font_names.borrow_mut();
            if reads.len() >= 1024 {
                self.reject_reuse_observation();
            } else {
                reads.entry(cs).or_insert_with(|| value.cloned());
            }
        }
        value
    }
    pub fn cs_name(&self, cs: i64) -> String {
        if let Some(name) = self.font_identifier(cs) {
            return name.clone();
        }
        if (C::ACTIVE_BASE..C::SINGLE_BASE).contains(&cs) {
            return char::from((cs - C::ACTIVE_BASE) as u8).to_string();
        }
        if (C::SINGLE_BASE..C::NULL_CS).contains(&cs) {
            return char::from((cs - C::SINGLE_BASE) as u8).to_string();
        }
        if let Some(key) = self.symbol_keys.get(&cs) {
            if let Some(name) = key
                .strip_prefix("control-symbol:")
                .or_else(|| key.strip_prefix("active:"))
            {
                return name.into();
            }
        }
        if cs > 0 {
            self.symbols.get(&cs).cloned().unwrap_or_default()
        } else {
            String::new()
        }
    }
    fn observe_symbol(&mut self, symbol: i64) {
        if self.reuse_expansions.get().is_some() {
            let key = self.symbol_key(symbol, false);
            if key.len() > 4096 || self.reuse_symbols.borrow().len() >= 1024 {
                self.reuse_expansions.set(None);
                self.reuse_symbols.borrow_mut().clear();
                self.values.cancel_reads();
                self.meanings.cancel_reads();
            } else {
                self.reuse_symbols.borrow_mut().entry(symbol).or_insert(key);
            }
        }
    }
    pub fn meaning(&mut self, symbol: i64) -> Option<&Equivalence> {
        self.observe_symbol(symbol);
        if let Some(key) = self.symbol_keys.get(&symbol) {
            return (if self.reuse_ignore_meanings.get() {
                self.meanings.get_for_assignment(key.as_str())
            } else {
                self.meanings.get(key.as_str())
            })
            .map(Arc::as_ref);
        }
        let key = self.symbol_key(symbol, false);
        (if self.reuse_ignore_meanings.get() {
            self.meanings.get_for_assignment(key.as_str())
        } else {
            self.meanings.get(key.as_str())
        })
        .map(Arc::as_ref)
    }
    pub(crate) fn meaning_value(
        &mut self,
        symbol: i64,
    ) -> Option<(MeaningCommand, CommandOperand)> {
        self.observe_symbol(symbol);
        if let Some(key) = self.symbol_keys.get(&symbol) {
            return (if self.meanings.recording() {
                (if self.reuse_ignore_meanings.get() {
                    self.meanings.get_for_assignment(key.as_str())
                } else {
                    self.meanings.get(key.as_str())
                })
                .cloned()
            } else {
                super::meaning_memo::resolve(self.meanings.raw(), symbol, key)
            })
            .map(|value| (value.command, value.value.clone()));
        }
        self.meaning(symbol)
            .map(|value| (value.command, value.value.clone()))
    }
    pub fn meaning_key(&self, symbol: &str) -> Option<&Equivalence> {
        self.meanings.get(symbol).map(Arc::as_ref)
    }
    pub fn original_primitive(&mut self, cs: i64) -> Option<&Equivalence> {
        let frozen = *self.frozen_primitives.get(&cs)?;
        self.meaning(frozen)
    }
    pub fn font_symbol(&mut self, number: usize) -> i64 {
        let address = if let Some(&address) = self.font_symbols.get(&number) {
            address
        } else {
            let address = if number <= 9000 {
                C::FONT_ID_BASE + number as i64
            } else {
                self.reserve_symbol()
            };
            self.font_symbols.insert(number, address);
            address
        };
        if self.reuse_stateful.get() {
            if number > 9000 || address != C::FONT_ID_BASE + number as i64 {
                self.reject_reuse_observation();
            } else {
                if self.reuse_font_symbol_outputs.borrow_mut().insert(address) {
                    if let Some(key) = self.reuse_symbols.borrow().get(&address).cloned() {
                        self.reuse_font_symbol_inputs
                            .borrow_mut()
                            .entry(address)
                            .or_insert(key);
                    }
                }
                self.record_reuse_effect(
                    128,
                    ReuseStateEffect::FontSymbol {
                        number,
                        id: address,
                    },
                );
            }
        }
        self.symbol_keys.insert(address, format!("font:{number}"));
        address
    }
    pub fn font_token(&mut self, number: usize, name: Option<&str>) -> Token {
        let cs = self.font_symbol(number);
        if let Some(name) = name {
            if self.reuse_stateful.get() {
                self.reuse_font_writes.borrow_mut().insert(cs);
                self.record_reuse_effect(
                    name.len() + 128,
                    ReuseStateEffect::FontName {
                        id: cs,
                        name: name.into(),
                    },
                );
            }
            self.font_identifiers.insert(cs, name.into());
            self.define(
                cs,
                MeaningCommand::Operation(Command::SetFont),
                CommandOperand::Number(number as i64),
                true,
            );
        }
        Token::control_sequence(cs)
    }
    pub fn begin_group(&mut self, kind: &str, group_type: Option<GroupKind>) -> Result<()> {
        let selected = match group_type {
            Some(group) => group,
            None => match kind {
                "brace" => GroupKind::Brace,
                "box" => GroupKind::Hbox,
                "alignment" | "alignment_entry" => GroupKind::Alignment,
                "explicit" => GroupKind::Explicit,
                "math_field" => GroupKind::MathField,
                "math_shift" | "math_eqno" => GroupKind::MathShift,
                "math_left" => GroupKind::MathLeft,
                _ => return Err(Error::value("Unknown group kind")),
            },
        };
        self.groups.push(GroupFrame::new(kind, self.cur_group));
        self.cur_group = selected;
        self.cur_level += 1;
        Ok(())
    }
    fn set_value_level(&mut self, key: RestorationKey, level: usize) {
        if level == C::LEVEL_ONE {
            self.value_levels.remove(&key);
        } else {
            self.value_levels.insert(key, level);
        }
    }
    fn save(&mut self, key: RestorationKey, value: SavedValue, global: bool) {
        let level = self.value_levels.get(&key).copied().unwrap_or(C::LEVEL_ONE);
        if !global && level != self.cur_level {
            if let Some(frame) = self.groups.last_mut() {
                frame.entries.push(Restoration {
                    key: key.clone(),
                    value,
                    level,
                });
            }
        }
        self.set_value_level(key, if global { C::LEVEL_ONE } else { self.cur_level });
    }
    pub fn save_after(&mut self, token: Token) {
        if let Some(frame) = self.groups.last_mut() {
            frame.after_tokens.push(token);
        }
    }
    pub fn define(
        &mut self,
        cs: i64,
        command: MeaningCommand,
        value: CommandOperand,
        global: bool,
    ) {
        let key = self.symbol_key(cs, true);
        self.define_key(key, command, value, global);
    }
    pub fn define_key(
        &mut self,
        key: String,
        command: MeaningCommand,
        value: CommandOperand,
        global: bool,
    ) {
        #[cfg(feature = "latency-diagnostics")]
        let _assignment = crate::output_diagnostics::detailed()
            .then(|| crate::output_diagnostics::operation("state_assignment"));
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::active() {
            crate::output_diagnostics::trigger(format!("meaning:{key}"));
            crate::output_diagnostics::assignment(&key);
            crate::output_diagnostics::event(
                "define",
                format!("{key}|{command:?}|global={global}|level={}", self.cur_level),
            );
        }
        if self.reuse_stateful.get() {
            use super::observed_table::ObservationSize;
            self.meanings.mark_assignment(&Arc::from(key.as_str()));
            if let Some(journal) = self.reuse_effects.borrow().as_ref() {
                journal
                    .borrow_mut()
                    .meanings
                    .insert(Arc::from(key.as_str()));
            }
            if global || self.reuse_paragraph_level.get() == Some(self.cur_level) {
                let estimate = Arc::new(Equivalence {
                    command,
                    value: value.clone(),
                    level: C::LEVEL_ONE,
                })
                .observation_size();
                self.record_reuse_effect(
                    estimate,
                    ReuseStateEffect::Meaning(key.clone(), command, value.clone(), global),
                );
            }
        }
        let previous = self
            .meanings
            .get_for_assignment(key.as_str())
            .map(Arc::as_ref);
        let default = Equivalence::default();
        let record = previous.unwrap_or(&default);
        if !global && record.command == command && record.value == value {
            return;
        }
        if !global && record.level != self.cur_level {
            if let Some(frame) = self.groups.last_mut() {
                frame.entries.push(Restoration {
                    key: RestorationKey::Meaning(key.clone()),
                    value: SavedValue::Meaning(previous.cloned()),
                    level: record.level,
                });
            }
        }
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::active() {
            let redundant_global = global
                && record.level == C::LEVEL_ONE
                && record.command == command
                && record.value == value;
            if redundant_global {
                crate::output_diagnostics::count("redundant_global_definitions", 1);
            }
            if crate::output_diagnostics::detailed() {
                crate::output_diagnostics::event(
                    "define_effect",
                    format!(
                        "{key}|{command:?}|{}|global={global}|level={}|redundant_global={redundant_global}",
                        crate::output_diagnostics::operand(&value, self),
                        self.cur_level
                    ),
                );
            }
        }
        self.meanings.insert_assigned(
            key.into(),
            Arc::new(Equivalence {
                command,
                value,
                level: if global { C::LEVEL_ONE } else { self.cur_level },
            }),
        );
    }
    pub fn assignment_global(&self, requested: bool) -> bool {
        match self.values.get(&ValueKey::parameter("globaldefs")) {
            Some(Quantity::Integer(setting)) if *setting != 0 => *setting > 0,
            _ => requested,
        }
    }
    pub fn assign_value(&mut self, key: ValueKey, value: Quantity, global: bool) {
        if self.reuse_stateful.get()
            && matches!(&key.index,super::records::ValueIndex::Name(name) if key.table=="parameter" && name.starts_with("tracing"))
            && value != Quantity::Integer(0)
        {
            self.reject_reuse_observation();
        }
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::detailed() {
            crate::output_diagnostics::event(
                "assign_value",
                format!("{key:?}|{value:?}|global={global}|level={}", self.cur_level),
            );
        }
        #[cfg(feature = "latency-diagnostics")]
        let _assignment = crate::output_diagnostics::detailed()
            .then(|| crate::output_diagnostics::operation("state_assignment"));
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::active() {
            crate::output_diagnostics::trigger(format!("value:{key:?}"));
        }
        if self.reuse_stateful.get() {
            use super::observed_table::ObservationSize;
            self.values.mark_assignment(&key);
            if let Some(journal) = self.reuse_effects.borrow().as_ref() {
                journal.borrow_mut().values.insert(key.clone());
            }
            if global || self.reuse_paragraph_level.get() == Some(self.cur_level) {
                let cost = value.observation_size();
                if cost <= 64 * 1024 {
                    self.record_reuse_effect(
                        cost,
                        ReuseStateEffect::Value(key.clone(), value.clone(), global),
                    );
                } else {
                    self.reject_reuse_observation();
                }
            }
        }
        let old = self.values.get_for_assignment(&key);
        if !global {
            if let Quantity::Integer(number) = &value {
                if old == Some(&value)
                    || (old.is_none()
                        && ["count", "dimen"].contains(&key.table.as_str())
                        && *number == 0)
                {
                    return;
                }
            }
        }
        self.save(
            RestorationKey::Value(key.clone()),
            SavedValue::Value(old.cloned()),
            global,
        );
        self.values.insert_assigned(key, value);
    }
    pub fn assign_box(
        &mut self,
        register: i64,
        node: i64,
        global: bool,
        services: Option<&mut dyn StateServices>,
    ) -> Result<()> {
        if self.reuse_stateful.get() {
            if global {
                self.reuse_rejected_by.set(Some("global-box-assignment"));
                self.reject_reuse_observation();
            }
            self.box_registers.mark_assignment(&register);
        }
        self.validate_register(register)?;
        let old = self
            .box_registers
            .get_for_assignment(&register)
            .copied()
            .unwrap_or(C::NULL);
        if !global && old == node {
            return Ok(());
        }
        #[cfg(feature = "latency-diagnostics")]
        if crate::output_diagnostics::active() {
            crate::output_diagnostics::trigger(format!("box_assign:{register}"));
            crate::output_diagnostics::event(
                "box_assign",
                format!(
                    "register={register}|global={global}|void={}",
                    node == C::NULL
                ),
            );
        }
        self.save(RestorationKey::Box(register), SavedValue::Box(old), global);
        if node == C::NULL {
            self.box_registers.remove_assigned(&register);
        } else {
            self.box_registers.insert_assigned(register, node);
        }
        if let Some(services) = services {
            services.reclaim_unreachable(&self.box_roots());
        }
        Ok(())
    }
    fn validate_register(&self, register: i64) -> Result<()> {
        if !(0..=self.register_max).contains(&register) {
            Err(Error::value("Bad box register"))
        } else {
            Ok(())
        }
    }
    pub fn take_box(
        &mut self,
        register: i64,
        copy: bool,
        services: Option<&mut dyn StateServices>,
    ) -> Result<i64> {
        self.validate_register(register)?;
        let pointer = self
            .box_registers
            .get(&register)
            .copied()
            .unwrap_or(C::NULL);
        if copy && pointer != C::NULL {
            return services
                .ok_or_else(|| Error::runtime("No list arena connected"))?
                .copy_box(pointer);
        }
        if !copy {
            self.box_registers.remove_assigned(&register);
        }
        Ok(pointer)
    }
    pub fn box_roots(&self) -> Vec<i64> {
        let mut roots: Vec<_> = self.box_registers.raw().values().copied().collect();
        for saved in &self.groups {
            for entry in &saved.entries {
                if let SavedValue::Box(pointer) = entry.value {
                    roots.push(pointer);
                }
            }
        }
        roots
    }
    pub fn assign_font(&mut self, current: &mut usize, number: usize, global: bool) {
        if self.reuse_stateful.get()
            && (global || self.reuse_paragraph_level.get() == Some(self.cur_level))
        {
            self.reuse_rejected_by.set(Some("escaping-font-selection"));
            self.reject_reuse_observation();
        }
        if !global && *current == number {
            return;
        }
        self.save(RestorationKey::Font, SavedValue::Font(*current), global);
        *current = number;
    }
    pub fn end_group(
        &mut self,
        kind: &str,
        current_font: &mut usize,
        mut services: Option<&mut dyn StateServices>,
    ) -> Result<Vec<Token>> {
        let frame = self
            .groups
            .last()
            .ok_or_else(|| Error::value("Extra group closing token"))?;
        if frame.kind != kind {
            return Err(Error::value(format!(
                "Mismatched group closing token: expected {}",
                frame.kind
            )));
        }
        let saved = self.groups.pop().expect("validated group");
        self.cur_group = saved.previous_group;
        let mut box_roots_changed = false;
        for entry in saved.entries.into_iter().rev() {
            box_roots_changed |= matches!(entry.key, RestorationKey::Box(_));
            if let RestorationKey::Meaning(key) = &entry.key {
                if self
                    .meanings
                    .get(key.as_str())
                    .is_some_and(|current| current.level == C::LEVEL_ONE)
                {
                    continue;
                }
                #[cfg(feature = "latency-diagnostics")]
                if crate::output_diagnostics::active() {
                    crate::output_diagnostics::trigger(format!("restore_meaning:{key}"));
                    crate::output_diagnostics::event("restore_meaning", key.clone());
                }
                if let SavedValue::Meaning(value) = entry.value {
                    if let Some(value) = value {
                        self.meanings
                            .insert_assigned(key.as_str().into(), Arc::new(value));
                    } else {
                        self.meanings.remove_assigned(key.as_str());
                    }
                }
            } else {
                if self
                    .value_levels
                    .get(&entry.key)
                    .copied()
                    .unwrap_or(C::LEVEL_ONE)
                    == C::LEVEL_ONE
                {
                    continue;
                }
                #[cfg(feature = "latency-diagnostics")]
                if crate::output_diagnostics::active() {
                    crate::output_diagnostics::trigger(format!("restore:{:?}", entry.key));
                }
                self.set_value_level(entry.key.clone(), entry.level);
                match (&entry.key, entry.value) {
                    (RestorationKey::Value(key), SavedValue::Value(value)) => {
                        if let Some(value) = value {
                            self.values.insert_assigned(key.clone(), value);
                        } else {
                            self.values.remove_assigned(key);
                        }
                    }
                    (RestorationKey::Box(register), SavedValue::Box(value)) => {
                        if value == C::NULL {
                            self.box_registers.remove_assigned(register);
                        } else {
                            self.box_registers.insert_assigned(*register, value);
                        }
                    }
                    (RestorationKey::Font, SavedValue::Font(value)) => *current_font = value,
                    _ => unreachable!("restoration key and value disagree"),
                }
            }
            match &entry.key {
                RestorationKey::Value(key) => self.values.restored(key),
                RestorationKey::Meaning(key) => self.meanings.restored(key.as_str()),
                RestorationKey::Box(register) => self.box_registers.restored(register),
                RestorationKey::Font => {}
            }
            if let Some(services) = services.as_deref_mut() {
                services.restored(&entry.key);
            }
        }
        self.cur_level -= 1;
        if box_roots_changed {
            if let Some(services) = services {
                services.reclaim_unreachable(&self.box_roots());
            }
        }
        Ok(saved.after_tokens)
    }
}
impl PersistentComponent for Environment {
    type State = EnvironmentSnapshot;
    fn dump_state(&self) -> Self::State {
        EnvironmentSnapshot {
            values: self.values.clone(),
            font_identifiers: self.font_identifiers.clone(),
            frozen_primitives: self.frozen_primitives.clone(),
            symbols: self.symbols.clone(),
            symbol_keys: self
                .symbol_keys
                .iter()
                .filter(|(_, key)| self.meanings.contains_key(key.as_str()))
                .map(|(&address, key)| (address, key.clone()))
                .collect(),
            symbol_next: self.next_symbol,
            font_symbols: self.font_symbols.clone(),
            meanings: self.meanings.clone(),
            boxes: self.box_registers.clone(),
        }
    }
    fn restore_state(&mut self, state: Self::State) -> Result<()> {
        self.values = state.values;
        self.font_identifiers = state.font_identifiers;
        self.frozen_primitives = state.frozen_primitives;
        self.symbol_ids = state
            .symbols
            .iter()
            .map(|(&address, name)| (name.clone(), address))
            .collect();
        self.symbols = state.symbols;
        self.symbol_keys = state.symbol_keys;
        self.font_symbols = state.font_symbols;
        self.meanings = state.meanings;
        self.next_symbol = self.next_symbol.max(state.symbol_next);
        self.box_registers = state.boxes;
        self.character_ids.clear();
        Ok(())
    }
}
