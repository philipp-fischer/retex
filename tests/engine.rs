//! End-to-end contracts across scanning, ownership, layout and serialization.
use retex_rs2::{
    Engine,
    core::{characters::LanguageProfile, config::EngineConfig, constants::Constants as C},
    engine::RunProgress,
    fonts::unicode::UnicodeFont,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Job {
    directory: PathBuf,
}
impl Job {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "retex-rs2-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        Self { directory }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
    fn engine(&self, config: EngineConfig) -> Engine {
        let mut config = config;
        if config.font_dir.is_none() {
            config.font_dir = Some(
                repo()
                    .join("tests/fixtures/resources")
                    .display()
                    .to_string(),
            );
        }
        Engine::new(config).unwrap()
    }
    fn run(&self, source: &str, config: EngineConfig, success: bool) -> Engine {
        let mut engine = self.engine(config);
        self.execute(&mut engine, source, success);
        engine
    }
    fn execute(&self, engine: &mut Engine, source: &str, success: bool) {
        let path = self.path("input.tex");
        std::fs::write(&path, source).unwrap();
        assert!(
            engine.start_input(&path),
            "{:?}",
            engine.core.input.last_input_error
        );
        assert_eq!(
            engine.main_body(),
            success,
            "{:?}",
            engine.core.diagnostics.errors
        );
        assert!(
            engine.builder.nodes.values().all(Option::is_none),
            "Owned nodes leaked after job: {source}"
        );
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_owned()
}
fn config() -> EngineConfig {
    EngineConfig {
        exact_repro: true,
        interaction_mode: Some(0),
        ..EngineConfig::default()
    }
}

#[test]
fn terminal_reads_fail_without_prompting_in_every_interaction_mode() {
    let job = Job::new();
    for mode in ["batchmode", "nonstopmode", "scrollmode", "errorstopmode"] {
        let engine = job.run(
            &format!("\\{mode}\\read-1 to\\answer\\end"),
            config(),
            false,
        );
        assert!(
            engine
                .core
                .diagnostics
                .errors
                .iter()
                .any(|error| error.contains("Terminal input is not supported"))
        );
    }
}

#[test]
fn package_errors_and_terminal_writes_drop_prompts_but_preserve_diagnostics() {
    let job = Job::new();
    let engine = job.run(
        "\\immediate\\write16{! LaTeX Error: File `missing.cls' not found.^^JType X to quit or <RETURN> to proceed,^^Jor enter new name. (Default extension: cls)}\n\\errhelp{Fix the package option.^^JTry typing <return> to proceed.}\n\\errmessage{Package sample Error: Bad option.^^JType H <return> for immediate help}\\end",
        EngineConfig { recovery_policy: retex_rs2::core::recovery::RecoveryPolicy::Interactive, ..config() },
        true,
    );
    assert!(engine.recovered());
    let messages = serde_json::to_string(&engine.core.diagnostics.records).unwrap();
    assert!(messages.contains("Bad option"));
    assert!(messages.contains("Fix the package option"));
    assert!(engine.core.diagnostics.terminal_log.contains("missing.cls"));
    assert!(!messages.to_ascii_lowercase().contains("<return>"));
    assert!(
        !engine
            .core
            .diagnostics
            .terminal_log
            .to_ascii_lowercase()
            .contains("<return>")
    );
}
fn glyphs(engine: &Engine) -> String {
    engine
        .core
        .output_pages
        .iter()
        .flat_map(|p| &p.lines)
        .flat_map(|l| &l.placements)
        .filter(|p| p.kind == "glyph")
        .map(|p| char::from_u32(p.code).unwrap())
        .collect()
}

#[test]
fn retained_paint_identity_depends_on_page_local_glyphs_and_physical_fonts() {
    use retex_rs2::{fonts::outlines::OutlineCache, pdf::preview::capture};
    let job = Job::new();
    let mut c = config();
    c.font_dir = Some(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/fonts/assets")
            .display()
            .to_string(),
    );
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets/cmr10.tfm"),
        job.path("painting.tfm"),
    )
    .unwrap();
    let source = r"\pdfinfoomitdate=1 \pdftrailerid{}\pdfmapline{+painting CMR10 <cmr10.pfb}\font\roman=painting \roman \shipout\hbox{A}\shipout\hbox{B}\end";
    let original = job.run(source, c.clone(), true);
    let changed = job.run(&source.replace("hbox{B}", "hbox{C}"), c.clone(), true);
    let mut cache = OutlineCache::default();
    let before = capture(&original, &mut cache).unwrap();
    let after = capture(&changed, &mut cache).unwrap();
    assert_eq!(before.pages.len(), 2);
    assert_eq!(
        before.pages[0].content_sha256,
        after.pages[0].content_sha256
    );
    assert_ne!(
        before.pages[1].content_sha256,
        after.pages[1].content_sha256
    );
    assert_ne!(
        serde_json::to_value(&before.fonts).unwrap(),
        serde_json::to_value(&after.fonts).unwrap()
    );
    // Existing operators stay identical when physical font bytes change. The
    // identity must still force repainting the page that uses that resource.
    let different = job.run(&source.replace("<cmr10.pfb", "<cmbx10.pfb"), c, true);
    let different = capture(&different, &mut cache).unwrap();
    assert_eq!(before.pages[0].operators, different.pages[0].operators);
    assert_ne!(
        before.pages[0].content_sha256,
        different.pages[0].content_sha256
    );
}
#[test]
fn retained_forms_include_nested_paint_fonts_geometry_and_page_dependencies() {
    use retex_rs2::{fonts::outlines::OutlineCache, pdf::preview::capture};
    let job = Job::new();
    let mut c = config();
    c.font_dir = Some(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/fonts/assets")
            .display()
            .to_string(),
    );
    let source = r"\pdfinfoomitdate=1 \pdftrailerid{}\font\roman=cmr10 \roman \shipout\hbox{A}\setbox0=\hbox{\pdfliteral direct{0.123456 0 0 1 0.234567 0 cm}F}\pdfxform attr{/Custom 42 /Matrix [1 0 0 1 0.123456 0.234567] /BBox [-1 -2 10.345678 12.456789]}0\edef\first{\the\pdflastxform}\setbox0=\hbox{N\pdfrefxform\first}\pdfxform0\edef\second{\the\pdflastxform}\shipout\hbox{\pdfrefxform\second}\shipout\hbox{\pdfrefxform\second}\end";
    let original = job.run(source, c.clone(), true);
    let changed = job.run(&source.replace("cm}F", "cm}G"), c.clone(), true);
    let mut cache = OutlineCache::default();
    let before = capture(&original, &mut cache).unwrap();
    let after = capture(&changed, &mut cache).unwrap();
    assert_eq!(
        before.pages[0].content_sha256,
        after.pages[0].content_sha256
    );
    for index in [1, 2] {
        assert_ne!(
            before.pages[index].content_sha256,
            after.pages[index].content_sha256
        );
        let paint = before.pages[index].operators.join("\n");
        assert_eq!(paint.matches("retexFormBegin").count(), 2);
        assert!(paint.contains("0.123456 0 0 1 0.234567 0 cm"));
        assert!(paint.contains("1 0 0 1 0.123456 0.234567 cm"));
        assert!(paint.contains("-1 -2 11.345678 14.456789 re W n"));
        assert!(!paint.contains(" Do"));
    }
    let codes = before
        .fonts
        .values()
        .flat_map(|font| font.paths.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(codes.is_superset(&std::collections::BTreeSet::from([65, 70, 78])));
    let override_font = job.run(
        &source.replace("12.456789]}0", "12.456789]}resources{/Font <<>>}0"),
        c,
        true,
    );
    let error = capture(&override_font, &mut cache).err().unwrap();
    assert!(error.to_string().contains("font/XObject override"));
}

#[test]
fn failed_form_shipment_releases_its_box_and_context() {
    // Unchanged original Python object-API source and assertion contract.
    let job = Job::new();
    let mut c = config();
    c.ini = true;
    let engine = job.run(
        r"\catcode123=1 \catcode125=2 \setbox0=\hbox{\pdfsave}\immediate\pdfxform0\end",
        c,
        false,
    );
    assert!(
        engine
            .core
            .diagnostics
            .errors
            .iter()
            .any(|error| error.contains("Unmatched PDF save"))
    );
    let document = &engine.core.backend.document;
    assert!(document.forms.values().all(|form| form.box_ptr == C::NULL));
    assert!(document.current_page.is_none());
    assert!(document.saved_positions.is_empty());
}

fn math_setup() -> String {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets");
    format!(
        r"\font\r=cmr10 \font\m={} \font\s={} \font\e={} \textfont0=\r \scriptfont0=\r \scriptscriptfont0=\r \textfont1=\m \scriptfont1=\m \scriptscriptfont1=\m \textfont2=\s \scriptfont2=\s \scriptscriptfont2=\s \textfont3=\e \scriptfont3=\e \scriptscriptfont3=\e \r ",
        assets.join("cmmi10.tfm").display(),
        assets.join("cmsy10.tfm").display(),
        assets.join("cmex10.tfm").display()
    )
}
#[test]
fn macro_scopes_expansion_and_expressions() {
    let job = Job::new();
    let e=job.run(r"\def\a#1{[#1]}\count0=7 {\advance\count0 by2 \gdef\saved{\a{B}}}\edef\value{\number\numexpr\count0*3+1\relax}\shipout\hbox{\saved\value}\end",config(),true);
    assert_eq!(glyphs(&e), "[B]22");
}
#[test]
fn reopening_input_closes_before_filename_expansion_observes_eof() {
    let job = Job::new();
    let mut settings = config();
    settings.output_directory = Some(job.directory.display().to_string());
    let source = r"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=probe.aux \immediate\write1{Existing}\immediate\closeout1\openin1=probe.aux \openin1=\ifeof1 missing\else probe\fi.aux \ifeof1 Closed\else Open\fi\par\end";
    let mut fresh = job.run(source, settings.clone(), true);
    // Independent Python closes the existing stream before scanning its name.
    // Reopening must not expose the previous buffered stream.
    assert_eq!(glyphs(&fresh), "Closed");
    assert!(fresh.core.streams.at_eof(1));
    let expected_pdf = fresh.export_pdf_bytes().unwrap().unwrap();
    for budget in [1, 3, 32] {
        let mut engine = job.engine(settings.clone());
        assert!(engine.start_input(&job.path("input.tex")));
        engine.begin_run().unwrap();
        loop {
            match engine.advance_run(budget).unwrap() {
                RunProgress::Yielded => {}
                RunProgress::Complete => break,
                RunProgress::Failed => panic!("{:?}", engine.core.diagnostics.errors),
            }
        }
        assert_eq!(glyphs(&engine), "Closed");
        assert!(engine.core.streams.at_eof(1));
        assert_eq!(engine.export_pdf_bytes().unwrap().unwrap(), expected_pdf);
    }
}

#[test]
fn inclusion_keeps_directory_through_macro_and_parent_eof() {
    let job = Job::new();
    std::fs::create_dir(job.path("nested")).unwrap();
    std::fs::write(job.path("nested/child.tex"), "CHILD ").unwrap();
    std::fs::write(
        job.path("nested/parent.tex"),
        r"\def\load{\input child}\load",
    )
    .unwrap();
    let e = job.run(
        r"BEFORE \input nested/parent \relax AFTER\end",
        config(),
        true,
    );
    assert_eq!(glyphs(&e), "BEFORECHILDAFTER");
}
#[test]
fn explicit_open_file_budget_fails_and_unwinds() {
    let job = Job::new();
    std::fs::write(job.path("child.tex"), r"\input child ").unwrap();
    let mut c = config();
    c.capacities.push(("max_in_open".into(), 12));
    let e = job.run(r"\input child ", c, false);
    assert!(
        e.core
            .diagnostics
            .errors
            .iter()
            .any(|e| e.contains("Too many open files"))
    );
    assert_eq!(e.core.input.in_open, 0);
}
#[test]
fn deep_alignment_keeps_nested_cell_roots() {
    let job = Job::new();
    let depth = 1100;
    let source = format!(
        r"\shipout\vbox{{{}\hbox{{A}}{}}}\end",
        r"\halign{#\cr\vbox{".repeat(depth),
        r"}\cr}".repeat(depth)
    );
    let e = job.run(&source, config(), true);
    assert_eq!(glyphs(&e), "A");
}
#[test]
fn output_callbacks_can_consume_display_and_alignment_transfers() {
    let job = Job::new();
    for (body, expected) in [
        (
            r"$$\halign{#\cr\noalign{\penalty0\kern2pt\penalty-10000}A\cr B\cr}$$",
            "AB",
        ),
        (r"$$A\leqno B$$", "BA"),
        (
            r"A\vadjust pre{\penalty0\kern2pt}\par B\vadjust{\penalty0\kern3pt}\par",
            "AB",
        ),
    ] {
        let source = format!(
            r"{}\vsize=12pt\output={{\setbox0=\vbox{{\unvbox255}}\shipout\box0}}{body}\end",
            math_setup()
        );
        for quantum in [1, 17] {
            let mut engine = job.engine(config());
            std::fs::write(job.path("input.tex"), &source).unwrap();
            assert!(engine.start_input(&job.path("input.tex")));
            engine.begin_run().unwrap();
            loop {
                match engine.advance_run(quantum).unwrap() {
                    RunProgress::Yielded => (),
                    progress => {
                        assert_eq!(progress, RunProgress::Complete, "{:?}", engine.core.failure);
                        break;
                    }
                }
            }
            assert_eq!(glyphs(&engine), expected);
            assert!(engine.builder.nodes.values().all(Option::is_none));
        }
    }
}
#[test]
fn deep_math_and_failed_fields_release_all_material() {
    let job = Job::new();
    let setup = math_setup();
    let source = format!(
        r"{setup}\shipout\hbox{{$ {}x{} $}}\end",
        r"\overline{".repeat(1200),
        "}".repeat(1200)
    );
    let e = job.run(&source, config(), true);
    assert_eq!(glyphs(&e), "x");
    let source = format!(
        r"{setup}\hbox{{$ {}\hbox{{owned}}\undefined",
        r"\overline{".repeat(1100)
    );
    let e = job.run(&source, config(), false);
    assert!(e.env.groups.is_empty());
    assert!(
        e.core
            .diagnostics
            .errors
            .iter()
            .any(|e| e.contains("Undefined control sequence"))
    );
}
#[test]
fn output_routine_resets_dead_cycles_and_ships_pages() {
    let job = Job::new();
    let e=job.run(r"\vsize=20pt\output={\shipout\box255}\noindent First\par\noindent Second\par\noindent Third\par\end",config(),true);
    assert_eq!(glyphs(&e), "FirstSecondThird");
    assert!(e.core.output_pages.len() >= 2);
    assert_eq!(e.page_builder.dead_cycles, 0);
}
#[test]
fn equal_paragraph_geometry_does_not_authorize_local_page_replacement() {
    use retex_rs2::core::enums::NodeKind;
    let job = Job::new();
    let source = include_str!("fixtures/exact-local-content-inspection.tex");
    let old = job.run(source, config(), true);
    let new = job.run(&source.replace("noindent A", "noindent Ai"), config(), true);
    let line_shape = |e: &Engine| {
        e.core
            .output_pages
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.nodes)
            .filter(|n| n.kind == NodeKind::Hlist && n.line_number == 1)
            .map(|n| (n.width_sp, n.height_sp, n.depth_sp, n.shift_sp))
            .collect::<Vec<_>>()
    };
    assert_eq!(line_shape(&old).len(), 1);
    assert_eq!(line_shape(&old), line_shape(&new));
    assert_eq!(old.layout.pages.len(), new.layout.pages.len());
    assert_eq!(old.layout.paragraph_count, new.layout.paragraph_count);
    assert_eq!(glyphs(&old), "AO");
    assert_eq!(glyphs(&new), "AiI");
    // Even an oracle-perfect replacement of A with Ai would leave the O from
    // the previous output routine. Geometry and line counts cannot prove that
    // the rest of the page (or its ordered effects) is independent of contents.
    let attempted_local_splice = glyphs(&old).replacen('A', "Ai", 1);
    assert_ne!(attempted_local_splice, glyphs(&new));
}
#[test]
fn live_missing_character_severity_is_enforced() {
    let job = Job::new();
    let e = job.run(
        r"\tracinglostchars=0\shipout\hbox{A\char255 B}\end",
        config(),
        true,
    );
    assert_eq!(glyphs(&e), "AB");
    assert!(
        !e.core
            .diagnostics
            .warnings
            .iter()
            .any(|w| w.contains("Missing character"))
    );
    let e = job.run(
        r"\tracinglostchars=3\hbox{owned\char255}\end",
        config(),
        false,
    );
    assert!(
        e.core
            .diagnostics
            .errors
            .iter()
            .any(|e| e.contains("Missing character"))
    );
}
#[test]
fn traces_cover_layout_and_obey_retention_budget() {
    let job = Job::new();
    let mut c = config();
    c.trace_enabled = true;
    let e = job.run(r"office words\par\end", c.clone(), true);
    let kinds = e
        .core
        .trace
        .events
        .iter()
        .filter_map(|e| e["event"].as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"paragraph_line"));
    assert!(kinds.contains(&"glue_set"));
    assert!(kinds.contains(&"shipout"));
    c.event_limit = 5;
    let e = job.run(r"office words\par\end", c, true);
    assert!(e.core.trace.events.len() <= 6);
    assert!(e.core.trace.dropped > 0);
}
#[test]
fn sliced_execution_preserves_pages_state_diagnostics_and_trace_order() {
    use retex_rs2::core::persistence::PersistentComponent;
    let job = Job::new();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets");
    std::fs::copy(assets.join("cmr10.tfm"), job.path("extra.tfm")).unwrap();
    std::fs::write(
        job.path("included file.tex"),
        r"\def\included{Included}\included ",
    )
    .unwrap();
    let font_setup = format!(
        r"\pdfmapline{{+extra CMR10 <{}}}\def\filename{{extra.tfm }}\font\late=\filename at10pt \pdfcopyfont\copied=\late \letterspacefont\spaced=\copied25 nolig \hyphenchar\spaced=42 \skewchar\spaced=-1 \lpcode\spaced65=125 \rpcode\spaced65=150 \pdfincludechars\copied{{A B}}\pdffontattr\copied{{/Interpolate false}}\spaced \hbox{{office}}\copied \hbox{{A B}}\input{{included file.tex}} ",
        assets.join("cmr10.pfb").display()
    );
    std::fs::copy(
        repo().join("tests/fixtures/resources/badge.png"),
        job.path("badge.png"),
    )
    .unwrap();
    // Both jobs must observe the same clock inputs; long sliced execution may
    // cross a real minute boundary even with deterministic PDF serialization.
    let source = format!(
        r#"\time=0 \day=1 \month=1 \year=2000 \pdfinfoomitdate=1 {}\def\twice#1{{#1#1}}\count0=17\output={{\shipout\box255}}\vsize=25pt\noindent\twice{{office}} $x^2+\overline{{x}}$ \delcode40="028300\delcode41="029301 $y_{{\hbox{{Box}}}}+\left(x\over y\right)$\par$$\vcenter{{\hbox{{V}}}}+x\eqno(1)$$\halign{{#&#\cr A&B\cr C&D\cr}}\halign to90pt{{\tabskip=1pt plus2pt #&\hfil#\hfil&&#\cr A&B&C&D\cr E\span F&G&H\cr\noalign{{\kern2pt}}I&J&K&L\cr}}\hbox{{\valign{{#&\vfil#\cr\hbox{{A}}&\hbox{{B}}\cr}}}}\discretionary{{pre}}{{post}}{{replace}}\accent19\count2=7\relax\char65\par\setbox3=\hbox{{Copy}}\unhcopy3\unhbox3\par\setbox4=\vbox{{\hbox{{Vertical}}}}\unvcopy4\unvbox4\immediate\openout4=forms.aux \def\formvalue{{Before}}\setbox8=\hbox{{F\pdfsavepos\write4{{Immediate:\formvalue:\the\pdflastxpos:\the\pdflastypos}}\special shipout{{pdf:direct:q Q}}}}\immediate\pdfxform attr{{/Custom 42}}resources{{/ColorSpace << /CS /DeviceRGB >>}}8\edef\firstform{{\the\pdflastxform}}\shipout\hbox{{P\pdfrefxform\firstform}}\shipout\hbox{{\pdfrefxform\firstform}}\setbox8=\hbox{{D\write4{{Referenced:\formvalue}}\special shipout{{src:\formvalue}}}}\pdfxform8\edef\delayedform{{\the\pdflastxform}}\def\formvalue{{After}}\setbox8=\hbox{{N\pdfrefxform\delayedform}}\pdfxform8\edef\nestedform{{\the\pdflastxform}}\shipout\hbox{{A\pdfsave\pdfsetmatrix{{2 0 0 2}}\pdfrefxform\nestedform\pdfrestore\pdfsavepos\write4{{Page:\formvalue:\the\pdflastxpos:\the\pdflastypos}}Z}}\shipout\hbox{{\pdfrefxform\nestedform}}\global\advance\count0 by2\immediate\openout3=shipment.aux \def\delayed{{Before}}\write3{{Page:\delayed}}\special shipout{{src:\delayed}}\par\def\delayed{{After}}\shipout\hbox{{\pdfsavepos\write3{{Box:\delayed:\the\pdflastxpos:\the\pdflastypos}}\special shipout{{pdf:direct:q Q}}}}\noindent\leaders\hbox to10pt{{\write3{{Forbidden}}\special shipout{{src:leader}}}}\hskip50pt Tail\par\end"#,
        font_setup
            + &math_setup()
            + r"\pdfinfo{/Title (Owned operands)}\pdfnames{/Owned 42}\pdftrailer{/OwnedTrailer 43}\pdftrailerid{}\pdfspacefont{Dummy}\pdffontexpand\copied20 10 5 autoexpand \par\pdfsnapy10pt plus1pt\pdfsnapycomp250\pdfobj reserveobjnum \edef\reserved{\the\pdflastobj}\immediate\pdfobj useobjnum\reserved stream attr{/Custom 42}{Captured:\the\pdflastobj}\pdfrefobj\reserved\pdfcolorstack0 push{0.1 0.2 0.3 rg}\hbox{Color}\pdfcolorstack0 pop "
            + r"\pdfcatalog{/Custom 42}openaction goto name{owned}\pdfoutline goto name{owned}count0{Owned navigation}\shipout\hbox{\pdfdest name{owned}xyz zoom1000 \pdfannot width10pt height5pt depth1pt{/Subtype /Text /Contents (Owned)}\pdfstartlink attr{/Border [0 0 0]}goto name{owned}Link\pdfendlink\pdfthread width10pt height5pt name{thread}\pdfximage width20pt page-1{badge.png}\pdfrefximage\pdflastximage} "
            + r"\immediate\openout1=sliced.aux \immediate\write1{Before:\the\hyphenchar\late}\immediate\closeout1\openin1=sliced.aux \read1 to\savedline\closein1\mark{\savedline}\special{src:owned}\pdfliteral direct{q Q}\setbox6=\hbox{Box}\parshape2 1pt40pt 2pt50pt \interlinepenalties3 10 20 30 \count5=\numexpr(7*3/2)+(9-2)/2\relax \dimen5=\dimexpr2pt*3/2+\wd6\relax \skip5=\glueexpr(2pt plus3fil minus1pt)*3/2+1pt plus2fill\relax \muskip5=\muexpr3mu*5/2\relax \edef\quantities{\the\count5;\the\dimen5;\the\skip5;\the\muskip5;\the\hyphenchar\spaced;\the\skewchar\spaced;\the\lpcode\spaced65;\the\fontdimen6\late;\the\wd6;\the\ht6;\the\dp6;\the\textfont1;\the\gluestretch\skip5;\the\glueshrink\skip5;\the\gluestretchorder\skip5;\the\glueshrinkorder\skip5;\the\fontcharwd\late65;\the\fontcharht\late65;\the\fontchardp\late65;\the\fontcharic\late65;\the\parshapelength2;\the\parshapeindent2;\the\parshapedimen3;\the\interlinepenalties3}\toks5={Unexpanded \undefined}\edef\protected{\unexpanded{\undefined}\detokenize{\undefined}}\copied\hbox{\quantities}\unhbox6 "
    );
    let mut c = config();
    c.trace_enabled = true;
    c.output_directory = Some(job.directory.display().to_string());
    let mut fresh = job.run(&source, c.clone(), true);
    fresh.dump_layout(&job.path("fresh.json")).unwrap();
    fresh.write_pdf(&job.path("fresh.pdf")).unwrap();
    let expected_state = bincode::serialize(&fresh.env.dump_state()).unwrap();
    let expected_fonts = bincode::serialize(&fresh.fonts.items).unwrap();
    let expected_document = bincode::serialize(&fresh.core.backend.document).unwrap();
    let expected_resources = bincode::serialize(&fresh.core.backend.resources).unwrap();
    let expected_trace = fresh.core.trace.events.clone();
    let expected_effect = std::fs::read(job.path("sliced.aux")).unwrap();
    let expected_shipment = std::fs::read(job.path("shipment.aux")).unwrap();
    let expected_forms = std::fs::read(job.path("forms.aux")).unwrap();
    let form_text = String::from_utf8_lossy(&expected_forms);
    assert_eq!(form_text.matches("Immediate:Before:").count(), 1);
    assert_eq!(form_text.matches("Referenced:After").count(), 1);
    assert!(!String::from_utf8_lossy(&expected_shipment).contains("Forbidden"));
    for budget in [1, 2, 7, 64] {
        let mut sliced = job.engine(c.clone());
        assert!(sliced.start_input(&job.path("input.tex")));
        sliced.begin_run().unwrap();
        let before = sliced.core.input.token_count;
        assert_eq!(sliced.advance_run(0).unwrap(), RunProgress::Yielded);
        assert_eq!(sliced.core.input.token_count, before);
        assert_eq!(sliced.status(), "running");
        assert!(sliced.write_pdf(&job.path("unfinished.pdf")).is_err());
        let mut slices = 0;
        loop {
            slices += 1;
            assert!(slices < 100_000);
            match sliced.advance_run(budget).unwrap() {
                RunProgress::Yielded => {}
                RunProgress::Complete => break,
                RunProgress::Failed => panic!("{:?}", sliced.core.diagnostics.errors),
            }
        }
        assert!(slices > 1);
        sliced.dump_layout(&job.path("sliced.json")).unwrap();
        sliced.write_pdf(&job.path("sliced.pdf")).unwrap();
        assert_eq!(
            std::fs::read(job.path("sliced.json")).unwrap(),
            std::fs::read(job.path("fresh.json")).unwrap()
        );
        assert_eq!(
            std::fs::read(job.path("sliced.pdf")).unwrap(),
            std::fs::read(job.path("fresh.pdf")).unwrap()
        );
        assert_eq!(
            bincode::serialize(&sliced.env.dump_state()).unwrap(),
            expected_state
        );
        assert_eq!(sliced.core.trace.events, expected_trace);
        assert_eq!(
            bincode::serialize(&sliced.core.backend.document).unwrap(),
            expected_document
        );
        assert_eq!(
            bincode::serialize(&sliced.core.backend.resources).unwrap(),
            expected_resources
        );
        assert_eq!(
            bincode::serialize(&sliced.fonts.items).unwrap(),
            expected_fonts
        );
        assert_eq!(
            std::fs::read(job.path("sliced.aux")).unwrap(),
            expected_effect
        );
        assert_eq!(
            std::fs::read(job.path("shipment.aux")).unwrap(),
            expected_shipment
        );
        assert_eq!(
            std::fs::read(job.path("forms.aux")).unwrap(),
            expected_forms
        );
        assert!(
            !sliced
                .core
                .backend
                .document
                .forms
                .values()
                .any(|form| form.emitting)
        );
        assert!(!sliced.core.input.scanning_filename);
        assert!(sliced.builder.nodes.values().all(Option::is_none));
    }
}

