//! Standalone layout inspection never imports or executes mutable engine state.
use crate::core::errors::{Error, Result};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
pub fn load_layout(path: &Path) -> Result<Value> {
    let data: Value =
        serde_json::from_slice(&std::fs::read(path)?).map_err(|e| Error::value(e.to_string()))?;
    if data["schema_version"] != 2 {
        return Err(Error::value("Unsupported layout schema version"));
    }
    Ok(data)
}
fn array(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| Error::value("Invalid layout array"))
}
fn integer(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .ok_or_else(|| Error::value("Invalid layout integer"))
}
fn number(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .ok_or_else(|| Error::value("Invalid layout number"))
}
pub fn describe(data: &Value, selected: Option<i64>) -> Result<String> {
    let pages = array(&data["pages"])?;
    let mut rows = vec![
        format!(
            "Run: {}; pages: {}",
            data["status"].as_str().unwrap_or("unknown"),
            pages.len()
        ),
        "Calculated: scaled points / top-left; emitted: PDF big points / bottom-left.".into(),
    ];
    for page in pages {
        let p = integer(&page["page"])?;
        if selected.is_some_and(|n| n != p) {
            continue;
        }
        rows.push(format!("Page {p}"));
        for (i, line) in array(&page["lines"])?.iter().enumerate() {
            let mut depths = BTreeMap::new();
            let mut emissions = BTreeMap::new();
            for g in array(&line["emitted"])? {
                emissions.insert(integer(&g["placement_id"])?, g);
            }
            let mut heading = format!(
                "  Line {} baseline={}pt",
                i + 1,
                number(&line["baseline_y_sp"])? / 65536.0
            );
            let over = line["overfull_sp"].as_f64().unwrap_or(0.0);
            if over != 0.0 {
                heading.push_str(&format!(" overfull={}pt", over / 65536.0))
            }
            rows.push(heading);
            for item in array(&line["placements"])? {
                let id = integer(&item["placement_id"])?;
                let depth = item["parent_id"]
                    .as_i64()
                    .and_then(|p| depths.get(&p).copied())
                    .unwrap_or(-1_i64)
                    + 1;
                depths.insert(id, depth);
                let kind = item["kind"].as_str().unwrap_or("");
                let label = if kind == "glyph" {
                    format!(
                        " {:?} code={} font={}",
                        item["glyph_name"].as_str(),
                        item["code"],
                        item["font"]
                    )
                } else {
                    String::new()
                };
                let position = if let Some(g) = emissions.get(&id) {
                    if let Some(x) = g["x_bp"].as_f64() {
                        format!(" emitted_x={x}bp")
                    } else {
                        " emitted_x=unknown".into()
                    }
                } else {
                    String::new()
                };
                rows.push(format!(
                    "    {}{kind} placement={id} node={}{label} x={}pt width={}pt{position}",
                    "  ".repeat(depth as usize),
                    item["node_id"],
                    number(&item["x_sp"])? / 65536.0,
                    number(&item["width_sp"])? / 65536.0
                ));
            }
        }
    }
    for (key, label) in [("warnings", "Warning"), ("errors", "Error")] {
        if let Some(items) = data[key].as_array() {
            for item in items {
                rows.push(format!("{label}: {}", item.as_str().unwrap_or("")));
            }
        }
    }
    Ok(rows.join("\n"))
}
pub fn write_html(data: &Value, path: &Path) -> Result<()> {
    let json = serde_json::to_string(data)
        .map_err(|e| Error::value(e.to_string()))?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    std::fs::write(
        path,
        include_str!("inspector.html").replace("__LAYOUT_DATA__", &json),
    )?;
    Ok(())
}
pub fn main(args: &[String]) -> i32 {
    let result = (|| {
        let path = args
            .first()
            .ok_or_else(|| Error::value("Usage: inspect-layout LAYOUT [--page N] [--html PATH]"))?;
        let data = load_layout(Path::new(path))?;
        let mut page = None;
        let mut html = None;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--page" => {
                    i += 1;
                    page = Some(
                        args.get(i)
                            .ok_or_else(|| Error::value("Missing page"))?
                            .parse::<i64>()
                            .map_err(|_| Error::value("Invalid page"))?,
                    )
                }
                "--html" => {
                    i += 1;
                    html = Some(
                        args.get(i)
                            .ok_or_else(|| Error::value("Missing HTML path"))?,
                    )
                }
                _ => return Err(Error::value("Unknown inspector option")),
            }
            i += 1
        }
        if let Some(path) = html {
            write_html(&data, Path::new(path))?
        } else {
            println!("{}", describe(&data, page)?)
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("Cannot inspect layout: {e}");
        1
    } else {
        0
    }
}
