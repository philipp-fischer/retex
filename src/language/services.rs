//! Typed synchronous command operands supplied by the engine's continuation driver.
use super::character_tokens::Token;
use super::input::{ExpandedInput, InputProcessor};
use super::scanning::{ScanRequest, ScanValue};
use super::state::Environment;
use crate::core::errors::{Error, Result};
use crate::core::values::GlueSpec;
use crate::fonts::catalog::FontCatalog;
use std::sync::Arc;
pub trait LanguageServices: ExpandedInput {
    fn env(&self) -> &Environment;
    fn env_mut(&mut self) -> &mut Environment;
    fn fonts(&self) -> &FontCatalog;
    fn fonts_mut(&mut self) -> &mut FontCatalog;
    fn scan(&mut self, request: ScanRequest) -> Result<ScanValue>;
    fn general_text(&mut self, expanded: bool) -> Result<Arc<[Token]>>;
    fn get_r_token(&mut self) -> Result<()>;
    fn text(&mut self) -> Result<String>;
    fn event(&mut self, kind: &str, fields: serde_json::Value);
    fn parameter(&self, name: &str, default: i64) -> i64 {
        InputProcessor::parameter(self.env(), name, default)
    }
    fn scan_int(&mut self) -> Result<i64> {
        self.scan(ScanRequest::Int)?.integer()
    }
    fn scan_dimen(&mut self) -> Result<i64> {
        self.scan(ScanRequest::Dimen {
            mu: false,
            infinite: false,
        })?
        .integer()
    }
    fn scan_register(&mut self, maximum: Option<i64>) -> Result<i64> {
        self.scan(ScanRequest::Register(maximum))?.integer()
    }
    fn scan_char_num(&mut self) -> Result<u32> {
        Ok(self.scan(ScanRequest::CharNum)?.integer()? as u32)
    }
    fn scan_font_ident(&mut self) -> Result<usize> {
        if let ScanValue::Font(font) = self.scan(ScanRequest::FontIdent)? {
            Ok(font)
        } else {
            Err(Error::value("Invalid font scanner result"))
        }
    }
    fn scan_keyword(&mut self, keyword: &str) -> Result<bool> {
        self.scan(ScanRequest::Keyword(keyword.into()))?.flag()
    }
    fn optional_equals(&mut self) -> Result<()> {
        self.scan(ScanRequest::OptionalEquals)?;
        Ok(())
    }
    fn nonblank(&mut self) -> Result<()> {
        self.scan(ScanRequest::Nonblank)?;
        Ok(())
    }
    fn scan_glue(&mut self, mu: bool) -> Result<GlueSpec> {
        if let ScanValue::Glue(glue) = self.scan(ScanRequest::Glue { mu })? {
            Ok(glue)
        } else {
            Err(Error::value("Invalid glue scanner result"))
        }
    }
}
pub trait ListServices: LanguageServices {
    fn builder(&self) -> &crate::typesetting::nodes::NodeBuilder;
    fn builder_mut(&mut self) -> &mut crate::typesetting::nodes::NodeBuilder;
    fn display_token(&mut self, token: Token) -> Result<String>;
    fn warning(&mut self, message: &str);
}
