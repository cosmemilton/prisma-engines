#![cfg(feature = "firebird")]
// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.

use psl::ValidatedSchema;

const SOURCE: &str = r#"
datasource db {
  provider = "firebird"
}
"#;

const TARGET_SINGLE: &str = "model Target {\n  id Int @id\n  first First[]\n}\n";
const TARGET_PAIR: &str = "model Target {\n  id Int @id\n  first First[]\n  second Second[]\n}\n";

#[derive(Clone, Copy, Debug)]
enum IdentifierKind {
    Table,
    Column,
    PrimaryKey,
    Unique,
    Index,
    ForeignKey,
}

const CONSTRAINT_KINDS: [IdentifierKind; 4] = [
    IdentifierKind::PrimaryKey,
    IdentifierKind::Unique,
    IdentifierKind::Index,
    IdentifierKind::ForeignKey,
];

const IDENTIFIER_KINDS: [IdentifierKind; 6] = [
    IdentifierKind::Table,
    IdentifierKind::Column,
    IdentifierKind::PrimaryKey,
    IdentifierKind::Unique,
    IdentifierKind::Index,
    IdentifierKind::ForeignKey,
];

fn model_with_identifier(model: &str, kind: IdentifierKind, name: &str) -> String {
    let primary_key = match kind {
        IdentifierKind::PrimaryKey => format!("@id(map: \"{name}\")"),
        _ => "@id".to_owned(),
    };
    let value_attribute = match kind {
        IdentifierKind::Column => format!("@map(\"{name}\")"),
        IdentifierKind::Unique => format!("@unique(map: \"{name}\")"),
        _ => String::new(),
    };
    let foreign_key = match kind {
        IdentifierKind::ForeignKey => name.to_owned(),
        _ => format!("{model}_target_fk"),
    };
    let model_attribute = match kind {
        IdentifierKind::Table => format!("@@map(\"{name}\")"),
        IdentifierKind::Index => format!("@@index([value], map: \"{name}\")"),
        _ => String::new(),
    };

    format!(
        r#"
model {model} {{
  id Int {primary_key}
  value String {value_attribute}
  targetId Int
  target Target @relation(fields: [targetId], references: [id], map: "{foreign_key}")

  {model_attribute}
}}
"#
    )
}

fn validate(models: &str) -> ValidatedSchema {
    psl::validate_without_extensions(format!("{SOURCE}\n{models}").into())
}

#[track_caller]
fn assert_valid(schema: &ValidatedSchema) {
    assert!(!schema.diagnostics.has_errors(), "{}", schema.render_own_diagnostics());
}

#[track_caller]
fn assert_single_identifier_error(schema: &ValidatedSchema, expected: &str) {
    let errors = schema.diagnostics.errors();
    assert_eq!(errors.len(), 1, "{}", schema.render_own_diagnostics());
    assert!(errors[0].message().contains(expected), "{}", errors[0].message());
}

#[test]
fn mapped_table_and_column_names_reject_non_bmp_characters_and_trailing_spaces() {
    let invalid_names = [
        ('\u{1f600}'.to_string(), "BMP"),
        ("same ".to_owned(), "must not end with an ASCII space"),
    ];

    for kind in [IdentifierKind::Table, IdentifierKind::Column] {
        for (name, expected) in &invalid_names {
            assert!(name.len() <= 31);
            let schema = validate(&format!(
                "{TARGET_SINGLE}{}",
                model_with_identifier("First", kind, name)
            ));
            assert_single_identifier_error(&schema, expected);
        }
    }
}

#[test]
fn every_mapped_constraint_kind_rejects_non_bmp_characters_and_trailing_spaces() {
    let invalid_names = [
        ('\u{1f600}'.to_string(), "BMP"),
        ("same ".to_owned(), "must not end with an ASCII space"),
    ];

    for kind in CONSTRAINT_KINDS {
        for (name, expected) in &invalid_names {
            assert!(name.len() <= 31);
            let schema = validate(&format!(
                "{TARGET_SINGLE}{}",
                model_with_identifier("First", kind, name)
            ));
            assert_single_identifier_error(&schema, expected);
        }
    }
}

#[test]
fn valid_bmp_and_unpadded_identifiers_validate_for_every_kind() {
    for kind in IDENTIFIER_KINDS {
        for name in ["\u{03b1}\u{00e9}", "same"] {
            let schema = validate(&format!(
                "{TARGET_SINGLE}{}",
                model_with_identifier("First", kind, name)
            ));
            assert_valid(&schema);
        }
    }
}

#[test]
fn space_padded_constraint_names_are_rejected_across_models_and_constraint_kinds() {
    for first_kind in CONSTRAINT_KINDS {
        for second_kind in CONSTRAINT_KINDS {
            let distinct = validate(&format!(
                "{TARGET_PAIR}{}{}",
                model_with_identifier("First", first_kind, "same"),
                model_with_identifier("Second", second_kind, "other"),
            ));
            assert_valid(&distinct);

            let space_padded = validate(&format!(
                "{TARGET_PAIR}{}{}",
                model_with_identifier("First", first_kind, "same"),
                model_with_identifier("Second", second_kind, "same "),
            ));
            assert_single_identifier_error(&space_padded, "must not end with an ASCII space");
        }
    }
}
