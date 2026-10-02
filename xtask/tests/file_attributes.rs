use std::{fs, process::Command};
use tempfile::tempdir;
use xtask::{ledger, measurements, metrics, scan};

fn record(lines: usize) -> String {
    let mut source = String::from("#[repr(C)]\n#[derive(Debug)]\npub struct Record {\n");
    for index in 0..lines - 2 {
        source.push_str(&format!("pub field_{index}: u8,\n"));
    }
    source.push_str("}\n");
    source
}

#[test]
fn compiled_attribute_records_obey_scan_collect_ledger_file_boundaries() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    for lines in [799, 800, 801] {
        let path = root.path().join("src/lib.rs");
        fs::write(&path, record(lines)).unwrap();
        let output = Command::new("rustc")
            .args(["--edition=2024", "--crate-type=lib", "-Dwarnings"])
            .arg(&path)
            .arg("-o")
            .arg(root.path().join("fixture.rlib"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = scan::scan(root.path(), &["src"]).unwrap();
        let measured = measurements::collect(&result, metrics::Limits::default());
        let file = measured
            .iter()
            .find(|m| m.key == "src/lib.rs::file:file_lines")
            .unwrap();
        assert_eq!(file.value, lines);
        assert_eq!(file.limit, 800);
        assert!(result.suppressions.is_empty());
        let findings = ledger::validate(&measured, &[]);
        if lines <= 800 {
            assert!(findings.is_empty(), "{findings:?}");
        } else {
            assert_eq!(
                findings,
                ["src/lib.rs::file:file_lines: file_lines=801 limit=800 (new-code limit=800)"]
            );
        }
    }
}

#[test]
fn attribute_spans_exclude_only_attribute_tokens_on_shared_physical_lines() {
    for (source, lines) in [
        ("#[repr(C)] #[derive(Debug)]\nstruct T;", 1),
        ("#[repr(C)] struct T;", 1),
        ("#[derive(\n Debug\n)] struct T;", 1),
        ("#[doc = \"first\nsecond\nthird\"]\nstruct T;", 1),
        ("#[doc = \"first\nsecond\"] struct T;", 1),
        ("/// documentation\nstruct T;", 1),
        ("/** multi\nline */ struct T;", 1),
        ("//! crate docs\n#![doc = \"docs\"]\nstruct T;", 1),
        ("#[cfg(any())]\nstruct T;\n#[cfg(not(any()))]\nstruct U;", 2),
        ("struct A; #[repr(C)]\nstruct B;", 2),
        ("const TEXT: &str = r#\"\n#[repr(C)]\n\"#;", 3),
        ("// #[repr(C)]\nstruct T; /* #[derive(Debug)] */", 1),
    ] {
        for input in [source.to_owned(), source.replace('\n', "\r\n")] {
            assert_eq!(
                metrics::analyze("src/lib.rs", &input).unwrap().file_lines,
                lines,
                "{input}"
            );
        }
    }
}

#[test]
fn nested_attribute_positions_and_non_file_metrics_keep_their_contracts() {
    let source = "#![doc = \"crate\"]
mod nested {
    #![doc = \"module\"]
    #[repr(C)]
    struct T {
        #[doc = \"field\"]
        value: u8,
    }
    enum E {
        #[doc = \"variant\"]
        V {
            #[doc = \"field\"]
            value: u8,
        },
    }
    impl T {
        #[inline]
        fn method(&self) {
            #[cfg(unix)]
            let _x = self.value;
        }
    }
    #[inline]
    fn run() {
        #[cfg(unix)]
        let _x = 1;
        #[doc = \"local\"]
        struct Local;
    }
    trait Q {
        #[inline]
        fn default() {}
    }
}";
    let report = metrics::analyze("src/lib.rs", source).unwrap();
    assert_eq!(report.file_lines, 22);
    let functions: Vec<_> = report
        .functions
        .iter()
        .map(|f| (f.symbol.as_str(), f.lines))
        .collect();
    assert_eq!(
        functions,
        [
            ("root::nested::T::method", 4),
            ("root::nested::run", 6),
            ("root::nested::Q::default", 1)
        ]
    );
    assert_eq!(report.types["root::nested::T"], (4, 1));
    assert_eq!(report.types["root::nested::Q"], (1, 1));
    assert_eq!(report.modules["root::nested"], 6);
}

#[test]
fn excluded_attributes_still_reach_suppression_validation() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    for attribute in [
        "#[allow(warnings)]",
        "#![expect(unused)]",
        "#[cfg_attr(any(), allow(warnings))]",
    ] {
        fs::write(
            root.path().join("src/lib.rs"),
            format!("{attribute}\n{}", record(800)),
        )
        .unwrap();
        let result = scan::scan(root.path(), &["src"]).unwrap();
        let measured = measurements::collect(&result, metrics::Limits::default());
        assert!(ledger::validate(&measured, &[]).is_empty());
        assert_eq!(result.reports[0].file_lines, 800);
        assert!(
            result
                .suppressions
                .iter()
                .any(|finding| finding.contains("forbidden lint suppression"))
        );
    }
}