#[test]
fn unbounded_macro_expansion_returns_control_between_slices() {
    let job = Job::new();
    let path = job.path("input.tex");
    std::fs::write(&path, r"\def\forever{\forever}\forever").unwrap();
    let mut engine = job.engine(config());
    assert!(engine.start_input(&path));
    engine.begin_run().unwrap();
    for _ in 0..100 {
        assert_eq!(engine.advance_run(1).unwrap(), RunProgress::Yielded);
    }
    assert!(engine.core.expansion.expansion_steps > 100);
    assert_eq!(engine.status(), "running");
    assert!(!engine.start_input(&path));
    assert!(engine.advance_run(0).is_ok());
}

#[test]
fn sliced_failures_restore_groups_and_release_owned_nodes() {
    let job = Job::new();
    for source in [
        r"\def\a#1{#1}{\count0=9\hbox{owned\a{\undefined}",
        r"\halign{#\cr Unfinished",
        r"\halign{\tabskip=\undefined#\cr X\cr}",
        r"\halign{#\cr X\cr\noalign{\hbox{owned}\undefined}",
        r"\halign{#\cr X&Y\cr}",
        r"\halign{#&#\cr X\span\undefined\cr}",
        r"\discretionary{\hbox{owned}\undefined}{}{}",
        r"\discretionary{A}{\hskip1pt}{}",
        r"\discretionary{A}{B}{unfinished",
        r"\accent65\setbox0=\hbox{Forbidden}X\end",
        r"\accent65\count0=\undefined X\end",
        r"\hbox{\setbox0=\vbox{\hbox{X}}\unvbox0}\end",
        r"\font\r=cmr10 at0pt\end",
        r"\font\r=cmr10 scaled0\end",
        r"\font\r=cmr10 \font\r=cmr10 at\fontdimen6\r\end",
        r"\font\r={unfinished",
        r"\input{unfinished",
        r"\input{\undefined}",
        r"\font\r=cmr10 \letterspacefont\s=\r\undefined",
        r"\font\r=cmr10 \hyphenchar\r=\undefined",
        r"\font\r=cmr10 \pdfincludechars\r{\undefined}",
        r"\font\r=cmr10 \lpcode\r65=\undefined",
        r"\edef\saved{\the\undefined}",
        r"\count0=\hyphenchar\undefined",
        r"\count0=\lpcode\font\undefined",
        r"\kern\fontdimen\undefined\font",
        r"\kern\wd\undefined",
        r"\kern\fontcharwd\font\undefined",
        r"\kern\gluestretch\undefined",
        r"\kern\parshapeindent\undefined",
        r"\count0=\numexpr(1+2\relax",
        r"\count0=\numexpr1/0\relax",
        r"\count0=\numexpr2147483647+1\relax",
        r"\edef\saved{\unexpanded unfinished",
        r"\shipout\hbox{X\write1{\undefined}}\end",
        r"\shipout\hbox{X\special shipout{\undefined}}\end",
        r"X\write1{\undefined}\par\end",
        r"X\special shipout{\undefined}\par\end",
        r"\shipout\hbox{\pdfsave X}\end",
        r"\setbox0=\hbox{X\write1{\undefined}}\immediate\pdfxform0\end",
        r"\setbox0=\hbox{X\special shipout{\undefined}}\pdfxform0 X\pdfrefxform\pdflastxform\par\end",
        r"\setbox0=\hbox{\pdfsave X}\immediate\pdfxform0\end",
        r"{\setbox0=\hbox{X}\pdfxform attr{\undefined}0}\end",
        r"{\setbox0=\hbox{X}\pdfxform resources{\undefined}0}\end",
        r"\pdfrefxform\undefined\end",
        r"\pdfobj stream attr{\undefined}{X}\end",
        r"\immediate\pdfobj{\undefined}\end",
        r"\immediate\pdfobj reserveobjnum \end",
        r"\pdfrefobj99\end",
        r"\pdfsetmatrix{\undefined}\end",
        r"\pdfcolorstack-1 push{X}\end",
        r"\pdfcolorstack0 push{\undefined}\end",
        r"\pdfinfo{\undefined}\end",
        r"\pdffontexpand\font20 10 0 autoexpand \end",
        r"\pdfsnapy0pt\end",
        r"\pdfannot width\undefined{X}\end",
        r"\pdfdest name{\undefined}xyz \end",
        r"\pdfoutline goto name{owned}{\undefined}\end",
        r"\pdfcatalog{X}openaction goto file{\undefined}name{owned}\end",
        r"\hbox{\pdfstartlink goto page0{X}}\end",
        r"\pdfthread width1pt name{\undefined}\end",
        r"\pdfximage width\undefined{X}\end",
        r"\pdfximage attr{\undefined}{X}\end",
        r"\pdfrefximage99\end",
        r"\font\r=cmr10 \r X\leaders\hbox to10pt{X\special shipout{\undefined}}\hskip50pt Y\par\end",
    ] {
        let mut c = config();
        c.trace_enabled = true;
        let fresh = job.run(source, c.clone(), false);
        for budget in [1, 3, 32] {
            let mut engine = job.engine(c.clone());
            assert!(engine.start_input(&job.path("input.tex")));
            engine.begin_run().unwrap();
            loop {
                match engine.advance_run(budget).unwrap() {
                    RunProgress::Yielded => {}
                    RunProgress::Failed => break,
                    RunProgress::Complete => panic!("Malformed input unexpectedly completed"),
                }
            }
            assert_eq!(
                engine.core.diagnostics.errors,
                fresh.core.diagnostics.errors
            );
            assert_eq!(engine.core.trace.events, fresh.core.trace.events);
            assert!(!engine.core.input.scanning_filename);
            assert!(engine.env.groups.is_empty());
            assert_eq!(engine.core.input.in_open, 0);
            assert!(engine.builder.nodes.values().all(Option::is_none));
        }
    }
}
#[test]
fn failed_nested_input_preserves_completed_outer_pages_and_delayed_writes() {
    let job = Job::new();
    // Independent Python: complete the queued outer paragraphs and their IO
    // during finalization, even when the later nested box is incomplete.
    let source = r"\pdfinfoomitdate=1 \pdftrailerid{}\immediate\openout1=notes.aux First\write1{Retained effect}\par Second\par \hbox{unfinished";
    let mut settings = config();
    settings.output_directory = Some(job.directory.display().to_string());
    let mut fresh = job.run(source, settings.clone(), false);
    assert_eq!(glyphs(&fresh), "FirstSecond");
    assert_eq!(
        std::fs::read(job.path("notes.aux")).unwrap(),
        b"Retained effect\n"
    );
    assert!(fresh.write_pdf(&job.path("failed.pdf")).unwrap());
    let expected = fresh.export_pdf_bytes().unwrap().unwrap();
    let mut sliced = job.engine(settings);
    assert!(sliced.start_input(&job.path("input.tex")));
    sliced.begin_run().unwrap();
    loop {
        match sliced.advance_run(1).unwrap() {
            RunProgress::Yielded => {}
            RunProgress::Failed => break,
            RunProgress::Complete => panic!("Incomplete box completed"),
        }
    }
    assert_eq!(glyphs(&sliced), "FirstSecond");
    assert_eq!(sliced.export_pdf_bytes().unwrap().unwrap(), expected);
    assert!(sliced.env.groups.is_empty());
    assert!(sliced.builder.nodes.values().all(Option::is_none));
}
#[test]
fn tracing_disabled_preserves_original_language_demo_pdf() {
    // Original object-API assertion:
    // test_macro_language.test_language_demo_has_identical_pdf_with_tracing_disabled.
    let job = Job::new();
    let source = format!(
        "\\pdfinfoomitdate=1 {}",
        std::fs::read_to_string(repo().join("tests/fixtures/resources/macro_language_demo.tex"))
            .unwrap()
    );
    let mut traced_config = config();
    traced_config.trace_enabled = true;
    let mut traced = job.run(&source, traced_config, true);
    let pdf = job.path("document.pdf");
    assert!(traced.write_pdf(&pdf).unwrap());
    let expected = std::fs::read(&pdf).unwrap();
    let mut untraced = job.run(&source, config(), true);
    assert!(untraced.write_pdf(&pdf).unwrap());
    assert_eq!(std::fs::read(&pdf).unwrap(), expected);
    assert!(untraced.core.trace.events.is_empty());
    assert!(!traced.core.trace.events.is_empty());
    assert!(untraced.core.input.peak_frame_depth > 0);
    assert!(untraced.core.input.peak_frame_depth < 10);
}
#[test]
fn unicode_clusters_are_native_and_pdf_embeds_a_subset() {
    let job = Job::new();
    let font = repo().join("tests/fixtures/resources/DejaVuSans.ttf");
    let program = UnicodeFont::read(&font, 12 * C::UNITY).unwrap();
    let original = "office é é x́ Ω Ж 😀";
    let shaped = program.shape(original, true).unwrap();
    assert_eq!(
        shaped.iter().map(|g| g.text.as_str()).collect::<String>(),
        original
    );
    assert!(shaped.iter().any(|g| g.text == "ffi"));
    let mut c = config();
    c.language_profile = LanguageProfile::Unicode;
    let e_source = format!(
        r#"\font\f="[{}]" at12pt\f\shipout\hbox{{{original}}}\end"#,
        font.display()
    );
    let mut e = job.run(&e_source, c, true);
    let path = job.path("unicode.pdf");
    assert!(e.write_pdf(&path).unwrap());
    let pdf = lopdf::Document::load(&path).unwrap();
    assert!(pdf.objects.values().any(|o| {
        o.as_dict()
            .ok()
            .and_then(|d| d.get(b"Subtype").ok())
            .and_then(|o| o.as_name().ok())
            == Some(b"Type0".as_slice())
    }));
    let cmap = pdf
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .filter_map(|s| s.decompressed_content().ok())
        .find(|d| d.windows(9).any(|w| w == b"begincmap"))
        .unwrap();
    assert!(String::from_utf8_lossy(&cmap).contains("D83DDE00"));
}
#[test]
fn extraction_spaces_embed_the_dummy_font_and_distinct_paint_ids() {
    let job = Job::new();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/assets");
    let mut c = config();
    c.font_dir = Some(assets.display().to_string());
    let mut e = job.run(
        r"\shipout\hbox{\pdfinterwordspaceon A B\pdffakespace C}\end",
        c,
        true,
    );
    let line = &e.core.output_pages[0].lines[0];
    assert!(line.emitted.iter().any(|p| p.code == Some(32)));
    let ids = line
        .emitted
        .iter()
        .map(|p| p.placement_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), line.emitted.len());
    let path = job.path("spaces.pdf");
    assert!(e.write_pdf(&path).unwrap());
    let pdf = lopdf::Document::load(path).unwrap();
    assert_eq!(pdf.get_pages().len(), 1);
}
#[test]
fn formats_restore_typed_state_and_reject_corruption() {
    let job = Job::new();
    let mut c = config();
    c.ini = true;
    let mut e=job.run(r"\catcode123=1\catcode125=2\catcode35=6\pdfoutput=1\count7=42\def\saved{Stored}\font\r=cmr10\r\dump",c,true);
    let format = job.path("job.rsfmt");
    e.write_format(&format).unwrap();
    let mut restore_config = config();
    restore_config.ini = true;
    let mut restored = job.engine(restore_config.clone());
    restored.load_format(&format).unwrap();
    job.execute(
        &mut restored,
        r"\shipout\hbox{\saved\number\count7}\end",
        true,
    );
    assert_eq!(glyphs(&restored), "Stored42");
    assert!(restored.write_pdf(&job.path("restored.pdf")).unwrap());
    let mut bytes = std::fs::read(&format).unwrap();
    let index = bytes.len() / 2;
    bytes[index] ^= 1;
    let corrupt = job.path("corrupt.rsfmt");
    std::fs::write(&corrupt, bytes).unwrap();
    assert!(job.engine(restore_config).load_format(&corrupt).is_err());
}

