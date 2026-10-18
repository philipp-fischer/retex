//! Job-owned color stacks and affine shipment transformations (`graphics.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiteralPaint {
    pub literal_mode: String,
    pub literal_text: String,
    pub x_sp: i64,
    pub baseline_y_sp: i64,
    pub kind: String,
}
impl LiteralPaint {
    pub fn new(mode: String, text: String, x: i64, y: i64) -> Self {
        Self {
            literal_mode: mode,
            literal_text: text,
            x_sp: x,
            baseline_y_sp: y,
            kind: "literal".into(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}
impl Default for Affine {
    fn default() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}
impl Affine {
    pub fn point(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    pub fn vector(self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y, self.b * x + self.d * y)
    }
    pub fn then(self, m: Self) -> Self {
        let Self { a, b, c, d, e, f } = self;
        Self {
            a: a * m.a + b * m.c,
            b: a * m.b + b * m.d,
            c: c * m.a + d * m.c,
            d: c * m.b + d * m.d,
            e: e * m.a + f * m.c + m.e,
            f: e * m.b + f * m.d + m.f,
        }
    }
    pub fn around(text: &str, x: f64, y: f64) -> Result<Self> {
        let values: Vec<f64> = text
            .split_whitespace()
            .map(|s| {
                s.parse().map_err(|_| {
                    Error::value("PDF matrix requires four numeric coefficients")
                        .with_issue(crate::core::recovery::Issue::PdfOperation)
                })
            })
            .collect::<Result<_>>()?;
        let [a, b, c, d] = <[f64; 4]>::try_from(values).map_err(|_| {
            Error::value("PDF matrix requires four numeric coefficients")
                .with_issue(crate::core::recovery::Issue::PdfOperation)
        })?;
        if ![a, b, c, d].iter().all(|n| n.is_finite()) {
            return Err(Error::value("PDF matrix coefficients must be finite")
                .with_issue(crate::core::recovery::Issue::PdfOperation));
        }
        Ok(Self {
            a,
            b,
            c,
            d,
            e: x * (1.0 - a) - y * c,
            f: y * (1.0 - d) - x * b,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColorStack {
    pub initial: String,
    pub mode: String,
    pub page_start: bool,
    pub page: Vec<String>,
    pub form: Vec<String>,
}
impl ColorStack {
    pub fn new(initial: String, mode: String, page_start: bool) -> Self {
        Self {
            page: vec![initial.clone()],
            form: vec![initial.clone()],
            initial,
            mode,
            page_start,
        }
    }
    pub fn apply(
        &mut self,
        action: &str,
        text: &str,
        page: bool,
        warn: &mut dyn FnMut(&str),
    ) -> Result<(String, String)> {
        let values = if page { &mut self.page } else { &mut self.form };
        match action {
            "set" => *values.last_mut().expect("initial color") = text.into(),
            "push" => values.push(text.into()),
            "pop" => {
                if values.len() == 1 {
                    warn("Cannot pop an empty PDF color stack");
                    return Ok((self.mode.clone(), String::new()));
                }
                values.pop();
            }
            "current" => {}
            _ => {
                return Err(Error::value(format!(
                    "Unknown color-stack action: {action}"
                )));
            }
        }
        Ok((
            self.mode.clone(),
            values.last().expect("current color").clone(),
        ))
    }
}
