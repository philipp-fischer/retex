//! Schema-v2 snapshots of frozen pages, font bindings and diagnostic histories.
use crate::{
    core::{
        constants::Constants as C,
        errors::{Error, Result},
    },
    diagnostics::messages::Diagnostics,
    fonts::{catalog::FontCatalog, materialization::FontResource},
    typesetting::{display::PageDisplayList, layout::Layout, nodes::NodeBuilder},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
pub fn json_value(value: &impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| Error::value(e.to_string()))
}
pub fn ascii_json(value: &Value, pretty: bool) -> Result<String> {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|e| Error::value(e.to_string()))?;
    let mut output = String::new();
    for c in text.chars() {
        if c.is_ascii() {
            output.push(c)
        } else {
            let mut units = [0; 2];
            for unit in c.encode_utf16(&mut units) {
                output.push_str(&format!("\\u{unit:04x}"))
            }
        }
    }
    Ok(output)
}
pub fn layout_snapshot(
    layout: &Layout,
    builder: &NodeBuilder,
    fonts: &FontCatalog,
    diagnostics: &Diagnostics,
    status: &str,
    pages: Option<&[PageDisplayList]>,
    retained: usize,
    dropped: usize,
) -> Result<Value> {
    let mut resources = Vec::new();
    for (id, font) in fonts.items.iter().enumerate().skip(1) {
        let m = &font.materialization;
        let virtual_font=font.virtual_font.as_ref().map(|vf|json!({"file":vf.path,"local_fonts":vf.fonts,"packet_characters":vf.packets.keys().collect::<Vec<_>>()}));
        let unicode=font.unicode_font.as_ref().map(|f|json!({"file":f.path,"sha256":f.sha256,"units_per_em":f.upem,"glyph_count":f.glyph_names.len(),"outline_kind":"truetype","encoding_source":"original_unicode_clusters"}));
        let type1 = if let Some(FontResource::Type1(r)) = m.resource.as_deref() {
            let metrics = r
                .descriptor_metrics
                .iter()
                .cloned()
                .collect::<std::collections::BTreeMap<_, _>>();
            json!({"font_bbox":r.bbox,"italic_angle":r.italic_angle,"fixed_pitch":r.fixed_pitch,"font_matrix":r.matrix,"encoding_source":r.encoding_source,"descriptor_metric_source":"type1_dictionary_with_bbox_fallback","descriptor_metrics":metrics,"length1":r.length1,"length2":r.length2,"length3":r.length3})
        } else {
            Value::Null
        };
        let sfnt = if let Some(FontResource::Sfnt(r)) = m.resource.as_deref() {
            let metrics = r
                .descriptor_metrics
                .iter()
                .cloned()
                .collect::<std::collections::BTreeMap<_, _>>();
            json!({"font_bbox":r.bbox,"descriptor_metrics":metrics,"glyph_count":r.glyph_names.len(),"tables":r.tables.keys().map(|tag|tag.iter().map(|&c|char::from(c)).collect::<String>()).collect::<Vec<_>>()})
        } else {
            Value::Null
        };
        let bitmap = if let Some(FontResource::Bitmap(r)) = m.resource.as_deref() {
            json!({"dpi":r.dpi,"glyph_codes":r.glyphs.keys().collect::<Vec<_>>()})
        } else {
            Value::Null
        };
        resources.push(json!({"font_id":id,"name":font.name,"pdf_name":m.pdf_name,"size_sp":font.size,"design_size_sp":font.tfm_design_size,"pfb_file":m.pfb_file,"tfm_file":font.tfm_file,"encoding_file":m.enc_file,"metric_fallback":font.metric_mode()=="approximate","encoding_fallback":font.glyph_encoding().is_empty(),"encoding_source":if m.encoding.is_some(){"explicit"}else if m.native_encoding.is_some(){"type1"}else if font.afm.is_some(){"standard"}else{"fallback"},"parameter_count":font.parameter_count,"embedding_available":m.embedding_available,"map_bound":m.map_bound,"nonembedded":m.nonembedded,"map_flags":m.map_flags,"outline_kind":m.resource.as_deref().map(FontResource::kind),"virtual_font":virtual_font,"emitted_advance_available":m.embedding_available||font.afm.is_some(),"metric_mode":font.metric_mode(),"unicode_font":unicode,"resource_error":m.resource_error,"type1":type1,"sfnt":sfnt,"bitmap":bitmap,"space_sp":font.get_parameter(2),"x_height_sp":font.get_parameter(5),"quad_sp":font.get_parameter(6),"afm_file":font.afm.as_ref().map(|_|fonts.locations.font_metrics.join(format!("{}.afm",m.pdf_name)).to_string_lossy().into_owned())}));
    }
    let mut result = json!({"schema_version":2,"status":status,"errors":diagnostics.errors,"warnings":diagnostics.warnings,"diagnostics":diagnostics.records,"help":diagnostics.help,"units":{"layout":"sp","sp_per_pt":C::UNITY,"emission":"bp","conversion":"bp = sp * 800 / (803 * 65536)","magnification":"Placements and emission include page magnification; packed node metrics retain TeX dimensions"},"coordinates":{"layout_origin":"page_top_left","layout_y_direction":"down","layout_y_reference":"baseline","emission_origin":"page_bottom_left","emission_y_direction":"up","bounds":"logical_metrics_not_ink"},"startup_profile":if fonts.ini{"ini"}else{"standalone"},"exact_repro":fonts.exact_repro,"language_profile":fonts.language_profile,"compatibility_target":"repository pdfTeX 1.40.25 / e-TeX 2.6","limitations":["Logical placements follow the implemented paragraph, page, math and alignment constructors.","Glyph positions honor calculated kern/glue widths; glue carries spacing metadata.","Advances for unavailable or unsupported fonts remain unknown (null).","Fallback glyph names are labels/assumptions, not verified outline identities.","Raw PDF literals may alter ink/transforms beyond calculated placement diagnostics."],"fonts":resources,"pages":pages.map(|pages| json!(pages)).unwrap_or_else(|| json!(&layout.pages)),"stats":{"paragraphs":layout.paragraph_count,"live_nodes":builder.nodes.values().filter(|n|n.is_some()).count(),"peak_nodes":builder.peak_nodes,"allocated_nodes":builder.node_ptr,"reclaimed_nodes":builder.reclaimed_nodes,"retained_events":retained,"dropped_events":dropped}});
    let repaired: Vec<_> = diagnostics
        .records
        .iter()
        .filter(|d| d.details.as_ref().is_some_and(|x| x.recovery.is_some()))
        .collect();
    if !repaired.is_empty() {
        result["recovery"] = json!({"outcome":"completed_with_errors","count":repaired.len(),"approximate":true,"export_available":false});
    }
    Ok(result)
}
pub fn write_layout(filename: &Path, data: &Value) -> Result<()> {
    let mut text = ascii_json(data, true)?;
    text.push('\n');
    crate::resources::host::write(filename, text)?;
    Ok(())
}