#[test]
fn decoded_initialized_images_isolate_mutable_font_codes_and_validate_each_restore() {
    use retex_rs2::{
        fonts::microtypography::{FontCode, code_value},
        serialization::formats::decode_snapshot,
    };
    let job = Job::new();
    let mut ini = config();
    ini.ini = true;
    let mut engine = job.run(
        r"\catcode123=1\catcode125=2\catcode35=6\font\r=cmr10\r\lpcode\r65=17\count7=42\dump",
        ini,
        true,
    );
    let path = job.path("initialized.rsfmt");
    engine.write_format(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        decode_snapshot(&bytes, Some(32))
            .unwrap_err()
            .to_string()
            .contains("decoded byte budget")
    );
    let image = decode_snapshot(&bytes, Some(16 * 1024 * 1024)).unwrap();
    let mut restore = config();
    restore.ini = true;
    let mut first = job.engine(restore.clone());
    first
        .restore_initialized_state(image.detached_clone().unwrap(), &job.directory)
        .unwrap();
    job.execute(
        &mut first,
        r"\lpcode\r65=901\global\count7=99\shipout\hbox{A}\end",
        true,
    );
    assert_eq!(
        code_value(&first.fonts.items[first.fonts.current], FontCode::Left, 65),
        901
    );
    let mut second = job.engine(restore.clone());
    second
        .restore_initialized_state(image.detached_clone().unwrap(), &job.directory)
        .unwrap();
    assert_eq!(
        code_value(
            &second.fonts.items[second.fonts.current],
            FontCode::Left,
            65
        ),
        17
    );
    job.execute(&mut second, r"\shipout\hbox{\number\count7}\end", true);
    assert_eq!(glyphs(&second), "42");
    // Even a predecoded image must reject changed resource observations.
    let mut changed = image.detached_clone().unwrap();
    let resource = changed
        .fonts
        .fonts
        .iter_mut()
        .find_map(|f| f.tfm.as_mut())
        .unwrap();
    resource.sha256 = "0".repeat(64);
    assert!(
        job.engine(restore)
            .restore_initialized_state(changed, &job.directory)
            .is_err()
    );
}

