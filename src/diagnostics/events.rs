//! Bounded event retention. Live tracing runs before retention budgets.
use crate::core::errors::{Error, Result};
use serde_json::{Map, Value, json};
use std::{
    io::{BufWriter, Write},
    path::Path,
};
pub struct TraceRecorder {
    pub enabled: bool,
    pub limit: usize,
    pub events: Vec<Value>,
    pub dropped: usize,
    pub observer: Option<Box<dyn FnMut(&str, &Map<String, Value>)>>,
}
impl Default for TraceRecorder {
    fn default() -> Self {
        Self::new(false, 10000)
    }
}
impl TraceRecorder {
    pub fn new(enabled: bool, limit: usize) -> Self {
        Self {
            enabled,
            limit,
            events: Vec::new(),
            dropped: 0,
            observer: None,
        }
    }
    pub fn event(&mut self, kind: &str, fields: Map<String, Value>) {
        if let Some(observer) = &mut self.observer {
            observer(kind, &fields)
        }
        if !self.enabled {
            return;
        }
        if self.events.len() >= self.limit {
            self.dropped += 1;
            return;
        }
        let mut record = Map::new();
        record.insert("sequence".into(), json!(self.events.len()));
        record.insert("event".into(), json!(kind));
        record.extend(fields);
        self.events.push(Value::Object(record));
    }
    pub fn write_events(&self, path: &Path, status: &str) -> Result<()> {
        let mut w = BufWriter::new(crate::resources::host::open_output(path, false)?);
        for event in &self.events {
            serde_json::to_writer(&mut w, event).map_err(|e| Error::value(e.to_string()))?;
            w.write_all(b"\n")?;
        }
        serde_json::to_writer(
            &mut w,
            &json!({"event":"trace_end","status":status,"dropped_events":self.dropped}),
        )
        .map_err(|e| Error::value(e.to_string()))?;
        w.write_all(b"\n")?;
        w.flush()?;
        Ok(())
    }
}