#[test]
fn unknown_attribute_expansion_and_hidden_paths_fail_closed() {
    for source in [
        "#[generate] fn f() {}",
        "#[tool::generate] fn f() {}",
        "#[derive(Generate)] struct T;",
        "#[cfg_attr(any(), generate)] fn f() {}",
        "#[cfg_attr(any(), cfg_attr(any(), path = \"hidden.rs\"))] mod m;",
    ] {
        assert!(metrics::analyze("src/lib.rs", source).is_err(), "{source}");
    }
}

#[test]
fn attributes_in_supported_macro_expressions_and_doc_data_are_distinguished() {
    let source = "fn f() {\nassert!({\n#[cfg(unix)]\nlet x = true;\nx\n});\n}";
    assert_eq!(
        metrics::analyze("src/lib.rs", source).unwrap().file_lines,
        6
    );
    assert_eq!(
        metrics::analyze(
            "src/lib.rs",
            "#[doc = include_str!(\"docs.md\")]\nstruct T;"
        )
        .unwrap()
        .file_lines,
        1
    );
    for source in [
        "#[doc = custom!()]\nstruct T;",
        "#[cfg_attr(any(), doc = custom!())]\nstruct T;",
        "#[doc = SOME_CONSTANT]\nstruct T;",
        "#[cfg_attr(any(), derive(Generate))]\nstruct T;",
        "#[cfg_attr(any())]\nstruct T;",
    ] {
        assert!(metrics::analyze("src/lib.rs", source).is_err(), "{source}");
    }
    for source in [
        "#[cfg_attr(any(), doc = \"path\")]\nstruct T;",
        "#[cfg_attr(any(), doc = include_str!(\"docs.md\"))]\nstruct T;",
    ] {
        assert_eq!(
            metrics::analyze("src/lib.rs", source).unwrap().file_lines,
            1
        );
    }
}

const NESTED_ATTRIBUTES: &str = "#![doc = \"crate\"]
pub mod nested {
    #![doc = \"module\"]
    #[derive(Debug)]
    pub struct T {
        #[doc = \"field\"]
        pub value: u8,
    }
    pub enum E {
        #[doc = \"variant\"]
        V {
            #[doc = \"field\"]
            value: u8,
        },
    }
    pub trait Q {
        #[doc = \"associated type\"]
        type Value;
        #[doc = \"constant\"]
        const VALUE: u8;
        #[inline]
        fn default() {}
    }
    impl T {
        #[inline]
        pub fn method(
            #[cfg(all())]
            &self,
        ) -> u8 {
            #[cfg(all())]
            let x = self.value;
            match x {
                #[cfg(all())]
                0 => 0,
                _ => x,
            }
        }
    }
    pub fn run() -> bool {
        assert!({
            #[cfg(all())]
            let x = true;
            x
        });
        true
    }
}";

#[test]
fn compiled_nested_attributes_cover_fields_variants_statements_and_macro_inputs() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    let path = root.path().join("src/lib.rs");
    fs::write(&path, NESTED_ATTRIBUTES).unwrap();
    let output = Command::new("rustc")
        .args(["--edition=2024", "--crate-type=lib", "-Dwarnings"])
        .arg(&path)
        .arg("-o")
        .arg(root.path().join("fixture.rlib"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = scan::scan(root.path(), &["src"]).unwrap();
    assert_eq!(result.reports[0].file_lines, 33);
    assert!(result.suppressions.is_empty());
    assert!(
        ledger::validate(
            &measurements::collect(&result, metrics::Limits::default()),
            &[]
        )
        .is_empty()
    );
}