#[test]
fn failed_form_text_keeps_the_unconsumed_outer_box() {
    use retex_rs2::core::persistence::PersistentComponent;
    let job = Job::new();
    for operand in [r"attr{\undefined}", r"resources{\undefined}"] {
        let source = format!(
            r"\time=0 \day=1 \month=1 \year=2000 \setbox0=\hbox{{X}}\pdfxform {operand}0\end"
        );
        std::fs::write(job.path("input.tex"), &source).unwrap();
        let mut settings = config();
        settings.trace_enabled = true;
        let mut fresh = job.engine(settings.clone());
        assert!(fresh.start_input(&job.path("input.tex")));
        assert!(!fresh.main_body());
        let pointer = *fresh
            .env
            .box_registers
            .get(&0)
            .expect("Attribute error consumed the box");
        assert!(fresh.builder.node(pointer).is_ok());
        assert!(fresh.env.groups.is_empty());
        for budget in [1, 3, 32] {
            let mut sliced = job.engine(settings.clone());
            assert!(sliced.start_input(&job.path("input.tex")));
            sliced.begin_run().unwrap();
            let mut complete = false;
            for _ in 0..10_000 {
                match sliced.advance_run(budget).unwrap() {
                    RunProgress::Yielded => {}
                    RunProgress::Failed => {
                        complete = true;
                        break;
                    }
                    RunProgress::Complete => panic!("Undefined form text unexpectedly completed"),
                }
            }
            assert!(complete);
            assert!(
                sliced
                    .builder
                    .node(
                        *sliced
                            .env
                            .box_registers
                            .get(&0)
                            .expect("Sliced error consumed the box")
                    )
                    .is_ok()
            );
            assert_eq!(
                bincode::serialize(&sliced.env.dump_state()).unwrap(),
                bincode::serialize(&fresh.env.dump_state()).unwrap()
            );
            assert_eq!(
                sliced.core.diagnostics.errors,
                fresh.core.diagnostics.errors
            );
            assert_eq!(sliced.core.trace.events, fresh.core.trace.events);
            assert!(
                !sliced
                    .core
                    .backend
                    .document
                    .forms
                    .values()
                    .any(|form| form.emitting)
            );
        }
    }
}

