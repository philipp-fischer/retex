//! Owned input/scantokens service on the existing expansion/scanner stack.
use super::*;
use std::path::PathBuf;

pub(super) struct InputExecution {
    code: i64,
    directory: PathBuf,
    started: bool,
}
impl InputExecution {
    pub(super) fn new(core: &EngineCore) -> Self {
        Self {
            code: core.input.current.character().unwrap_or(0),
            directory: core
                .input
                .cursor
                .name
                .as_deref()
                .map(Path::new)
                .and_then(Path::parent)
                .unwrap_or(Path::new("."))
                .to_path_buf(),
            started: false,
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EvalView<'_>,
        returned: Option<Result<Value>>,
    ) -> Result<Step> {
        if !self.started {
            if returned.is_some() {
                return Err(Error::runtime("Unrequested input service result"));
            }
            self.started = true;
            if self.code == 1 {
                view.core.input.request_end_input();
                return Ok(Step::Complete(Value::Expansion(EV::Unit)));
            }
            return Ok(Step::Yield(Action::Evaluation(if self.code == 2 {
                Request::Expand(ER::GeneralText { expanded: false })
            } else {
                Request::Scan(SR::Filename)
            })));
        }
        let value = returned.ok_or_else(|| Error::runtime("Missing input service result"))?;
        if self.code == 2 {
            let Value::Expansion(value) = value? else {
                return Err(Error::runtime("Invalid scantokens result"));
            };
            let tokens = value.tokens()?;
            let text = crate::language::streams::display_tokens(view, &tokens)?;
            let newline = view.parameter("newlinechar", 10);
            let pseudo = crate::language::input_adapters::PseudoInput::new(
                &text,
                newline,
                view.core.config.language_profile == LanguageProfile::Unicode,
            );
            if !view.core.input.start_input(
                view.env,
                Path::new("<scantokens>"),
                None,
                Some(Box::new(pseudo)),
            ) {
                return Err(Error::value(
                    view.core.input.last_input_error.clone().unwrap_or_default(),
                ));
            }
            view.event("rescan", json!({"bytes":text.len()}));
        } else {
            let value = value.map_err(|error| {
                if error.kind == ErrorKind::Eof {
                    Error::value("Missing filename for input")
                } else {
                    error
                }
            })?;
            let Value::Scan(SV::Filename(filename)) = value else {
                return Err(Error::runtime("Invalid input filename result"));
            };
            if !view.core.input.start_input(
                view.env,
                Path::new(&filename),
                Some(&self.directory),
                None,
            ) {
                let mut error = Error::value(format!(
                    "Cannot open file: {}: {}",
                    filename.to_string_lossy(),
                    view.core.input.last_input_error.clone().unwrap_or_default()
                ));
                error.missing_resource = view.core.input.last_missing_resource.clone();
                return Err(error);
            }
            if view.env.reuse_physical_input.get().is_some() {
                let resolved = view.core.input.cursor.name.as_deref().map(PathBuf::from);
                let mut lookups = view.env.reuse_input_lookups.borrow_mut();
                if lookups.len() >= 512
                    || filename.as_os_str().len() > 4096
                    || self.directory.as_os_str().len() > 4096
                    || filename.to_str().is_none()
                    || self.directory.to_str().is_none()
                    || resolved.is_none()
                {
                    drop(lookups);
                    view.env
                        .reuse_rejected_by
                        .set(Some("input-lookup-contract"));
                    view.env.reject_reuse_observation();
                } else {
                    lookups.push(crate::language::state::ReuseInputLookup {
                        filename: filename.into(),
                        directory: self.directory.clone(),
                        resolved: resolved.unwrap(),
                    });
                }
            }
            view.event("file_open", json!({"file":view.core.input.cursor.name}));
        }
        Ok(Step::Complete(Value::Expansion(EV::Unit)))
    }
}
