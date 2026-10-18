//! Classic show commands inspect actual meanings and ownership without consuming boxes.
use crate::{
    core::{
        constants::Constants as C,
        enums::{ListMode, NodeKind},
        errors::{Error, Result},
        units::print_scaled,
    },
    language::services::ListServices,
    typesetting::model::NodePayload,
};
pub trait ClassicServices: ListServices {
    fn font_identifier(&mut self, font: usize) -> Result<String>;
    fn show_meaning(&mut self) -> Result<(String, String)>;
    fn show_the(&mut self) -> Result<String>;
    fn box_register(&self, n: i64) -> i64;
    fn page_nodes(&self) -> Vec<i64>;
    fn show_event(&mut self, code: i64, lines: &[String]);
}
enum Visit {
    List(i64, i64),
    Node(i64, i64),
    Text(String),
}
pub fn box_lines(s: &mut impl ClassicServices, pointer: i64) -> Result<Vec<String>> {
    if pointer == C::NULL {
        return Ok(vec!["void".into()]);
    }
    let breadth = s.parameter("showboxbreadth", 5);
    let breadth = if breadth > 0 { breadth as usize } else { 5 };
    let depth = s.parameter("showboxdepth", 3);
    let mut out = Vec::new();
    let mut pending = vec![Visit::List(pointer, 0)];
    while let Some(task) = pending.pop() {
        match task {
            Visit::Text(text) => out.push(text),
            Visit::List(head, level) => {
                if head == C::NULL {
                    continue;
                }
                if level > depth {
                    out.push(format!("{} []", ".".repeat(level as usize)));
                    continue;
                }
                let siblings = s.builder().siblings(head)?;
                if siblings.len() > breadth {
                    pending.push(Visit::Text(format!("{}etc.", ".".repeat(level as usize))))
                }
                for &n in siblings.iter().take(breadth).rev() {
                    pending.push(Visit::Node(n, level));
                }
            }
            Visit::Node(pointer, level) => {
                let n = s.builder().node(pointer)?.clone();
                let prefix = ".".repeat(level as usize);
                let text = match n.node_type {
                    NodeKind::Hlist | NodeKind::Vlist => {
                        let mut text = format!(
                            "\\{}({}+{})x{}",
                            if n.node_type == NodeKind::Hlist {
                                "hbox"
                            } else {
                                "vbox"
                            },
                            print_scaled(n.height),
                            print_scaled(n.depth),
                            print_scaled(n.width)
                        );
                        if n.glue_sign != 0 {
                            text.push_str(&format!(
                                ", glue set {}/{}, order {}",
                                n.glue_sign * n.glue_numerator,
                                n.glue_denominator,
                                n.glue_order
                            ))
                        }
                        if n.shift != 0 {
                            text.push_str(&format!(", shifted {}", print_scaled(n.shift)))
                        }
                        pending.push(Visit::List(n.list_ptr, level + 1));
                        text
                    }
                    NodeKind::Char | NodeKind::Ligature => {
                        let mut text = format!(
                            "{} {}",
                            s.font_identifier(n.font)?,
                            char::from_u32(n.char)
                                .ok_or_else(|| Error::value("Invalid show character"))?
                        );
                        if n.node_type == NodeKind::Ligature {
                            text.push_str(&format!(
                                " (ligature {})",
                                n.components
                                    .iter()
                                    .filter_map(|c| char::from_u32(*c))
                                    .collect::<String>()
                            ))
                        }
                        text
                    }
                    NodeKind::Glue => {
                        pending.push(Visit::List(n.leader_ptr, level + 1));
                        format!(
                            "{}{}",
                            if n.leader_kind.is_empty() {
                                "\\glue ".into()
                            } else {
                                format!("\\{} ", n.leader_kind)
                            },
                            n.glue.unwrap_or_default().text(n.glue_kind == "mu")
                        )
                    }
                    NodeKind::Kern => format!("\\kern {}", print_scaled(n.width)),
                    NodeKind::Penalty => format!("\\penalty {}", n.penalty),
                    NodeKind::Rule => {
                        let dim = |v, r| if r { "*".into() } else { print_scaled(v) };
                        format!(
                            "\\rule({}+{})x{}",
                            dim(n.height, n.running_height),
                            dim(n.depth, n.running_depth),
                            dim(n.width, n.running_width)
                        )
                    }
                    NodeKind::Disc => {
                        for p in [n.list_ptr, n.post_break, n.pre_break] {
                            pending.push(Visit::List(p, level + 1));
                        }
                        "\\discretionary".into()
                    }
                    NodeKind::Insert | NodeKind::Adjust => {
                        pending.push(Visit::List(n.list_ptr, level + 1));
                        if let NodePayload::Insertion { number, .. } = n.payload {
                            format!("\\insert{number}")
                        } else {
                            "\\vadjust".into()
                        }
                    }
                    NodeKind::Mark => {
                        let NodePayload::Mark { number, ref tokens } = n.payload else {
                            return Err(Error::value("Invalid mark"));
                        };
                        let text = tokens
                            .iter()
                            .map(|&t| s.display_token(t))
                            .collect::<Result<Vec<_>>>()?
                            .join("");
                        format!("\\mark{number}{{{text}}}")
                    }
                    NodeKind::Whatsit => format!("\\{} {:?}", n.whatsit, n.payload),
                    NodeKind::Math => format!(
                        "\\math{}, surrounded {}",
                        if n.automatic { "on" } else { "off" },
                        print_scaled(n.width)
                    ),
                    _ => return Err(Error::value("Unknown node type in show")),
                };
                out.push(format!("{prefix}{text}"));
            }
        }
    }
    Ok(out)
}
pub fn show(s: &mut impl ClassicServices, code: i64) -> Result<()> {
    let lines = match code {
        0 => {
            s.raw_token()?;
            let (identity, meaning) = s.show_meaning()?;
            vec![format!("> {identity}={meaning}.")]
        }
        1 => {
            let n = s.scan_register(None)?;
            let mut lines = vec![format!("> \\box{n}=")];
            lines.extend(box_lines(s, s.box_register(n))?);
            lines
        }
        2 => vec![format!("> {}.", s.show_the()?)],
        _ => {
            let mut lines = Vec::new();
            let mut contexts = s.builder().context_stack.clone();
            contexts.push(s.builder().context.clone());
            for c in contexts {
                let name = match c.mode {
                    ListMode::Vertical => "vertical",
                    ListMode::Horizontal => "horizontal",
                    ListMode::Math => "math",
                    ListMode::Disabled => "disabled",
                };
                lines.push(format!(
                    "### {}{name} mode",
                    if c.restricted { "internal " } else { "" }
                ));
                if c.cur_list_head != C::NULL {
                    lines.extend(box_lines(s, c.cur_list_head)?);
                }
            }
            lines.push("### current page:".into());
            for n in s.page_nodes() {
                lines.extend(box_lines(s, n)?)
            }
            lines
        }
    };
    s.show_event(code, &lines);
    crate::resources::host::console_line(true, format!("{}", lines.join("\n")));
    Ok(())
}