#[test]
fn captured_and_pending_image_sources_survive_formats_and_isolate_restores() {
    let job = Job::new();
    std::fs::copy(
        repo().join("tests/fixtures/resources/badge.png"),
        job.path("image.png"),
    )
    .unwrap();
    let mut source = job.run(
        r"\shipout\hbox{Imported PDF 1}\shipout\hbox{\pdfdest name{chosen}xyz Imported PDF 2}\end",
        config(),
        true,
    );
    let imported = job.path("source.pdf");
    source.write_pdf(&imported).unwrap();
    let mut ini = config();
    ini.ini = true;
    let mut initialized = job.run(&format!(
        r"\catcode123=1 \catcode125=2 \pdfoutput=1\immediate\pdfximage width30pt{{{}}}\pdfximage width80pt named{{chosen}}{{{}}}\count7=\pdflastximage \pdfximage colorspace-1{{{}}}\dump",
        job.path("image.png").display(), imported.display(), job.path("image.png").display(),
    ), ini, true);
    assert_eq!(
        initialized
            .core
            .backend
            .document
            .images
            .values()
            .last()
            .unwrap()
            .colorspace,
        -1
    );
    let format = job.path("images.rsfmt");
    initialized.write_format(&format).unwrap();
    std::fs::remove_file(job.path("image.png")).unwrap();
    std::fs::remove_file(imported).unwrap();
    for index in 0..2 {
        let mut restore = config();
        restore.ini = true;
        let mut restored = job.engine(restore);
        restored.load_format(&format).unwrap();
        assert_eq!(
            restored
                .core
                .backend
                .document
                .images
                .values()
                .last()
                .unwrap()
                .colorspace,
            -1
        );
        job.execute(
            &mut restored,
            r"\pdfoutput=1\shipout\hbox{\pdfrefximage1\kern10pt\pdfrefximage\count7}\end",
            true,
        );
        let output = job.path(&format!("restored-{index}.pdf"));
        restored.write_pdf(&output).unwrap();
        let pdf = lopdf::Document::load(output).unwrap();
        let page = *pdf.get_pages().get(&1).unwrap();
        let resources = pdf.get_dictionary(page).unwrap().get(b"Resources").unwrap();
        let resources = pdf.dereference(resources).unwrap().1.as_dict().unwrap();
        let xobjects = pdf
            .dereference(resources.get(b"XObject").unwrap())
            .unwrap()
            .1
            .as_dict()
            .unwrap();
        for (name, subtype) in [(b"Im1".as_slice(), b"Image".as_slice()), (b"Im2", b"Form")] {
            let object = pdf.dereference(xobjects.get(name).unwrap()).unwrap().1;
            assert_eq!(
                object
                    .as_stream()
                    .unwrap()
                    .dict
                    .get(b"Subtype")
                    .unwrap()
                    .as_name()
                    .unwrap(),
                subtype
            );
        }
        // lopdf's page extractor does not descend into Do. Inspect the actual
        // imported Form content instead, including every Tj/TJ text string.
        let imported = pdf
            .dereference(xobjects.get(b"Im2").unwrap())
            .unwrap()
            .1
            .as_stream()
            .unwrap();
        let content =
            lopdf::content::Content::decode(&imported.decompressed_content().unwrap()).unwrap();
        let text: Vec<u8> = content
            .operations
            .iter()
            .filter(|op| op.operator == "Tj" || op.operator == "TJ")
            .flat_map(|op| {
                op.operands.iter().flat_map(|operand| match operand {
                    lopdf::Object::Array(values) => values
                        .iter()
                        .filter_map(|v| v.as_str().ok())
                        .flatten()
                        .copied()
                        .collect::<Vec<_>>(),
                    _ => operand.as_str().unwrap().to_vec(),
                })
            })
            .collect();
        assert!(
            String::from_utf8(text)
                .unwrap()
                .replace(' ', "")
                .contains("ImportedPDF2")
        );
        assert_eq!(xobjects.len(), 2);
    }
}

#[test]
fn creating_a_link_preserves_the_last_annotation_query() {
    let job = Job::new();
    let engine = job.run(r"\shipout\hbox{\pdfannot{/Subtype /Text /Contents (Owned)}\pdfstartlink goto name{owned}Link\pdfendlink\pdfdest name{owned}xyz }\end", config(), true);
    let navigation = &engine.core.backend.document.navigation;
    assert_ne!(navigation.last_annotation, navigation.last_link);
    assert!(
        navigation.annotation_templates[&navigation.last_annotation]
            .action
            .is_none()
    );
    assert!(
        navigation.annotation_templates[&navigation.last_link]
            .action
            .is_some()
    );
}

#[test]
fn retained_image_resource_changes_only_invalidate_pages_that_paint_it() {
    use retex_rs2::{fonts::outlines::OutlineCache, pdf::preview::capture};
    let job = Job::new();
    let write = |pixel: &[u8]| {
        let mut encoder = png::Encoder::new(
            std::fs::File::create(job.path("painting.png")).unwrap(),
            2,
            1,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(pixel)
            .unwrap();
    };
    let source = r"\shipout\hbox{Unchanged}\pdfximage width20pt{painting.png}\shipout\hbox{\pdfrefximage\pdflastximage}\end";
    write(&[255, 0, 0, 255, 0, 0, 255, 128]);
    let original = job.run(source, config(), true);
    let mut cache = OutlineCache::default();
    let mut before = capture(&original, &mut cache).unwrap();
    assert_eq!(before.images.len(), 1);
    assert_eq!(
        before.images.values().next().unwrap().rgba.as_ref(),
        [255, 0, 0, 255, 0, 0, 255, 128]
    );
    write(&[0, 255, 0, 255, 0, 0, 255, 128]);
    let changed = job.run(source, config(), true);
    let after = capture(&changed, &mut cache).unwrap();
    assert_eq!(
        before.pages[0].content_sha256,
        after.pages[0].content_sha256
    );
    assert_ne!(
        before.pages[1].content_sha256,
        after.pages[1].content_sha256
    );
    assert_ne!(
        before.images.keys().next().unwrap(),
        after.images.keys().next().unwrap()
    );
    assert_eq!(
        after.images.values().next().unwrap().rgba.as_ref(),
        [0, 255, 0, 255, 0, 0, 255, 128]
    );
    assert!(
        after.pages[1]
            .operators
            .iter()
            .any(|op| op.ends_with("retexImage"))
    );
    let image_key = before.images.keys().next().unwrap().clone();
    let (spans, pixels) = before.pack_pixels();
    assert_eq!(spans[&image_key], [0, 8, 8, 0]);
    assert_eq!(pixels, [255, 0, 0, 255, 0, 0, 255, 128]);
    assert!(before.images[&image_key].rgba.is_empty());
}
#[test]
fn retained_jpeg_paints_emitted_samples_and_rejects_inconsistent_pdf_geometry() {
    use retex_rs2::{fonts::outlines::OutlineCache, pdf::preview::capture};
    for (data, color) in [
        (
            include_bytes!("fixtures/preview-RGB.jpg").as_slice(),
            [45, 100, 217, 255],
        ),
        (
            include_bytes!("fixtures/preview-L.jpg").as_slice(),
            [150, 150, 150, 255],
        ),
        (
            include_bytes!("fixtures/preview-CMYK.jpg").as_slice(),
            [106, 64, 155, 255],
        ),
    ] {
        let job = Job::new();
        std::fs::write(job.path("painting.jpg"), data).unwrap();
        let mut engine = job.run(
            r"\pdfximage width32bp{painting.jpg}\shipout\hbox{\pdfrefximage\pdflastximage}\end",
            config(),
            true,
        );
        // Paint uses the emitted stream, including a format restored after the
        // original source is unavailable. It must never reopen the input JPEG.
        std::fs::remove_file(job.path("painting.jpg")).unwrap();
        let mut scene = capture(&engine, &mut OutlineCache::default()).unwrap();
        let image = scene.images.values().next().unwrap();
        assert_eq!((image.width, image.height), (16, 12));
        assert!(image.rgba.chunks_exact(4).all(|pixel| pixel == color));
        if color == [106, 64, 155, 255] {
            let display = image.device_cmyk.as_ref().unwrap();
            assert_eq!(display.profile, "retex-device-cmyk16-v1");
            assert!(
                display
                    .display_rgba
                    .chunks_exact(4)
                    .all(|p| p == [116, 67, 118, 255])
            );
        } else {
            assert!(image.device_cmyk.is_none());
        }
        let original_identity = scene.images.keys().next().unwrap().clone();
        let (spans, pixels) = scene.pack_pixels();
        let length = 16 * 12 * 4;
        let display_length = if color == [106, 64, 155, 255] {
            length
        } else {
            0
        };
        assert_eq!(
            spans[&original_identity],
            [0, length, length, display_length]
        );
        assert!(pixels[..length].chunks_exact(4).all(|pixel| pixel == color));
        if display_length != 0 {
            assert!(
                pixels[length..]
                    .chunks_exact(4)
                    .all(|pixel| pixel == [116, 67, 118, 255])
            );
        }
        std::fs::write(job.path("painting.jpg"), data).unwrap();
        let mut attributed = job.run(
            r"\pdfximage width32bp attr{/Interpolate true}{painting.jpg}\shipout\hbox{\pdfrefximage\pdflastximage}\end",
            config(), true,
        );
        std::fs::remove_file(job.path("painting.jpg")).unwrap();
        let interpolated = capture(&attributed, &mut OutlineCache::default()).unwrap();
        assert!(interpolated.images.values().next().unwrap().interpolate);
        assert_ne!(
            interpolated.images.keys().next().unwrap(),
            &original_identity
        );
        let frozen = attributed
            .core
            .backend
            .document
            .images
            .values_mut()
            .next()
            .unwrap();
        frozen.dictionary = frozen
            .dictionary
            .replace("/Interpolate true", "/Interpolate 1");
        assert!(capture(&attributed, &mut OutlineCache::default()).is_err());
        let image = engine
            .core
            .backend
            .document
            .images
            .values_mut()
            .next()
            .unwrap();
        image.dictionary = image.dictionary.replace("/Width 16", "/Width 17");
        assert!(capture(&engine, &mut OutlineCache::default()).is_err());
    }
}
#[test]
fn retained_imported_pdf_uses_emitted_graph_and_rejects_cycles() {
    use retex_rs2::{fonts::outlines::OutlineCache, pdf::preview::capture};
    let job = Job::new();
    std::fs::write(
        job.path("painting.pdf"),
        include_bytes!("fixtures/preview-curves.pdf"),
    )
    .unwrap();
    let mut engine = job.run(
        r"\pdfximage width100bp{painting.pdf}\shipout\hbox{\pdfrefximage\pdflastximage}\end",
        config(),
        true,
    );
    std::fs::remove_file(job.path("painting.pdf")).unwrap();
    let scene = capture(&engine, &mut OutlineCache::default()).unwrap();
    assert!(
        scene.pages[0]
            .operators
            .iter()
            .any(|op| op == "retexFormBegin q")
    );
    assert!(
        scene.pages[0]
            .operators
            .iter()
            .any(|op| op.contains("20 50 80 50 90 10 c"))
    );
    let image = engine
        .core
        .backend
        .document
        .images
        .values_mut()
        .next()
        .unwrap();
    let number = image.number;
    image.dictionary = format!(
        "/Subtype /Form /BBox [0 0 100 60] /Resources << /XObject << /Recursive {number} 0 R >> >>"
    );
    image.data = Some(b"/Recursive Do".as_slice().into());
    let error = match capture(&engine, &mut OutlineCache::default()) {
        Ok(_) => panic!("Recursive imported form unexpectedly painted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("recursion"));
    std::fs::write(
        job.path("painting.pdf"),
        include_bytes!("fixtures/preview-curves.pdf"),
    )
    .unwrap();
    let overridden = job.run(r"\pdfximage width100bp attr{/Matrix [1 0 0 1 5 5]}{painting.pdf}\shipout\hbox{\pdfrefximage\pdflastximage}\end", config(), true);
    let error = match capture(&overridden, &mut OutlineCache::default()) {
        Ok(_) => panic!("Unsupported dictionary override unexpectedly painted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("dictionary override"));
}

#[test]
fn interactive_recovery_continues_scalars_commands_packages_and_groups() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    let job = Job::new();
    let mut engine = job.run(
        r"\font\body=cmr10 \body Before \undefinedcommand
\count0=\relax \dimen0=12zz } \else
\count1=7 \divide\count1 by0
\errhelp{A package-specific explanation}\errmessage{A recoverable package error}
After the errors.\par\end",
        EngineConfig {
            recovery_policy: RecoveryPolicy::Interactive,
            ..EngineConfig::default()
        },
        true,
    );
    assert!(engine.recovered());
    let codes: Vec<_> = engine
        .core
        .diagnostics
        .records
        .iter()
        .filter_map(|r| r.details.as_ref().map(|d| d.code.as_str()))
        .collect();
    for code in [
        "tex.undefined_command",
        "tex.missing_number",
        "tex.missing_unit",
        "tex.extra_closer",
        "tex.conditional",
        "tex.arithmetic",
        "latex.error",
    ] {
        assert!(codes.contains(&code), "{codes:?}");
    }
    assert!(!engine.layout.pages.is_empty());
    assert!(!engine.pdf_output_available());
    assert!(engine.write_pdf(&job.path("repaired.pdf")).is_err());
    assert!(engine.export_pdf_bytes().is_err());
    assert!(!job.path("repaired.pdf").exists());
    let snapshot = retex_rs2::diagnostics::snapshot::layout_snapshot(
        &engine.layout,
        &engine.builder,
        &engine.fonts,
        &engine.core.diagnostics,
        engine.status(),
        None,
        0,
        0,
    )
    .unwrap();
    assert_eq!(snapshot["recovery"]["outcome"], "completed_with_errors");
    let text = serde_json::to_string(&snapshot).unwrap();
    assert!(text.contains("package-specific explanation"));
    assert!(engine.core.failure.is_none());
}

#[test]
fn interactive_replay_isolates_math_runaways_and_resources_without_changing_source() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    for body in [
        "$x^1^2$\nAfter recovery.\\par\\end",
        "\\def\\foo#1{#1}\n\\foo{broken\n\nAfter recovery.\\par\\end",
        "\\def\\forever{\\forever}\n\\forever\nAfter recovery.\\par\\end",
        "\\font\\bad=doesnotexist\nAfter recovery.\\par\\end",
        "\\input missing-body-file\nAfter recovery.\\par\\end",
    ] {
        let job = Job::new();
        let source = format!("\\font\\body=cmr10 \\body\n{body}");
        let engine = job.run(
            &source,
            EngineConfig {
                recovery_policy: RecoveryPolicy::Interactive,
                expansion_limit: Some(1000),
                ..EngineConfig::default()
            },
            true,
        );
        assert_eq!(
            std::fs::read_to_string(job.path("input.tex")).unwrap(),
            source
        );
        assert!(engine.recovered(), "{body}");
        assert!(!engine.layout.pages.is_empty(), "{body}");
        let snapshot = retex_rs2::diagnostics::snapshot::layout_snapshot(
            &engine.layout,
            &engine.builder,
            &engine.fonts,
            &engine.core.diagnostics,
            engine.status(),
            None,
            0,
            0,
        )
        .unwrap();
        assert!(
            snapshot["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["details"]["recovery"].as_str().is_some()),
            "{body}"
        );
    }
}

#[test]
fn interactive_pdf_and_output_routine_recovery_preserves_pages() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    let job = Job::new();
    let engine = job.run(
        r"\font\body=cmr10 \body
Before \pdfsetmatrix{bad} after the invalid matrix.\par
\output={\unvbox255}\maxdeadcycles=2
Body text.\par\vfill\penalty-10000
After output recovery.\par\end",
        EngineConfig {
            recovery_policy: RecoveryPolicy::Interactive,
            ..EngineConfig::default()
        },
        true,
    );
    assert!(engine.core.diagnostics.records.iter().any(|r| {
        r.details
            .as_ref()
            .is_some_and(|d| d.code == "pdf.operation")
    }));
    assert!(engine.core.diagnostics.records.iter().any(|r| {
        r.details
            .as_ref()
            .is_some_and(|d| d.code == "tex.output_routine")
    }));
    assert!(!engine.layout.pages.is_empty());
}

#[test]
fn interactive_bad_graphic_is_a_bounded_placeholder_and_source_stays_original() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    let job = Job::new();
    std::fs::write(job.path("bad.png"), b"not an image").unwrap();
    let source = format!(
        "\\font\\body=cmr10 \\body\n\\pdfximage width30pt height20pt{{{}}}\nAfter the graphic.\\par\\end",
        job.path("bad.png").display()
    );
    let engine = job.run(
        &source,
        EngineConfig {
            recovery_policy: RecoveryPolicy::Interactive,
            ..EngineConfig::default()
        },
        true,
    );
    assert_eq!(
        std::fs::read_to_string(job.path("input.tex")).unwrap(),
        source
    );
    let diagnostic = engine
        .core
        .diagnostics
        .records
        .iter()
        .find(|r| {
            r.details
                .as_ref()
                .is_some_and(|d| d.code == "resource.image")
        })
        .unwrap();
    assert!(!diagnostic.details.as_ref().unwrap().related.is_empty());
    assert!(engine.core.input.recovery.count <= 8);
    assert!(
        engine
            .core
            .output_pages
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.placements)
            .any(|p| p
                .box_warning
                .as_deref()
                .is_some_and(|w| w.starts_with("Recovered region:"))
                && p.width_sp == 30 * C::UNITY
                && p.height_sp == 20 * C::UNITY)
    );
}

#[test]
fn package_warnings_are_collected_in_batch_mode_and_unit_fixes_are_explicit() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    let job = Job::new();
    let engine = job.run(
        r"\font\body=cmr10 \body
\immediate\write16{LaTeX Warning: Reference missing on input line 2.}
\hbox to12{Text}\par\end",
        EngineConfig {
            recovery_policy: RecoveryPolicy::Interactive,
            interaction_mode: Some(0),
            ..EngineConfig::default()
        },
        true,
    );
    assert!(engine.core.diagnostics.records.iter().any(|r| {
        r.severity == "warning"
            && r.details
                .as_ref()
                .is_some_and(|d| d.code == "latex.warning")
    }));
    let fixes: Vec<_> = engine
        .core
        .diagnostics
        .records
        .iter()
        .filter_map(|r| r.details.as_ref())
        .flat_map(|d| &d.fixes)
        .collect();
    assert!(fixes.iter().any(|f| f.replacement == "pt"));
    assert!(
        !std::fs::read_to_string(job.path("input.tex"))
            .unwrap()
            .contains("to12pt")
    );
}

#[test]
fn emergency_layout_strategies_keep_the_suffix_and_explain_the_assumption() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    for (body, code) in [
        (
            r"\output={\relax} Body.\par\vfill\penalty-10000 AfterRecovery.\par\end",
            "tex.output_routine",
        ),
        (
            r"Before\hskip0pt minus1fil AfterRecovery.\par\end",
            "tex.infinite_shrinkage",
        ),
        (
            r"\halign{#&#\cr a&b&c\cr}\par AfterRecovery.\par\end",
            "tex.alignment",
        ),
        (
            r"\pdfadjustspacing=2 \font\a=cmr10 \font\b=cmr10 at12pt \pdffontexpand\a20 10 5 autoexpand \pdffontexpand\b30 10 5 autoexpand \a Before \b AfterRecovery.\par\end",
            "tex.font_expansion",
        ),
    ] {
        let job = Job::new();
        let source = format!("\\font\\body=cmr10 \\body\n{body}");
        let engine = job.run(
            &source,
            EngineConfig {
                recovery_policy: RecoveryPolicy::Interactive,
                ..config()
            },
            true,
        );
        assert!(
            engine
                .core
                .diagnostics
                .records
                .iter()
                .any(|r| r.details.as_ref().is_some_and(|d| d.code == code)),
            "{code}: {:?}",
            engine.core.diagnostics.records
        );
        assert!(
            glyphs(&engine).contains("AfterRecovery."),
            "{code}: {}",
            glyphs(&engine)
        );
        assert_eq!(
            std::fs::read_to_string(job.path("input.tex")).unwrap(),
            source
        );
    }
}

#[test]
fn interactive_watchdog_limits_uninterrupted_expansion_not_document_length() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    let job = Job::new();
    let mut engine = job.engine(EngineConfig {
        recovery_policy: RecoveryPolicy::Interactive,
        ..config()
    });
    engine.core.expansion.uninterrupted_expansion_limit = Some(64);
    engine.core.expansion.expansion_steps = 2_000_000;
    let source = format!(
        "\\font\\body=cmr10 \\body\\def\\word{{Word}}\n{}\n\\par\\end",
        "\\word ".repeat(100)
    );
    job.execute(&mut engine, &source, true);
    assert!(!engine.recovered());
    assert!(engine.core.expansion.expansion_steps > 2_000_064);
    let mut runaway = job.engine(EngineConfig {
        recovery_policy: RecoveryPolicy::Interactive,
        ..config()
    });
    runaway.core.expansion.uninterrupted_expansion_limit = Some(64);
    job.execute(
        &mut runaway,
        "\\font\\body=cmr10 \\body\n\\def\\forever{\\forever}\n\\forever\nAfterRecovery.\\par\\end",
        true,
    );
    assert!(runaway.recovered());
    assert!(glyphs(&runaway).contains("AfterRecovery."));
    assert!(
        runaway
            .core
            .diagnostics
            .terminal_log
            .contains("Failed attempt")
    );
}

#[test]
fn missing_glyph_marks_the_preview_and_reports_one_warning() {
    use retex_rs2::core::recovery::RecoveryPolicy;
    let job = Job::new();
    let engine = job.run(
        r"\font\body=cmr10 \body Before \char255 AfterGlyph.\par\end",
        EngineConfig {
            recovery_policy: RecoveryPolicy::Interactive,
            ..config()
        },
        true,
    );
    assert_eq!(
        engine
            .core
            .diagnostics
            .records
            .iter()
            .filter(|r| r.message.starts_with("Missing character:"))
            .count(),
        1
    );
    assert_eq!(
        engine
            .core
            .diagnostics
            .warnings
            .iter()
            .filter(|r| r.starts_with("Missing character:"))
            .count(),
        1
    );
    assert!(
        engine
            .core
            .output_pages
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.placements)
            .any(|p| p.kind == "rule")
    );
    assert!(glyphs(&engine).contains("AfterGlyph."));
}

#[test]
fn latex_required_missing_file_reports_a_host_resource_request_without_prompting() {
    let job = Job::new();
    let engine = job.run(
        "\\catcode`\\@=11 \\def\\@missingfileerror#1#2{\\immediate\\write16{! LaTeX Error: File `#1.#2' not found.^^JType X to quit or <RETURN> to proceed.}\\read-1 to\\answer}\\@missingfileerror{aliascnt}{sty}\\end",
        config(), false,
    );
    let failure = engine.core.failure.as_ref().unwrap();
    let resource = failure
        .missing_resource
        .as_ref()
        .unwrap_or_else(|| panic!("{failure:?}"));
    assert_eq!(resource.kind, "tex");
    assert_eq!(resource.name, b"aliascnt.sty");
    assert!(!failure.message.contains("<RETURN>"));
}
